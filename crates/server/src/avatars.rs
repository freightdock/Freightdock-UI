//! A person's picture, as their sign-in provider gives it ("Users" 1): the provider's `picture` claim is kept on the
//! account at each sign-in, and Freightdock fetches the image itself and serves it from its own address, cached, so the
//! browser never calls the picture's host (Gravatar and the like learn nothing about who looks at Freightdock, and no
//! third-party address is needed in the page). Only raster images come back: an SVG can carry script.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::Authed;
use crate::db::rusqlite::{OptionalExtension, params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// A picture is fetched again after this; until then the copy kept is served.
const KEEP_FOR: Duration = Duration::from_secs(24 * 3600);
/// Larger than any avatar needs to be.
const MAX_BYTES: usize = 1024 * 1024;
/// A user's picture address, and the copy kept of it: its type, its bytes and when it was fetched.
type Kept = (Option<String>, Option<String>, Option<Vec<u8>>, Option<i64>);

const TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The picture address a provider's claims give, when it is one Freightdock will fetch: https only (plain http as well
/// only in tests, to reach a stand-in on loopback), at most 2 KB.
pub fn picture_of(claims: &serde_json::Map<String, Value>) -> Option<String> {
    let url = claims.get("picture")?.as_str()?.trim();
    (url.len() <= 2048 && (url.starts_with("https://") || (cfg!(test) && url.starts_with("http://")))).then(|| url.to_string())
}

/// Keeps the picture a provider gave for user `id` at this sign-in; one it no longer gives is forgotten.
pub async fn remember(app: &App, id: i64, picture: Option<String>) -> anyhow::Result<()> {
    app.db
        .call(move |c| {
            let had: Option<String> = c.query_row("SELECT picture FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?.flatten();
            if had != picture {
                c.execute("UPDATE users SET picture = ?1 WHERE id = ?2", params![picture, id])?;
                c.execute("DELETE FROM avatars WHERE user_id = ?1", [id])?;
            }
            Ok(())
        })
        .await
}

/// How user `id` signs in ("Users" 2, 3): `local` (a password kept here), `oidc` or `github` (a provider, which gives the
/// sign-in and its two-factor), or `ldap` (a password checked by a directory), with the provider's name while it is
/// still set up.
pub async fn signin_of(app: &App, id: i64) -> Value {
    let issuer: Option<String> = app
        .db
        .call(move |c| Ok(c.query_row("SELECT oidc_issuer FROM users WHERE id = ?1 AND oidc_sub IS NOT NULL", [id], |r| r.get(0)).optional()?.flatten()))
        .await
        .ok()
        .flatten();
    let Some(issuer) = issuer else { return serde_json::json!({ "kind": "local", "provider": null }) };
    let providers = crate::signin::providers::load(app).await.unwrap_or_default();
    let found = providers.iter().find(|p| p.identity_issuer() == issuer);
    let kind = match found {
        Some(p) => serde_json::to_value(p.kind).unwrap_or(Value::Null),
        None if issuer.starts_with("ldap:") => Value::from("ldap"),
        None => Value::from("oidc"),
    };
    serde_json::json!({ "kind": kind, "provider": found.map(|p| p.label.clone()) })
}

/// The address the UI shows user `id`'s picture at, when they have one: Freightdock's own, with a version so a new
/// picture is not hidden by the browser's cache.
pub async fn path_for(app: &App, id: i64) -> Option<String> {
    let pic: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT picture FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?.flatten())).await.ok()?;
    let pic = pic?;
    use sha2::Digest;
    let v = &hex::encode(sha2::Sha256::digest(pic.as_bytes()))[..12];
    Some(format!("/api/users/{id}/avatar?v={v}"))
}

/// The image at `url`, when it is a raster image no larger than `MAX_BYTES`.
async fn fetch(url: &str) -> anyhow::Result<(String, Vec<u8>)> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).redirect(crate::outbound::outward()).build()?;
    let r = client.get(url).header(header::ACCEPT, "image/png, image/jpeg, image/gif, image/webp").send().await?.error_for_status()?;
    let kind = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    anyhow::ensure!(TYPES.contains(&kind.as_str()), "the picture is {kind:?}, not an image Freightdock shows");
    let body = crate::outbound::capped(r, MAX_BYTES).await.map_err(|e| anyhow::anyhow!("the picture: {e}"))?;
    Ok((kind, body))
}

/// `GET /api/users/{id}/avatar`: one's own picture, or anybody's for an administrator (the Users page shows them).
/// 404 when there is none or it cannot be had; a copy kept is served while it is fresh, and while the host is down.
pub async fn get(State(app): State<App>, a: Authed, Path(id): Path<i64>) -> ApiResult<Response> {
    let me = &a.session.user;
    if me.id != id && !me.is_admin() {
        return Err(ApiError::not_found("picture"));
    }
    let row: Option<Kept> = app
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT u.picture, a.content_type, a.data, a.fetched_at FROM users u LEFT JOIN avatars a ON a.user_id = u.id WHERE u.id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?)
        })
        .await?;
    let Some((Some(url), kind, data, at)) = row else { return Err(ApiError::not_found("picture")) };
    let fresh = at.is_some_and(|t| crate::db::now() - t < KEEP_FOR.as_secs() as i64);
    let (kind, data) = match (fresh, kind, data) {
        (true, Some(k), Some(d)) => (k, d),
        (_, kept_kind, kept_data) => match fetch(&url).await {
            Ok((k, d)) => {
                let (k2, d2) = (k.clone(), d.clone());
                app.db
                    .call(move |c| Ok(c.execute("INSERT INTO avatars (user_id, content_type, data, fetched_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(user_id) DO UPDATE SET content_type = excluded.content_type, data = excluded.data, fetched_at = excluded.fetched_at", params![id, k2, d2, crate::db::now()])?))
                    .await?;
                (k, d)
            }
            Err(e) => {
                tracing::info!("user {id}'s picture could not be fetched: {e:#}");
                match (kept_kind, kept_data) {
                    (Some(k), Some(d)) => (k, d),
                    _ => return Err(ApiError::not_found("picture")),
                }
            }
        },
    };
    Ok(([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "private, max-age=3600".into()), (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into())], data).into_response())
}

/// A test's stand-in for a picture host: `/png` answers a PNG (and counts the calls), `/svg` an SVG.
#[cfg(test)]
pub(crate) async fn picture_host() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let router = axum::Router::new()
        .route("/png", axum::routing::get(move || {
            let h = h.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                ([(header::CONTENT_TYPE, "image/png")], b"\x89PNG\r\n\x1a\nfake".to_vec())
            }
        }))
        .route("/svg", axum::routing::get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], "<svg onload=alert(1)/>") }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    (base, hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use axum::http::StatusCode;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_picture_is_taken_only_from_a_web_address() {
        let m = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(picture_of(&m(json!({ "picture": "https://www.gravatar.com/avatar/abc?size=158" }))).as_deref(), Some("https://www.gravatar.com/avatar/abc?size=158"));
        assert_eq!(picture_of(&m(json!({ "picture": "file:///etc/passwd" }))), None);
        assert_eq!(picture_of(&m(json!({ "picture": "javascript:alert(1)" }))), None);
        assert_eq!(picture_of(&m(json!({ "picture": 3 }))), None);
        assert_eq!(picture_of(&m(json!({}))), None);
    }

    #[tokio::test]
    async fn a_picture_that_never_ends_is_refused_once_it_passes_the_limit() {
        let router = axum::Router::new().route("/png", axum::routing::get(|| async {
            let endless = futures_util::stream::repeat_with(|| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![0u8; 64 * 1024])));
            ([(header::CONTENT_TYPE, "image/png")], axum::body::Body::from_stream(endless))
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/png", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
        let e = fetch(&url).await.unwrap_err().to_string();
        assert!(e.contains("too big"), "refused at the limit, not when the time ran out: {e}");
    }

    #[tokio::test]
    async fn a_picture_s_redirect_is_followed_only_over_https() {
        let (base, hits) = picture_host().await;
        let to = format!("{base}/png");
        let router = axum::Router::new().route("/moved", axum::routing::get(move || {
            let to = to.clone();
            async move { (axum::http::StatusCode::FOUND, [(header::LOCATION, to)]) }
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/moved", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
        assert!(fetch(&url).await.is_err());
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the plain-http address it pointed at was never asked");
    }

    #[tokio::test]
    async fn me_says_whether_the_account_is_linked_to_a_sign_in_provider() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let me = || async { call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.2 };
        assert_eq!(me().await["user"]["sso"], json!(false), "a local account");
        app.db.call(|c| Ok(c.execute("UPDATE users SET oidc_issuer = 'https://idp/', oidc_sub = 's-1' WHERE id = 1", [])?)).await.unwrap();
        assert_eq!(me().await["user"]["sso"], json!(true), "linked: the provider gives its sign-in and two-factor, so Account shows neither");
        app.db.call(|c| Ok(c.execute("UPDATE users SET oidc_issuer = 'ldap:corp' WHERE id = 1", [])?)).await.unwrap();
        assert_eq!(me().await["user"]["sso"], json!(false), "a directory account signs in with a password, and keeps two-factor");
    }

    #[tokio::test]
    async fn me_says_how_the_account_signs_in_and_through_which_provider() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let me = || async { call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.2["user"]["signin"].clone() };
        assert_eq!(me().await, json!({ "kind": "local", "provider": null }));
        let sso = json!({ "providers": [{ "id": "oidc", "kind": "oidc", "label": "Authentik", "issuer": "https://idp/", "enabled": true }] }).to_string();
        app.db.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('sso', ?1)", [sso])?)).await.unwrap();
        app.db.call(|c| Ok(c.execute("UPDATE users SET oidc_issuer = 'https://idp/', oidc_sub = 's-1' WHERE id = 1", [])?)).await.unwrap();
        assert_eq!(me().await, json!({ "kind": "oidc", "provider": "Authentik" }), "the provider's own name");
        app.db.call(|c| Ok(c.execute("UPDATE users SET oidc_issuer = 'ldap:corp' WHERE id = 1", [])?)).await.unwrap();
        assert_eq!(me().await, json!({ "kind": "ldap", "provider": null }), "a directory account, its directory since removed");
    }

    #[tokio::test]
    async fn ones_own_picture_is_served_from_freightdock_kept_and_only_ever_an_image() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let get = |path: String| Request::get(path).header(header::COOKIE, &cookie).body(Body::empty()).unwrap();
        let (_, _, me) = call(&app, get("/api/me".into())).await;
        assert_eq!(me["user"]["avatar"], Value::Null, "no picture, no address");
        let (s, _, _) = call(&app, get("/api/users/1/avatar".into())).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let (host, hits) = picture_host().await;
        remember(&app, 1, Some(format!("{host}/png"))).await.unwrap();
        let (_, _, me) = call(&app, get("/api/me".into())).await;
        let path = me["user"]["avatar"].as_str().expect("an address once there is a picture").to_string();
        assert!(path.starts_with("/api/users/1/avatar?v="), "{path}");
        let (s, h, _) = call(&app, get(path.clone())).await;
        assert_eq!((s, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/png"));
        call(&app, get(path)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1, "the second look is served from the copy kept");

        remember(&app, 1, Some(format!("{host}/svg"))).await.unwrap();
        let (s, _, _) = call(&app, get("/api/users/1/avatar".into())).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "an SVG is never passed on");
        remember(&app, 1, None).await.unwrap();
        let (_, _, me) = call(&app, get("/api/me".into())).await;
        assert_eq!(me["user"]["avatar"], Value::Null, "a picture the provider no longer gives is forgotten");
    }
}
