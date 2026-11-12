//! A Docker host over TCP with TLS and a client certificate (`dockerd --tlsverify`, port 2376). Its socket
//! (`sockets`) dials the host and does the TLS handshake for every connection, with the key opened from its seal in
//! memory only: it is never written to disk in the clear ("Secrets" 1).

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};

use super::sockets::{DialFuture, Io};

/// A TLS host's address, `tcp://host:port` (or `https://`), as host and port.
pub fn host_port(url: &str) -> Result<(String, u16)> {
    let rest = url.strip_prefix("tcp://").or_else(|| url.strip_prefix("https://")).ok_or_else(|| anyhow!("The address starts tcp://, as tcp://docker.example:2376."))?;
    let rest = rest.trim_end_matches('/');
    let (host, port) = rest.rsplit_once(':').ok_or_else(|| anyhow!("The address needs its port, as tcp://docker.example:2376."))?;
    let port: u16 = port.parse().map_err(|_| anyhow!("\"{port}\" is not a port."))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.contains(['/', '@', ' ']) {
        anyhow::bail!("\"{host}\" is not a host name.");
    }
    Ok((host.to_string(), port))
}

fn certs(pem: &str, what: &str) -> Result<Vec<CertificateDer<'static>>> {
    let c = CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<Vec<_>, _>>().map_err(|_| anyhow!("The {what} is not a PEM certificate."))?;
    if c.is_empty() {
        anyhow::bail!("The {what} is not a PEM certificate.");
    }
    Ok(c)
}

/// The TLS settings for a host: its CA, Freightdock's client certificate and key for it. Each is checked here, so a bad
/// paste is refused when it is saved, not on the first request.
pub fn client_config(ca: &str, cert: &str, key: &str) -> Result<Arc<rustls::ClientConfig>, (&'static str, String)> {
    let mut roots = rustls::RootCertStore::empty();
    for c in certs(ca, "CA").map_err(|e| ("tls_ca", e.to_string()))? {
        roots.add(c).map_err(|_| ("tls_ca", "The CA certificate cannot be used.".to_string()))?;
    }
    let chain = certs(cert, "client certificate").map_err(|e| ("tls_cert", e.to_string()))?;
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).map_err(|_| ("tls_key", "The key is not a PEM private key.".to_string()))?;
    let c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| ("tls_ca", e.to_string()))?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|_| ("tls_key", "The key does not belong to the client certificate.".to_string()))?;
    Ok(Arc::new(c))
}

/// Dials `host:port` and shakes hands, each time it is asked.
pub fn dial(host: String, port: u16, config: Arc<rustls::ClientConfig>) -> Result<impl Fn() -> DialFuture + Send + Sync + 'static> {
    let name = ServerName::try_from(host.clone()).context("the host's name")?;
    let connector = tokio_rustls::TlsConnector::from(config);
    Ok(move || -> DialFuture {
        let (host, name, connector) = (host.clone(), name.clone(), connector.clone());
        Box::pin(async move {
            let within = std::time::Duration::from_secs(10);
            let tcp = tokio::time::timeout(within, tokio::net::TcpStream::connect((host.as_str(), port))).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
            let _ = tcp.set_nodelay(true);
            let tls = tokio::time::timeout(within, connector.connect(name, tcp)).await.map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
            Ok(Box::new(tls) as Box<dyn Io>)
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn an_address_is_tcp_host_and_port() {
        assert_eq!(host_port("tcp://docker.lan:2376").unwrap(), ("docker.lan".into(), 2376));
        assert_eq!(host_port("https://192.0.2.5:2376/").unwrap(), ("192.0.2.5".into(), 2376));
        assert_eq!(host_port("tcp://[fd00::5]:2376").unwrap(), ("fd00::5".into(), 2376));
        assert!(host_port("unix:///var/run/docker.sock").is_err());
        assert!(host_port("tcp://docker.lan").is_err());
        assert!(host_port("tcp://:2376").is_err());
        assert!(host_port("tcp://a:b").is_err());
    }

    /// A CA, and a certificate from it for `name`: what `dockerd --tlsverify` would be set up with.
    pub fn pki(name: &str, client: bool) -> (String, String, String) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec![]).unwrap();
        p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = p.self_signed(&ca_key).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        p.extended_key_usages = vec![if client { rcgen::ExtendedKeyUsagePurpose::ClientAuth } else { rcgen::ExtendedKeyUsagePurpose::ServerAuth }];
        let cert = p.signed_by(&key, &ca, &ca_key).unwrap();
        (ca.pem(), cert.pem(), key.serialize_pem())
    }

    #[test]
    fn pasted_certificates_are_checked_field_by_field() {
        let (ca, cert, key) = pki("client", true);
        assert!(client_config(&ca, &cert, &key).is_ok());
        assert_eq!(client_config("nonsense", &cert, &key).err().unwrap().0, "tls_ca");
        assert_eq!(client_config(&ca, "", &key).err().unwrap().0, "tls_cert");
        assert_eq!(client_config(&ca, &cert, "nope").err().unwrap().0, "tls_key");
        let (_, _, other_key) = pki("x", true);
        assert_eq!(client_config(&ca, &cert, &other_key).err().unwrap().0, "tls_key");
    }
}
