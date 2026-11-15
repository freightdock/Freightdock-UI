//! The HTTP API under `/api`. Every route but status, setup, sign-in and a webhook's call needs a session or an API
//! token; every request with a session that changes something also needs the session's CSRF token in `X-CSRF-Token`,
//! and an `Origin`, when the browser sends one, that is this server ("Sign-in" in DECISIONS.md). A token needs neither
//! (`tokens.rs`); a webhook's token is its own credential ("Sign-in" 8).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::audit::{self, entry};
use crate::auth::{self, COOKIE, Session};
use crate::authlog::{self, Event};
use crate::db::{now, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/confirm", post(crate::signin::confirm::confirm))
        .route("/api/users/{id}/avatar", get(crate::avatars::get))
        .route("/api/audit", get(crate::audit::list))
        .route("/api/auth-log", get(crate::authlog::list))
        .route("/api/me/sso-link", post(crate::oidc::link))
        .route("/api/changelog", get(crate::changelog::changelog))
        .route("/api/auth/oidc/login", get(crate::oidc::login))
        // The callback from before the list of providers: the provider kept from then is still sent back here.
        .route("/api/auth/oidc/callback", get(crate::oidc::callback))
        .route("/api/auth/sso/{id}/login", get(crate::oidc::login_with))
        .route("/api/auth/sso/{id}/callback", get(crate::oidc::callback))
        .route("/api/login/totp", post(crate::signin::totp::login_code))
        .route("/api/login/totp/enrol", post(crate::signin::totp::login_enrol))
        .route("/api/me/totp", get(crate::signin::totp::status).post(crate::signin::totp::start))
        .route("/api/me/totp/confirm", post(crate::signin::totp::confirm))
        .route("/api/me/totp/off", post(crate::signin::totp::off))
        .route("/api/me/totp/recovery", post(crate::signin::totp::new_recovery))
        .route("/api/users/{id}/totp", axum::routing::delete(crate::signin::totp::reset))
        .route("/api/settings/two-factor", get(crate::signin::totp::get_policy).put(crate::signin::totp::put_policy))
        .route("/api/settings/sso", get(crate::signin::providers::get_settings))
        .route("/api/settings/sso/test", post(crate::signin::providers::test))
        .route("/api/settings/sso/{id}", axum::routing::put(crate::signin::providers::put_provider).delete(crate::signin::providers::remove_provider))
        .route("/api/settings/sso/{id}/ldap-test", post(crate::signin::ldap::test))
        .route("/api/settings/appearance", get(crate::appearance::get_settings).put(crate::appearance::put_settings))
        .merge(crate::tokens::router())
        .merge(crate::tls::router())
        .merge(crate::users::router())
        .merge(crate::cluster::router())
        .merge(crate::mcp::router())
        .merge(crate::portainer::router())
        // Last, so it is on every route above: each is held to its rule in access::routes::RULES ("Access" 5).
        .route_layer(axum::middleware::from_fn_with_state(app.clone(), crate::access::routes::guard))
        .with_state(app)
}

/// The address a request came from, when the server knows it.
pub fn peer(parts: &Parts) -> Option<String> {
    parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string())
}

pub(crate) fn cookie_of(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The browser's `Origin`, when it sent one, must be this server: the one it asked by `Host`.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return true };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else { return false };
    origin.split_once("://").is_some_and(|(_, rest)| rest == host)
}

/// A signed-in person. Refuses a request without a good session, and a changing one without its CSRF token.
#[derive(Clone)]
pub struct Authed {
    pub session: Session,
    pub ip: Option<String>,
}

impl<S> FromRequestParts<S> for Authed
where
    App: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // The guard (access::routes) signed them in already: once is enough.
        if let Some(a) = parts.extensions.get::<Authed>() {
            return Ok(a.clone());
        }
        let app = App::from_ref(state);
        // An API token is a program's, never sent by a browser on its own, so it needs neither the CSRF token nor the
        // same origin ("Sign-in" 9). A read-only one may only read.
        if let Some(bearer) = crate::tokens::bearer(&parts.headers) {
            let token = bearer.map_err(|()| ApiError::unauthorized())?;
            let user = crate::tokens::lookup(&app.db, token, peer(parts)).await?.ok_or_else(ApiError::unauthorized)?;
            if !safe(&parts.method) && user.token.as_ref().is_some_and(|t| t.read_only) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "This API token may only read."));
            }
            return Ok(Authed { session: Session { user, csrf: String::new(), token_hash: String::new(), confirmed_at: 0 }, ip: peer(parts) });
        }
        let token = cookie_of(&parts.headers, COOKIE).ok_or_else(ApiError::unauthorized)?;
        let session = auth::lookup(&app.db, &token).await?.ok_or_else(ApiError::unauthorized)?;
        // A WebSocket upgrade is a GET, so it is held to the origin rule too: a page elsewhere cannot open a console.
        let upgrade = parts.headers.get(header::UPGRADE).is_some();
        if (!safe(&parts.method) || upgrade) && !same_origin(&parts.headers) {
            return Err(ApiError::forbidden());
        }
        if !safe(&parts.method) {
            let sent = parts.headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).unwrap_or("");
            if !auth::same(sent, &session.csrf) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "This page is out of date. Reload it and try again."));
            }
        }
        Ok(Authed { session, ip: peer(parts) })
    }
}

/// A signed-in administrator.
pub struct Admin(#[allow(dead_code)] pub Authed);

impl<S> FromRequestParts<S> for Admin
where
    App: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let a = Authed::from_request_parts(parts, state).await?;
        if !a.session.user.is_admin() {
            return Err(ApiError::forbidden());
        }
        // A console or an attach opens with a GET, but is a change: a read-only token may not open one.
        if parts.headers.contains_key(header::UPGRADE) && a.session.user.token.as_ref().is_some_and(|t| t.read_only) {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "This API token may only read."));
        }
        Ok(Admin(a))
    }
}

/// Where a request came from, for routes anyone may call.
pub struct Peer {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub same_origin: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Peer {
            ip: peer(parts),
            user_agent: parts.headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string),
            same_origin: same_origin(&parts.headers),
        })
    }
}

/// The session cookie's header value. SameSite=Lax, not Strict: coming back from the sign-in provider is a navigation from
/// its site, and a Strict cookie set then would not be sent on the very next page. Lax still keeps it off every
/// cross-site request but a top-level GET, and every change needs the CSRF token anyway ("Sign-in" 3, "SSO" 4).
pub fn cookie_value(app: &App, token: &str, max_age: i64) -> String {
    let secure = if app.secure { "; Secure" } else { "" };
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

fn session_cookie(app: &App, token: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&cookie_value(app, token, max_age)).unwrap_or(HeaderValue::from_static(""))
}

pub(crate) async fn configured(app: &App) -> ApiResult<bool> {
    Ok(app.db.call(|c| Ok(c.query_row("SELECT EXISTS (SELECT 1 FROM users)", [], |r| r.get::<_, bool>(0))?)).await?)
}

async fn status(State(app): State<App>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "name": "Freightdock", "version": env!("CARGO_PKG_VERSION"), "configured": configured(&app).await?, "sso": crate::signin::public(&app).await })))
}

#[derive(Deserialize)]
struct Credentials {
    name: String,
    password: String,
}

/// The first run's form: the first administrator's name and password, and the setup token from the log.
#[derive(Deserialize)]
struct SetupBody {
    name: String,
    password: String,
    #[serde(default)]
    token: String,
}

/// The first run: the first administrator, and the local Docker as the first environment. Refused once anybody exists,
/// so it can never be used to take over a running server.
async fn setup(State(app): State<App>, peer: Peer, Json(c): Json<SetupBody>) -> ApiResult<Response> {
    if !peer.same_origin {
        return Err(ApiError::forbidden());
    }
    // The same `Origin` cannot tell a page that rebinds a name of its own to this server from the person setting it
    // up; reading the log can ("Sign-in" 14). Compared by their hashes, so in a time that says nothing of either's
    // length or where they differ. No token at all (a test app never given one) refuses too.
    if !app.setup_token.get().is_some_and(|t| auth::same(&auth::token_hash(&c.token), &auth::token_hash(t))) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "That is not the setup token. `freightdock setup-token` shows it (docker exec freightdock freightdock setup-token), and Freightdock printed it to its log as it started.").field("token"));
    }
    let c = Credentials { name: c.name, password: c.password };
    auth::check_name(&c.name).map_err(|e| ApiError::bad(e.to_string()).field("name"))?;
    auth::check_password(&c.password).map_err(|e| ApiError::bad(e.to_string()).field("password"))?;
    let hash = auth::hash_password(&c.password)?;
    let name = c.name.clone();
    let id = app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            if tx.query_row("SELECT EXISTS (SELECT 1 FROM users)", [], |r| r.get::<_, bool>(0))? {
                return Ok(None);
            }
            let t = now();
            tx.execute("INSERT INTO users (name, password_hash, role, created_at) VALUES (?1, ?2, 'admin', ?3)", params![name, hash, t])?;
            let id = tx.last_insert_rowid();
            tx.commit()?;
            Ok(Some(id))
        })
        .await?
        .ok_or_else(|| ApiError::conflict("Freightdock is already set up."))?;
    if let Some(dir) = app.data_dir.get() {
        auth::forget_setup_token(dir);
    }
    let token = auth::start(&app.db, id, peer.ip.clone(), peer.user_agent).await?;
    let user = auth::User { id, name: c.name, role: "admin".into(), token: None };
    audit::record(&app.db, Some(&user), peer.ip, entry("setup", user.name.clone())).await;
    Ok(([(header::SET_COOKIE, session_cookie(&app, &token, auth::LIFETIME_S))], Json(json!({ "user": user }))).into_response())
}

/// A password sign-in: a local account first, then each LDAP directory that is on ("LDAP" 1). With a second factor
/// (or one required), the answer is a ticket for the code step rather than a session ("Two-factor" 2).
async fn login(State(app): State<App>, peer: Peer, Json(c): Json<Credentials>) -> ApiResult<Response> {
    if !peer.same_origin {
        return Err(ApiError::forbidden());
    }
    // No account has a longer name, so there is nothing to check, count or log: the limiter and the auth log never
    // keep a name of any size somebody sends.
    if c.name.chars().count() > auth::MAX_NAME {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "That name and password don't match."));
    }
    let ip = peer.ip.clone().unwrap_or_default();
    // Counted before the password is checked, so tries sent at once are held to the limit too ("Sign-in" 5).
    let at = now();
    app.limiter.attempt(&ip, &c.name, at).map_err(auth::too_many)?;
    if let Some(user) = auth::check(&app.db, c.name.clone(), c.password.clone()).await? {
        app.limiter.release(&ip, &c.name, at);
        return crate::signin::totp::after_password(&app, user, "password", peer).await;
    }
    match crate::signin::ldap::sign_in(&app, &peer, &c.name, &c.password).await {
        Some(Ok(user)) => {
            app.limiter.release(&ip, &c.name, at);
            crate::signin::totp::after_password(&app, user, "ldap", peer).await
        }
        Some(Err(crate::signin::Refused(message, _))) => {
            let e = Event { event: "login.failed", method: "ldap", ok: false, user: None, name: Some(c.name.clone()), ip: peer.ip, user_agent: peer.user_agent, detail: Some(message.clone()) };
            authlog::record(&app.db, e).await;
            Err(ApiError::new(StatusCode::FORBIDDEN, message))
        }
        None => {
            let e = Event { event: "login.failed", method: "password", ok: false, user: None, name: Some(c.name.clone()), ip: peer.ip, user_agent: peer.user_agent, detail: Some("wrong name or password".into()) };
            authlog::record(&app.db, e).await;
            Err(ApiError::new(StatusCode::UNAUTHORIZED, "That name and password don't match."))
        }
    }
}

/// The end of a sign-in with a password (and its second factor, when it has one): the misses forgiven, a session, the
/// auth log, the cookie.
pub(crate) async fn signed_in(app: &App, user: &auth::User, method: &'static str, peer: &Peer, detail: Option<String>) -> ApiResult<Response> {
    app.limiter.forgive(&peer.ip.clone().unwrap_or_default(), &user.name);
    let token = auth::start(&app.db, user.id, peer.ip.clone(), peer.user_agent.clone()).await?;
    authlog::record(&app.db, Event { event: "login", method, ok: true, user: Some(user), name: None, ip: peer.ip.clone(), user_agent: peer.user_agent.clone(), detail }).await;
    Ok(([(header::SET_COOKIE, session_cookie(app, &token, auth::LIFETIME_S))], Json(json!({ "user": user }))).into_response())
}

/// Signs out here; for somebody who came through the provider, the answer also names where to sign out there.
async fn logout(State(app): State<App>, a: Authed, peer: Peer) -> ApiResult<Response> {
    auth::end(&app.db, a.session.token_hash.clone()).await?;
    authlog::record(&app.db, Event { event: "logout", method: "session", ok: true, user: Some(&a.session.user), name: None, ip: a.ip.clone(), user_agent: peer.user_agent, detail: None }).await;
    let clear = [(header::SET_COOKIE, session_cookie(&app, "", 0))];
    Ok(match crate::oidc::end_session(&app, a.session.user.id).await {
        Some(url) => (clear, Json(json!({ "redirect": url }))).into_response(),
        None => (clear, StatusCode::NO_CONTENT).into_response(),
    })
}

async fn me(State(app): State<App>, a: Authed) -> Json<serde_json::Value> {
    let mut user = json!(a.session.user);
    user["avatar"] = json!(crate::avatars::path_for(&app, a.session.user.id).await);
    let signin = crate::avatars::signin_of(&app, a.session.user.id).await;
    // Signed in through a provider that gives its own sign-in and two-factor: no password or two-factor card ("Users" 2).
    user["sso"] = json!(matches!(signin["kind"].as_str(), Some("oidc" | "github")));
    user["signin"] = signin;
    // Until when what wants a recent sign-in goes through without the password again ("Sign-in" 18).
    let until = (!a.session.token_hash.is_empty()).then_some(a.session.confirmed_at + auth::CONFIRM_S);
    // What else they may operate in the cluster, for the pages to offer it ("Access" 24).
    if !a.session.user.is_admin() {
        user["grants"] = crate::grants::mine(&app, a.session.user.id).await.unwrap_or(json!([]));
    }
    Json(json!({ "user": user, "csrf": a.session.csrf, "confirmed_until": until }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    pub fn app() -> App {
        let db = crate::db::Db::open_in_memory().unwrap();
        crate::state::AppState::new(db, false, crate::secret::Sealer::for_test())
    }

    pub async fn call(app: &App, req: Request<Body>) -> (StatusCode, HeaderMap, serde_json::Value) {
        let r = router(app.clone()).oneshot(req).await.unwrap();
        let (status, headers) = (r.status(), r.headers().clone());
        let bytes = http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    pub fn post(path: &str) -> axum::http::request::Builder {
        Request::post(path).header(header::CONTENT_TYPE, "application/json").header(header::HOST, "freightdock.test")
    }

    pub fn json_body(v: serde_json::Value) -> Body {
        Body::from(v.to_string())
    }

    /// The setup token this app was started with ("Sign-in" 14): one is made for a test app when it is first asked for.
    pub fn setup_token(app: &App) -> String {
        app.setup_token.get_or_init(auth::setup_token).clone()
    }

    /// Sets up the first administrator; answers the cookie and the CSRF token.
    pub async fn signed_in(app: &App) -> (String, String) {
        let creds = json!({ "name": "admin", "password": "a long enough password", "token": setup_token(app) });
        let (s, h, _) = call(app, post("/api/setup").body(json_body(creds)).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        let cookie = h[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
        let (_, _, me) = call(app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        (cookie, me["csrf"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn setup_makes_the_first_admin_once_and_never_again() {
        let app = app();
        let (_, _, s) = call(&app, Request::get("/api/status").body(Body::empty()).unwrap()).await;
        assert_eq!(s["configured"], false);
        let (cookie, _) = signed_in(&app).await;
        assert!(cookie.starts_with("freightdock_session="));
        let (_, _, s) = call(&app, Request::get("/api/status").body(Body::empty()).unwrap()).await;
        assert_eq!(s["configured"], true);

        let again = json!({ "name": "intruder", "password": "another long password", "token": setup_token(&app) });
        let (s, _, _) = call(&app, post("/api/setup").body(json_body(again)).unwrap()).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    /// The token kept in the data folder for `freightdock setup-token` opens nothing once setup is done, so it goes.
    #[tokio::test]
    async fn setup_done_removes_the_kept_setup_token() {
        let app = app();
        let dir = tempfile::tempdir().unwrap();
        let _ = app.data_dir.set(dir.path().to_path_buf());
        let kept = auth::kept_setup_token(dir.path()).unwrap();
        let _ = app.setup_token.set(kept);
        signed_in(&app).await;
        assert_eq!(auth::read_setup_token(dir.path()), None);
    }

    /// A page that rebinds a name of its own to an unconfigured Freightdock sends a `Host` and an `Origin` that agree, so
    /// the same-origin check alone cannot tell it from the person setting up; the token printed to the log can.
    #[tokio::test]
    async fn setup_wants_the_token_freightdock_printed_as_it_started() {
        let app = app();
        let token = setup_token(&app);
        let rebound = |body: serde_json::Value| {
            Request::post("/api/setup").header(header::CONTENT_TYPE, "application/json").header(header::HOST, "rebind.attacker.example:7080").header(header::ORIGIN, "http://rebind.attacker.example:7080").body(json_body(body)).unwrap()
        };
        let creds = |t: Option<&str>| {
            let mut c = json!({ "name": "admin", "password": "a long enough password" });
            if let Some(t) = t {
                c["token"] = json!(t);
            }
            c
        };
        let (s, _, b) = call(&app, rebound(creds(None))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::FORBIDDEN, Some("token")), "no token, no administrator: {b}");
        let (s, _, _) = call(&app, rebound(creds(Some("not the token")))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, rebound(creds(Some(&token[..token.len() - 1])))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "nearly is not enough");
        let (_, _, st) = call(&app, Request::get("/api/status").body(Body::empty()).unwrap()).await;
        assert_eq!(st["configured"], false);
        let (s, _, _) = call(&app, post("/api/setup").body(json_body(creds(Some(&token)))).unwrap()).await;
        assert_eq!(s, StatusCode::OK, "the person who can read the log sets it up");
    }

    #[tokio::test]
    async fn a_freightdock_with_no_setup_token_cannot_be_set_up() {
        let app = app();
        let (s, _, _) = call(&app, post("/api/setup").body(json_body(json!({ "name": "admin", "password": "a long enough password", "token": "" }))).unwrap()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "fails closed");
    }

    #[tokio::test]
    async fn setup_holds_the_password_to_its_length() {
        let app = app();
        let short = json!({ "name": "admin", "password": "short", "token": setup_token(&app) });
        let (s, _, b) = call(&app, post("/api/setup").body(json_body(short)).unwrap()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["field"], "password");
    }

    #[tokio::test]
    async fn the_api_wants_a_session() {
        let app = app();
        let (s, _, _) = call(&app, Request::get("/api/me").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _, _) = call(&app, Request::get("/api/me").header(header::COOKIE, "freightdock_session=forged").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_change_without_the_csrf_token_or_from_elsewhere_is_refused() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let req = |token: &str, origin: &str| {
            Request::post("/api/logout").header(header::HOST, "freightdock.test").header(header::ORIGIN, origin).header(header::COOKIE, &cookie).header("x-csrf-token", token).body(Body::empty()).unwrap()
        };
        let (s, _, _) = call(&app, req("wrong", "https://freightdock.test")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, req(&csrf, "https://evil.example")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, req(&csrf, "https://freightdock.test")).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "signed out means signed out");
    }

    /// "Sign-in" 5 holds for tries made at the same moment: each is counted as it starts, not when its answer comes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tries_sent_at_once_are_held_to_the_limit_too() {
        let app = app();
        signed_in(&app).await;
        let bad = json!({ "name": "admin", "password": "not the password" });
        let tries = (0..20).map(|_| {
            let (app, bad) = (app.clone(), bad.clone());
            tokio::spawn(async move { call(&app, post("/api/login").body(json_body(bad)).unwrap()).await.0 })
        });
        let answers: Vec<StatusCode> = futures_util::future::join_all(tries).await.into_iter().map(Result::unwrap).collect();
        let checked = answers.iter().filter(|s| **s == StatusCode::UNAUTHORIZED).count();
        assert_eq!(checked, auth::Limiter::LIMIT, "only five passwords were checked: {answers:?}");
        assert_eq!(answers.iter().filter(|s| **s == StatusCode::TOO_MANY_REQUESTS).count(), 20 - auth::Limiter::LIMIT);
    }

    #[tokio::test]
    async fn a_name_no_account_can_have_is_refused_before_anything_keeps_it() {
        let app = app();
        signed_in(&app).await;
        let long = json!({ "name": "x".repeat(100_000), "password": "not the password" });
        let (s, _, _) = call(&app, post("/api/login").body(json_body(long)).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let kept: i64 = app.db.call(|c| Ok(c.query_row("SELECT COALESCE(MAX(length(user_name)), 0) FROM auth_log", [], |r| r.get(0))?)).await.unwrap();
        assert!(kept <= auth::MAX_NAME as i64, "the auth log keeps no name longer than a name can be: {kept}");
    }

    #[tokio::test]
    async fn sign_in_works_and_too_many_misses_wait() {
        let app = app();
        signed_in(&app).await;
        let good = json!({ "name": "admin", "password": "a long enough password" });
        let (s, h, _) = call(&app, post("/api/login").body(json_body(good.clone())).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h[header::SET_COOKIE].to_str().unwrap().contains("HttpOnly; SameSite=Lax"));
        let bad = json!({ "name": "admin", "password": "not the password" });
        for _ in 0..auth::Limiter::LIMIT {
            let (s, _, _) = call(&app, post("/api/login").body(json_body(bad.clone())).unwrap()).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED);
        }
        let (s, h, _) = call(&app, post("/api/login").body(json_body(good)).unwrap()).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "even the right password waits");
        assert!(h.contains_key(header::RETRY_AFTER));
        let failed: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM auth_log WHERE event = 'login.failed' AND ok = 0 AND user_name = 'admin'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(failed as usize, auth::Limiter::LIMIT);
    }
}
