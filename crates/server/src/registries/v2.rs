//! A registry's own HTTP API (the OCI distribution API, `/v2/`), as Freightdock reads it to test a sign-in and to browse:
//! the version check, the catalog, a repository's tags. Each request goes without credentials first; a 401 answers how
//! to sign in (`WWW-Authenticate`): Basic, sent again with the user name and password, or Bearer, a token asked of the
//! realm it names for the scope it names, with the user name and password, and the request sent again with that token
//! ("Registries" 4 in DECISIONS.md). Docker Hub lists no catalog; its own API lists an account's repositories.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::{StatusCode, header};
use serde::Deserialize;

/// Why talking to a registry did not work, said for a person.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    /// The registry, or its token server, refused these credentials (or wants some and has none).
    Denied(String),
    /// The registry does not offer this (a catalog, mostly).
    Unsupported(String),
    /// No such repository.
    NotFound(String),
    /// It could not be reached, or answered something that is not the API.
    Unreachable(String),
}

impl Fail {
    pub fn message(&self) -> &str {
        match self {
            Fail::Denied(m) | Fail::Unsupported(m) | Fail::NotFound(m) | Fail::Unreachable(m) => m,
        }
    }
}

/// How long a call to a registry may take.
const TIMEOUT: Duration = Duration::from_secs(15);
/// The most repositories or tags read from one registry: the browser filters, it does not page.
pub const MOST: usize = 5000;

fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("Freightdock/", env!("CARGO_PKG_VERSION")))
            // A token realm on another host is followed, but credentials never go with a redirect (reqwest drops them).
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

/// A `WWW-Authenticate` challenge: its scheme (lower case) and parameters.
#[derive(Debug, PartialEq, Eq)]
pub struct Challenge {
    pub scheme: String,
    pub params: HashMap<String, String>,
}

/// Reads a challenge such as `Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="a:b:pull,push"`:
/// values may be quoted (and hold commas) or bare.
pub fn challenge(value: &str) -> Option<Challenge> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(' ').unwrap_or((value, ""));
    if scheme.is_empty() {
        return None;
    }
    let mut params = HashMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|c| *c == ',' || c.is_whitespace()).is_some() {}
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if key.trim().is_empty() {
            break;
        }
        if chars.next_if_eq(&'=').is_none() {
            continue;
        }
        let mut val = String::new();
        if chars.next_if_eq(&'"').is_some() {
            while let Some(c) = chars.next() {
                match c {
                    '\\' => val.extend(chars.next()),
                    '"' => break,
                    c => val.push(c),
                }
            }
        } else {
            val = std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string();
        }
        params.insert(key.trim().to_ascii_lowercase(), val);
    }
    Some(Challenge { scheme: scheme.to_ascii_lowercase(), params })
}

fn basic(user: &str, password: &str) -> String {
    use base64::Engine;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
}

fn unreachable(e: reqwest::Error) -> Fail {
    let why = if e.is_timeout() { "it did not answer in time".to_string() } else if e.is_connect() { "nothing answered there".to_string() } else { e.without_url().to_string() };
    Fail::Unreachable(format!("The registry could not be reached: {why}."))
}

/// What a registry's error body says (`{"errors":[{"message": ...}]}`), when it says anything.
async fn said(r: reqwest::Response) -> String {
    #[derive(Deserialize)]
    struct E {
        message: Option<String>,
        code: Option<String>,
    }
    #[derive(Deserialize)]
    struct Errors {
        errors: Vec<E>,
    }
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    serde_json::from_str::<Errors>(&text)
        .ok()
        .and_then(|e| e.errors.into_iter().next())
        .and_then(|e| e.message.or(e.code))
        .unwrap_or_else(|| format!("it answered {status}"))
}

/// The address a `Link: <...>; rel="next"` header points at.
fn next_link(h: &header::HeaderMap) -> Option<String> {
    let v = h.get(header::LINK)?.to_str().ok()?;
    v.split(',').find(|l| l.contains("rel=\"next\"") || l.contains("rel=next")).and_then(|l| Some(l.split_once('<')?.1.split_once('>')?.0.to_string()))
}

const REFUSED: &str = "The registry refused that user name and password.";
const NEEDS_USER: &str = "The registry wants a user name and password.";
/// Where the Basic header is remembered once a registry has asked for it.
const BASIC: &str = "\0basic";

/// One registry, reached at `base` (`https://host[:port]`, or `http://` for one on plain HTTP), as `user`.
pub struct Registry {
    base: String,
    user: Option<(String, String)>,
    /// Tokens asked for so far, by scope.
    tokens: Mutex<HashMap<String, String>>,
}

impl Registry {
    pub fn new(base: &str, user: Option<(String, String)>) -> Self {
        Registry { base: base.trim_end_matches('/').to_string(), user, tokens: Mutex::new(HashMap::new()) }
    }

    /// Does the registry answer, and take these credentials? Signs in as a pull would.
    pub async fn check(&self) -> Result<(), Fail> {
        let r = self.get("/v2/", "").await?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(Fail::Unreachable(format!("That is not a registry's API: {}.", said(r).await)))
        }
    }

    /// Every repository the registry lists to this user (`/v2/_catalog`, page by page).
    pub async fn catalog(&self) -> Result<Vec<String>, Fail> {
        const NOT_LISTED: &str = "This registry does not list its repositories to this user. Type a repository's name to see its tags.";
        let mut out = Vec::new();
        let mut path = "/v2/_catalog?n=1000".to_string();
        loop {
            let r = match self.get(&path, "registry:catalog:*").await {
                Err(Fail::Denied(_)) => return Err(Fail::Unsupported(NOT_LISTED.into())),
                r => r?,
            };
            match r.status() {
                s if s.is_success() => {}
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED => return Err(Fail::Unsupported(NOT_LISTED.into())),
                _ => return Err(Fail::Unreachable(format!("The registry did not list its repositories: {}.", said(r).await))),
            }
            let next = next_link(r.headers());
            #[derive(Deserialize)]
            struct Catalog {
                #[serde(default)]
                repositories: Option<Vec<String>>,
            }
            let page: Catalog = r.json().await.map_err(|_| Fail::Unreachable("The registry's catalog is not what the API says it is.".into()))?;
            let page = page.repositories.unwrap_or_default();
            let empty = page.is_empty();
            out.extend(page);
            match next.and_then(|n| self.same_registry(&n)) {
                Some(n) if !empty && out.len() < MOST => path = n,
                _ => break,
            }
        }
        out.truncate(MOST);
        Ok(out)
    }

    /// Every tag of `repository`.
    pub async fn tags(&self, repository: &str) -> Result<Vec<String>, Fail> {
        let mut out = Vec::new();
        let mut path = format!("/v2/{repository}/tags/list?n=1000");
        let scope = format!("repository:{repository}:pull");
        loop {
            let r = self.get(&path, &scope).await?;
            match r.status() {
                s if s.is_success() => {}
                StatusCode::NOT_FOUND => return Err(Fail::NotFound(format!("There is no repository {repository} there."))),
                _ => return Err(Fail::Unreachable(format!("The registry did not list the tags: {}.", said(r).await))),
            }
            let next = next_link(r.headers());
            #[derive(Deserialize)]
            struct Tags {
                #[serde(default)]
                tags: Option<Vec<String>>,
            }
            let page: Tags = r.json().await.map_err(|_| Fail::Unreachable("The registry's tag list is not what the API says it is.".into()))?;
            let page = page.tags.unwrap_or_default();
            let empty = page.is_empty();
            out.extend(page);
            match next.and_then(|n| self.same_registry(&n)) {
                Some(n) if !empty && out.len() < MOST => path = n,
                _ => break,
            }
        }
        out.truncate(MOST);
        Ok(out)
    }

    /// A next page's address as a path on this registry; one elsewhere is not followed, credentials and all.
    fn same_registry(&self, link: &str) -> Option<String> {
        if link.starts_with('/') {
            return Some(link.to_string());
        }
        link.strip_prefix(&self.base).filter(|p| p.starts_with('/')).map(str::to_string)
    }

    fn remembered(&self, scope: &str) -> Option<String> {
        let t = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        t.get(scope).or_else(|| t.get(BASIC)).cloned()
    }

    fn remember(&self, scope: &str, auth: String) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).insert(scope.to_string(), auth);
    }

    async fn send(&self, url: &str, auth: Option<&str>) -> Result<reqwest::Response, Fail> {
        let mut r = http().get(url).header(header::ACCEPT, "application/json");
        if let Some(a) = auth {
            r = r.header(header::AUTHORIZATION, a);
        }
        r.send().await.map_err(unreachable)
    }

    /// `path` on the registry, signed in as its challenge asks.
    async fn get(&self, path: &str, scope: &str) -> Result<reqwest::Response, Fail> {
        let url = format!("{}{path}", self.base);
        let known = self.remembered(scope);
        let first = self.send(&url, known.as_deref()).await?;
        if first.status() != StatusCode::UNAUTHORIZED {
            return Ok(first);
        }
        let ch = first
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .and_then(challenge)
            .ok_or_else(|| Fail::Denied("The registry wants a sign-in and does not say how.".into()))?;
        let auth = match ch.scheme.as_str() {
            "basic" => {
                let (u, p) = self.user.as_ref().ok_or_else(|| Fail::Denied(NEEDS_USER.into()))?;
                let a = basic(u, p);
                self.remember(BASIC, a.clone());
                a
            }
            "bearer" => {
                let realm = ch.params.get("realm").ok_or_else(|| Fail::Denied("The registry asks for a token and does not say where to get one.".into()))?;
                let wanted = ch.params.get("scope").cloned().unwrap_or_else(|| scope.to_string());
                let a = format!("Bearer {}", self.token(realm, ch.params.get("service").map(String::as_str), &wanted).await?);
                self.remember(scope, a.clone());
                a
            }
            other => return Err(Fail::Denied(format!("The registry asks to sign in a way Freightdock does not know ({other})."))),
        };
        let again = self.send(&url, Some(&auth)).await?;
        if matches!(again.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(Fail::Denied(if self.user.is_some() { REFUSED.into() } else { NEEDS_USER.into() }));
        }
        Ok(again)
    }

    /// A token from the realm a Bearer challenge named, for `scope`, asked with the user name and password.
    async fn token(&self, realm: &str, service: Option<&str>, scope: &str) -> Result<String, Fail> {
        let mut u = url::Url::parse(realm).map_err(|_| Fail::Unreachable(format!("The registry's token address is not an address: {realm}")))?;
        {
            let mut q = u.query_pairs_mut();
            if let Some(s) = service {
                q.append_pair("service", s);
            }
            if !scope.is_empty() {
                q.append_pair("scope", scope);
            }
        }
        let mut r = http().get(u.as_str()).header(header::ACCEPT, "application/json");
        if let Some((user, p)) = &self.user {
            r = r.header(header::AUTHORIZATION, basic(user, p));
        }
        let r = r.send().await.map_err(unreachable)?;
        if matches!(r.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(Fail::Denied(if self.user.is_some() { REFUSED.into() } else { NEEDS_USER.into() }));
        }
        if !r.status().is_success() {
            return Err(Fail::Unreachable(format!("The registry's token server answered {}.", r.status())));
        }
        #[derive(Deserialize)]
        struct T {
            token: Option<String>,
            access_token: Option<String>,
        }
        let t: T = r.json().await.map_err(|_| Fail::Unreachable("The registry's token server answered something that is not a token.".into()))?;
        t.token.or(t.access_token).filter(|t| !t.is_empty()).ok_or_else(|| Fail::Unreachable("The registry's token server answered no token.".into()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use base64::Engine;
    use std::sync::Arc;

    #[test]
    fn a_challenge_is_read_with_its_quoted_commas() {
        let c = challenge(r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:samalba/my-app:pull,push""#).unwrap();
        assert_eq!(c.scheme, "bearer");
        assert_eq!(c.params["realm"], "https://auth.docker.io/token");
        assert_eq!(c.params["service"], "registry.docker.io");
        assert_eq!(c.params["scope"], "repository:samalba/my-app:pull,push");
        let c = challenge(r#"Basic realm="Registry Realm""#).unwrap();
        assert_eq!((c.scheme.as_str(), c.params["realm"].as_str()), ("basic", "Registry Realm"));
        let c = challenge("Bearer realm=https://r.example/token, service=r.example").unwrap();
        assert_eq!((c.params["realm"].as_str(), c.params["service"].as_str()), ("https://r.example/token", "r.example"));
        assert_eq!(challenge("").map(|c| c.scheme), None);
    }

    /// How the fake registry asks to be signed in.
    #[derive(Clone, Copy, PartialEq)]
    pub enum Auth {
        Bearer,
        Basic,
    }

    pub struct Fake {
        pub auth: Auth,
        pub user: &'static str,
        pub password: &'static str,
        pub repos: Vec<&'static str>,
        /// Whether `/v2/_catalog` is offered.
        pub catalog: bool,
        pub base: String,
        pub token_asks: std::sync::atomic::AtomicUsize,
    }

    fn basic_ok(f: &Fake, h: &HeaderMap) -> bool {
        let want = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", f.user, f.password)));
        h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(want.as_str())
    }

    /// A token is the scope it was given for, signed by nobody: enough to see the flow asked for the right one.
    fn bearer_for(h: &HeaderMap) -> Option<String> {
        h.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer tok:").map(str::to_string)
    }

    fn challenge_for(f: &Fake, scope: &str) -> Response {
        let v = match f.auth {
            Auth::Bearer => format!(r#"Bearer realm="{}/token",service="fake",scope="{scope}""#, f.base),
            Auth::Basic => r#"Basic realm="Registry Realm""#.to_string(),
        };
        (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, v)], r#"{"errors":[{"code":"UNAUTHORIZED"}]}"#).into_response()
    }

    /// The challenge, when a request for this scope is not let in.
    fn refused(f: &Fake, h: &HeaderMap, scope: &str) -> Option<Response> {
        let ok = match f.auth {
            Auth::Basic => basic_ok(f, h),
            Auth::Bearer => bearer_for(h).is_some_and(|s| s.split(' ').any(|x| x == scope || scope.is_empty())),
        };
        (!ok).then(|| challenge_for(f, scope))
    }

    async fn token(State(f): State<Arc<Fake>>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
        f.token_asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if q.get("service").map(String::as_str) != Some("fake") {
            return (StatusCode::BAD_REQUEST, "service").into_response();
        }
        if h.contains_key(header::AUTHORIZATION) && !basic_ok(&f, &h) {
            return (StatusCode::UNAUTHORIZED, r#"{"details":"incorrect username or password"}"#).into_response();
        }
        if !h.contains_key(header::AUTHORIZATION) {
            return (StatusCode::UNAUTHORIZED, r#"{"details":"authentication required"}"#).into_response();
        }
        let scope = q.get("scope").cloned().unwrap_or_default();
        axum::Json(serde_json::json!({ "token": format!("tok:{scope}"), "expires_in": 300 })).into_response()
    }

    async fn v2(State(f): State<Arc<Fake>>, h: HeaderMap, uri: axum::http::Uri) -> Response {
        let path = uri.path().trim_start_matches("/v2/");
        let q: HashMap<String, String> = uri.query().map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect()).unwrap_or_default();
        if path.is_empty() {
            return refused(&f, &h, "").unwrap_or_else(|| axum::Json(serde_json::json!({})).into_response());
        }
        if path == "_catalog" {
            if let Some(r) = refused(&f, &h, "registry:catalog:*") {
                return r;
            }
            if !f.catalog {
                return (StatusCode::NOT_FOUND, r#"{"errors":[{"code":"UNSUPPORTED"}]}"#).into_response();
            }
            // Two at a time, as `n` and `last` say, with a Link to the next page.
            let n: usize = q.get("n").and_then(|n| n.parse().ok()).unwrap_or(100).min(2);
            let mut all = f.repos.clone();
            all.sort();
            let from = q.get("last").map_or(0, |l| all.iter().position(|r| r == l).map_or(all.len(), |i| i + 1));
            let page: Vec<_> = all.iter().skip(from).take(n).collect();
            let mut r = axum::Json(serde_json::json!({ "repositories": page })).into_response();
            if from + n < all.len() {
                let link = format!(r#"</v2/_catalog?last={}&n={n}>; rel="next""#, page.last().unwrap());
                r.headers_mut().insert(header::LINK, link.parse().unwrap());
            }
            return r;
        }
        if let Some(repo) = path.strip_suffix("/tags/list") {
            if let Some(r) = refused(&f, &h, &format!("repository:{repo}:pull")) {
                return r;
            }
            if !f.repos.contains(&repo) {
                return (StatusCode::NOT_FOUND, r#"{"errors":[{"code":"NAME_UNKNOWN","message":"repository name not known to registry"}]}"#).into_response();
            }
            return axum::Json(serde_json::json!({ "name": repo, "tags": ["1.0", "latest"] })).into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    /// A registry on loopback, answering as `auth` says, with these repositories.
    pub async fn fake(auth: Auth, repos: Vec<&'static str>, catalog: bool) -> Arc<Fake> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let f = Arc::new(Fake { auth, user: "deck", password: "s3cret-pass", repos, catalog, base, token_asks: Default::default() });
        let app = Router::new().route("/token", get(token)).fallback(v2).with_state(f.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        f
    }

    fn me(f: &Fake) -> Option<(String, String)> {
        Some((f.user.to_string(), f.password.to_string()))
    }

    #[tokio::test]
    async fn the_token_flow_signs_in_for_each_scope_the_registry_names() {
        let f = fake(Auth::Bearer, vec!["team/app", "team/web", "solo"], true).await;
        let r = Registry::new(&f.base, me(&f));
        r.check().await.unwrap();
        assert_eq!(r.catalog().await.unwrap(), vec!["solo", "team/app", "team/web"], "every page, followed by its Link");
        assert_eq!(r.tags("team/app").await.unwrap(), vec!["1.0", "latest"]);
        let asks = f.token_asks.load(std::sync::atomic::Ordering::SeqCst);
        r.tags("team/app").await.unwrap();
        assert_eq!(f.token_asks.load(std::sync::atomic::Ordering::SeqCst), asks, "a token is asked for once per scope");
        assert!(matches!(r.tags("team/gone").await, Err(Fail::NotFound(_))));
    }

    #[tokio::test]
    async fn a_wrong_password_or_none_is_denied_with_the_reason() {
        let f = fake(Auth::Bearer, vec!["a"], true).await;
        let wrong = Registry::new(&f.base, Some(("deck".into(), "nope".into())));
        let e = wrong.check().await.unwrap_err();
        assert!(matches!(e, Fail::Denied(_)), "{e:?}");
        assert!(e.message().contains("user name") || e.message().contains("refused"), "{e:?}");
        let nobody = Registry::new(&f.base, None);
        assert!(matches!(nobody.tags("a").await, Err(Fail::Denied(_))));
    }

    #[tokio::test]
    async fn basic_auth_is_sent_again_with_the_password() {
        let f = fake(Auth::Basic, vec!["qa/alpine"], true).await;
        let r = Registry::new(&f.base, me(&f));
        r.check().await.unwrap();
        assert_eq!(r.catalog().await.unwrap(), vec!["qa/alpine"]);
        assert_eq!(r.tags("qa/alpine").await.unwrap(), vec!["1.0", "latest"]);
        assert!(matches!(Registry::new(&f.base, Some(("deck".into(), "nope".into()))).check().await, Err(Fail::Denied(_))));
        assert!(matches!(Registry::new(&f.base, None).check().await, Err(Fail::Denied(_))));
    }

    #[tokio::test]
    async fn a_registry_without_a_catalog_says_so() {
        let f = fake(Auth::Bearer, vec!["a"], false).await;
        let r = Registry::new(&f.base, me(&f));
        assert!(matches!(r.catalog().await, Err(Fail::Unsupported(_))));
    }

    #[tokio::test]
    async fn nothing_there_is_unreachable() {
        let r = Registry::new("http://127.0.0.1:1", None);
        assert!(matches!(r.check().await, Err(Fail::Unreachable(_))));
    }
}
