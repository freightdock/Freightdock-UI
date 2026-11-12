//! HTTPS by default ("HTTPS" in DECISIONS.md). Without a certificate of its own, Freightdock keeps a small certificate
//! authority of its own in `<data>/tls/` (`ca.pem`, `ca.key`), made on first start, and a server certificate from it
//! (`cert.pem`, `key.pem`) that names the host: `localhost`, `127.0.0.1`, `::1`, the machine's name, the addresses it
//! listens on and whatever `FREIGHTDOCK_TLS_NAMES` adds. A person trusts the CA once (`GET /api/tls/ca.pem`), and every
//! certificate it signs from then on checks out, in a browser, in curl and in Node (Claude Code).
//!
//! The certificate is made again at a start when its names no longer cover the host's, when it nears its end, or when
//! it is the old one without names that Freightdock made before it had a CA. `FREIGHTDOCK_TLS_CERT` and `FREIGHTDOCK_TLS_KEY`
//! name a certificate of the person's own instead, and that is never touched; nor is a certificate at the default path
//! that Freightdock did not make.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use rcgen::SanType;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
use time::{Duration, OffsetDateTime};

use crate::ca::Ca;
use crate::error::ApiError;
use crate::state::App;

/// How long a server certificate is good for: within the 825 days Apple's platforms accept for a CA a person added.
pub const DAYS: i64 = 825;
/// A certificate with less than this left is made again at the next start.
pub const RENEW_BEFORE_DAYS: i64 = 30;
/// What rcgen named the certificates Freightdock made before it had a CA (self-signed, so no client could be told to trust
/// them as a CA; the first had no subject alternative names at all).
const OLD_NAME: &str = "rcgen self signed cert";

/// The certificate and key the web server answers with.
#[derive(Debug, Clone, PartialEq)]
pub struct Files {
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Named by `FREIGHTDOCK_TLS_CERT` or `FREIGHTDOCK_TLS_KEY`: the person's own, never made or replaced.
    pub own: bool,
}

/// A name a certificate is good for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Name {
    Dns(String),
    Ip(IpAddr),
}

impl Name {
    fn san(&self) -> Result<SanType> {
        Ok(match self {
            Name::Dns(d) => SanType::DnsName(d.as_str().try_into().with_context(|| format!("\"{d}\" is not a DNS name"))?),
            Name::Ip(ip) => SanType::IpAddress(*ip),
        })
    }

    fn of(san: &SanType) -> Option<Name> {
        match san {
            SanType::DnsName(d) => Some(Name::Dns(d.as_str().to_ascii_lowercase())),
            SanType::IpAddress(ip) => Some(Name::Ip(*ip)),
            _ => None,
        }
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Name::Dns(d) => f.write_str(d),
            Name::Ip(ip) => write!(f, "{ip}"),
        }
    }
}

/// Whether `s` is a host name a certificate can carry: labels of letters, digits and hyphens, a wildcard first label.
fn dns_name(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    s.len() <= 253
        && labels.iter().enumerate().all(|(i, l)| {
            (i == 0 && *l == "*" && labels.len() > 2)
                || (!l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        })
}

/// The names the certificate is to have: the loopback names, the host's name, the address Freightdock listens on (every
/// interface's when it listens on all of them), and `extra` (`FREIGHTDOCK_TLS_NAMES`, comma-separated names and addresses).
pub fn names(bind: IpAddr, hostname: Option<&str>, interfaces: &[IpAddr], extra: Option<&str>) -> Result<Vec<Name>> {
    let mut out = vec![Name::Dns("localhost".into()), Name::Ip(IpAddr::from([127, 0, 0, 1])), Name::Ip(std::net::Ipv6Addr::LOCALHOST.into())];
    let mut push = |n: Name| {
        if !out.contains(&n) {
            out.push(n);
        }
    };
    if let Some(h) = hostname.map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase()).filter(|h| !h.is_empty() && dns_name(h)) {
        push(Name::Dns(h));
    }
    // Link-local addresses need a scope a URL cannot carry; loopback is named above.
    let reachable = |ip: &IpAddr| !ip.is_loopback() && !ip.is_unspecified() && !matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
    if bind.is_unspecified() {
        // 0.0.0.0 takes IPv4 only; [::] takes both.
        for ip in interfaces.iter().filter(|ip| reachable(ip) && (bind.is_ipv6() || ip.is_ipv4())) {
            push(Name::Ip(*ip));
        }
    } else if reachable(&bind) {
        push(Name::Ip(bind));
    }
    for item in extra.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let item = item.trim_start_matches('[').trim_end_matches(']');
        match item.parse::<IpAddr>() {
            Ok(ip) => push(Name::Ip(ip)),
            Err(_) if dns_name(&item.to_ascii_lowercase()) => push(Name::Dns(item.to_ascii_lowercase())),
            Err(_) => anyhow::bail!("FREIGHTDOCK_TLS_NAMES: \"{item}\" is not a host name or an IP address"),
        }
    }
    Ok(out)
}

/// This machine's name, as the kernel has it.
pub fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is ours and its length is given; gethostname writes at most that many bytes.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8(buf[..end].to_vec()).ok().filter(|h| !h.is_empty());
        }
    }
    std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string()).filter(|h| !h.is_empty())
}

/// Every address of this machine's interfaces.
pub fn interfaces() -> Vec<IpAddr> {
    let mut out = Vec::new();
    #[cfg(unix)]
    {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills in a list we walk read-only and give back with freeifaddrs, once.
        unsafe {
            if libc::getifaddrs(&mut list) != 0 {
                return out;
            }
            let mut at = list;
            while !at.is_null() {
                let a = (*at).ifa_addr;
                if !a.is_null() {
                    match i32::from((*a).sa_family) {
                        libc::AF_INET => {
                            let v4 = &*(a as *const libc::sockaddr_in);
                            out.push(IpAddr::from(u32::from_be(v4.sin_addr.s_addr).to_be_bytes()));
                        }
                        libc::AF_INET6 => {
                            let v6 = &*(a as *const libc::sockaddr_in6);
                            out.push(IpAddr::from(v6.sin6_addr.s6_addr));
                        }
                        _ => {}
                    }
                }
                at = (*at).ifa_next;
            }
            libc::freeifaddrs(list);
        }
    }
    out
}

/// What to do with the certificate at the default path.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Keep,
    /// Make it (again), and why.
    Make(String),
    /// Not one Freightdock made: left as it is.
    Foreign,
}

fn parse(pem: &str) -> Option<(CertificateDer<'static>, rcgen::CertificateParams)> {
    let der = CertificateDer::from_pem_slice(pem.as_bytes()).ok()?;
    let params = rcgen::CertificateParams::from_ca_cert_der(&der).ok()?;
    Some((der, params))
}

fn unix(t: OffsetDateTime) -> UnixTime {
    UnixTime::since_unix_epoch(std::time::Duration::from_secs(t.unix_timestamp().max(0) as u64))
}

/// Whether `ca` signed `der` (checked at a time inside the certificate's life, so an expired one is still known).
fn signed_by(ca: &Ca, der: &CertificateDer<'_>, params: &rcgen::CertificateParams) -> bool {
    let mut roots = rustls::RootCertStore::empty();
    if roots.add(CertificateDer::from(ca.der().to_vec())).is_err() {
        return false;
    }
    let Ok(v) = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build() else { return false };
    let Some(name) = params.subject_alt_names.iter().find_map(|s| match s {
        SanType::DnsName(d) => ServerName::try_from(d.as_str().to_string()).ok(),
        SanType::IpAddress(ip) => Some(ServerName::IpAddress((*ip).into())),
        _ => None,
    }) else {
        return false;
    };
    v.verify_server_cert(der, &[], &name, &[], unix(params.not_before + Duration::minutes(1))).is_ok()
}

/// The renewal rule: what to do, given the certificate there is (if any), the CA, the names wanted and the time.
pub fn verdict(cert_pem: Option<&str>, has_key: bool, ca: &Ca, wanted: &[Name], now: OffsetDateTime) -> Verdict {
    let Some(pem) = cert_pem else { return Verdict::Make("there was none".into()) };
    let Some((der, params)) = parse(pem) else { return Verdict::Foreign };
    if !signed_by(ca, &der, &params) {
        let old = params.distinguished_name.get(&rcgen::DnType::CommonName).is_some_and(|cn| format!("{cn:?}").contains(OLD_NAME));
        return if old { Verdict::Make("it was the old self-signed one, without a CA".into()) } else { Verdict::Foreign };
    }
    if !has_key {
        return Verdict::Make("its key was missing".into());
    }
    let has: Vec<Name> = params.subject_alt_names.iter().filter_map(Name::of).collect();
    let missing: Vec<String> = wanted.iter().filter(|n| !has.contains(n)).map(Name::to_string).collect();
    if !missing.is_empty() {
        return Verdict::Make(format!("it did not name {}", missing.join(", ")));
    }
    if params.not_after - now < Duration::days(RENEW_BEFORE_DAYS) {
        return Verdict::Make(format!("it ends {}", params.not_after.date()));
    }
    Verdict::Keep
}

/// A certificate from `ca` for `names`, good for `days` from a day before `now` (clocks differ).
pub fn issue(ca: &Ca, names: &[Name], now: OffsetDateTime, days: i64) -> Result<(String, String)> {
    let sans = names.iter().map(Name::san).collect::<Result<Vec<_>>>()?;
    let cn = names.iter().find_map(|n| match n {
        Name::Dns(d) if d != "localhost" => Some(d.clone()),
        _ => None,
    });
    let start = now - Duration::days(1);
    ca.issue_server(&cn.unwrap_or_else(|| "localhost".into()), sans, start, start + Duration::days(days))
}

/// The CA's name: Freightdock's, with the host's so a person who trusts several can tell them apart.
fn ca_name(names: &[Name]) -> String {
    match names.iter().find_map(|n| match n {
        Name::Dns(d) if d != "localhost" => Some(d.as_str()),
        _ => None,
    }) {
        Some(h) => format!("Freightdock local CA ({h})"),
        None => "Freightdock local CA".into(),
    }
}

/// The web server's certificate ready at start. The person's own (`own`) must be there and is only read. Otherwise the
/// CA in `dir` is made or opened, and the certificate made again when [`verdict`] says so. Answers the CA's PEM, for
/// `GET /api/tls/ca.pem`, when the certificate served is the CA's.
pub fn prepare(files: &Files, dir: &Path, wanted: &[Name]) -> Result<Option<String>> {
    if files.own {
        for p in [&files.cert, &files.key] {
            anyhow::ensure!(p.exists(), "{} is not there: FREIGHTDOCK_TLS_CERT and FREIGHTDOCK_TLS_KEY name a certificate and key of your own, which Freightdock never makes", p.display());
        }
        return Ok(None);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    if let Some(parent) = files.cert.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ca = Ca::open_or_make_named(dir, &ca_name(wanted))?;
    let pem = std::fs::read_to_string(&files.cert).ok();
    let now = OffsetDateTime::now_utc();
    match verdict(pem.as_deref(), files.key.exists(), &ca, wanted, now) {
        Verdict::Keep => Ok(Some(ca.pem().to_string())),
        Verdict::Foreign => {
            tracing::warn!("{} is not a certificate Freightdock made, so it is left as it is; name it with FREIGHTDOCK_TLS_CERT and FREIGHTDOCK_TLS_KEY", files.cert.display());
            Ok(None)
        }
        Verdict::Make(why) => {
            let (cert, key) = issue(&ca, wanted, now, DAYS)?;
            write_private(&files.key, key.as_bytes())?;
            std::fs::write(&files.cert, cert).with_context(|| format!("writing {}", files.cert.display()))?;
            let list: Vec<String> = wanted.iter().map(Name::to_string).collect();
            tracing::info!("made the HTTPS certificate in {} ({why}) for {}, from the CA in {}", files.cert.display(), list.join(", "), dir.join("ca.pem").display());
            Ok(Some(ca.pem().to_string()))
        }
    }
}

/// Writes a file only its owner can read.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    o.open(path).and_then(|mut f| f.write_all(bytes)).with_context(|| format!("writing {}", path.display()))
}

pub fn acceptor(cert: &Path, key: &Path) -> Result<tokio_rustls::TlsAcceptor> {
    let certs = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("reading {}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key).with_context(|| format!("reading {}", key.display()))?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("the certificate and key do not belong together")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

pub fn router() -> Router<App> {
    Router::new().route("/api/tls/ca.pem", get(ca_pem))
}

/// `GET /api/tls/ca.pem`: the CA that signed the certificate served, to trust once. Public: it is a certificate.
async fn ca_pem(State(app): State<App>) -> Response {
    match app.tls_ca.get() {
        Some(pem) => (
            [(header::CONTENT_TYPE, "application/x-pem-file"), (header::CONTENT_DISPOSITION, "attachment; filename=\"freightdock-ca.pem\""), (header::CACHE_CONTROL, "no-cache")],
            pem.clone(),
        )
            .into_response(),
        None => ApiError::new(StatusCode::NOT_FOUND, "This Freightdock has no certificate authority of its own: it serves a certificate it was given, or plain HTTP behind a proxy.").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn dns(s: &str) -> Name {
        Name::Dns(s.into())
    }

    /// Whether a client that trusts `ca` (and nothing else) takes `cert_pem` for `name`, as curl or Node would.
    fn verifies(ca: &Ca, cert_pem: &str, name: &str) -> bool {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(ca.der().to_vec())).unwrap();
        let v = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build().unwrap();
        let leaf = CertificateDer::from_pem_slice(cert_pem.as_bytes()).unwrap();
        v.verify_server_cert(&leaf, &[], &ServerName::try_from(name.to_string()).unwrap(), &[], UnixTime::now()).is_ok()
    }

    fn files(dir: &Path) -> Files {
        Files { cert: dir.join("cert.pem"), key: dir.join("key.pem"), own: false }
    }

    #[test]
    fn the_names_are_loopback_the_host_the_bind_address_and_the_extra_ones() {
        let n = names(ip("0.0.0.0"), Some("Boat"), &[ip("127.0.0.1"), ip("192.0.2.20"), ip("fd00::5"), ip("fe80::1")], Some(" freightdock.lan, 198.51.100.9 ,[2001:db8::1]")).unwrap();
        assert_eq!(n, vec![dns("localhost"), Name::Ip(ip("127.0.0.1")), Name::Ip(ip("::1")), dns("boat"), Name::Ip(ip("192.0.2.20")), dns("freightdock.lan"), Name::Ip(ip("198.51.100.9")), Name::Ip(ip("2001:db8::1"))],
            "0.0.0.0 takes IPv4 only: every IPv4 interface but loopback");
        let all = names(ip("::"), None, &[ip("192.0.2.20"), ip("fd00::5"), ip("fe80::1")], None).unwrap();
        assert!(all.contains(&Name::Ip(ip("fd00::5"))) && all.contains(&Name::Ip(ip("192.0.2.20"))), "[::] takes both");
        assert!(!all.contains(&Name::Ip(ip("fe80::1"))), "no link-local address: a URL cannot carry its scope");
        let one = names(ip("192.0.2.20"), Some("localhost"), &[ip("198.51.100.11")], None).unwrap();
        assert_eq!(one, vec![dns("localhost"), Name::Ip(ip("127.0.0.1")), Name::Ip(ip("::1")), Name::Ip(ip("192.0.2.20"))], "bound to one address: that one, no other interface");
        assert!(names(ip("127.0.0.1"), None, &[], Some("not a name!")).is_err());
        assert!(names(ip("127.0.0.1"), None, &[], Some("*.lan.example")).is_ok());
    }

    #[test]
    fn the_certificate_checks_out_against_the_ca_for_localhost_and_127_0_0_1() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let wanted = names(ip("0.0.0.0"), Some("boat"), &[ip("192.0.2.20")], Some("freightdock.lan")).unwrap();
        let ca_pem = prepare(&f, dir.path(), &wanted).unwrap().expect("a self-made certificate has its CA");
        let ca = Ca::open_or_make_named(dir.path(), "Test CA").unwrap();
        assert_eq!(ca.pem(), ca_pem, "the CA kept beside it");
        let cert = std::fs::read_to_string(&f.cert).unwrap();
        for name in ["localhost", "127.0.0.1", "::1", "boat", "192.0.2.20", "freightdock.lan"] {
            assert!(verifies(&ca, &cert, name), "good for {name}");
        }
        assert!(!verifies(&ca, &cert, "elsewhere.example"), "and for nothing else");
        assert!(!verifies(&Ca::generate_named("Test CA").unwrap(), &cert, "localhost"), "and only through this CA");
        let (_, p) = parse(&cert).unwrap();
        assert!(p.not_after - p.not_before <= Duration::days(DAYS), "825 days at most");
        assert!(format!("{:?}", p.distinguished_name.get(&rcgen::DnType::CommonName)).contains("boat"), "the host's name as its common name");
        assert!(acceptor(&f.cert, &f.key).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f.key).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(dir.path().join("ca.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Started again with the same names: the same certificate.
        prepare(&f, dir.path(), &wanted).unwrap();
        assert_eq!(std::fs::read_to_string(&f.cert).unwrap(), cert);
    }

    #[test]
    fn it_is_made_again_when_its_names_no_longer_cover_the_host_or_it_nears_its_end() {
        let ca = Ca::generate_named("Test CA").unwrap();
        let now = OffsetDateTime::now_utc();
        let wanted = names(ip("127.0.0.1"), Some("boat"), &[], None).unwrap();
        let (cert, _) = issue(&ca, &wanted, now, DAYS).unwrap();
        assert_eq!(verdict(Some(&cert), true, &ca, &wanted, now), Verdict::Keep);
        let more = names(ip("192.0.2.30"), Some("boat"), &[], None).unwrap();
        assert!(matches!(verdict(Some(&cert), true, &ca, &more, now), Verdict::Make(w) if w.contains("192.0.2.30")), "the address moved");
        let fewer = names(ip("127.0.0.1"), None, &[], None).unwrap();
        assert_eq!(verdict(Some(&cert), true, &ca, &fewer, now), Verdict::Keep, "a name more than needed is no reason");
        assert!(matches!(verdict(Some(&cert), true, &ca, &wanted, now + Duration::days(DAYS - RENEW_BEFORE_DAYS)), Verdict::Make(_)), "near its end");
        assert!(matches!(verdict(Some(&cert), true, &ca, &wanted, now + Duration::days(DAYS + 10)), Verdict::Make(_)), "past its end, still known as Freightdock's");
        assert!(matches!(verdict(None, false, &ca, &wanted, now), Verdict::Make(_)));
        assert!(matches!(verdict(Some(&cert), false, &ca, &wanted, now), Verdict::Make(_)), "its key gone");
    }

    #[test]
    fn the_old_certificate_without_names_is_replaced_and_a_stranger_s_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let wanted = names(ip("127.0.0.1"), Some("boat"), &[], None).unwrap();
        // What Freightdock made before it had a CA: rcgen's default name, no subject alternative names.
        let old = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap().self_signed(&rcgen::KeyPair::generate().unwrap()).unwrap();
        std::fs::write(&f.cert, old.pem()).unwrap();
        std::fs::write(&f.key, "old key").unwrap();
        assert!(prepare(&f, dir.path(), &wanted).unwrap().is_some());
        let ca = Ca::open_or_make_named(dir.path(), "Test CA").unwrap();
        assert!(verifies(&ca, &std::fs::read_to_string(&f.cert).unwrap(), "localhost"), "replaced by one from the CA");

        // Somebody's own certificate put at the default path: Freightdock did not make it, so it stays.
        let theirs = rcgen::generate_simple_self_signed(vec!["mine.example".to_string()]).unwrap();
        let mut p = rcgen::CertificateParams::new(vec!["mine.example".to_string()]).unwrap();
        p.distinguished_name.push(rcgen::DnType::CommonName, "mine.example");
        let theirs_cert = p.self_signed(&theirs.key_pair).unwrap().pem();
        std::fs::write(&f.cert, &theirs_cert).unwrap();
        assert_eq!(prepare(&f, dir.path(), &wanted).unwrap(), None, "no CA to hand out for a certificate it did not sign");
        assert_eq!(std::fs::read_to_string(&f.cert).unwrap(), theirs_cert);
    }

    #[test]
    fn a_certificate_of_one_s_own_is_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let own = tempfile::tempdir().unwrap();
        let made = rcgen::generate_simple_self_signed(vec!["freightdock.example".to_string()]).unwrap();
        let f = Files { cert: own.path().join("c.pem"), key: own.path().join("k.pem"), own: true };
        std::fs::write(&f.cert, made.cert.pem()).unwrap();
        std::fs::write(&f.key, made.key_pair.serialize_pem()).unwrap();
        let before = (std::fs::read(&f.cert).unwrap(), std::fs::read(&f.key).unwrap(), std::fs::metadata(&f.cert).unwrap().modified().unwrap());
        let wanted = names(ip("0.0.0.0"), Some("boat"), &[ip("198.51.100.1")], None).unwrap();
        assert_eq!(prepare(&f, dir.path(), &wanted).unwrap(), None, "no CA: the certificate is not Freightdock's");
        assert_eq!((std::fs::read(&f.cert).unwrap(), std::fs::read(&f.key).unwrap(), std::fs::metadata(&f.cert).unwrap().modified().unwrap()), before);
        assert!(!dir.path().join("ca.pem").exists(), "no CA made for it");
        let gone = Files { cert: own.path().join("missing.pem"), key: f.key.clone(), own: true };
        let e = prepare(&gone, dir.path(), &wanted).unwrap_err().to_string();
        assert!(e.contains("missing.pem") && e.contains("never makes"), "{e}");
        assert!(!gone.cert.exists());
    }

    #[test]
    fn this_machine_has_a_name_and_addresses() {
        assert!(hostname().is_some());
        assert!(interfaces().iter().any(|i| i.is_loopback()), "lo is always there");
    }

    #[tokio::test]
    async fn the_ca_is_public_and_there_only_when_freightdock_signed_the_certificate() {
        use axum::body::Body;
        use axum::http::Request;
        let app = crate::api::tests::app();
        let get = || Request::get("/api/tls/ca.pem").header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        let (s, _, b) = crate::api::tests::call(&app, get()).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "plain HTTP, or a certificate of one's own: none");
        assert!(b["error"].as_str().unwrap().contains("no certificate authority"), "{b}");
        let ca = Ca::generate_named("Test CA").unwrap();
        app.tls_ca.set(ca.pem().to_string()).unwrap();
        let r = tower::ServiceExt::oneshot(crate::api::router(app.clone()), get()).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "no sign-in: it is fetched before anybody trusts this server");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/x-pem-file");
        assert!(r.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().contains("freightdock-ca.pem"));
        let body = http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes();
        assert_eq!(body, ca.pem().as_bytes());
    }
}
