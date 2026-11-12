//! Freightdock's own certificate authority: the web server's, in `<data>/tls/` (`ca.pem`, `ca.key`, the key 0600), made
//! on first start and kept. It signs the server certificate `tls.rs` makes ("HTTPS" 3).

use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::CertificateDer;
use time::{Duration, OffsetDateTime};

pub struct Ca {
    cert: rcgen::Certificate,
    key: KeyPair,
    pem: String,
    der: Vec<u8>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ca_is_made_once_and_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = Ca::open_or_make_named(dir.path(), "Test CA").unwrap();
        let b = Ca::open_or_make_named(dir.path(), "Test CA").unwrap();
        assert_eq!((a.pem(), a.der()), (b.pem(), b.der()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.path().join("ca.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
