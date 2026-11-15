//! Giving the password again ("Sign-in" 18): what can take a node over (a shell, an upgrade of the cluster, a binary
//! for it) wants a session that signed in, or gave its password (and code) again, within `auth::CONFIRM_S`. A stolen
//! cookie alone then reads and changes what a person can, but never runs a command as root.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::api::{Authed, Peer};
use crate::auth;
use crate::authlog::{self, Event};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Refuses, with the `confirm` code the page answers with a password dialog, unless the session is recent enough.
/// An API token is a program's, made by its person on purpose and never in a browser: it is not asked.
pub fn recent(a: &Authed) -> ApiResult<()> {
    if a.session.token_hash.is_empty() || now() - a.session.confirmed_at < auth::CONFIRM_S {
        return Ok(());
    }
    Err(ApiError { code: Some("confirm"), ..ApiError::new(StatusCode::FORBIDDEN, "Give your password again first: this needs a recent sign-in.") })
}

#[derive(Deserialize)]
pub struct Confirm {
    #[serde(default)]
    password: String,
    #[serde(default)]
    code: String,
}

/// `POST /api/me/confirm`: the signed-in person's password (and their code, with two-factor on) again, which makes
/// the session recent. Somebody who signed in through a provider signs in there again instead.
pub async fn confirm(State(app): State<App>, a: Authed, peer: Peer, Json(b): Json<Confirm>) -> ApiResult<StatusCode> {
    let user = &a.session.user;
    if a.session.token_hash.is_empty() {
        return Err(ApiError::bad("An API token is never asked for a password."));
    }
    let signin = crate::avatars::signin_of(&app, user.id).await;
    if matches!(signin["kind"].as_str(), Some("oidc" | "github")) {
        return Err(ApiError { code: Some("sso"), ..ApiError::conflict("You signed in through a provider: sign out and in again there.") });
    }
    let ip = peer.ip.clone().unwrap_or_default();
    let at = now();
    app.limiter.attempt(&ip, &user.name, at).map_err(auth::too_many)?;
    let local = auth::check(&app.db, user.name.clone(), b.password.clone()).await?.is_some_and(|u| u.id == user.id);
    let ok = local || matches!(crate::signin::ldap::sign_in(&app, &peer, &user.name, &b.password).await, Some(Ok(u)) if u.id == user.id);
    let refused = |detail: &str, e: ApiError| {
        let ev = Event { event: "confirm.failed", method: "password", ok: false, user: Some(user), name: None, ip: peer.ip.clone(), user_agent: peer.user_agent.clone(), detail: Some(detail.into()) };
        (ev, e)
    };
    if !ok {
        let (ev, e) = refused("wrong password", ApiError::bad("That is not your password.").field("password"));
        authlog::record(&app.db, ev).await;
        return Err(e);
    }
    if crate::signin::totp::enabled(&app, user.id).await? {
        if b.code.trim().is_empty() {
            // The password was right: asking for the code is not another miss.
            app.limiter.release(&ip, &user.name, at);
            return Err(ApiError { code: Some("totp"), ..ApiError::bad("Enter the code from your authenticator app too.").field("code") });
        }
        if crate::signin::totp::verify(&app, user.id, &b.code).await?.is_none() {
            let (ev, e) = refused("wrong code", ApiError::bad("That code is not right, or was used already.").field("code"));
            authlog::record(&app.db, ev).await;
            return Err(e);
        }
    }
    app.limiter.release(&ip, &user.name, at);
    auth::confirmed(&app.db, a.session.token_hash.clone()).await?;
    authlog::record(&app.db, Event { event: "confirm", method: "password", ok: true, user: Some(user), name: None, ip: peer.ip, user_agent: peer.user_agent, detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use serde_json::{Value, json};

    use crate::api::tests::{app, call, json_body, post, signed_in};
    use crate::state::App;

    fn send(path: &str, cookie: &str, csrf: &str, body: Value) -> Request<Body> {
        post(path).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap()
    }

    async fn age(app: &App, by: i64) {
        app.db.call(move |c| Ok(c.execute("UPDATE sessions SET confirmed_at = confirmed_at - ?1", [by])?)).await.unwrap();
    }

    /// What starting an upgrade answers: with no cluster here, anything but `confirm` means it got past the check.
    async fn upgrade(app: &App, cookie: &str, csrf: &str) -> (StatusCode, Value) {
        let (s, _, b) = call(app, send("/api/cluster/upgrade", cookie, csrf, json!({}))).await;
        (s, b)
    }

    #[tokio::test]
    async fn a_fresh_sign_in_is_recent_and_ten_minutes_later_it_is_not() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = upgrade(&app, &cookie, &csrf).await;
        assert_ne!(b["code"], "confirm", "{b}");
        age(&app, crate::auth::CONFIRM_S + 1).await;
        let (s, b) = upgrade(&app, &cookie, &csrf).await;
        assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("confirm")), "{b}");
    }

    #[tokio::test]
    async fn the_password_again_makes_it_recent_and_a_wrong_one_does_not() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        age(&app, crate::auth::CONFIRM_S + 1).await;
        let (s, _, b) = call(&app, send("/api/me/confirm", &cookie, &csrf, json!({ "password": "not the password" }))).await;
        // Not 401: that would send the page to the sign-in form, and the session is still good.
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("password")));
        assert_eq!(upgrade(&app, &cookie, &csrf).await.1["code"], "confirm");
        let (s, _, b) = call(&app, send("/api/me/confirm", &cookie, &csrf, json!({ "password": "a long enough password" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert_ne!(upgrade(&app, &cookie, &csrf).await.1["code"], "confirm");
        let (_, _, log) = call(&app, Request::get("/api/auth-log").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let events: Vec<&str> = log["entries"].as_array().into_iter().flatten().filter_map(|e| e["event"].as_str()).collect();
        assert!(events.contains(&"confirm") && events.contains(&"confirm.failed"), "{log}");
    }

    #[tokio::test]
    async fn a_shell_and_a_release_want_it_recent_too() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        age(&app, crate::auth::CONFIRM_S + 1).await;
        let (_, _, b) = call(&app, Request::get("/api/cluster/nodes/abcd1234/shell").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(b["code"], "confirm", "{b}");
        let up = post("/api/cluster/release/worker?version=1.0.1").method("PUT").header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::from("binary")).unwrap();
        let (_, _, b) = call(&app, up).await;
        assert_eq!(b["code"], "confirm", "{b}");
        let (_, _, b) = call(&app, send("/api/cluster/release/fetch", &cookie, &csrf, json!({ "tag": "v1.0.1" }))).await;
        assert_eq!(b["code"], "confirm", "{b}");
    }

    #[tokio::test]
    async fn the_password_again_needs_the_csrf_token() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let (s, _, _) = call(&app, send("/api/me/confirm", &cookie, "wrong", json!({ "password": "a long enough password" }))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn with_two_factor_on_the_code_is_wanted_too() {
        use crate::signin::totp::{STEP, code_at};
        let app = app();
        let (cookie, csrf, key, _) = crate::signin::totp::tests::enrolled(&app).await;
        age(&app, crate::auth::CONFIRM_S + 1).await;
        let (s, _, b) = call(&app, send("/api/me/confirm", &cookie, &csrf, json!({ "password": "a long enough password" }))).await;
        assert_eq!((s, b["code"].as_str(), b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("totp"), Some("code")), "{b}");
        let (s, _, b) = call(&app, send("/api/me/confirm", &cookie, &csrf, json!({ "password": "a long enough password", "code": "000000" }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("code")));
        assert_eq!(upgrade(&app, &cookie, &csrf).await.1["code"], "confirm");
        // The enrolment took this step's code; the next step's is in the window too.
        let code = code_at(&key, crate::db::now() / STEP + 1);
        let (s, _, b) = call(&app, send("/api/me/confirm", &cookie, &csrf, json!({ "password": "a long enough password", "code": code }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert_ne!(upgrade(&app, &cookie, &csrf).await.1["code"], "confirm");
    }

    #[tokio::test]
    async fn me_says_until_when_the_session_is_recent() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let until = me["confirmed_until"].as_i64().unwrap();
        assert!((until - crate::db::now() - crate::auth::CONFIRM_S).abs() <= 2, "{me}");
    }
}
