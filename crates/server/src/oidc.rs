//! Single sign-on through an OAuth provider of the list (`signin::providers`): OpenID Connect (Authentik, Keycloak,
//! Entra ID, Google, anything that speaks it) and GitHub's OAuth (`signin::github`, which shares this flow). The
//! authorization-code flow with PKCE and a state; for OpenID Connect a nonce, and the ID token checked against the
//! provider's keys (or, for a provider set to sign with the client secret, HS256 with it). Who may come in, and as
//! what, is the provider's groups ("SSO" in DECISIONS.md). The local sign-in always stays, as the way in when a
//! provider is down.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::api::Peer;
use crate::audit::{self, Entry};
use crate::auth::{self, User};
use crate::db::{now, rusqlite::{OptionalExtension, params}};
use crate::error::{ApiError, ApiResult};
use crate::signin::providers::{self, Kind, Provider};
use crate::signin::{Identity, Refused, event};
use crate::state::App;

/// A sign-in started must come back within this.
const PENDING_FOR: Duration = Duration::from_secs(10 * 60);
/// The cookie that carries a started sign-in, sealed, from its start to its callback.
pub const PENDING_COOKIE: &str = "freightdock_sso";
/// The longest address inside the app a sign-in goes on to.
const MAX_NEXT: usize = 1024;
/// The provider's discovery document and keys are asked again after this (and at once for a key it has not seen).
const DISCOVERY_FOR: Duration = Duration::from_secs(10 * 60);

/// PKCE's S256: the challenge sent out for the verifier kept.
pub fn challenge(verifier: &str) -> String {
    B64URL.encode(Sha256::digest(verifier.as_bytes()))
}

fn random(n: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; n];
    rand::rng().fill_bytes(&mut b);
    B64URL.encode(b)
}

#[derive(Deserialize, Clone, Debug)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: Option<String>,
    pub userinfo_endpoint: Option<String>,
    pub end_session_endpoint: Option<String>,
}

/// A sign-in started and not yet back. It is kept by the browser that started it, sealed in a cookie (`PENDING_COOKIE`),
/// never on the server: only that browser can finish it, and nobody starting sign-ins elsewhere can crowd it out
/// ("SSO" 14).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub(crate) struct Pending {
    /// What the cookie is, so nothing else sealed with the same key passes for one.
    kind: String,
    /// The state sent to the provider: it must come back in the callback's query as well as in the cookie.
    state: String,
    /// The provider it was started with: only its own callback finishes it ("SSO" 9).
    pub provider: String,
    pub verifier: String,
    pub nonce: String,
    next: String,
    /// When it was started, in seconds since the epoch.
    at: i64,
    /// Linking this signed-in user's account to the provider, from Account, rather than signing in.
    link: Option<i64>,
}

const PENDING_KIND: &str = "sso-pending/1";

/// Two strings alike, in a time that does not say how much of them is.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

impl Pending {
    /// The cookie's value: the pending sign-in, sealed with the server's key.
    fn seal(&self, app: &App) -> Result<String> {
        app.sealer.seal(&serde_json::to_string(self)?)
    }

    /// The pending sign-in a cookie holds, if it is one this server sealed, for `state`, and not too old.
    fn open(app: &App, cookie: &str, state: &str) -> Option<Pending> {
        let p: Pending = serde_json::from_str(&app.sealer.open(cookie).ok()?).ok()?;
        let age = now() - p.at;
        (p.kind == PENDING_KIND && same(&p.state, state) && (0..PENDING_FOR.as_secs() as i64).contains(&age)).then_some(p)
    }
}

/// The `Set-Cookie` for a pending sign-in's cookie: only the sign-in routes see it, and `""` with no age takes it away.
fn pending_cookie(app: &App, value: &str, max_age: u64) -> HeaderValue {
    let secure = if app.secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{PENDING_COOKIE}={value}; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")).unwrap_or(HeaderValue::from_static(""))
}

/// What the flow keeps across sign-ins: the providers' documents and keys (a sign-in under way is the browser's).
#[derive(Default)]
pub struct Oidc {
    /// Each issuer's discovery document, by issuer.
    pub(crate) discovery: Mutex<HashMap<String, (Discovery, Instant)>>,
    jwks: Mutex<HashMap<String, (jsonwebtoken::jwk::JwkSet, Instant)>>,
}

pub(crate) fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("Freightdock/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

impl Oidc {
    pub async fn discover(&self, issuer: &str) -> Result<Discovery> {
        if let Some((d, at)) = self.discovery.lock().unwrap_or_else(|e| e.into_inner()).get(issuer)
            && at.elapsed() < DISCOVERY_FOR
        {
            return Ok(d.clone());
        }
        let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
        let d: Discovery = http().get(&url).send().await.and_then(|r| r.error_for_status()).with_context(|| format!("asking {url}"))?.json().await.with_context(|| format!("reading {url}"))?;
        // The document must be the issuer's own: a provider that names another is answering for somebody else.
        if d.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
            bail!("the provider says its issuer is {}, not {issuer}", d.issuer);
        }
        // Each address it names is held to the issuer's own rule: a browser is sent to two of them, and a `javascript:`
        // one would run in Freightdock's page ("SSO" 16).
        let named = [Some(&d.authorization_endpoint), Some(&d.token_endpoint), d.jwks_uri.as_ref(), d.userinfo_endpoint.as_ref(), d.end_session_endpoint.as_ref()];
        if let Some(bad) = named.into_iter().flatten().find(|u| !providers::https_or_local(u)) {
            bail!("the provider names {bad:?}, which is neither https nor http to this machine");
        }
        let mut m = self.discovery.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 64 {
            m.clear();
        }
        m.insert(issuer.to_string(), (d.clone(), Instant::now()));
        Ok(d)
    }

    async fn keys(&self, uri: &str, fresh: bool) -> Result<jsonwebtoken::jwk::JwkSet> {
        if !fresh
            && let Some((k, at)) = self.jwks.lock().unwrap_or_else(|e| e.into_inner()).get(uri)
            && at.elapsed() < DISCOVERY_FOR
        {
            return Ok(k.clone());
        }
        let k: jsonwebtoken::jwk::JwkSet = http().get(uri).send().await.and_then(|r| r.error_for_status()).with_context(|| format!("asking {uri}"))?.json().await?;
        let mut m = self.jwks.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 64 {
            m.clear();
        }
        m.insert(uri.to_string(), (k.clone(), Instant::now()));
        Ok(k)
    }

    /// The ID token's claims, once its signature, issuer, audience, time and nonce are what they must be.
    pub async fn verify(&self, token: &str, d: &Discovery, cfg: &Provider, secret: &str, nonce: &str) -> Result<serde_json::Map<String, Value>> {
        use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
        let head = decode_header(token).context("the ID token is not a JWT")?;
        let key = match head.alg {
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => DecodingKey::from_secret(secret.as_bytes()),
            _ => {
                let uri = d.jwks_uri.as_deref().ok_or_else(|| anyhow!("the provider names no keys (jwks_uri)"))?;
                let find = |set: &jsonwebtoken::jwk::JwkSet| match &head.kid {
                    Some(kid) => set.find(kid).cloned(),
                    None => (set.keys.len() == 1).then(|| set.keys[0].clone()),
                };
                let jwk = match find(&self.keys(uri, false).await?) {
                    Some(k) => k,
                    // A key it has not seen: the provider may have rotated its keys since they were last read.
                    None => find(&self.keys(uri, true).await?).ok_or_else(|| anyhow!("the ID token is signed with a key the provider does not list"))?,
                };
                DecodingKey::from_jwk(&jwk)?
            }
        };
        let mut v = Validation::new(head.alg);
        v.set_issuer(&[d.issuer.as_str(), d.issuer.trim_end_matches('/')]);
        v.set_audience(&[cfg.client_id.as_str()]);
        v.leeway = 60;
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let data = decode::<serde_json::Map<String, Value>>(token, &key, &v).context("the ID token did not check out")?;
        if data.claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
            bail!("the ID token is not for this sign-in (its nonce differs)");
        }
        Ok(data.claims)
    }
}

/// A claim's groups: a list of names, or one name alone.
pub fn groups_of(claims: &serde_json::Map<String, Value>, claim: &str) -> Vec<String> {
    match claims.get(claim) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    }
}

/// Why a person the provider vouched for still cannot come in.
#[derive(Debug, PartialEq)]
pub enum NotIn {
    Disabled(String),
    /// Linking by name is on, and the account of that name is linked to somebody else, at this provider or another.
    NameTaken(String),
    /// The provider's name is not one a local account could have ("SSO" 15).
    BadName(String),
}

/// The Freightdock user for this person at this provider: found by the provider's subject, its e-mail (and, with
/// `set_role`, its role) brought up to date; else, with `link_by_name`, the local account of the same name that is
/// linked to nobody yet, linked now; else made, under the name the provider gives, or (only with `link_by_name` off)
/// that name with " (SSO)" when a local account already has it ("SSO" 3, 6). Without `set_role` an account found keeps
/// its role, and one made is `role` ("SSO" 12). A disabled account stays out. A name a local account could not have
/// is refused rather than changed to fit, and the last administrator stays one ("SSO" 15).
#[allow(clippy::too_many_arguments)]
pub fn sso_user(c: &mut crate::db::rusqlite::Connection, issuer: &str, sub: &str, name: &str, email: Option<&str>, role: &str, set_role: bool, link_by_name: bool) -> Result<Result<User, NotIn>> {
    let tx = c.transaction()?;
    let found = tx
        .query_row("SELECT id, name, disabled, role FROM users WHERE oidc_issuer = ?1 AND oidc_sub = ?2", params![issuer, sub], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?, r.get::<_, String>(3)?)))
        .optional()?;
    // The very name, case aside, or none: a name changed to fit (trimmed, controls dropped, cut short) could be
    // another person's ("SSO" 15).
    if found.is_none() && auth::check_name(name).is_err() {
        return Ok(Err(NotIn::BadName(name.to_string())));
    }
    let by_name = if found.is_none() && link_by_name {
        tx.query_row("SELECT id, name, disabled, oidc_sub IS NOT NULL, role FROM users WHERE name = ?1", [name], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?, r.get::<_, bool>(3)?, r.get::<_, String>(4)?))
        })
        .optional()?
    } else {
        None
    };
    // The groups set the role, except that an SSO sign-in never takes the last administrator away ("SSO" 15).
    let pick = |id: i64, had: String| -> Result<String> {
        if !set_role || role == "admin" || had != "admin" {
            return Ok(if set_role { role.to_string() } else { had });
        }
        let others: i64 = tx.query_row("SELECT COUNT(*) FROM users WHERE id <> ?1 AND role = 'admin' AND disabled = 0", [id], |r| r.get(0))?;
        if others == 0 {
            tracing::warn!("an SSO sign-in's groups make the last administrator (user {id}) a {role}; the account stays an administrator");
            return Ok(had);
        }
        Ok(role.to_string())
    };
    let user = match (found, by_name) {
        (Some((_, name, true, _)), _) => return Ok(Err(NotIn::Disabled(name))),
        (Some((id, name, false, had)), _) => {
            let role = pick(id, had)?;
            tx.execute("UPDATE users SET role = ?1, email = COALESCE(?2, email) WHERE id = ?3", params![role, email, id])?;
            User { id, name, role, token: None }
        }
        (None, Some((_, name, _, true, _))) => return Ok(Err(NotIn::NameTaken(name))),
        (None, Some((_, name, true, false, _))) => return Ok(Err(NotIn::Disabled(name))),
        (None, Some((id, name, false, false, had))) => {
            let role = pick(id, had)?;
            tx.execute("UPDATE users SET oidc_issuer = ?1, oidc_sub = ?2, role = ?3, email = COALESCE(?4, email) WHERE id = ?5", params![issuer, sub, role, email, id])?;
            User { id, name, role, token: None }
        }
        (None, None) => {
            let free = |n: &str| -> Result<bool> { Ok(!tx.query_row("SELECT EXISTS (SELECT 1 FROM users WHERE name = ?1)", [n], |r| r.get::<_, bool>(0))?) };
            let base: String = name.chars().take(56).collect();
            let mut chosen = name.to_string();
            let mut i = 1;
            while !free(&chosen)? {
                chosen = if i == 1 { format!("{base} (SSO)") } else { format!("{base} (SSO {i})") };
                i += 1;
            }
            tx.execute(
                "INSERT INTO users (name, password_hash, role, created_at, oidc_issuer, oidc_sub, email) VALUES (?1, '!', ?2, ?3, ?4, ?5, ?6)",
                params![chosen, role, now(), issuer, sub, email],
            )?;
            User { id: tx.last_insert_rowid(), name: chosen, role: role.into(), token: None }
        }
    };
    tx.commit()?;
    Ok(Ok(user))
}

/// Links signed-in user `id` to this person at the provider (from Account, "SSO" 7): refused when somebody else is
/// linked to them already. The role stays as it is until they next sign in through the provider.
pub fn link_user(c: &mut crate::db::rusqlite::Connection, id: i64, issuer: &str, sub: &str, email: Option<&str>) -> Result<Result<String, String>> {
    let tx = c.transaction()?;
    let other: Option<(i64, String)> = tx.query_row("SELECT id, name FROM users WHERE oidc_issuer = ?1 AND oidc_sub = ?2", params![issuer, sub], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    if let Some((o, name)) = other
        && o != id
    {
        return Ok(Err(format!("That sign-in is linked to {name} already.")));
    }
    tx.execute("UPDATE users SET oidc_issuer = ?1, oidc_sub = ?2, email = COALESCE(?3, email) WHERE id = ?4", params![issuer, sub, email, id])?;
    let name: String = tx.query_row("SELECT name FROM users WHERE id = ?1", [id], |r| r.get(0))?;
    tx.commit()?;
    Ok(Ok(name))
}

/// The name the provider gives: its username claim, else the e-mail address when the provider vouches for it, else the
/// subject ("SSO" 15).
fn name_of(claims: &serde_json::Map<String, Value>, username_claim: &str, email_verified: bool) -> String {
    let sub = claims.get("sub").and_then(Value::as_str).unwrap_or_default();
    let email = claims.get("email").and_then(Value::as_str).filter(|_| email_verified);
    claims.get(username_claim).and_then(Value::as_str).or(email).unwrap_or(sub).to_string()
}

/// Only an address inside the app, and a short one.
fn safe_next(next: &str) -> String {
    if next.len() <= MAX_NEXT && next.starts_with('/') && !next.starts_with("//") && !next.starts_with("/\\") && !next.chars().any(char::is_control) { next.to_string() } else { "/".into() }
}

fn to_login(message: &str) -> Response {
    let q = url::form_urlencoded::Serializer::new(String::new()).append_pair("sso_error", message).finish();
    Redirect::to(&format!("/login?{q}")).into_response()
}

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

/// The provider's sign-in address for a new pending sign-in (or link), and the cookie that keeps it in this browser.
async fn authorize(app: &App, p: &Provider, next: String, link: Option<i64>) -> Result<(String, HeaderValue), &'static str> {
    let (state, nonce, verifier) = (random(24), random(24), random(48));
    let endpoint = match p.kind {
        Kind::Github => format!("{}/login/oauth/authorize", p.issuer.trim_end_matches('/')),
        Kind::Oidc => match app.oidc.discover(&p.issuer).await {
            Ok(d) => d.authorization_endpoint,
            Err(e) => {
                tracing::warn!("OIDC discovery: {e:#}");
                return Err("The sign-in provider could not be reached. Sign in with a password, or try again.");
            }
        },
        Kind::Ldap => return Err("A directory signs in with the password form."),
    };
    let mut url = url::Url::parse(&endpoint).map_err(|_| "The sign-in provider's address is not one Freightdock can use.")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &p.client_id)
            .append_pair("redirect_uri", &p.redirect_uri)
            .append_pair("scope", &p.scopes)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        match p.kind {
            Kind::Github => q.append_pair("allow_signup", "false"),
            _ => q.append_pair("nonce", &nonce),
        };
    }
    let pending = Pending { kind: PENDING_KIND.into(), state, provider: p.id.clone(), verifier, nonce, next, at: now(), link };
    let sealed = pending.seal(app).map_err(|e| {
        tracing::warn!("sealing a sign-in: {e:#}");
        "The sign-in could not be started. The server log says why."
    })?;
    Ok((url.to_string(), pending_cookie(app, &sealed, PENDING_FOR.as_secs())))
}

/// The first provider that is on and signs in through a button: the one kept from before the list (`oidc`) first.
async fn default_provider(app: &App) -> Option<Provider> {
    let list = providers::load(app).await.ok()?;
    let on = |p: &&Provider| p.enabled && p.kind != Kind::Ldap;
    list.iter().find(|p| on(p) && p.id == "oidc").or_else(|| list.iter().find(on)).cloned()
}

/// `GET /api/auth/oidc/login`: off to the first provider (the one kept from before the list, for old links).
pub async fn login(State(app): State<App>, Query(q): Query<LoginQuery>) -> Response {
    let Some(p) = default_provider(&app).await else { return to_login("Single sign-on is not set up.") };
    go(&app, &p, q).await
}

/// `GET /api/auth/sso/{id}/login`: off to that provider.
pub async fn login_with(State(app): State<App>, Path(id): Path<String>, Query(q): Query<LoginQuery>) -> Response {
    let Some(p) = providers::enabled(&app, &id).await.filter(|p| p.kind != Kind::Ldap) else { return to_login("That way of signing in is not set up.") };
    go(&app, &p, q).await
}

async fn go(app: &App, p: &Provider, q: LoginQuery) -> Response {
    match authorize(app, p, safe_next(q.next.as_deref().unwrap_or("/")), None).await {
        Ok((url, cookie)) => {
            let mut r = Redirect::to(&url).into_response();
            r.headers_mut().insert(header::SET_COOKIE, cookie);
            r
        }
        Err(m) => to_login(m),
    }
}

#[derive(Deserialize)]
pub struct LinkQuery {
    provider: Option<String>,
}

/// `POST /api/me/sso-link?provider=<id>`: `{redirect}`, the provider's sign-in, after which the signed-in local account
/// is linked to whoever signs in there ("SSO" 7). A POST with the CSRF token, so no other site can start it.
pub async fn link(State(app): State<App>, a: crate::api::Authed, Query(q): Query<LinkQuery>) -> ApiResult<Response> {
    if a.session.user.token.is_some() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "An account is linked in the Account page, not with a token."));
    }
    let id = a.session.user.id;
    let linked: bool = app.db.call(move |c| Ok(c.query_row("SELECT oidc_sub IS NOT NULL FROM users WHERE id = ?1", [id], |r| r.get(0))?)).await?;
    if linked {
        return Err(ApiError::conflict("Your account is linked to a sign-in provider already."));
    }
    let p = match q.provider {
        Some(pid) => providers::enabled(&app, &pid).await,
        None => default_provider(&app).await,
    };
    let p = p.filter(|p| p.kind != Kind::Ldap).ok_or_else(|| ApiError::conflict("Single sign-on is not set up."))?;
    let (url, cookie) = authorize(&app, &p, "/account".into(), Some(id)).await.map_err(ApiError::conflict)?;
    let mut r = Json(json!({ "redirect": url })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(r)
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenAnswer {
    id_token: Option<String>,
    access_token: Option<String>,
}

fn to_account(q: &str, message: &str) -> Response {
    let q = url::form_urlencoded::Serializer::new(String::new()).append_pair(q, message).finish();
    Redirect::to(&format!("/account?{q}")).into_response()
}

/// `GET /api/auth/oidc/callback` and `GET /api/auth/sso/{id}/callback`: back from a provider with a code; signed in (or
/// the account linked), or back to the sign-in page (Account) saying why. The state says which provider it was, and it
/// must have come back on that provider's own callback ("SSO" 9), to the browser that started it, whose cookie holds
/// it ("SSO" 14). Whatever happens, that cookie is spent.
pub async fn callback(State(app): State<App>, peer: Peer, uri: axum::http::Uri, headers: axum::http::HeaderMap, Query(q): Query<CallbackQuery>) -> Response {
    let mut r = finish_callback(&app, &peer, &uri, &headers, q).await;
    r.headers_mut().append(header::SET_COOKIE, pending_cookie(&app, "", 0));
    r
}

async fn finish_callback(app: &App, peer: &Peer, uri: &axum::http::Uri, headers: &axum::http::HeaderMap, q: CallbackQuery) -> Response {
    if let Some(e) = q.error {
        let why = q.error_description.unwrap_or(e);
        return to_login(&format!("The sign-in provider said no: {why}"));
    }
    let bound = crate::api::cookie_of(headers, PENDING_COOKIE);
    let Some(pending) = q.state.as_deref().zip(bound.as_deref()).and_then(|(s, c)| Pending::open(app, c, s)) else {
        return to_login("That sign-in was not started here, or took too long. Try again.");
    };
    let Some(p) = providers::enabled(app, &pending.provider).await else { return to_login("That way of signing in is not set up.") };
    if p.callback_path().as_deref() != Some(uri.path()) {
        crate::authlog::record(&app.db, event("login.sso.failed", "sso", false, None, Some("unknown".into()), peer, Some(format!("came back on {}, not {}'s callback", uri.path(), p.id)))).await;
        return to_login("That sign-in came back to the wrong address. Try again.");
    }
    let Some(code) = q.code else { return to_login("The sign-in provider sent no code.") };
    if let Some(uid) = pending.link {
        return link_back(app, &p, peer, headers, uid, code, &pending).await;
    }
    let done = match identify(app, &p, code, &pending).await {
        Ok(who) => crate::signin::admit(app, &p, who, peer, "sso").await,
        Err(r) => Err(r),
    };
    let started = match done {
        Ok(user) => auth::start(&app.db, user.id, peer.ip.clone(), peer.user_agent.clone()).await.map(|t| (user, t)).map_err(Refused::from),
        Err(r) => Err(r),
    };
    match started {
        Ok((user, token)) => {
            crate::authlog::record(&app.db, event("login.sso", "sso", true, Some(&user), None, peer, Some(p.label.clone()))).await;
            let mut r = Redirect::to(&pending.next).into_response();
            if let Ok(v) = HeaderValue::from_str(&crate::api::cookie_value(app, &token, auth::LIFETIME_S)) {
                r.headers_mut().append(header::SET_COOKIE, v);
            }
            r
        }
        Err(Refused(message, who)) => {
            crate::authlog::record(&app.db, event("login.sso.failed", "sso", false, None, Some(who.unwrap_or_else(|| "unknown".into())), peer, Some(format!("{}: {message}", p.label)))).await;
            to_login(&message)
        }
    }
}

/// Back from the provider to link account `uid`: only in the browser still signed in as it, and only to a person at
/// the provider nobody else is linked to.
async fn link_back(app: &App, p: &Provider, peer: &Peer, headers: &axum::http::HeaderMap, uid: i64, code: String, pending: &Pending) -> Response {
    let session = match crate::api::cookie_of(headers, auth::COOKIE) {
        Some(t) => auth::lookup(&app.db, &t).await.ok().flatten(),
        None => None,
    };
    let Some(session) = session.filter(|s| s.user.id == uid) else {
        return to_login("Sign in to the account to link first, then link it from Account.");
    };
    let user = &session.user;
    let refuse = |m: String| async move {
        crate::authlog::record(&app.db, event("sso.link", "sso", false, Some(user), None, peer, Some(m.clone()))).await;
        to_account("sso_error", &m)
    };
    let who = match identify(app, p, code, pending).await {
        Ok(x) => x,
        Err(Refused(m, _)) => return refuse(m).await,
    };
    let (iss, sub, email) = (p.identity_issuer(), who.sub.clone(), who.email.clone());
    match app.db.call(move |c| link_user(c, uid, &iss, &sub, email.as_deref())).await {
        Ok(Ok(_)) => {
            let detail = Some(format!("as {} at {}", who.name, p.label));
            crate::authlog::record(&app.db, event("sso.link", "sso", true, Some(&session.user), None, peer, detail.clone())).await;
            audit::record(&app.db, Some(&session.user), peer.ip.clone(), Entry { action: "account.sso-link", target: Some(session.user.name.clone()), detail }).await;
            to_account("sso", "linked")
        }
        Ok(Err(m)) => refuse(m).await,
        Err(e) => refuse(Refused::from(e).0).await,
    }
}

/// The code swapped for tokens and what the provider says about the person: for OpenID Connect the ID token checked
/// (and userinfo when it says too little), for GitHub its API (`signin::github`).
async fn identify(app: &App, p: &Provider, code: String, pending: &Pending) -> Result<Identity, Refused> {
    let secret = if p.client_secret.is_empty() { String::new() } else { app.sealer.open(&p.client_secret)? };
    if p.kind == Kind::Github {
        return Ok(crate::signin::github::identify(p, &secret, &code, &pending.verifier).await?);
    }
    let d = app.oidc.discover(&p.issuer).await?;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", p.redirect_uri.as_str()),
        ("code_verifier", pending.verifier.as_str()),
        ("client_id", p.client_id.as_str()),
        ("client_secret", secret.as_str()),
    ];
    let r = http().post(&d.token_endpoint).basic_auth(&p.client_id, Some(&secret)).form(&form).send().await.context("asking for the tokens")?;
    if !r.status().is_success() {
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("the token endpoint answered {status}: {}", body.chars().take(300).collect::<String>()).into());
    }
    let t: TokenAnswer = r.json().await.context("reading the tokens")?;
    let id_token = t.id_token.ok_or_else(|| anyhow!("the provider sent no ID token (is `openid` among the scopes?)"))?;
    let mut claims = app.oidc.verify(&id_token, &d, p, &secret, &pending.nonce).await?;
    // The groups and the name may be only in userinfo: some providers keep the ID token small.
    if (!claims.contains_key(&p.groups_claim) || !claims.contains_key(&p.username_claim))
        && let (Some(ui), Some(at)) = (&d.userinfo_endpoint, &t.access_token)
        && let Ok(r) = http().get(ui).bearer_auth(at).send().await
        && let Ok(extra) = r.json::<serde_json::Map<String, Value>>().await
        && extra.get("sub") == claims.get("sub")
    {
        for (k, v) in extra {
            claims.entry(k).or_insert(v);
        }
    }
    let sub = claims.get("sub").and_then(Value::as_str).ok_or_else(|| anyhow!("the ID token has no subject"))?.to_string();
    let email = claims.get("email").and_then(Value::as_str).map(str::to_string);
    // `email_verified` is a boolean, or (from some providers) the string "true".
    let email_verified = matches!(claims.get("email_verified"), Some(Value::Bool(true))) || claims.get("email_verified").and_then(Value::as_str) == Some("true");
    let name = name_of(&claims, &p.username_claim, email_verified);
    let groups = groups_of(&claims, &p.groups_claim);
    let picture = crate::avatars::picture_of(&claims);
    Ok(Identity { sub, name, email, email_verified, groups, picture })
}

/// Where a person signed in through a provider goes to sign out there too, if the provider says.
pub async fn end_session(app: &App, user_id: i64) -> Option<String> {
    let issuer: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT oidc_issuer FROM users WHERE id = ?1 AND oidc_sub IS NOT NULL", [user_id], |r| r.get(0)).optional()?)).await.ok()?;
    let issuer = issuer?;
    let p = providers::load(app).await.ok()?.into_iter().find(|p| p.enabled && p.kind == Kind::Oidc && p.identity_issuer() == issuer)?;
    let d = app.oidc.discover(&p.issuer).await.ok()?;
    let mut url = url::Url::parse(d.end_session_endpoint.as_deref()?).ok()?;
    url.query_pairs_mut().append_pair("client_id", &p.client_id);
    Some(url.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn pkce_s256_is_rfc_7636_s_own_example() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn groups_come_as_a_list_or_one_name() {
        let m = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(groups_of(&m(json!({ "groups": ["a", "b"] })), "groups"), vec!["a", "b"]);
        assert_eq!(groups_of(&m(json!({ "groups": "a" })), "groups"), vec!["a"]);
        assert!(groups_of(&m(json!({})), "groups").is_empty());
    }

    #[test]
    fn a_pending_sign_in_opens_only_for_its_own_state_and_not_forever() {
        let app = crate::api::tests::app();
        let p = |at| Pending { kind: PENDING_KIND.into(), state: "s".into(), provider: "oidc".into(), verifier: "v".into(), nonce: "n".into(), next: "/".into(), at, link: None };
        let sealed = p(now()).seal(&app).unwrap();
        assert!(!sealed.contains("\"v\""), "the verifier is not readable in the cookie");
        assert_eq!(Pending::open(&app, &sealed, "s").map(|o| o.verifier), Some("v".to_string()));
        assert!(Pending::open(&app, &sealed, "other").is_none(), "only for the state it was started with");
        assert!(Pending::open(&app, &p(now() - PENDING_FOR.as_secs() as i64 - 1).seal(&app).unwrap(), "s").is_none(), "not after ten minutes");
        assert!(Pending::open(&app, &app.sealer.seal("not a sign-in").unwrap(), "s").is_none());
        let other_key = crate::secret::Sealer::from_key(&[9u8; 32]).seal(&serde_json::to_string(&p(now())).unwrap()).unwrap();
        assert!(Pending::open(&app, &other_key, "s").is_none(), "only what this server sealed");
        let wrong_kind = Pending { kind: "something-else".into(), ..p(now()) };
        assert!(Pending::open(&app, &wrong_kind.seal(&app).unwrap(), "s").is_none());
    }

    #[test]
    fn only_addresses_inside_the_app_are_followed() {
        assert_eq!(safe_next("/containers?x=1"), "/containers?x=1");
        assert_eq!(safe_next("//evil.example"), "/");
        assert_eq!(safe_next("https://evil.example"), "/");
        assert_eq!(safe_next("/\\evil.example"), "/");
    }

    #[tokio::test]
    async fn a_provider_s_person_is_found_again_by_subject_and_never_takes_a_local_account() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute("INSERT INTO users (name, password_hash, role, created_at) VALUES ('alice', 'x', 'admin', 0)", [])?;
            let a = sso_user(c, "https://idp/", "sub-1", "alice", Some("o@x"), "user", true, false)?.unwrap();
            assert_eq!(a.name, "alice (SSO)", "with linking by name off, the local alice stays the local one");
            assert_eq!(a.role, "user");
            let again = sso_user(c, "https://idp/", "sub-1", "renamed-at-the-idp", None, "admin", true, false)?.unwrap();
            assert_eq!((again.id, again.name.as_str(), again.role.as_str()), (a.id, "alice (SSO)", "admin"), "the role follows the groups at each sign-in");
            let other = sso_user(c, "https://idp/", "sub-2", "alice", None, "user", true, false)?.unwrap();
            assert_eq!(other.name, "alice (SSO 2)");
            let local_role: String = c.query_row("SELECT role FROM users WHERE name = 'alice'", [], |r| r.get(0))?;
            assert_eq!(local_role, "admin");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn with_linking_by_name_a_provider_s_person_signs_in_to_the_local_account_of_that_name() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute_batch(
                "INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'alice', 'hash', 'admin', 0), (2, 'gone', 'x', 'user', 0);
                 UPDATE users SET disabled = 1 WHERE id = 2;",
            )?;
            let a = sso_user(c, "https://idp/", "sub-1", "ALICE", Some("o@x"), "admin", true, true)?.unwrap();
            assert_eq!((a.id, a.name.as_str()), (1, "alice"), "the local account, its name matched without case");
            let (sub, hash): (String, String) = c.query_row("SELECT oidc_sub, password_hash FROM users WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            assert_eq!((sub.as_str(), hash.as_str()), ("sub-1", "hash"), "linked, and its password kept for the local sign-in");
            let again = sso_user(c, "https://idp/", "sub-1", "renamed", None, "admin", true, true)?.unwrap();
            assert_eq!(again.id, 1, "found by its subject from then on");
            assert_eq!(sso_user(c, "https://idp/", "sub-2", "alice", None, "user", true, true)?, Err(NotIn::NameTaken("alice".into())), "never a second, (SSO) account");
            assert_eq!(sso_user(c, "https://other-idp/", "sub-1", "alice", None, "user", true, true)?, Err(NotIn::NameTaken("alice".into())), "nor an account another provider has");
            assert_eq!(sso_user(c, "https://idp/", "sub-3", "gone", None, "user", true, true)?, Err(NotIn::Disabled("gone".into())));
            let fresh = sso_user(c, "https://idp/", "sub-4", "newcomer", None, "user", true, true)?.unwrap();
            assert_eq!(fresh.name, "newcomer");
            let users: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE name LIKE '%(SSO%'", [], |r| r.get(0))?;
            assert_eq!(users, 0);
            c.execute("UPDATE users SET disabled = 1 WHERE id = 1", [])?;
            assert_eq!(sso_user(c, "https://idp/", "sub-1", "alice", None, "admin", true, true)?, Err(NotIn::Disabled("alice".into())), "disabled means out, through SSO too");
            Ok(())
        })
        .await
        .unwrap();
        assert!(Provider::default().link_by_name, "on by default (owner's decision)");
        let old: Provider = serde_json::from_str(r#"{"enabled": true, "issuer": "https://idp/"}"#).unwrap();
        assert!(old.link_by_name, "settings kept before the switch was there get it on");
    }

    #[tokio::test]
    async fn linking_by_name_takes_only_the_very_name_and_never_one_a_local_account_could_not_have() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            let long = "b".repeat(56);
            c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'root', 'h', 'admin', 0), (2, 'ops', 'h', 'admin', 0), (3, ?1, 'h', 'admin', 0)", [&long])?;
            let bad = |r: Result<User, NotIn>| matches!(r, Err(NotIn::BadName(_)));
            assert!(bad(sso_user(c, "https://idp/", "x-1", "ro\0ot", None, "user", true, true)?), "a control character is not dropped to make root");
            assert!(bad(sso_user(c, "https://idp/", "x-2", "\tops\x7f ", None, "user", true, true)?), "nor spaces and controls at the ends trimmed to make ops");
            assert!(bad(sso_user(c, "https://idp/", "x-3", &format!("{long}-another-person"), None, "user", true, true)?), "nor a name too long cut to make another");
            let other = sso_user(c, "https://idp/", "x-4", &format!("{long}-x"), None, "user", true, true)?.unwrap();
            assert_eq!(other.name, format!("{long}-x"), "a longer name that fits is somebody new, under that very name");
            let linked: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE oidc_sub IS NOT NULL", [], |r| r.get(0))?;
            assert_eq!(linked, 1, "no local account was taken");
            let admins: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| r.get(0))?;
            assert_eq!(admins, 3);
            let root = sso_user(c, "https://idp/", "x-5", "ROOT", None, "user", true, true)?.unwrap();
            assert_eq!(root.id, 1, "the same name without regard to case still links");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_sso_sign_in_never_demotes_the_last_administrator() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'ola', 'h', 'admin', 0), (2, 'kari', 'h', 'user', 0)", [])?;
            let a = sso_user(c, "https://idp/", "s-1", "ola", None, "user", true, true)?.unwrap();
            assert_eq!(a.role, "admin", "linked by name, and the last administrator stays one");
            let a = sso_user(c, "https://idp/", "s-1", "ola", None, "user", true, true)?.unwrap();
            assert_eq!(a.role, "admin", "found by subject at the next sign-in, still");
            c.execute("UPDATE users SET role = 'admin' WHERE id = 2", [])?;
            let a = sso_user(c, "https://idp/", "s-1", "ola", None, "user", true, true)?.unwrap();
            assert_eq!(a.role, "user", "with another administrator the groups decide again");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[test]
    fn the_name_falls_back_to_an_e_mail_address_only_when_the_provider_vouches_for_it() {
        let m = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(name_of(&m(json!({ "sub": "s-1", "preferred_username": "ola", "email": "x@y" })), "preferred_username", false), "ola");
        assert_eq!(name_of(&m(json!({ "sub": "s-1", "email": "root" })), "preferred_username", false), "s-1", "an unverified address is no name");
        assert_eq!(name_of(&m(json!({ "sub": "s-1", "email": "ola@example.org" })), "preferred_username", true), "ola@example.org");
    }

    /// A provider whose discovery document names `endpoint` as where to sign in and to sign out.
    async fn provider_naming(endpoint: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let b = base.clone();
        let app = axum::Router::new().route("/.well-known/openid-configuration", axum::routing::get(move || async move {
            Json(json!({ "issuer": b, "authorization_endpoint": endpoint, "token_endpoint": format!("{b}/token"), "end_session_endpoint": endpoint }))
        }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn a_provider_s_addresses_are_https_or_this_machine_s_http() {
        for bad in ["javascript:alert(document.domain)//", "data:text/html,hi", "http://idp.example.org/authorize"] {
            let o = Oidc::default();
            assert!(o.discover(&provider_naming(bad).await).await.is_err(), "{bad} is refused");
        }
        for good in ["https://idp.example.org/authorize", "http://127.0.0.1:9/authorize", "http://localhost/authorize"] {
            let o = Oidc::default();
            assert!(o.discover(&provider_naming(good).await).await.is_ok(), "{good} is taken");
        }
        let (app, _, cookie, csrf) = with_local_ola(json!([]), false).await;
        keep(&app, authentik(&app, provider_naming("javascript:alert(document.domain)//").await, false)).await;
        let link = crate::api::tests::post("/api/me/sso-link").header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(axum::body::Body::empty()).unwrap();
        let (s, _, b) = crate::api::tests::call(&app, link).await;
        assert_eq!(s, StatusCode::CONFLICT, "no javascript: address goes to the page to follow: {b}");
    }

    #[tokio::test]
    async fn a_provider_that_names_no_administrator_group_leaves_the_role_as_it_is() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'ola@example.org', 'hash', 'admin', 0)", [])?;
            let a = sso_user(c, "https://accounts.google.com", "g-1", "ola@example.org", None, "user", false, true)?.unwrap();
            assert_eq!(a.role, "admin", "linked by name, and still an administrator");
            let a = sso_user(c, "https://accounts.google.com", "g-1", "ola@example.org", None, "user", false, true)?.unwrap();
            assert_eq!(a.role, "admin", "at the next sign-in too");
            let b = sso_user(c, "https://accounts.google.com", "g-2", "kari@example.org", None, "user", false, true)?.unwrap();
            assert_eq!(b.role, "user", "somebody new is a user");
            Ok(())
        })
        .await
        .unwrap();
    }

    /// The provider kept as the settings page keeps it.
    pub(crate) async fn keep(app: &App, p: Provider) {
        let mut list = providers::load(app).await.unwrap();
        list.retain(|o| o.id != p.id);
        list.push(p);
        app.db.put_setting(providers::KEY, &json!({ "providers": list })).await.unwrap();
    }

    /// A provider of our own on a local port: discovery, and a token endpoint that signs an ID token HS256 with the
    /// client secret, with the nonce and the groups the test sets.
    async fn provider(groups: Value) -> (String, std::sync::Arc<Mutex<String>>) {
        let nonce = std::sync::Arc::new(Mutex::new(String::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (b, n) = (base.clone(), nonce.clone());
        let app = axum::Router::new()
            .route("/.well-known/openid-configuration", axum::routing::get({
                let b = b.clone();
                move || async move { Json(json!({ "issuer": b, "authorization_endpoint": format!("{b}/authorize"), "token_endpoint": format!("{b}/token") })) }
            }))
            .route("/token", axum::routing::post(move |body: String| async move {
                assert!(body.contains("code_verifier="), "PKCE's verifier goes with the code");
                let claims = json!({ "iss": b, "aud": "freightdock", "sub": "u-1", "exp": now() + 300, "nonce": *n.lock().unwrap(), "preferred_username": "ola", "email": "ola@example.com", "groups": groups });
                let t = jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256), &claims, &jsonwebtoken::EncodingKey::from_secret(b"s3cret")).unwrap();
                Json(json!({ "id_token": t, "access_token": "at", "token_type": "Bearer" }))
            }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, nonce)
    }

    /// The provider kept from before the list: id `oidc`, on the old callback.
    fn authentik(app: &App, issuer: String, link_by_name: bool) -> Provider {
        Provider { id: "oidc".into(), enabled: true, label: "Authentik".into(), issuer, client_id: "freightdock".into(), client_secret: app.sealer.seal("s3cret").unwrap(), redirect_uri: "https://ship.test/api/auth/oidc/callback".into(), link_by_name, ..Default::default() }
    }

    async fn sign_in_through(groups: Value) -> (axum::http::StatusCode, axum::http::HeaderMap, App) {
        use crate::api::tests::{app, call};
        use axum::body::Body;
        use axum::http::Request;
        let app = app();
        let (base, nonce) = provider(groups).await;
        keep(&app, authentik(&app, base, true)).await;

        let (to, bound) = start(&app, "/api/auth/oidc/login?next=/images").await;
        let to = url::Url::parse(&to).unwrap();
        let q: HashMap<String, String> = to.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "https://ship.test/api/auth/oidc/callback");
        *nonce.lock().unwrap() = q["nonce"].clone();
        let (s, h, _) = call(&app, Request::get(format!("/api/auth/oidc/callback?code=c-1&state={}", q["state"])).header(header::COOKIE, bound).body(Body::empty()).unwrap()).await;
        (s, h, app)
    }

    #[tokio::test]
    async fn signing_in_through_the_provider_makes_the_user_its_groups_say() {
        use crate::api::tests::call;
        use axum::body::Body;
        use axum::http::Request;
        let (s, h, app) = sign_in_through(json!(["freightdock-write"])).await;
        assert!(s.is_redirection());
        assert_eq!(h[header::LOCATION], "/images", "on to where they were going");
        let cookie = session_set(&h).expect("signed in");
        let (s, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(me["user"]["name"], "ola");
        assert_eq!(me["user"]["role"], "admin");
        let (_, _, st) = call(&app, Request::get("/api/status").body(Body::empty()).unwrap()).await;
        assert_eq!(st["sso"]["providers"][0]["label"], "Authentik");
    }

    /// The provider set up for `app`, which has `ola` as a local administrator signed in already: its cookie and CSRF.
    async fn with_local_ola(groups: Value, link_by_name: bool) -> (App, std::sync::Arc<Mutex<String>>, String, String) {
        use crate::api::tests::{app, call};
        use axum::body::Body;
        use axum::http::Request;
        let app = app();
        let creds = json!({ "name": "ola", "password": "a long enough password", "token": crate::api::tests::setup_token(&app) });
        let (_, h, _) = call(&app, crate::api::tests::post("/api/setup").body(crate::api::tests::json_body(creds)).unwrap()).await;
        let cookie = h[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let (base, nonce) = provider(groups).await;
        keep(&app, authentik(&app, base, link_by_name)).await;
        (app, nonce, cookie, me["csrf"].as_str().unwrap().to_string())
    }

    /// The value of cookie `name` that a response sets, if it sets one with a value.
    pub(crate) fn set_cookie(h: &axum::http::HeaderMap, name: &str) -> Option<String> {
        h.get_all(header::SET_COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
            let (k, rest) = v.split(';').next()?.split_once('=')?;
            (k == name && !rest.is_empty()).then(|| format!("{k}={rest}"))
        })
    }

    /// The session a response signs in, as a `Cookie` header's value.
    pub(crate) fn session_set(h: &axum::http::HeaderMap) -> Option<String> {
        set_cookie(h, auth::COOKIE)
    }

    /// Starts a sign-in (or the link) at `path` in a browser of its own: the provider's address, and the cookie that
    /// binds the sign-in to that browser.
    pub(crate) async fn start(app: &App, path: &str) -> (String, String) {
        let (s, h, _) = crate::api::tests::call(app, axum::http::Request::get(path).body(axum::body::Body::empty()).unwrap()).await;
        assert!(s.is_redirection(), "{h:?}");
        let bound = set_cookie(&h, PENDING_COOKIE).expect("the sign-in is bound to the browser that started it, by a cookie");
        (h[header::LOCATION].to_str().unwrap().to_string(), bound)
    }

    #[tokio::test]
    async fn the_callback_signs_in_only_the_browser_that_started_the_sign_in() {
        // Somebody starts a sign-in as themselves and hands the address back from the provider to somebody else.
        let (app, nonce, victim, _) = with_local_ola(json!(["freightdock-write"]), false).await;
        let (to, theirs) = start(&app, "/api/auth/oidc/login?next=/registries").await;
        let (s, h) = come_back(&app, &nonce, &to, Some(&victim)).await;
        assert!(s.is_redirection() && h[header::LOCATION].to_str().unwrap().starts_with("/login?sso_error="), "{h:?}");
        assert!(session_set(&h).is_none(), "the other browser is signed in as nobody new");
        let other: Option<String> = None;
        let (_, h) = come_back(&app, &nonce, &to, other.as_deref()).await;
        assert!(session_set(&h).is_none(), "nor is a browser with no cookie at all");
        let (s, h) = come_back(&app, &nonce, &to, Some(&theirs)).await;
        assert!(s.is_redirection() && session_set(&h).is_some(), "the browser that started it is signed in: {h:?}");
        assert_eq!(h[header::LOCATION], "/registries");
        assert!(h.get_all(header::SET_COOKIE).iter().any(|v| v.to_str().unwrap().starts_with(&format!("{PENDING_COOKIE}=;"))), "and its sign-in is spent: {h:?}");
    }

    #[tokio::test]
    async fn sign_ins_started_elsewhere_never_crowd_out_one_s_own() {
        use crate::api::tests::call;
        let (app, nonce, _, _) = with_local_ola(json!(["freightdock-write"]), true).await;
        let (to, mine) = start(&app, "/api/auth/oidc/login").await;
        // The server keeps nothing for a sign-in under way, so no number of them fills anything up; a few hundred
        // stand for the ten thousand that once closed single sign-on to everybody.
        let long = format!("/{}", "b".repeat(60_000));
        for i in 0..300 {
            let path = if i < 50 { format!("/api/auth/oidc/login?next={long}") } else { "/api/auth/oidc/login".to_string() };
            let (_, h, _) = call(&app, axum::http::Request::get(path).body(axum::body::Body::empty()).unwrap()).await;
            assert!(set_cookie(&h, PENDING_COOKIE).unwrap().len() < 1024, "a long address to go on to is not kept");
        }
        let (s, h) = come_back(&app, &nonce, &to, Some(&mine)).await;
        assert!(s.is_redirection() && session_set(&h).is_some(), "signed in, however many sign-ins others started: {h:?}");
    }

    #[test]
    fn an_address_to_go_on_to_is_kept_short() {
        let long = format!("/{}", "b".repeat(MAX_NEXT));
        assert_eq!(safe_next(&long), "/");
        let fits = format!("/{}", "b".repeat(MAX_NEXT - 1));
        assert_eq!(safe_next(&fits), fits);
    }

    /// Back from the provider for the sign-in (or link) that `to` started, with `cookie` when given.
    async fn come_back(app: &App, nonce: &Mutex<String>, to: &str, cookie: Option<&str>) -> (StatusCode, axum::http::HeaderMap) {
        use crate::api::tests::call;
        let to = url::Url::parse(to).unwrap();
        let q: HashMap<String, String> = to.query_pairs().into_owned().collect();
        *nonce.lock().unwrap() = q["nonce"].clone();
        let mut r = axum::http::Request::get(format!("/api/auth/oidc/callback?code=c-1&state={}", q["state"]));
        if let Some(c) = cookie {
            r = r.header(header::COOKIE, c);
        }
        let (s, h, _) = call(app, r.body(axum::body::Body::empty()).unwrap()).await;
        (s, h)
    }

    #[tokio::test]
    async fn the_first_sso_sign_in_lands_in_the_local_account_of_the_same_name() {
        let (app, nonce, _, _) = with_local_ola(json!(["freightdock-write"]), true).await;
        let (to, bound) = start(&app, "/api/auth/oidc/login").await;
        let (s, h) = come_back(&app, &nonce, &to, Some(&bound)).await;
        assert!(s.is_redirection() && session_set(&h).is_some(), "signed in: {h:?}");
        let names: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT name FROM users ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(names, ["ola"], "no second, \"(SSO)\" account");
        let linked: Option<String> = app.db.call(|c| Ok(c.query_row("SELECT oidc_sub FROM users WHERE name = 'ola'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(linked.as_deref(), Some("u-1"));
        let events: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT event FROM auth_log")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(events, ["login.sso"]);
    }

    #[tokio::test]
    async fn with_linking_by_name_off_the_provider_s_ola_is_somebody_else() {
        let (app, nonce, _, _) = with_local_ola(json!(["freightdock-read"]), false).await;
        let (to, bound) = start(&app, "/api/auth/oidc/login").await;
        come_back(&app, &nonce, &to, Some(&bound)).await;
        let names: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT name FROM users ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(names, ["ola", "ola (SSO)"], "as before the switch");
    }

    #[tokio::test]
    async fn a_signed_in_local_user_links_their_account_from_account() {
        use crate::api::tests::call;
        let (app, nonce, cookie, csrf) = with_local_ola(json!([]), false).await;
        let begin = |cookie: &str, csrf: &str| crate::api::tests::post("/api/me/sso-link").header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(axum::body::Body::empty()).unwrap();
        let (s, _, _) = call(&app, begin(&cookie, "forged")).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "it is started with the CSRF token");
        let (s, h, b) = call(&app, begin(&cookie, &csrf)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let to = b["redirect"].as_str().unwrap().to_string();
        let bound = set_cookie(&h, PENDING_COOKIE).expect("the link is bound to this browser too");
        let (s, h) = come_back(&app, &nonce, &to, Some(&bound)).await;
        assert!(s.is_redirection() && h[header::LOCATION].to_str().unwrap().starts_with("/login?sso_error="), "not in the browser signed in as ola: {h:?}");
        let (_, h, b) = call(&app, begin(&cookie, &csrf)).await;
        let both = format!("{cookie}; {}", set_cookie(&h, PENDING_COOKIE).unwrap());
        let (s, h) = come_back(&app, &nonce, b["redirect"].as_str().unwrap(), Some(&both)).await;
        assert!(s.is_redirection());
        assert_eq!(h[header::LOCATION], "/account?sso=linked", "even in no group: linking is not signing in");
        let linked: Option<String> = app.db.call(|c| Ok(c.query_row("SELECT oidc_sub FROM users WHERE name = 'ola'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(linked.as_deref(), Some("u-1"));
        let (s, _, _) = call(&app, begin(&cookie, &csrf)).await;
        assert_eq!(s, StatusCode::CONFLICT, "linked already");
        let (_, _, log) = call(&app, axum::http::Request::get("/api/auth-log?event=sso").header(header::COOKIE, &cookie).body(axum::body::Body::empty()).unwrap()).await;
        assert_eq!(log["entries"][0]["ok"], true);
    }

    #[tokio::test]
    async fn somebody_in_no_group_of_freightdock_s_is_sent_back_with_the_reason() {
        let (s, h, app) = sign_in_through(json!(["portainer-write"])).await;
        assert!(s.is_redirection());
        let to = h[header::LOCATION].to_str().unwrap();
        assert!(to.starts_with("/login?sso_error="), "{to}");
        assert!(session_set(&h).is_none());
        let users: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(users, 0, "nobody is made for them");
    }

    #[tokio::test]
    async fn a_state_that_was_never_handed_out_signs_nobody_in() {
        use crate::api::tests::{app, call};
        use axum::body::Body;
        use axum::http::Request;
        let app = app();
        let (s, h, _) = call(&app, Request::get("/api/auth/oidc/callback?code=x&state=forged").body(Body::empty()).unwrap()).await;
        assert!(s.is_redirection());
        assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login?sso_error="));
    }

    #[tokio::test]
    async fn the_groups_put_a_person_in_their_teams_at_every_sign_in() {
        use crate::signin::teams::{TeamRule, TeamSync};
        let (app, nonce, _, _) = with_local_ola(json!(["freightdock-read", "team-blue", "devs"]), true).await;
        let mut p = providers::load(&app).await.unwrap().remove(0);
        p.teams = TeamSync { enabled: true, create_missing: true, rules: vec![TeamRule { group: "devs".into(), team: "Developers".into() }, TeamRule { group: "/^team-(.+)$/".into(), team: "Team $1".into() }] };
        keep(&app, p).await;
        let (to, bound) = start(&app, "/api/auth/oidc/login").await;
        let (s, h) = come_back(&app, &nonce, &to, Some(&bound)).await;
        assert!(s.is_redirection() && session_set(&h).is_some(), "{h:?}");
        let teams: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id ORDER BY t.name")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(teams, ["Developers", "Team blue"]);
        let detail: String = app.db.call(|c| Ok(c.query_row("SELECT detail FROM auth_log WHERE event = 'teams.sync'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(detail, "joined Developers, Team blue; made Developers, Team blue");
    }

    #[tokio::test]
    async fn a_sign_in_that_comes_back_on_another_provider_s_callback_is_refused() {
        use crate::api::tests::{app, call};
        let app = app();
        let (base, _) = provider(json!(["freightdock-read"])).await;
        keep(&app, Provider { id: "work".into(), redirect_uri: "https://ship.test/api/auth/sso/work/callback".into(), ..authentik(&app, base.clone(), true) }).await;
        keep(&app, Provider { id: "home".into(), redirect_uri: "https://ship.test/api/auth/sso/home/callback".into(), ..authentik(&app, base, true) }).await;
        let (to, bound) = start(&app, "/api/auth/sso/work/login").await;
        let to = url::Url::parse(&to).unwrap();
        let q: HashMap<String, String> = to.query_pairs().into_owned().collect();
        assert_eq!(q["redirect_uri"], "https://ship.test/api/auth/sso/work/callback");
        let (s, h, _) = call(&app, axum::http::Request::get(format!("/api/auth/sso/home/callback?code=c&state={}", q["state"])).header(header::COOKIE, bound).body(axum::body::Body::empty()).unwrap()).await;
        assert!(s.is_redirection() && session_set(&h).is_none());
        assert!(h[header::LOCATION].to_str().unwrap().contains("wrong+address"), "{h:?}");
    }
}
