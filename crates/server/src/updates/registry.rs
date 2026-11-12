//! A registry, as the update check asks it: what digest a tag points at now. One HEAD on the manifest with every
//! manifest type in `Accept` (an index for a multi-arch image, or one image's manifest), signed in the way the
//! registry's `WWW-Authenticate` asks (a bearer token from its realm, anonymous unless there are credentials, or basic).
//! A HEAD is not counted as a pull by Docker Hub; the index itself is fetched (a GET) only when the local digest is not
//! the index's, to look for the local platform's own manifest in it ("Updates" in DECISIONS.md).

use serde_json::Value;
use sha2::Digest;

/// Every kind of manifest a tag may point at, newest first: an OCI index or Docker manifest list (multi-arch), or one
/// image's OCI or Docker manifest.
pub const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json";

const HUB: &str = "registry-1.docker.io";

/// The most of a manifest or an index that is read: real ones are a few KiB, and a registry that sends more is not
/// answering as a registry. A token answer is smaller still.
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
const MAX_TOKEN: usize = 64 * 1024;

/// An image reference split as a registry reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reference {
    /// `registry-1.docker.io` for Docker Hub, else the host (and port) as written.
    pub registry: String,
    /// `library/nginx` on Docker Hub for `nginx`.
    pub repository: String,
    pub tag: String,
}

impl Reference {
    /// `nginx`, `nginx:1.27`, `user/app`, `ghcr.io/o/a:v2`, `localhost:5000/x:1`. None for what names no tag: a digest
    /// (`a@sha256:...`, pinned: nothing to update), an image id, or nonsense.
    pub fn parse(image: &str) -> Option<Reference> {
        let image = image.trim();
        if image.is_empty() || image.contains('@') || image.starts_with("sha256:") || image.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        let (name, tag) = match image.rfind(':') {
            Some(i) if !image[i + 1..].contains('/') => (&image[..i], &image[i + 1..]),
            _ => (image, "latest"),
        };
        if name.is_empty() || tag.is_empty() {
            return None;
        }
        let (registry, repository) = Self::split_name(name)?;
        Some(Reference { registry, repository, tag: tag.to_string() })
    }

    /// A name without its tag (as in `RepoDigests`, `name@sha256:...`) as registry and repository.
    pub fn split_name(name: &str) -> Option<(String, String)> {
        let (first, rest) = match name.split_once('/') {
            Some((f, r)) if f.contains(['.', ':']) || f == "localhost" => (Some(f), r),
            _ => (None, name),
        };
        if rest.is_empty() || !rest.split('/').all(path_part) {
            return None;
        }
        let registry = match first {
            None | Some("docker.io" | "index.docker.io" | "registry-1.docker.io") => HUB.to_string(),
            Some(r) => r.to_ascii_lowercase(),
        };
        let repository = if registry == HUB && !rest.contains('/') { format!("library/{rest}") } else { rest.to_string() };
        Some((registry, repository))
    }

    /// The reference whole, as the cache keeps it: `registry-1.docker.io/library/nginx:latest`.
    pub fn key(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.tag)
    }

    /// A registry on this machine is spoken to over plain HTTP, as Docker does (127.0.0.0/8 is insecure by default).
    pub fn insecure(&self) -> bool {
        let host = self.registry.rsplit_once(':').map_or(self.registry.as_str(), |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { self.registry.as_str() });
        host == "localhost" || host.starts_with("127.") || host == "[::1]"
    }

    pub fn manifest_url(&self, reference: &str) -> String {
        format!("{}://{}/v2/{}/manifests/{reference}", if self.insecure() { "http" } else { "https" }, self.registry, self.repository)
    }

    /// The digests among an image's `RepoDigests` that are of this repository.
    pub fn digests_in(&self, repo_digests: &[String]) -> Vec<String> {
        repo_digests
            .iter()
            .filter_map(|rd| {
                let (name, digest) = rd.split_once('@')?;
                let (registry, repository) = Self::split_name(name)?;
                (registry == self.registry && repository == self.repository).then(|| digest.to_string())
            })
            .collect()
    }
}

/// What a registry asks for in `WWW-Authenticate`.
#[derive(Debug, Clone, PartialEq)]
pub enum Challenge {
    Bearer { realm: String, service: Option<String>, scope: Option<String> },
    Basic,
}

/// `Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull"`.
pub fn parse_challenge(h: &str) -> Option<Challenge> {
    let h = h.trim();
    let (scheme, rest) = h.split_once(' ').unwrap_or((h, ""));
    if scheme.eq_ignore_ascii_case("basic") {
        return Some(Challenge::Basic);
    }
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut params = std::collections::HashMap::new();
    let mut s = rest.trim();
    while !s.is_empty() {
        let (k, after) = s.split_once('=')?;
        let after = after.trim_start();
        let (v, tail) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"')?;
            (&q[..end], &q[end + 1..])
        } else {
            after.split_once(',').map_or((after, ""), |(v, t)| (v, t))
        };
        params.insert(k.trim().to_ascii_lowercase(), v.to_string());
        s = tail.trim_start_matches([',', ' ']);
    }
    Some(Challenge::Bearer { realm: params.remove("realm")?, service: params.remove("service"), scope: params.remove("scope") })
}

/// One platform's manifest in an index.
#[derive(Debug, Clone, PartialEq)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

impl Platform {
    /// Whether the index's entry is for this platform: the same OS and architecture, and the variant when both say one.
    fn fits(&self, os: &str, arch: &str, variant: Option<&str>) -> bool {
        self.os == os && self.architecture == arch && (variant.is_none() || self.variant.is_none() || self.variant.as_deref() == variant)
    }
}

/// An index's platforms and their manifests' digests (attestations, `unknown/unknown`, left out).
pub fn platforms(index: &Value) -> Vec<(Platform, String)> {
    index["manifests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let p = &m["platform"];
            let os = p["os"].as_str()?.to_string();
            if os == "unknown" {
                return None;
            }
            Some((Platform { os, architecture: p["architecture"].as_str()?.to_string(), variant: p["variant"].as_str().map(str::to_string) }, m["digest"].as_str()?.to_string()))
        })
        .collect()
}

/// What the registry says a tag is now.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    /// The digest the tag points at: an index's, for a multi-arch image.
    pub digest: String,
    pub index: bool,
}

/// How an image here stands against the registry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Current,
    Outdated,
    /// Its digest is not the index's, and the index's platforms are not known yet: they must be asked.
    NeedPlatforms,
}

/// `local`: the image's digests from this repository; `platform_digest`: the local platform's manifest in the
/// registry's index, once known. Equal to the tag's digest is current; for an index, so is equal to the platform's own
/// manifest (an image pulled for one platform, or pushed from here, records that one).
pub fn verdict(local: &[String], remote: &Remote, platform_digest: Option<Option<&str>>) -> Verdict {
    if local.contains(&remote.digest) {
        return Verdict::Current;
    }
    if !remote.index {
        return Verdict::Outdated;
    }
    match platform_digest {
        None => Verdict::NeedPlatforms,
        Some(Some(p)) if local.iter().any(|d| d == p) => Verdict::Current,
        Some(_) => Verdict::Outdated,
    }
}

/// The local platform's digest among an index's platforms.
pub fn platform_digest<'a>(list: &'a [(Platform, String)], os: &str, arch: &str, variant: Option<&str>) -> Option<&'a str> {
    list.iter().find(|(p, _)| p.fits(os, arch, variant)).map(|(_, d)| d.as_str())
}

/// A registry account: a user name and a password or token, and the server Docker is told they are for.
#[derive(Clone, Debug, PartialEq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub server: String,
}

/// The credentials saved for `registry` (any of Docker Hub's names for Docker Hub), from `registries::auth_for`: the
/// check and the pull an update makes sign in as every other pull does ("Updates" 6). None when nothing is saved
/// there, and when its sign-in fails (said in the log): the registry is then asked anonymously.
pub async fn credentials(app: &crate::state::App, registry: &str) -> Option<Credentials> {
    match crate::registries::auth_for(app, registry).await {
        Ok(Some(a)) => {
            let c = a.credentials;
            Some(Credentials { username: c.username?, password: c.password?, server: c.serveraddress.unwrap_or_else(|| a.host.clone()) })
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("signing in to {registry} for an update check: {}", e.message);
            None
        }
    }
}

/// A part of a repository's path as registries take it: letters, digits and `.`, `_`, `-`, and not `.` or `..`, so a
/// reference cannot step out of `/v2/` or carry a query.
fn path_part(p: &str) -> bool {
    !p.is_empty() && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether the token realm a registry's challenge names, at the addresses `at` it has, is somewhere Freightdock follows
/// it ("Outbound" 2): https (plain http only for a registry on this machine, spoken to so too), and not inside
/// (`outbound::internal_ip`) unless the registry itself is, at `own`. A registry on the internet cannot send the
/// server to this machine, its network or a cloud's metadata service.
fn realm_allowed(r: &Reference, realm: &url::Url, at: &[std::net::SocketAddr], own: &[std::net::SocketAddr]) -> Result<(), String> {
    let host = realm.host_str().unwrap_or("");
    let refused = || format!("{} names a sign-in address Freightdock does not follow: {}://{host}", r.registry, realm.scheme());
    if !(realm.scheme() == "https" || (realm.scheme() == "http" && r.insecure())) {
        return Err(refused());
    }
    let inside = |addrs: &[std::net::SocketAddr]| addrs.iter().any(|a| crate::outbound::internal_ip(a.ip()));
    let registry_inside = !own.is_empty() && own.iter().all(|a| crate::outbound::internal_ip(a.ip()));
    if inside(at) && !registry_inside {
        return Err(refused());
    }
    Ok(())
}

fn is_index(content_type: &str) -> bool {
    content_type.contains("image.index") || content_type.contains("manifest.list")
}

/// A registry client for one check pass.
pub struct Client {
    http: reqwest::Client,
}

/// How every client of the check is made: a time limit, and redirects only on the host first asked ("Outbound" 2).
fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).redirect(crate::outbound::same_host()).user_agent(concat!("freightdock/", env!("CARGO_PKG_VERSION")))
}

impl Default for Client {
    fn default() -> Self {
        Client { http: builder().build().unwrap_or_default() }
    }
}

impl Client {
    /// Sends `req` (made again by `make` for each try), signing in as the registry's answer asks when it is a 401.
    async fn signed(&self, r: &Reference, creds: Option<&Credentials>, make: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
        let first = make().send().await.map_err(|e| e.without_url().to_string())?;
        if first.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(first);
        }
        let challenge = first.headers().get(reqwest::header::WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).and_then(parse_challenge);
        let auth = match challenge {
            Some(Challenge::Bearer { realm, service, scope }) => {
                let scope = scope.unwrap_or_else(|| format!("repository:{}:pull", r.repository));
                let mut q: Vec<(&str, String)> = vec![("scope", scope)];
                if let Some(s) = service {
                    q.push(("service", s));
                }
                let realm = url::Url::parse(&realm).map_err(|_| format!("{} names a sign-in address that is not one", r.registry))?;
                let at = crate::outbound::resolve(&realm).await.map_err(|e| format!("the registry's sign-in: {e}"))?;
                let own = match url::Url::parse(&r.manifest_url(&r.tag)) {
                    Ok(u) => crate::outbound::resolve(&u).await.unwrap_or_default(),
                    Err(_) => vec![],
                };
                realm_allowed(r, &realm, &at, &own)?;
                // Asked at the addresses just checked, so a second lookup cannot answer somewhere else.
                let pinned = match realm.host() {
                    Some(url::Host::Domain(d)) => builder().resolve_to_addrs(d, &at).build().map_err(|e| e.to_string())?,
                    _ => self.http.clone(),
                };
                let mut t = pinned.get(realm).query(&q);
                if let Some(c) = creds {
                    t = t.basic_auth(&c.username, Some(&c.password));
                }
                let answer = t.send().await.map_err(|e| format!("the registry's sign-in: {}", e.without_url()))?;
                if !answer.status().is_success() {
                    return Err(format!("the registry's sign-in answered {}", answer.status()));
                }
                let body = crate::outbound::capped(answer, MAX_TOKEN).await.map_err(|e| format!("the registry's sign-in: {e}"))?;
                let body: Value = serde_json::from_slice(&body).map_err(|e| format!("the registry's sign-in: {e}"))?;
                let token = body["token"].as_str().or(body["access_token"].as_str()).ok_or("the registry's sign-in gave no token")?;
                format!("Bearer {token}")
            }
            Some(Challenge::Basic) => {
                let c = creds.ok_or("the registry wants a sign-in, and Freightdock has none for it")?;
                use base64::Engine;
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", c.username, c.password)))
            }
            None => return Err("the registry wants a sign-in Freightdock does not know".into()),
        };
        make().header(reqwest::header::AUTHORIZATION, auth).send().await.map_err(|e| e.without_url().to_string())
    }

    fn said(r: &Reference, status: reqwest::StatusCode) -> String {
        match status.as_u16() {
            401 | 403 => format!("{} refused: it needs a sign-in Freightdock does not have", r.registry),
            404 => format!("{} has no {}:{}", r.registry, r.repository, r.tag),
            429 => format!("{} says too many requests; Freightdock asks again later", r.registry),
            _ => format!("{} answered {status}", r.registry),
        }
    }

    /// The digest the tag points at now.
    pub async fn head(&self, r: &Reference, creds: Option<&Credentials>) -> Result<Remote, String> {
        let url = r.manifest_url(&r.tag);
        let answer = self.signed(r, creds, || self.http.head(&url).header(reqwest::header::ACCEPT, ACCEPT)).await?;
        if !answer.status().is_success() {
            return Err(Self::said(r, answer.status()));
        }
        let ct = answer.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if let Some(d) = answer.headers().get("docker-content-digest").and_then(|v| v.to_str().ok()) {
            return Ok(Remote { digest: d.to_string(), index: is_index(&ct) });
        }
        // A registry that leaves the digest out of a HEAD: the manifest's own bytes say it.
        let answer = self.signed(r, creds, || self.http.get(&url).header(reqwest::header::ACCEPT, ACCEPT)).await?;
        if !answer.status().is_success() {
            return Err(Self::said(r, answer.status()));
        }
        let ct = answer.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = crate::outbound::capped(answer, MAX_MANIFEST).await.map_err(|e| format!("{}'s manifest: {e}", r.registry))?;
        Ok(Remote { digest: format!("sha256:{}", hex::encode(sha2::Sha256::digest(&body))), index: is_index(&ct) })
    }

    /// The platforms of the index at `digest`.
    pub async fn platforms(&self, r: &Reference, digest: &str, creds: Option<&Credentials>) -> Result<Vec<(Platform, String)>, String> {
        let url = r.manifest_url(digest);
        let answer = self.signed(r, creds, || self.http.get(&url).header(reqwest::header::ACCEPT, ACCEPT)).await?;
        if !answer.status().is_success() {
            return Err(Self::said(r, answer.status()));
        }
        let index = crate::outbound::capped(answer, MAX_MANIFEST).await.map_err(|e| format!("{}'s index: {e}", r.registry))?;
        let index: Value = serde_json::from_slice(&index).map_err(|e| format!("{}'s index: {e}", r.registry))?;
        Ok(platforms(&index))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    fn r(registry: &str, repository: &str, tag: &str) -> Option<Reference> {
        Some(Reference { registry: registry.into(), repository: repository.into(), tag: tag.into() })
    }

    #[test]
    fn a_reference_is_split_as_a_registry_reads_it() {
        assert_eq!(Reference::parse("nginx"), r(HUB, "library/nginx", "latest"));
        assert_eq!(Reference::parse("nginx:1.27-alpine"), r(HUB, "library/nginx", "1.27-alpine"));
        assert_eq!(Reference::parse("docker.io/library/nginx:1"), r(HUB, "library/nginx", "1"));
        assert_eq!(Reference::parse("grafana/grafana"), r(HUB, "grafana/grafana", "latest"));
        assert_eq!(Reference::parse("ghcr.io/Owner/app:v2"), r("ghcr.io", "Owner/app", "v2"));
        assert_eq!(Reference::parse("localhost:5000/qa/app:1"), r("localhost:5000", "qa/app", "1"));
        assert_eq!(Reference::parse("registry.local:5000/app"), r("registry.local:5000", "app", "latest"));
        assert_eq!(Reference::parse("nginx@sha256:abc"), None, "a digest is pinned: there is nothing to update");
        assert_eq!(Reference::parse("sha256:0123abcd"), None, "an image id names no tag");
        assert_eq!(Reference::parse(""), None);
    }

    #[test]
    fn a_registry_on_this_machine_is_plain_http() {
        assert_eq!(Reference::parse("localhost:5000/a").unwrap().manifest_url("1"), "http://localhost:5000/v2/a/manifests/1");
        assert_eq!(Reference::parse("127.0.0.1:5000/a").unwrap().manifest_url("1"), "http://127.0.0.1:5000/v2/a/manifests/1");
        assert_eq!(Reference::parse("nginx").unwrap().manifest_url("latest"), "https://registry-1.docker.io/v2/library/nginx/manifests/latest");
    }

    #[test]
    fn only_the_repository_s_own_digests_count() {
        let nginx = Reference::parse("nginx:1.27").unwrap();
        let local = ["nginx@sha256:aaa".to_string(), "docker.io/library/nginx@sha256:bbb".into(), "myregistry.example/nginx@sha256:ccc".into(), "broken".into()];
        assert_eq!(nginx.digests_in(&local), vec!["sha256:aaa", "sha256:bbb"]);
    }

    #[test]
    fn docker_hub_s_challenge_is_read() {
        let c = parse_challenge(r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#).unwrap();
        assert_eq!(c, Challenge::Bearer { realm: "https://auth.docker.io/token".into(), service: Some("registry.docker.io".into()), scope: Some("repository:library/nginx:pull".into()) });
        let ghcr = parse_challenge(r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:o/a:pull""#).unwrap();
        assert!(matches!(ghcr, Challenge::Bearer { ref realm, .. } if realm == "https://ghcr.io/token"));
        assert_eq!(parse_challenge(r#"Basic realm="Registry Realm""#), Some(Challenge::Basic));
        assert_eq!(parse_challenge("Negotiate"), None);
    }

    /// A trimmed `docker buildx imagetools inspect --raw` of a multi-arch image, with its attestation.
    pub fn index_fixture() -> Value {
        json!({
            "schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                { "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:amd64", "size": 1, "platform": { "architecture": "amd64", "os": "linux" } },
                { "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:armv7", "size": 1, "platform": { "architecture": "arm", "os": "linux", "variant": "v7" } },
                { "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:arm64", "size": 1, "platform": { "architecture": "arm64", "os": "linux", "variant": "v8" } },
                { "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:attest", "size": 1, "platform": { "architecture": "unknown", "os": "unknown" } },
            ]
        })
    }

    #[test]
    fn an_index_s_platforms_are_read_and_the_local_one_found() {
        let p = platforms(&index_fixture());
        assert_eq!(p.len(), 3, "the attestation is no platform");
        assert_eq!(platform_digest(&p, "linux", "amd64", None), Some("sha256:amd64"));
        assert_eq!(platform_digest(&p, "linux", "arm64", Some("v8")), Some("sha256:arm64"));
        assert_eq!(platform_digest(&p, "linux", "arm64", None), Some("sha256:arm64"));
        assert_eq!(platform_digest(&p, "linux", "arm", Some("v6")), None);
        assert_eq!(platform_digest(&p, "windows", "amd64", None), None);
    }

    #[test]
    fn the_verdict_compares_digests_and_looks_inside_an_index_only_when_it_must() {
        let single = Remote { digest: "sha256:new".into(), index: false };
        assert_eq!(verdict(&["sha256:new".into()], &single, None), Verdict::Current);
        assert_eq!(verdict(&["sha256:old".into()], &single, None), Verdict::Outdated);
        let index = Remote { digest: "sha256:index".into(), index: true };
        assert_eq!(verdict(&["sha256:index".into()], &index, None), Verdict::Current, "pulled by tag: the index's digest is recorded");
        assert_eq!(verdict(&["sha256:amd64".into()], &index, None), Verdict::NeedPlatforms);
        assert_eq!(verdict(&["sha256:amd64".into()], &index, Some(Some("sha256:amd64"))), Verdict::Current, "the platform's own manifest");
        assert_eq!(verdict(&["sha256:old-amd64".into()], &index, Some(Some("sha256:amd64"))), Verdict::Outdated);
        assert_eq!(verdict(&["sha256:x".into()], &index, Some(None)), Verdict::Outdated, "the index has no manifest for this platform");
    }

    /// A registry on loopback that wants a bearer token from its own realm, as Docker Hub does, and serves one tag.
    pub async fn fake_registry(digest: &'static str, index: bool) -> String {
        fake_registry_counted(digest, index).await.0
    }

    /// The same, and how many times a manifest was asked for with the token.
    pub async fn fake_registry_counted(digest: &'static str, index: bool) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::http::{HeaderMap, StatusCode, header};
        use axum::response::IntoResponse;
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = asked.clone();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let realm = format!("http://{addr}/token");
        let manifest = move |headers: HeaderMap| {
            let realm = realm.clone();
            let counter = counter.clone();
            async move {
                let accepts = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer t0k3n") {
                    let challenge = format!(r#"Bearer realm="{realm}",service="fake",scope="repository:qa/app:pull""#);
                    return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, challenge)]).into_response();
                }
                assert!(accepts.contains("image.index") && accepts.contains("manifest.v2"), "every manifest type is asked for: {accepts}");
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let ct = if index { "application/vnd.oci.image.index.v1+json" } else { "application/vnd.docker.distribution.manifest.v2+json" };
                ([(header::CONTENT_TYPE, ct.to_string()), (header::HeaderName::from_static("docker-content-digest"), digest.to_string())], index_fixture().to_string()).into_response()
            }
        };
        let app = axum::Router::new()
            .route("/token", axum::routing::get(|q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                assert_eq!(q.get("scope").map(String::as_str), Some("repository:qa/app:pull"));
                axum::Json(json!({ "token": "t0k3n" }))
            }))
            .route("/v2/qa/app/manifests/{reference}", axum::routing::get(manifest.clone()).head(manifest));
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("127.0.0.1:{}", addr.port()), asked)
    }

    #[tokio::test]
    async fn the_credentials_are_the_saved_registry_s_for_its_host() {
        let app = crate::api::tests::app();
        let (team, hub) = (app.sealer.seal("hunter2").unwrap(), app.sealer.seal("hub-token").unwrap());
        app.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO registries (name, kind, url, host, username, secret, created_at) VALUES ('team', 'custom', 'registry.example.com', 'registry.example.com', 'deployer', ?1, 0)",
                    crate::db::rusqlite::params![team],
                )?;
                c.execute(
                    "INSERT INTO registries (name, kind, url, host, username, secret, created_at) VALUES ('hub', 'dockerhub', 'docker.io', 'docker.io', 'me', ?1, 0)",
                    crate::db::rusqlite::params![hub],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let r = Reference::parse("registry.example.com/team/app:1").unwrap();
        assert_eq!(credentials(&app, &r.registry).await, Some(Credentials { username: "deployer".into(), password: "hunter2".into(), server: "registry.example.com".into() }));
        let r = Reference::parse("nginx:1").unwrap();
        let c = credentials(&app, &r.registry).await.expect("Docker Hub by its registry's name");
        assert_eq!((c.username.as_str(), c.password.as_str()), ("me", "hub-token"));
        assert_eq!(credentials(&app, "ghcr.io").await, None, "nothing saved there: asked anonymously");
    }

    /// A registry on loopback whose every answer is `size` bytes: its token, a manifest without a digest header, an index.
    async fn bloated_registry(size: usize, token_too: bool) -> String {
        use axum::http::{HeaderMap, StatusCode, header};
        use axum::response::IntoResponse;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let realm = format!("http://{addr}/token");
        let big = move || format!(r#"{{"token":"t0k3n","pad":"{}"}}"#, "x".repeat(size));
        let manifest = move |headers: HeaderMap| {
            let realm = realm.clone();
            async move {
                if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer t0k3n") {
                    return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, format!(r#"Bearer realm="{realm}""#))]).into_response();
                }
                ([(header::CONTENT_TYPE, "application/vnd.oci.image.index.v1+json")], big()).into_response()
            }
        };
        let token = move || async move { if token_too { big() } else { r#"{"token":"t0k3n"}"#.to_string() } };
        let app = axum::Router::new().route("/token", axum::routing::get(token)).route("/v2/qa/app/manifests/{reference}", axum::routing::get(manifest.clone()).head(manifest));
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn a_registry_s_answer_too_big_to_be_one_is_refused() {
        let c = Client::default();
        let host = bloated_registry(MAX_TOKEN + 1, true).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        let e = c.head(&r, None).await.unwrap_err();
        assert!(e.contains("too big"), "the token answer: {e}");

        let host = bloated_registry(MAX_MANIFEST + 1, false).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        let e = c.head(&r, None).await.unwrap_err();
        assert!(e.contains("too big"), "the manifest, read for its digest: {e}");
        let e = c.platforms(&r, "sha256:index", None).await.unwrap_err();
        assert!(e.contains("too big"), "the index: {e}");

        let host = bloated_registry(1024, false).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        assert!(c.head(&r, None).await.unwrap().digest.starts_with("sha256:"), "an answer of a usual size is read");
    }

    #[test]
    fn a_repository_s_path_is_one_a_registry_takes() {
        assert_eq!(Reference::parse("ghcr.io/o/../../x:1"), None, "no step out of /v2/");
        assert_eq!(Reference::parse("ghcr.io/o/a?b:1"), None);
        assert_eq!(Reference::parse("ghcr.io/o/a#b:1"), None);
        assert_eq!(Reference::parse("ghcr.io/o/a%2f:1"), None);
        assert!(Reference::parse("ghcr.io/my-org/my_app.v2:1").is_some());
    }

    #[test]
    fn a_token_realm_is_followed_only_where_a_registry_s_would_be() {
        let ip = |s: &str| -> std::net::SocketAddr { format!("{s}:443").parse().unwrap() };
        let hub = Reference::parse("nginx").unwrap();
        let public = [ip("34.1.2.3")];
        let ok = |r: &Reference, realm: &str, at: &[std::net::SocketAddr], own: &[std::net::SocketAddr]| realm_allowed(r, &url::Url::parse(realm).unwrap(), at, own);
        assert!(ok(&hub, "https://auth.docker.io/token", &[ip("44.5.6.7")], &public).is_ok());
        assert!(ok(&hub, "http://auth.docker.io/token", &[ip("44.5.6.7")], &public).is_err(), "plain http for a registry spoken to over https");
        assert!(ok(&hub, "file:///etc/passwd", &[], &public).is_err());
        for inside in ["127.0.0.1", "10.0.0.5", "192.168.1.1", "172.16.0.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "[::1]", "[fd00::1]", "[fe80::1]", "[::ffff:127.0.0.1]"] { // gitleaks:allow (private ranges, refused on purpose)
            let e = ok(&hub, "https://auth.example/token", &[ip(inside)], &public).unwrap_err();
            assert!(e.contains("auth.example"), "{inside}: the refusal names the realm: {e}");
        }
        assert!(ok(&hub, "https://auth.example/token", &[ip("44.5.6.7"), ip("10.0.0.5")], &public).is_err(), "any of its addresses inside is one too many"); // gitleaks:allow (private ranges, refused on purpose)
        let lan = Reference::parse("registry.lan:5000/a").unwrap();
        assert!(ok(&lan, "https://auth.lan/token", &[ip("10.0.0.6")], &[ip("10.0.0.5")]).is_ok(), "a registry inside may have its realm inside"); // gitleaks:allow (private ranges, refused on purpose)
        let local = Reference::parse("localhost:5000/a").unwrap();
        assert!(ok(&local, "http://localhost:5001/token", &[ip("127.0.0.1")], &[ip("127.0.0.1")]).is_ok(), "a registry on this machine is plain http");
    }

    #[tokio::test]
    async fn a_registry_s_redirect_to_another_host_is_not_followed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let inside = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = inside.local_addr().unwrap().port();
        let elsewhere = axum::Router::new().fallback(move || {
            h.fetch_add(1, Ordering::SeqCst);
            async { "secret" }
        });
        tokio::spawn(async move { axum::serve(inside, elsewhere).await.unwrap() });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let to = format!("http://localhost:{port}/internal");
        let app = axum::Router::new().fallback(move || {
            let to = to.clone();
            async move { (axum::http::StatusCode::FOUND, [(axum::http::header::LOCATION, to)]) }
        });
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let r = Reference::parse(&format!("127.0.0.1:{}/qa/app:1", addr.port())).unwrap();
        assert!(Client::default().head(&r, None).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0, "the server never asked the host the registry pointed it at");
    }

    #[tokio::test]
    async fn a_registry_is_asked_with_the_token_its_challenge_names() {
        let host = fake_registry("sha256:feed", false).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        let remote = Client::default().head(&r, None).await.unwrap();
        assert_eq!(remote, Remote { digest: "sha256:feed".into(), index: false });

        let host = fake_registry("sha256:index", true).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        let c = Client::default();
        assert!(c.head(&r, None).await.unwrap().index);
        let p = c.platforms(&r, "sha256:index", None).await.unwrap();
        assert_eq!(platform_digest(&p, "linux", "amd64", None), Some("sha256:amd64"));

        let missing = Reference::parse(&format!("{host}/qa/other:1")).unwrap();
        let e = c.head(&missing, None).await.unwrap_err();
        assert!(e.contains("has no qa/other:1"), "{e}");
    }
}
