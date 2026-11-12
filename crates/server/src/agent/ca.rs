//! Freightdock's own certificate authorities. The agents' is made on first start into `<data>/ca/` (the key 0600) and
//! kept. It signs each agent's client certificate at enrolment, and the agent endpoint's server certificate (made again
//! at each start), which agents check against it: the join token pins its fingerprint ("Agent" 2, 3 in DECISIONS.md).
//! The web server's own, in `<data>/tls/`, is another of these, kept apart (`tls.rs`, "HTTPS" 3).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rcgen::{BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use time::{Duration, OffsetDateTime};

/// The agents' CA's name.
const AGENT_CA: &str = "Freightdock agent CA";

pub struct Ca {
    cert: rcgen::Certificate,
    key: KeyPair,
    pem: String,
    der: Vec<u8>,
}

/// An agent certificate, as handed out at enrolment.
pub struct Issued {
    pub pem: String,
    /// The SHA-256 of the certificate (DER): how the agent endpoint knows which environment a tunnel is for.
    pub sha256: String,
}

fn validity(p: &mut CertificateParams, years: i64) {
    let now = OffsetDateTime::now_utc();
    p.not_before = now - Duration::days(1);
    p.not_after = now + Duration::days(365 * years);
}

fn serial(p: &mut CertificateParams) {
    use rand::RngCore;
    let mut s = [0u8; 16];
    rand::rng().fill_bytes(&mut s);
    s[0] &= 0x7f;
    p.serial_number = Some(s.to_vec().into());
}

impl Ca {
    /// A new agents' CA, in memory.
    #[cfg(test)]
    pub fn generate() -> Result<Self> {
        Self::generate_named(AGENT_CA)
    }

    /// A new CA by this name, in memory.
    pub fn generate_named(name: &str) -> Result<Self> {
        let key = KeyPair::generate().context("making the CA's key")?;
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, name);
        p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
        validity(&mut p, 30);
        serial(&mut p);
        let cert = p.self_signed(&key).context("making the CA's certificate")?;
        Ok(Self::from_parts(cert, key))
    }

    fn from_parts(cert: rcgen::Certificate, key: KeyPair) -> Self {
        Ca { pem: cert.pem(), der: cert.der().to_vec(), cert, key }
    }

    /// The agents' CA in `dir` (`ca.pem`, `ca.key`), made first when it is not there.
    pub fn open_or_make(dir: &Path) -> Result<Self> {
        Self::open_or_make_named(dir, AGENT_CA)
    }

    /// The CA in `dir` (`ca.pem`, `ca.key`), made by this name first when it is not there.
    pub fn open_or_make_named(dir: &Path, name: &str) -> Result<Self> {
        let (cert, key) = (dir.join("ca.pem"), dir.join("ca.key"));
        if !cert.exists() || !key.exists() {
            std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
            let ca = Self::generate_named(name)?;
            crate::tls::write_private(&key, ca.key.serialize_pem().as_bytes())?;
            std::fs::write(&cert, &ca.pem).with_context(|| format!("writing {}", cert.display()))?;
            tracing::info!("made the certificate authority \"{name}\" in {}", dir.display());
            return Ok(ca);
        }
        let pem = std::fs::read_to_string(&cert).with_context(|| format!("reading {}", cert.display()))?;
        let key = KeyPair::from_pem(&std::fs::read_to_string(&key).with_context(|| format!("reading {}", key.display()))?).context("the CA's key")?;
        let params = CertificateParams::from_ca_cert_pem(&pem).context("the CA's certificate")?;
        // Signing needs only the CA's name, key identifier and key; the certificate given out stays the one on disk.
        let cert = params.self_signed(&key).context("the CA's certificate and key do not belong together")?;
        let mut ca = Self::from_parts(cert, key);
        ca.der = rustls::pki_types::pem::PemObject::from_pem_slice(pem.as_bytes()).map(|c: CertificateDer| c.to_vec()).context("the CA's certificate")?;
        ca.pem = pem;
        Ok(ca)
    }

    pub fn pem(&self) -> &str {
        &self.pem
    }

    /// The CA's certificate, DER.
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// A server certificate from this CA: `name` as its common name, `sans` as the names it is good for, server
    /// authentication only, good from `not_before` to `not_after`. Answers the certificate and its new key, as PEM.
    pub fn issue_server(&self, name: &str, sans: Vec<SanType>, not_before: OffsetDateTime, not_after: OffsetDateTime) -> Result<(String, String)> {
        let key = KeyPair::generate().context("making the certificate's key")?;
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, name);
        p.subject_alt_names = sans;
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.use_authority_key_identifier_extension = true;
        p.not_before = not_before;
        p.not_after = not_after;
        serial(&mut p);
        let cert = p.signed_by(&key, &self.cert, &self.key).context("signing the certificate")?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// The fingerprint a join token carries.
    pub fn sha256(&self) -> String {
        freightdock_tunnel::sha256_hex(&self.der)
    }

    /// Signs an agent's request for a certificate. Only its key is taken from it: the name, the use (client
    /// authentication only, so it can never pass for the agent endpoint) and the dates are Freightdock's.
    pub fn issue_agent(&self, csr_pem: &str, env: i64) -> Result<Issued> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem).context("not a certificate request")?;
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, format!("Freightdock agent {env}"));
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        p.use_authority_key_identifier_extension = true;
        validity(&mut p, 10);
        serial(&mut p);
        let cert = p.signed_by(&csr.public_key, &self.cert, &self.key).context("signing the agent's certificate")?;
        Ok(Issued { pem: cert.pem(), sha256: freightdock_tunnel::sha256_hex(cert.der()) })
    }

    /// The agent endpoint's TLS: a server certificate for `SERVER_NAME` from this CA, and a client certificate from this
    /// CA asked for but not required (enrolment comes without one; the tunnel refuses to start without one).
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        let key = KeyPair::generate()?;
        let mut p = CertificateParams::new(vec![freightdock_tunnel::SERVER_NAME.to_string()])?;
        p.distinguished_name.push(DnType::CommonName, "Freightdock agent endpoint");
        p.subject_alt_names = vec![SanType::DnsName(freightdock_tunnel::SERVER_NAME.try_into()?)];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.use_authority_key_identifier_extension = true;
        validity(&mut p, 10);
        serial(&mut p);
        let leaf = p.signed_by(&key, &self.cert, &self.key)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(self.der.clone()))?;
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone()).allow_unauthenticated().build()?;
        let chain = vec![leaf.der().clone(), CertificateDer::from(self.der.clone())];
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rustls::client::danger::ServerCertVerifier;

    pub fn csr() -> (KeyPair, String) {
        let key = KeyPair::generate().unwrap();
        let req = CertificateParams::new(vec![]).unwrap().serialize_request(&key).unwrap();
        (key, req.pem().unwrap())
    }

    #[test]
    fn the_ca_is_made_once_and_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = Ca::open_or_make(dir.path()).unwrap();
        let b = Ca::open_or_make(dir.path()).unwrap();
        assert_eq!(a.sha256(), b.sha256(), "the same CA, so tokens and agents stay good across restarts");
        assert_eq!(a.pem(), b.pem());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.path().join("ca.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // What the reloaded CA signs, the certificate on disk vouches for.
        let issued = b.issue_agent(&csr().1, 3).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(a.der.clone())).unwrap();
        let v = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build().unwrap();
        let leaf: CertificateDer = rustls::pki_types::pem::PemObject::from_pem_slice(issued.pem.as_bytes()).unwrap();
        v.verify_client_cert(&leaf, &[], rustls::pki_types::UnixTime::now()).expect("the agent's certificate checks out against the CA on disk");
    }

    #[test]
    fn an_agent_certificate_is_for_clients_only_and_has_its_fingerprint() {
        let ca = Ca::generate().unwrap();
        let issued = ca.issue_agent(&csr().1, 7).unwrap();
        let leaf: CertificateDer = rustls::pki_types::pem::PemObject::from_pem_slice(issued.pem.as_bytes()).unwrap();
        assert_eq!(issued.sha256, freightdock_tunnel::sha256_hex(&leaf));
        // As a server certificate for the endpoint's name it is refused: an agent cannot stand in for Freightdock.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(ca.der.clone())).unwrap();
        let sv = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build().unwrap();
        let name = rustls::pki_types::ServerName::try_from(freightdock_tunnel::SERVER_NAME).unwrap();
        assert!(sv.verify_server_cert(&leaf, &[], &name, &[], rustls::pki_types::UnixTime::now()).is_err());
        assert!(ca.issue_agent("not a request", 7).is_err());
        assert!(ca.server_config().is_ok());
    }
}
