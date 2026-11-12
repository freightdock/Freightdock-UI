//! Webhooks: an address a CI job calls to redeploy a stack (fetched again first, when it is from Git, and its images
//! pulled) or a container (its image pulled, the container recreated). They are the one place a change is made without
//! a session ("Sign-in" 8): the token in the address is the whole credential, so it is 32 random bytes, kept only as its
//! SHA-256 and shown once, every call is in the activity log, and calls are rate limited, by address for tokens that are
//! wrong and by webhook for ones that are right.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Allowed, Peer};
use crate::audit::{self, Entry, Who};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Wrong tokens from one address within the window before it waits the window out.
pub const MISSES: usize = 20;
pub const WINDOW_S: i64 = 15 * 60;
/// One call a webhook within this many seconds of the last one ending, and none while one runs: a CI that fires twice
/// deploys once.
pub const GAP_S: i64 = 10;

/// What a token looks like (`auth::random_token`: 64 hex digits). Anything else is turned away before the database.
pub fn plausible(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The rate limits, in memory: wrong tokens by address, and each webhook's last call.
#[derive(Default)]
pub struct Gate {
    misses: Mutex<HashMap<String, Vec<i64>>>,
    /// Each webhook's last call, by its token's hash: when it ended, or `None` while it runs. Not by its row: a webhook
    /// taken away leaves its id to the next one made, which must not wait out the old one's gap ("Sign-in" 15).
    last: Mutex<HashMap<String, Option<i64>>>,
}

impl Gate {
    /// Seconds before `ip` may call again after its wrong tokens; 0 when it may now.
    pub fn wait(&self, ip: &str, at: i64) -> i64 {
        let mut m = self.misses.lock().unwrap_or_else(|e| e.into_inner());
        let Some(v) = m.get_mut(ip) else { return 0 };
        v.retain(|t| at - t < WINDOW_S);
        if v.len() >= MISSES { v[0] + WINDOW_S - at } else { 0 }
    }

    pub fn miss(&self, ip: &str, at: i64) {
        let mut m = self.misses.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, v| v.last().is_some_and(|t| at - t < WINDOW_S));
        }
        m.entry(ip.to_string()).or_default().push(at);
    }

    /// Takes the turn of the webhook whose token hashes to `key` at `at`, or says how long until it may have one (the
    /// whole gap while one runs).
    pub fn take(&self, key: &str, at: i64) -> Result<(), i64> {
        let mut l = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if l.len() > 10_000 {
            l.retain(|_, t| t.is_none_or(|t| at - t < GAP_S));
        }
        match l.get(key) {
            Some(None) => Err(GAP_S),
            Some(Some(t)) if at - t < GAP_S => Err(t + GAP_S - at),
            _ => {
                l.insert(key.to_string(), None);
                Ok(())
            }
        }
    }

    /// The call of the webhook whose token hashes to `key` ended at `at`; the gap counts from here.
    pub fn done(&self, key: &str, at: i64) {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), Some(at));
    }
}

/// A webhook as the page sees it: never its token.
fn row(r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "target": r.get::<_, String>(2)?,
        "created_by": r.get::<_, Option<String>>(3)?, "created_at": r.get::<_, i64>(4)?, "last_used_at": r.get::<_, Option<i64>>(5)?,
    }))
}

/// `GET /api/envs/{env}/webhooks`: every webhook of the environment.
/// A Standard or Read-only user sees only those of the stacks and containers they can see.
pub async fn list(State(app): State<App>, access: crate::access::Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let owners = crate::access::ownership::Owners::load(&app.db, env).await?;
    let rows = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, kind, target, created_by, created_at, last_used_at FROM webhooks WHERE env_id = ?1 ORDER BY kind, target")?;
            let rows = st.query_map([env], row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let rows: Vec<Value> = rows
        .into_iter()
        .filter(|w| {
            let target = w["target"].as_str().unwrap_or_default();
            access.sees(match w["kind"].as_str() {
                Some("stack") => target.parse().ok().and_then(|id| owners.stack(id)),
                _ => owners.container(target, None),
            })
        })
        .collect();
    Ok(Json(json!(rows)))
}

/// A new token for `kind`/`target` in `env`, replacing the one it had: answers the address, which is shown this once.
async fn make(app: &App, admin: &Allowed, env: i64, kind: &'static str, target: String, shown: String) -> ApiResult<(StatusCode, Json<Value>)> {
    let token = crate::auth::random_token();
    let hash = crate::auth::token_hash(&token);
    let by = admin.0.session.user.name.clone();
    let t = target.clone();
    app.db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM webhooks WHERE env_id = ?1 AND kind = ?2 AND target = ?3", params![env, kind, t])?;
            tx.execute("INSERT INTO webhooks (token_hash, env_id, kind, target, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![hash, env, kind, t, by, now()])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    audit::record_by(&app.db, &Who::from(admin), Entry { action: "webhook.create", target: Some(format!("{kind} {shown}")), detail: None }).await;
    Ok((StatusCode::CREATED, Json(json!({ "url": format!("/api/webhooks/{token}") }))))
}

async fn forget(app: &App, admin: &Allowed, env: i64, kind: &'static str, target: String, shown: String) -> ApiResult<StatusCode> {
    let n = app.db.call(move |c| Ok(c.execute("DELETE FROM webhooks WHERE env_id = ?1 AND kind = ?2 AND target = ?3", params![env, kind, target])?)).await?;
    if n == 0 {
        return Err(ApiError::not_found("webhook"));
    }
    audit::record_by(&app.db, &Who::from(admin), Entry { action: "webhook.remove", target: Some(format!("{kind} {shown}")), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes every webhook of stack `id`, when the stack goes.
pub async fn forget_stack(app: &App, id: i64) {
    let r = app.db.call(move |c| Ok(c.execute("DELETE FROM webhooks WHERE kind = 'stack' AND target = ?1", [id.to_string()])?)).await;
    if let Err(e) = r {
        tracing::error!("removing stack {id}'s webhook: {e:#}");
    }
}

/// `POST /api/envs/{env}/stacks/{id}/webhook`: a new webhook address for the stack (the old one stops working).
pub async fn make_stack(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, i64)>) -> ApiResult<(StatusCode, Json<Value>)> {
    let s = crate::stacks::load(&app, env, id).await?;
    make(&app, &admin, env, "stack", id.to_string(), s.name).await
}

pub async fn remove_stack(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, i64)>) -> ApiResult<StatusCode> {
    let s = crate::stacks::load(&app, env, id).await?;
    forget(&app, &admin, env, "stack", id.to_string(), s.name).await
}

/// A container's name, the target a container webhook keeps: its id changes every time it is recreated.
async fn container_name(app: &App, env: i64, id: &str) -> ApiResult<String> {
    if !crate::docker::valid_ref(id) {
        return Err(ApiError::bad("That is not a container's id or name."));
    }
    let d = crate::docker::endpoint(app, env).await?;
    let c = d.client().inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>).await.map_err(crate::docker::api_error)?;
    Ok(crate::docker::containers::shown_name(&c))
}

/// `POST /api/envs/{env}/containers/{id}/webhook`: a webhook that pulls the container's image and recreates it.
pub async fn make_container(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = container_name(&app, env, &id).await?;
    make(&app, &admin, env, "container", name.clone(), name).await
}

pub async fn remove_container(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    // By name, as kept: the container may already be gone.
    let name = if crate::docker::valid_name(&id) { id } else { container_name(&app, env, &id).await? };
    forget(&app, &admin, env, "container", name.clone(), name).await
}

#[derive(Deserialize)]
pub struct CallQuery {
    /// Pull the images again first (the default).
    pull: Option<bool>,
}

/// `POST /api/webhooks/{token}`: the redeploy, done before it answers: 204 when it went well, Docker's or Compose's
/// reason when it did not.
pub async fn call(State(app): State<App>, peer: Peer, Path(token): Path<String>, Query(q): Query<CallQuery>) -> ApiResult<StatusCode> {
    let at = now();
    let ip = peer.ip.clone().unwrap_or_default();
    let wait = app.webhooks.wait(&ip, at);
    if wait > 0 {
        let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "Too many wrong webhook addresses from here. Try again later.");
        e.retry_after = Some(wait);
        return Err(e);
    }
    let key = crate::auth::token_hash(&token);
    let found = if plausible(&token) {
        let hash = key.clone();
        app.db
            .call(move |c| {
                Ok(c.query_row("SELECT id, env_id, kind, target, created_by FROM webhooks WHERE token_hash = ?1", [hash], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?))
                })
                .optional()?)
            })
            .await?
    } else {
        None
    };
    let who = Who { user: None, ip: peer.ip.clone() };
    let Some((id, env, kind, target, owner)) = found else {
        app.webhooks.miss(&ip, at);
        audit::record_by(&app.db, &who, Entry { action: "webhook.refused", target: None, detail: Some("no webhook has that address".into()) }).await;
        return Err(ApiError::not_found("webhook"));
    };
    if let Err(wait) = app.webhooks.take(&key, at) {
        let mut e = ApiError::new(StatusCode::TOO_MANY_REQUESTS, format!("This webhook is running or has just run; call it again in {wait}s."));
        e.retry_after = Some(wait);
        return Err(e);
    }
    app.db.call(move |c| Ok(c.execute("UPDATE webhooks SET last_used_at = ?1 WHERE id = ?2", params![at, id])?)).await?;
    // Held to the host's security settings as the person who made it ("Access" 13).
    let by = crate::access::security::Judge::Named(owner);
    // The redeploy is a task of its own, which ends its turn itself: hyper drops this handler when the caller hangs
    // up, and a redeploy stopped halfway would leave the container set aside and the webhook "running" for good.
    let pull = q.pull.unwrap_or(true);
    let a = app.clone();
    let job = tokio::spawn(async move {
        let r = redeploy(&a, &who, &by, id, env, &kind, &target, pull).await;
        a.webhooks.done(&key, now());
        r
    });
    let r = job.await.map_err(|e| ApiError::from(anyhow::anyhow!("the webhook's redeploy failed: {e}")))?;
    r.map(|()| StatusCode::NO_CONTENT)
}

#[allow(clippy::too_many_arguments)]
async fn redeploy(app: &App, who: &Who, by: &crate::access::security::Judge, id: i64, env: i64, kind: &str, target: &str, pull: bool) -> ApiResult<()> {
    let via = format!("by webhook {id}");
    match kind {
        "stack" => {
            let sid: i64 = target.parse().map_err(|_| ApiError::not_found("stack"))?;
            let s = crate::stacks::load(app, env, sid).await?;
            app.stacks.compose.bin()?;
            crate::stacks::run(app, &s, crate::stacks::Act::Deploy { pull }, who, by, Some(&via), None).await.map_err(|m| ApiError::new(StatusCode::BAD_GATEWAY, m))?;
        }
        _ => {
            let _ = crate::docker::containers::recreate_as(app, who, Some(&via), env, target, pull).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{call as call_app, post, signed_in};
    use crate::stacks::tests::{app_with_compose, calls, send};
    use axum::body::Body;
    use axum::http::{Request, header};

    #[test]
    fn a_token_is_64_hex_digits() {
        assert!(plausible(&crate::auth::random_token()));
        assert!(!plausible("") && !plausible(&"g".repeat(64)) && !plausible(&"a".repeat(63)) && !plausible("../../etc/passwd"));
    }

    #[test]
    fn wrong_tokens_wait_by_address_and_a_webhook_runs_once_a_gap() {
        let g = Gate::default();
        for i in 0..MISSES as i64 {
            assert_eq!(g.wait("1.2.3.4", 100 + i), 0);
            g.miss("1.2.3.4", 100 + i);
        }
        assert_eq!(g.wait("1.2.3.4", 120), WINDOW_S - 20);
        assert_eq!(g.wait("5.6.7.8", 120), 0, "another address is not held to it");
        assert_eq!(g.wait("1.2.3.4", 100 + WINDOW_S + MISSES as i64), 0, "the window passes");
        assert!(g.take("a", 1000).is_ok());
        assert_eq!(g.take("a", 1030), Err(GAP_S), "not while it runs, however long that is");
        g.done("a", 1040);
        assert_eq!(g.take("a", 1044), Err(6), "the gap counts from its end");
        assert!(g.take("b", 1044).is_ok(), "each webhook has its own turn");
        assert!(g.take("a", 1050).is_ok());
    }

    #[tokio::test]
    async fn a_stack_webhook_redeploys_without_a_session_and_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "hooked", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        let (s, b) = send(&app, &cookie, &csrf, "POST", &format!("/api/envs/1/stacks/{id}/webhook"), json!({})).await;
        assert_eq!(s, StatusCode::CREATED);
        let url = b["url"].as_str().unwrap().to_string();
        let token = url.trim_start_matches("/api/webhooks/");
        assert!(plausible(token));
        let kept: String = app.db.call(|c| Ok(c.query_row("SELECT token_hash FROM webhooks", [], |r| r.get(0))?)).await.unwrap();
        assert_ne!(kept, token, "only its hash is kept");

        let hook = |u: &str| Request::post(u).body(Body::empty()).unwrap();
        let (s, _, _) = call_app(&app, hook(&url)).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "no cookie, no CSRF token, no Origin: the token is the credential");
        assert!(calls(dir.path()).pop().unwrap().ends_with("up --detach --remove-orphans --pull always"));
        let (s, h, _) = call_app(&app, hook(&url)).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "a second call straight after waits");
        assert!(h.contains_key(header::RETRY_AFTER));
        let (detail, user): (String, Option<String>) = app.db.call(|c| Ok(c.query_row("SELECT detail, user_name FROM audit WHERE action = 'stack.deploy'", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert!(detail.starts_with("by webhook ") && user.is_none(), "{detail}");

        let (_, _, l) = call_app(&app, Request::get("/api/envs/1/webhooks").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(l[0]["kind"], "stack");
        assert!(!l.to_string().contains(token), "the list never says the token");

        let (_, b) = send(&app, &cookie, &csrf, "POST", &format!("/api/envs/1/stacks/{id}/webhook"), json!({})).await;
        let (s, _, _) = call_app(&app, hook(&url)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "a new address ends the old one");
        assert_ne!(b["url"], url);
        let (s, _) = send(&app, &cookie, &csrf, "DELETE", &format!("/api/envs/1/stacks/{id}/webhook"), json!({})).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = call_app(&app, hook(b["url"].as_str().unwrap())).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_addresses_are_refused_logged_and_then_wait() {
        let app = crate::api::tests::app();
        signed_in(&app).await;
        let wrong = format!("/api/webhooks/{}", crate::auth::random_token());
        for _ in 0..MISSES {
            let (s, _, _) = call_app(&app, post(&wrong).body(Body::empty()).unwrap()).await;
            assert_eq!(s, StatusCode::NOT_FOUND);
        }
        let (s, _, _) = call_app(&app, post(&wrong).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        let n: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit WHERE action = 'webhook.refused'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n as usize, MISSES);
    }

    #[tokio::test]
    async fn removing_a_stack_takes_its_webhook() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "brief", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        send(&app, &cookie, &csrf, "POST", &format!("/api/envs/1/stacks/{id}/webhook"), json!({})).await;
        crate::stacks::tests::stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/remove"), json!({})).await;
        let n: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM webhooks", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 0);
    }

    /// A Docker with one running container, `web`, whose create takes `slow` and which writes down every change it is
    /// asked for, and an app on it with its administrator.
    pub(crate) async fn app_on_slow_docker(slow: std::time::Duration) -> (App, String, String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::response::IntoResponse;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let answer = move |method: axum::http::Method, uri: axum::http::Uri| {
            let log = log.clone();
            async move {
                let path = uri.path().trim_start_matches('/');
                let path = match path.split_once('/') {
                    Some((v, rest)) if v.starts_with('v') && v[1..].starts_with(|c: char| c.is_ascii_digit()) => rest.to_string(),
                    _ => path.to_string(),
                };
                if method != axum::http::Method::GET {
                    log.lock().unwrap().push(format!("{method} {path}"));
                }
                let parts: Vec<&str> = path.split('/').collect();
                match (method.as_str(), parts.as_slice()) {
                    ("GET", ["containers", id, "json"]) if ["web", "id-web"].contains(id) => {
                        axum::Json(json!({ "Id": "id-web", "Name": "/web", "Image": "sha256:img", "Config": { "Image": "nginx:1", "Labels": {} }, "HostConfig": {}, "State": { "Running": true } })).into_response()
                    }
                    ("GET", ["containers", id, "json"]) => (StatusCode::NOT_FOUND, axum::Json(json!({ "message": format!("No such container: {id}") }))).into_response(),
                    ("GET", ["images", ..]) => axum::Json(json!({ "Id": "sha256:img", "Config": {} })).into_response(),
                    ("POST", ["images", "create"]) => "{}\n".into_response(),
                    ("POST", ["containers", "create"]) => {
                        tokio::time::sleep(slow).await;
                        (StatusCode::CREATED, axum::Json(json!({ "Id": "id-new", "Warnings": [] }))).into_response()
                    }
                    _ => StatusCode::NO_CONTENT.into_response(),
                }
            }
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("tcp://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, axum::Router::new().fallback(answer)).await.unwrap() });
        let db = crate::db::Db::open_in_memory().unwrap();
        let stacks = crate::stacks::Stacks::new(std::env::temp_dir().join(format!("freightdock-test-{}", crate::auth::random_token())), crate::stacks::compose::Compose::none());
        let app = crate::state::AppState::new(db, false, crate::docker::Docker::connect(&url).unwrap(), crate::secret::Sealer::for_test(), crate::docker::sockets::Sockets::for_test(), crate::agent::Agents::for_test(), stacks);
        let (cookie, csrf) = signed_in(&app).await;
        (app, cookie, csrf, seen)
    }

    #[tokio::test]
    async fn a_webhook_whose_caller_hangs_up_still_ends_and_frees_its_turn() {
        // hyper drops a handler whose client closes the connection: the redeploy must not stop halfway (the old
        // container stopped and set aside) nor leave the webhook "running" until Freightdock restarts ("Sign-in" 8,
        // "Containers" 3).
        let (app, cookie, csrf, seen) = app_on_slow_docker(std::time::Duration::from_millis(400)).await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/containers/web/webhook", json!({})).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let url = b["url"].as_str().unwrap().to_string();
        let gone = tokio::time::timeout(std::time::Duration::from_millis(100), call_app(&app, Request::post(&url).body(Body::empty()).unwrap())).await;
        assert!(gone.is_err(), "the caller hung up while Docker made the new container");
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let changes = seen.lock().unwrap().clone();
        assert!(changes.iter().any(|c| c == "POST containers/id-new/start"), "the new one was started: {changes:?}");
        assert!(changes.iter().any(|c| c.starts_with("DELETE containers/id-web")), "and the old one removed: {changes:?}");
        let key: String = app.db.call(|c| Ok(c.query_row("SELECT token_hash FROM webhooks", [], |r| r.get(0))?)).await.unwrap();
        assert!(app.webhooks.take(&key, now() + GAP_S + 1).is_ok(), "the webhook's turn is free once the gap has passed");
    }

    #[tokio::test]
    async fn a_new_webhook_in_a_reused_row_is_not_held_to_the_old_ones_gap() {
        // A webhook's row goes when it is taken away (or its container goes, "Containers" 3), and SQLite gives the next
        // webhook the same id. The new one has a token of its own and must not wait out the old one's gap: the qa
        // suite's stack webhook was refused 429 straight after the container webhook in the row before it.
        let (app, cookie, csrf, _) = app_on_slow_docker(std::time::Duration::ZERO).await;
        let make = || async {
            let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/containers/web/webhook", json!({})).await;
            assert_eq!(s, StatusCode::CREATED, "{b}");
            b["url"].as_str().unwrap().to_string()
        };
        let first = make().await;
        let (s, _, _) = call_app(&app, Request::post(&first).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = send(&app, &cookie, &csrf, "DELETE", "/api/envs/1/containers/web/webhook", json!({})).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let second = make().await;
        let ids: Vec<i64> = app.db.call(|c| Ok(c.prepare("SELECT id FROM webhooks")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(ids, vec![1], "the new webhook has the old one's row");
        let (s, _, b) = call_app(&app, Request::post(&second).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "the new webhook runs at once: {b}");
    }
}
