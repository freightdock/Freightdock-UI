//! Portainer's API, read only: the one method this client has is GET ("Portainer" 2). It signs in with an access token
//! in `X-API-Key`, as Portainer CE and Business 2.x take it, follows no redirect (the token goes to the address given and
//! nowhere else), and trusts Portainer's certificate as the administrator chose: the usual check, a certificate they
//! pasted (pinned, or a CA), or no check at all, an explicit choice that is recorded ("Portainer" 3).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// How Freightdock reaches a Portainer: its address, the token, and how its certificate is trusted.
#[derive(Clone, Debug, Default)]
pub struct Conn {
    pub url: String,
    pub token: String,
    /// A pasted PEM certificate: Portainer's own (pinned) or the CA that signed it.
    pub ca: String,
    pub skip_verify: bool,
}

/// The address as Freightdock keeps it: http or https, a host, maybe a path (behind a proxy), no trailing slash.
pub fn check_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let bad = || "Portainer's address starts https:// (or http://), such as https://portainer.example:9443.".to_string();
    let parsed = url::Url::parse(u).map_err(|_| bad())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) || parsed.query().is_some() || !parsed.username().is_empty() {
        return Err(bad());
    }
    Ok(u.to_string())
}

/// The certificates in a pasted PEM; an error in words when it is not one.
pub fn pem_certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let c = CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<Vec<_>, _>>().map_err(|_| "That is not a PEM certificate.".to_string())?;
    if c.is_empty() {
        return Err("That is not a PEM certificate.".into());
    }
    Ok(c)
}

/// Accepts Portainer's certificate when it is one pasted (pinned: a self-signed certificate is trusted as itself, for
/// any name), else checks it against the pasted certificates as CAs; or accepts anything when the check is skipped.
/// The handshake's signatures are always checked.
#[derive(Debug)]
struct Trust {
    pinned: Vec<CertificateDer<'static>>,
    webpki: Option<Arc<rustls::client::WebPkiServerVerifier>>,
    any: bool,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Trust {
    fn verify_server_cert(&self, end: &CertificateDer<'_>, inter: &[CertificateDer<'_>], name: &ServerName<'_>, ocsp: &[u8], now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if self.any || self.pinned.iter().any(|p| p.as_ref() == end.as_ref()) {
            return Ok(ServerCertVerified::assertion());
        }
        match &self.webpki {
            Some(v) => v.verify_server_cert(end, inter, name, ocsp, now),
            None => Err(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)),
        }
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(ca: &str, skip: bool) -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let pinned = if ca.trim().is_empty() { Vec::new() } else { pem_certs(ca)? };
    let mut roots = rustls::RootCertStore::empty();
    for c in &pinned {
        let _ = roots.add(c.clone());
    }
    let webpki = if roots.is_empty() { None } else { rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().ok() };
    let trust = Trust { pinned, webpki, any: skip, provider: provider.clone() };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(trust))
        .with_no_client_auth())
}

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// Portainer's error body, `{"message": ..., "details": ...}`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Said {
    message: String,
    details: String,
}

impl Client {
    pub fn new(c: &Conn) -> Result<Client, String> {
        let base = check_url(&c.url)?;
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(20)).connect_timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none());
        if c.skip_verify || !c.ca.trim().is_empty() {
            b = b.use_preconfigured_tls(tls_config(&c.ca, c.skip_verify)?);
        }
        Ok(Client { base, token: c.token.clone(), http: b.build().map_err(|e| format!("Freightdock could not set up its client for Portainer: {e}"))? })
    }

    /// `GET <path>` as JSON. This is the only request the client can make.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let r = self.http.get(format!("{}{path}", self.base)).header("X-API-Key", &self.token).header("Accept", "application/json").send().await.map_err(words)?;
        let status = r.status();
        let bytes = r.bytes().await.map_err(words)?;
        if !status.is_success() {
            let said: Said = serde_json::from_slice(&bytes).unwrap_or_default();
            let why = [said.message, said.details].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(": ");
            return Err(match status.as_u16() {
                401 => "Portainer refused the access token.".into(),
                403 => "Portainer says that token may not read this: use an administrator's token.".into(),
                s if (300..400).contains(&s) => format!("Portainer answered {path} with a redirect ({s}): give the address it is served at."),
                s => format!("Portainer answered {s} for {path}{}", if why.is_empty() { ".".into() } else { format!(": {why}.") }),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("Portainer's answer to {path} could not be read: {e}."))
    }

    pub async fn status(&self) -> Result<Status, String> {
        self.get("/api/system/status").await
    }
    pub async fn endpoints(&self) -> Result<Vec<Endpoint>, String> {
        self.get("/api/endpoints").await
    }
    pub async fn docker_info(&self, endpoint: i64) -> Result<Info, String> {
        self.get(&format!("/api/endpoints/{endpoint}/docker/info")).await
    }
    pub async fn stacks(&self) -> Result<Vec<Stack>, String> {
        self.get("/api/stacks").await
    }
    pub async fn stack_file(&self, id: i64) -> Result<String, String> {
        Ok(self.get::<StackFile>(&format!("/api/stacks/{id}/file")).await?.stack_file_content)
    }
    pub async fn custom_templates(&self) -> Result<Vec<CustomTemplate>, String> {
        self.get("/api/custom_templates").await
    }
    pub async fn custom_template_file(&self, id: i64) -> Result<String, String> {
        Ok(self.get::<TemplateFile>(&format!("/api/custom_templates/{id}/file")).await?.file_content)
    }
    pub async fn registries(&self) -> Result<Vec<Registry>, String> {
        self.get("/api/registries").await
    }
    pub async fn groups(&self) -> Result<Vec<Group>, String> {
        self.get("/api/endpoint_groups").await
    }
    pub async fn tags(&self) -> Result<Vec<Tag>, String> {
        self.get("/api/tags").await
    }
    pub async fn users(&self) -> Result<Vec<User>, String> {
        self.get("/api/users").await
    }
    pub async fn teams(&self) -> Result<Vec<Team>, String> {
        self.get("/api/teams").await
    }
    pub async fn memberships(&self) -> Result<Vec<Membership>, String> {
        self.get("/api/team_memberships").await
    }
}

/// A transport error in words: a certificate Freightdock does not trust says what to do about it.
fn words(e: reqwest::Error) -> String {
    if e.is_timeout() {
        return "Portainer took too long to answer.".into();
    }
    let mut chain = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        chain.push_str(": ");
        chain.push_str(&s.to_string());
        src = s.source();
    }
    if chain.contains("certificate") || chain.contains("UnknownIssuer") {
        return format!("Portainer's certificate is not trusted ({chain}). Paste its certificate, or choose to skip the check.");
    }
    format!("Portainer did not answer: {chain}")
}

/// `null` as the type's default: Portainer writes `null` for an empty list.
fn nullable<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Status {
    pub version: String,
    #[serde(rename = "InstanceID")]
    pub instance_id: String,
}

/// An environment ("endpoint"): 1 Docker on its socket, 2 an agent, 4 an Edge agent; 3 Azure, 5 to 7 Kubernetes.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Endpoint {
    pub id: i64,
    pub name: String,
    #[serde(rename = "Type")]
    pub kind: i64,
    #[serde(rename = "URL")]
    pub url: String,
    pub group_id: i64,
    #[serde(deserialize_with = "nullable")]
    pub tag_ids: Vec<i64>,
    /// 1 up, 2 down.
    pub status: i64,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Info {
    #[serde(rename = "ID")]
    pub id: String,
    pub name: String,
    pub server_version: String,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct GitAuth {
    pub username: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct GitConfig {
    #[serde(rename = "URL")]
    pub url: String,
    pub reference_name: String,
    pub config_file_path: String,
    pub authentication: Option<GitAuth>,
    #[serde(rename = "TLSSkipVerify")]
    pub tls_skip_verify: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct AutoUpdate {
    /// A Go duration, `5m`, `1h30m`; empty when only the webhook is on.
    pub interval: String,
    pub webhook: String,
}

/// A stack: Type 1 Swarm, 2 Compose, 3 Kubernetes; Status 1 active, 2 inactive.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Stack {
    pub id: i64,
    pub name: String,
    #[serde(rename = "Type")]
    pub kind: i64,
    pub endpoint_id: i64,
    pub entry_point: String,
    #[serde(deserialize_with = "nullable")]
    pub env: Vec<EnvVar>,
    pub status: i64,
    pub project_path: String,
    #[serde(deserialize_with = "nullable")]
    pub additional_files: Vec<String>,
    pub auto_update: Option<AutoUpdate>,
    pub git_config: Option<GitConfig>,
    pub webhook: String,
    pub update_date: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StackFile {
    stack_file_content: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct TemplateVar {
    pub name: String,
    pub label: String,
    pub description: String,
    #[serde(rename = "defaultValue")]
    pub default_value: String,
}

/// A custom template: Type 1 Swarm, 2 Compose, 3 Kubernetes.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct CustomTemplate {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub note: String,
    pub logo: String,
    #[serde(rename = "Type")]
    pub kind: i64,
    #[serde(deserialize_with = "nullable")]
    pub variables: Vec<TemplateVar>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TemplateFile {
    file_content: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Ecr {
    pub region: String,
}

/// A registry: Type 1 Quay, 2 Azure, 3 custom, 4 GitLab, 5 ProGet, 6 Docker Hub, 7 ECR, 8 GitHub. Portainer never gives
/// its password out.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Registry {
    pub id: i64,
    pub name: String,
    #[serde(rename = "Type")]
    pub kind: i64,
    #[serde(rename = "URL")]
    pub url: String,
    pub authentication: bool,
    pub username: String,
    pub ecr: Option<Ecr>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub description: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Tag {
    #[serde(rename = "ID")]
    pub id: i64,
    pub name: String,
    /// The environments that have it, by id as a string.
    #[serde(deserialize_with = "nullable")]
    pub endpoints: HashMap<String, bool>,
}

/// A user: Role 1 administrator, 2 standard user.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: i64,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Team {
    pub id: i64,
    pub name: String,
}

/// Who is in which team: Role 1 leader, 2 member.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Membership {
    #[serde(rename = "UserID")]
    pub user_id: i64,
    #[serde(rename = "TeamID")]
    pub team_id: i64,
    #[serde(rename = "Role")]
    pub role: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portainer_s_recorded_answers_are_read() {
        let e: Vec<Endpoint> = serde_json::from_str(include_str!("fixtures/endpoints.json")).unwrap();
        assert_eq!(e.len(), 7);
        assert_eq!((e[0].id, e[0].name.as_str(), e[0].kind, e[0].url.as_str()), (2, "alpha", 1, "unix:///var/run/docker.sock"));
        assert_eq!((e[1].kind, e[1].url.as_str(), e[1].status), (2, "tcp://192.0.2.6:9001", 1));
        let s: Vec<Stack> = serde_json::from_str(include_str!("fixtures/stacks.json")).unwrap();
        let git = s.iter().find(|s| s.name == "webapp").unwrap();
        let g = git.git_config.as_ref().unwrap();
        assert_eq!((g.url.as_str(), g.reference_name.as_str(), g.config_file_path.as_str()), ("http://git.example.test/owner/webapp", "refs/heads/master", "compose.yaml"));
        assert_eq!(g.authentication.as_ref().unwrap().username, "owner");
        assert_eq!(git.auto_update.as_ref().unwrap().interval, "5m");
        assert_eq!(git.env.len(), 2);
        let nginx = s.iter().find(|s| s.name == "nginx").unwrap();
        assert_eq!((nginx.kind, nginx.endpoint_id, nginx.project_path.as_str(), nginx.env.len()), (2, 3, "/data/compose/147", 5));
        assert!(nginx.additional_files.is_empty() && nginx.git_config.is_none(), "null is empty");
        let i: Info = serde_json::from_str(include_str!("fixtures/info.json")).unwrap();
        assert_eq!(i.id, "0f3c5e2a-7b1d-4c8e-9a6f-2d4b8e1c7a90");
        let t: Vec<Tag> = serde_json::from_str(r#"[{"ID":4,"Name":"prod","Endpoints":{"3":true},"EndpointGroups":{}}]"#).unwrap();
        assert_eq!(t[0].endpoints.get("3"), Some(&true));
        let r: Vec<Registry> = serde_json::from_str(r#"[{"Id":1,"Name":"ecr","Type":7,"URL":"1.dkr.ecr.eu-north-1.amazonaws.com","Authentication":true,"Username":"AKIA","Password":"","Ecr":{"Region":"eu-north-1"}}]"#).unwrap();
        assert_eq!(r[0].ecr.as_ref().unwrap().region, "eu-north-1");
    }

    #[test]
    fn an_address_is_http_or_https_with_a_host() {
        assert_eq!(check_url("https://portainer.example:9443/").unwrap(), "https://portainer.example:9443");
        assert_eq!(check_url(" http://192.0.2.4:9000/portainer ").unwrap(), "http://192.0.2.4:9000/portainer");
        for bad in ["portainer.example", "ftp://x", "https://", "https://u:p@x", "https://x/?a=1"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    /// An HTTPS server on loopback with a fresh self-signed certificate, answering `/api/system/status`; its address and
    /// the certificate.
    async fn https() -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        std::fs::write(&cert, made.cert.pem()).unwrap();
        std::fs::write(&key, made.key_pair.serialize_pem()).unwrap();
        let acceptor = crate::tls::acceptor(&cert, &key).unwrap();
        let pem = std::fs::read_to_string(&cert).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                    let mut buf = [0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    let body = r#"{"Version":"2.45.3","InstanceID":"i"}"#;
                    let _ = tls.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (format!("https://127.0.0.1:{}", addr.port()), pem)
    }

    #[tokio::test]
    async fn a_self_signed_portainer_is_trusted_pasted_or_by_choice_and_not_otherwise() {
        let (url, pem) = https().await;
        let conn = |ca: &str, skip: bool| Conn { url: url.clone(), token: "t".into(), ca: ca.into(), skip_verify: skip };
        let e = Client::new(&conn("", false)).unwrap().status().await.unwrap_err();
        assert!(e.starts_with("Portainer's certificate is not trusted") && e.ends_with("Paste its certificate, or choose to skip the check."), "{e}");
        assert_eq!(Client::new(&conn(&pem, false)).unwrap().status().await.unwrap().version, "2.45.3", "pinned, whatever name it is for");
        let other = rcgen::generate_simple_self_signed(vec!["other".into()]).unwrap().cert.pem();
        assert!(Client::new(&conn(&other, false)).unwrap().status().await.is_err(), "another certificate is not this one");
        assert!(Client::new(&conn("", true)).unwrap().status().await.is_ok(), "the check skipped, as chosen");
    }

    #[test]
    fn a_pasted_certificate_must_be_pem() {
        assert!(pem_certs("hello").is_err());
        let c = rcgen::generate_simple_self_signed(vec!["portainer".into()]).unwrap();
        assert_eq!(pem_certs(&c.cert.pem()).unwrap().len(), 1);
        assert!(tls_config(&c.cert.pem(), false).is_ok());
    }
}
