//! API tokens: for a program, not a browser (an AI agent through `/mcp`, a script, CI). A person makes one in their
//! Account page and sees it once; the database keeps only its SHA-256, as it does a session's ("Sign-in" 2). A token
//! acts as its owner, with the owner's role as it is at each use, or only reads when it was made read-only. It comes
//! as `Authorization: Bearer fdk_...` and needs no CSRF token or `Origin`: a browser never sends it on its own, so
//! there is no forged request to guard against ("Sign-in" 9, 10).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::{Json, Router, routing::delete, routing::get};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Authed;
use crate::audit::{self, Entry};
use crate::auth::{self, User};
use crate::db::{Db, now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Every token starts so, so it can be told from anything else (and found by secret scanners).
pub const PREFIX: &str = "fdk_";
/// The longest a token may be made good for.
pub const MAX_DAYS: i64 = 3650;

/// What a request made with a token carries: which token, and whether it may only read.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenUse {
    pub id: i64,
    pub name: String,
    pub read_only: bool,
}

pub fn router() -> Router<App> {
    Router::new().route("/api/tokens", get(list).post(create)).route("/api/tokens/{id}", delete(revoke))
}

/// A new token: what the person is shown once, and the hash the database keeps.
pub fn make() -> (String, String) {
    let token = format!("{PREFIX}{}", auth::random_token());
    let hash = auth::token_hash(&token);
    (token, hash)
}

/// `Some(token)` when `s` has a token's shape: the prefix, then 64 lowercase hex digits.
pub fn parse(s: &str) -> Option<&str> {
    let secret = s.strip_prefix(PREFIX)?;
    (secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(s)
}

/// The bearer credential of a request, when it has an `Authorization: Bearer ...` header: `Some(Err(()))` when there
/// is one but it is not a token, so a bad one is refused rather than ignored.
pub fn bearer(headers: &HeaderMap) -> Option<Result<&str, ()>> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    Some(parse(rest.trim()).ok_or(()))
}

/// The owner of a token, as they are now, if the token is still good: not revoked, not expired, its owner not
/// disabled. Using it notes when and from where (no more often than once a minute).
pub async fn lookup(db: &Db, token: &str, ip: Option<String>) -> anyhow::Result<Option<User>> {
    let hash = auth::token_hash(token);
    db.call(move |c| {
        let t = now();
        let row = c
            .query_row(
                "SELECT u.id, u.name, u.role, k.id, k.name, k.read_only, k.last_used_at FROM api_tokens k JOIN users u ON u.id = k.user_id
                 WHERE k.token_hash = ?1 AND (k.expires_at IS NULL OR k.expires_at > ?2) AND u.disabled = 0",
                params![hash, t],
                |r| {
                    let token = TokenUse { id: r.get(3)?, name: r.get(4)?, read_only: r.get(5)? };
                    Ok((User { id: r.get(0)?, name: r.get(1)?, role: r.get(2)?, token: Some(token) }, r.get::<_, Option<i64>>(6)?))
                },
            )
            .optional()?;
        let Some((user, used)) = row else { return Ok(None) };
        if used.is_none_or(|u| t - u > 60) {
            let id = user.token.as_ref().map(|k| k.id);
            c.execute("UPDATE api_tokens SET last_used_at = ?1, last_used_ip = ?2 WHERE id = ?3", params![t, ip, id])?;
            // In the auth log as often as its last use is noted: at most once a minute ("Activity" 3).
            let detail = user.token.as_ref().map(|k| format!("token {}{}", k.name, if k.read_only { ", read-only" } else { "" }));
            crate::authlog::write(c, "token.use", "token", true, Some(user.id), Some(&user.name), ip.as_deref(), None, detail.as_deref())?;
        }
        Ok(Some(user))
    })
    .await
}

/// Tokens are made and revoked in the browser, with a session: a token cannot make another, so one that leaks cannot
/// be used to make itself a longer life.
pub(crate) fn session_only(a: &Authed) -> ApiResult<()> {
    if a.session.user.token.is_some() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "API tokens are made and revoked in the Account page, not with a token."));
    }
    Ok(())
}

/// `GET /api/tokens`: the signed-in person's tokens (never the tokens themselves).
async fn list(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let uid = a.session.user.id;
    let rows = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, name, hint, read_only, created_at, expires_at, last_used_at, last_used_ip FROM api_tokens WHERE user_id = ?1 ORDER BY id")?;
            let rows = st.query_map([uid], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "hint": r.get::<_, String>(2)?, "read_only": r.get::<_, bool>(3)?,
                    "created_at": r.get::<_, i64>(4)?, "expires_at": r.get::<_, Option<i64>>(5)?, "last_used_at": r.get::<_, Option<i64>>(6)?,
                    "last_used_ip": r.get::<_, Option<String>>(7)?,
                }))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
struct Create {
    name: String,
    /// Good for this many days; none is until it is revoked.
    expires_days: Option<i64>,
    #[serde(default)]
    read_only: bool,
}

/// `POST /api/tokens`: a new token, answered once.
async fn create(State(app): State<App>, a: Authed, Json(c): Json<Create>) -> ApiResult<(StatusCode, Json<Value>)> {
    session_only(&a)?;
    let name = c.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("A token's name is 1 to 64 characters.").field("name"));
    }
    if let Some(d) = c.expires_days
        && !(1..=MAX_DAYS).contains(&d)
    {
        return Err(ApiError::bad(format!("A token is good for 1 to {MAX_DAYS} days, or until it is revoked.")).field("expires_days"));
    }
    let (token, hash) = make();
    let hint = token[token.len() - 4..].to_string();
    let t = now();
    let expires_at = c.expires_days.map(|d| t + d * 86_400);
    let (uid, n, read_only) = (a.session.user.id, name.clone(), c.read_only);
    let id = app
        .db
        .call(move |conn| {
            let taken: bool = conn.query_row("SELECT EXISTS (SELECT 1 FROM api_tokens WHERE user_id = ?1 AND name = ?2 COLLATE NOCASE)", params![uid, n], |r| r.get(0))?;
            if taken {
                return Ok(None);
            }
            conn.execute(
                "INSERT INTO api_tokens (user_id, name, token_hash, hint, read_only, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![uid, n, hash, hint, read_only, t, expires_at],
            )?;
            Ok(Some(conn.last_insert_rowid()))
        })
        .await?
        .ok_or_else(|| ApiError::conflict("You have a token by that name already.").field("name"))?;
    let detail = [read_only.then_some("read-only".to_string()), c.expires_days.map(|d| format!("{d} days"))].into_iter().flatten().collect::<Vec<_>>().join(", ");
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action: "token.create", target: Some(name.clone()), detail: (!detail.is_empty()).then_some(detail) }).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name, "token": token, "read_only": read_only, "expires_at": expires_at }))))
}

/// `DELETE /api/tokens/{id}`: the token stops working at once.
async fn revoke(State(app): State<App>, a: Authed, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    session_only(&a)?;
    let uid = a.session.user.id;
    let name = app
        .db
        .call(move |c| {
            let name: Option<String> = c.query_row("SELECT name FROM api_tokens WHERE id = ?1 AND user_id = ?2", params![id, uid], |r| r.get(0)).optional()?;
            c.execute("DELETE FROM api_tokens WHERE id = ?1 AND user_id = ?2", params![id, uid])?;
            Ok(name)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("token"))?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("token.revoke", name)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::tests::{app, call, json_body, post, signed_in};
    use axum::body::Body;
    use axum::http::Request;

    #[test]
    fn a_token_has_its_prefix_and_the_database_keeps_only_its_hash() {
        let (t, h) = make();
        assert!(t.starts_with("fdk_") && t.len() == 4 + 64, "{t}");
        assert_eq!(parse(&t), Some(t.as_str()));
        assert_eq!(h, auth::token_hash(&t));
        assert!(!h.contains(&t[4..]));
        assert_ne!(make().0, t, "every token is new");
    }

    #[test]
    fn only_a_token_s_shape_parses() {
        let good = format!("fdk_{}", "a".repeat(64));
        assert!(parse(&good).is_some());
        assert!(parse(&"a".repeat(64)).is_none(), "no prefix");
        assert!(parse(&format!("fdk_{}", "a".repeat(63))).is_none(), "too short");
        assert!(parse(&format!("fdk_{}", "A".repeat(64))).is_none(), "upper case is not what make() gives");
        assert!(parse(&format!("fdk_{}g", "a".repeat(63))).is_none(), "not hex");
    }

    #[test]
    fn a_bearer_header_is_read_and_a_bad_one_is_not_ignored() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        let good = format!("fdk_{}", "b".repeat(64));
        h.insert(header::AUTHORIZATION, format!("Bearer {good}").parse().unwrap());
        assert_eq!(bearer(&h), Some(Ok(good.as_str())));
        h.insert(header::AUTHORIZATION, format!("bearer {good}").parse().unwrap());
        assert_eq!(bearer(&h), Some(Ok(good.as_str())), "the scheme is case-blind");
        h.insert(header::AUTHORIZATION, "Bearer nonsense".parse().unwrap());
        assert_eq!(bearer(&h), Some(Err(())));
        h.insert(header::AUTHORIZATION, "Basic YWJjOmRlZg==".parse().unwrap());
        assert_eq!(bearer(&h), None, "another scheme is not ours");
    }

    /// Makes a token as the signed-in administrator; answers it.
    pub async fn token(app: &App, cookie: &str, csrf: &str, body: Value) -> String {
        let req = post("/api/tokens").header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(app, req).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        b["token"].as_str().unwrap().to_string()
    }

    fn with(token: &str, req: axum::http::request::Builder) -> Request<Body> {
        req.header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_token_signs_its_owner_in_on_any_route_without_a_cookie() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "claude" })).await;
        let (s, _, me) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(me["user"]["name"], "admin");
        assert_eq!(me["user"]["role"], "admin");
        let (s, _, _) = call(&app, with(&t, Request::get("/api/audit"))).await;
        assert_eq!(s, StatusCode::OK, "an administrator's token reads what an administrator reads");
        let (s, _, _) = call(&app, with(&format!("fdk_{}", "0".repeat(64)), Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a token nobody made");
        let bad = Request::get("/api/me").header(header::COOKIE, &cookie).header(header::AUTHORIZATION, "Bearer forged").body(Body::empty()).unwrap();
        let (s, _, _) = call(&app, bad).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a bad bearer is refused, not passed over for the cookie");

        let (_, _, list) = call(&app, Request::get("/api/tokens").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(list[0]["name"], "claude");
        assert!(list[0]["last_used_at"].is_i64(), "a use is noted");
        assert!(list[0].get("token").is_none() && !list.to_string().contains(&t[4..]), "never sent back");
    }

    #[tokio::test]
    async fn a_change_with_a_token_needs_no_csrf_token_and_says_so_in_the_activity_log() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "ci" })).await;
        let req = Request::put("/api/settings/appearance")
            .header(header::HOST, "freightdock.test")
            .header(header::ORIGIN, "https://elsewhere.example")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {t}"))
            .body(json_body(json!({ "preset": "" })))
            .unwrap();
        let (s, _, b) = call(&app, req).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let (_, _, log) = call(&app, Request::get("/api/audit?action=settings").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(log["entries"][0]["user_name"], "admin");
        assert_eq!(log["entries"][0]["via"], "token ci");
    }

    #[tokio::test]
    async fn a_read_only_token_reads_and_changes_nothing() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "look", "read_only": true })).await;
        let (s, _, _) = call(&app, with(&t, Request::get("/api/users"))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = call(&app, with(&t, Request::post("/api/cluster/join-tokens"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "an administrator's read-only token may not change the cluster");
        let (s, _, _) = call(&app, with(&t, Request::delete("/api/users/1"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_user_s_token_is_refused_what_the_user_is_refused() {
        let app = app();
        signed_in(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0);")?)).await.unwrap();
        let s = auth::start(&app.db, 9, None, None).await.unwrap();
        let cookie = format!("freightdock_session={s}");
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let t = token(&app, &cookie, me["csrf"].as_str().unwrap(), json!({ "name": "mine" })).await;
        let (s, _, _) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _, _) = call(&app, with(&t, Request::get("/api/audit"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, with(&t, Request::post("/api/cluster/join-tokens"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_revoked_or_expired_token_or_one_whose_owner_is_disabled_signs_nobody_in() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "short", "expires_days": 1 })).await;
        let (s, _, _) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::OK);
        app.db.call(|c| Ok(c.execute("UPDATE api_tokens SET expires_at = ?1", [now() - 1])?)).await.unwrap();
        let (s, _, _) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "expired");

        let t = token(&app, &cookie, &csrf, json!({ "name": "gone" })).await;
        let (_, _, list) = call(&app, Request::get("/api/tokens").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let id = list.as_array().unwrap().iter().find(|k| k["name"] == "gone").unwrap()["id"].as_i64().unwrap();
        let del = |c: &str| Request::delete(format!("/api/tokens/{id}")).header(header::HOST, "freightdock.test").header(header::COOKIE, c).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        let (s, _, _) = call(&app, with(&t, Request::delete(format!("/api/tokens/{id}")))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "a token does not revoke tokens");
        let (s, _, _) = call(&app, del(&cookie)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "revoked");

        let t = token(&app, &cookie, &csrf, json!({ "name": "owner off" })).await;
        app.db.call(|c| Ok(c.execute("UPDATE users SET disabled = 1", [])?)).await.unwrap();
        let (s, _, _) = call(&app, with(&t, Request::get("/api/me"))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "disabled");
    }

    #[tokio::test]
    async fn a_token_cannot_make_a_token_and_names_and_lifetimes_are_checked() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "first" })).await;
        let req = Request::post("/api/tokens").header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").header(header::AUTHORIZATION, format!("Bearer {t}")).body(json_body(json!({ "name": "second" }))).unwrap();
        let (s, _, _) = call(&app, req).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let make = |b: Value| post("/api/tokens").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(b)).unwrap();
        let (s, _, b) = call(&app, make(json!({ "name": "FIRST" }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")));
        let (s, _, b) = call(&app, make(json!({ "name": "  " }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("name")));
        let (s, _, b) = call(&app, make(json!({ "name": "x", "expires_days": 0 }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("expires_days")));
        let (_, _, log) = call(&app, Request::get("/api/audit?action=token").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(log["entries"][0]["action"], "token.create");
        assert_eq!(log["entries"][0]["target"], "first");
    }
}
