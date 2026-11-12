//! Environments: the Docker hosts Freightdock manages. `local` is made at setup; an administrator adds `agent` hosts (they
//! dial in with `freightdock-agent`) and `tls` hosts (Docker's own TCP port with a client certificate), renames and
//! removes them. Every environment is checked every 30 seconds: up or down, the Docker version, and when it last
//! answered ("Environments" in DECISIONS.md).

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use freightdock_tunnel::token::JoinToken;

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// How long a join token is good for.
pub const JOIN_FOR_S: i64 = 24 * 3600;
/// How often every environment is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(30);

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/environments", get(list).post(create))
        .route("/api/environments/{id}", get(one).put(update).delete(remove))
        .route("/api/environments/{id}/join", post(join))
}

const COLUMNS: &str = "id, name, kind, url, status, last_seen_at, last_error, docker_version, agent_cert_sha256 IS NOT NULL, join_token_expires_at, tls_cert IS NOT NULL, \
    group_id, (SELECT name FROM environment_groups g WHERE g.id = environments.group_id), \
    (SELECT group_concat(t.name, char(10)) FROM environment_tags et JOIN tags t ON t.id = et.tag_id WHERE et.environment_id = environments.id), \
    agent_version, agent_peer, snapshot, snapshot_at";

/// Whether an agent's version is older than this Freightdock's, compared number by number. A version that cannot be read,
/// or a build of the same one (`0.1.0-dev`), is not called old.
pub fn outdated(agent: &str, server: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split(['-', '+']).next()?.split('.').map(|n| n.parse().ok()).collect() };
    match (parse(agent), parse(server)) {
        (Some(mut a), Some(mut s)) => {
            let n = a.len().max(s.len());
            a.resize(n, 0);
            s.resize(n, 0);
            a < s
        }
        _ => false,
    }
}

fn row(app: &App, r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Value> {
    let id: i64 = r.get(0)?;
    let kind: String = r.get(2)?;
    let mut v = json!({
        "id": id, "name": r.get::<_, String>(1)?, "kind": kind, "url": r.get::<_, String>(3)?,
        "status": r.get::<_, String>(4)?, "last_seen_at": r.get::<_, Option<i64>>(5)?, "last_error": r.get::<_, Option<String>>(6)?,
        "docker_version": r.get::<_, Option<String>>(7)?,
    });
    let group: Option<i64> = r.get(11)?;
    v["group"] = match group {
        Some(g) => json!({ "id": g, "name": r.get::<_, Option<String>>(12)? }),
        None => Value::Null,
    };
    let mut tags: Vec<String> = r.get::<_, Option<String>>(13)?.map(|t| t.split('\n').map(str::to_string).collect()).unwrap_or_default();
    tags.sort_by_key(|t| t.to_lowercase());
    v["tags"] = json!(tags);
    let snapshot = r.get::<_, Option<String>>(16)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()).filter(Value::is_object);
    v["snapshot"] = match snapshot {
        Some(mut s) => {
            s["at"] = json!(r.get::<_, Option<i64>>(17)?);
            s
        }
        None => Value::Null,
    };
    if kind == "agent" {
        let joining = r.get::<_, Option<i64>>(9)?.filter(|t| *t > now());
        let live = app.sockets.info(id);
        let version: Option<String> = r.get(14)?;
        let outdated = version.as_deref().is_some_and(|a| outdated(a, env!("CARGO_PKG_VERSION")));
        v["agent"] = json!({
            "enrolled": r.get::<_, bool>(8)?, "join_expires_at": joining,
            "connected_since": live.as_ref().map(|l| l.0), "peer": live.and_then(|l| l.1),
            "version": version, "outdated": outdated, "last_peer": r.get::<_, Option<String>>(15)?,
        });
    }
    if kind == "tls" {
        v["tls"] = json!({ "has_cert": r.get::<_, bool>(10)? });
    }
    Ok(v)
}

async fn fetch(app: &App, id: Option<i64>) -> ApiResult<Vec<Value>> {
    let a = app.clone();
    Ok(app
        .db
        .call(move |c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM environments WHERE ?1 IS NULL OR id = ?1 ORDER BY id"))?;
            let rows = st.query_map([id], |r| row(&a, r))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}

/// The environments among `all` the person has a role on, each with that role (`role`: `admin` for an
/// Administrator) and the actions it allows (`actions`, for the UI to hide the rest; "Access" 6).
///
/// Where consoles are turned off for them ("Access" 17), `console` is not among the actions.
async fn seen(app: &App, a: &Authed, all: Vec<Value>) -> ApiResult<Vec<Value>> {
    let levels = crate::access::levels(&app.db, &a.session.user).await?;
    let mut out = vec![];
    for mut e in all {
        let Some(level) = e["id"].as_i64().and_then(|id| levels.get(&id)).copied() else { continue };
        let mut actions = level.actions();
        if !crate::access::security::exempt(level) && actions.contains(&"console") {
            let env = e["id"].as_i64().unwrap_or_default();
            if crate::access::security::rules(&app.db, env).await?.block_consoles {
                actions.retain(|a| *a != "console");
            }
        }
        e["role"] = json!(level.name());
        e["actions"] = json!(actions);
        out.push(e);
    }
    Ok(out)
}

/// `GET /api/environments`: every environment the person has a role on, for the switcher, Home and the list.
async fn list(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let all = fetch(&app, None).await?;
    Ok(Json(json!(seen(&app, &a, all).await?)))
}

async fn one(State(app): State<App>, a: Authed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = fetch(&app, Some(id)).await?;
    seen(&app, &a, all).await?.pop().map(Json).ok_or_else(|| ApiError::not_found("environment"))
}

fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(ApiError::bad("A name is 1 to 64 characters.").field("name"));
    }
    Ok(n.to_string())
}

#[derive(Deserialize, Default)]
struct Fields {
    name: Option<String>,
    kind: Option<String>,
    url: Option<String>,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    /// Kept sealed; empty or left out keeps the one there is.
    tls_key: Option<String>,
    /// Its group: null is none; left out keeps the one it has.
    #[serde(default, deserialize_with = "crate::groups::some")]
    group_id: Option<Option<i64>>,
    /// Every tag it has, by name, made as needed; left out keeps them.
    tags: Option<Vec<String>>,
}

impl Fields {
    fn placement(&self) -> crate::groups::Placement {
        crate::groups::Placement { group: self.group_id, tags: self.tags.clone() }
    }
}

fn field_error((field, message): (&'static str, String)) -> ApiError {
    ApiError::bad(message).field(field)
}

/// A TLS host's settings, checked: its address, CA, certificate, and its key sealed.
fn tls_settings(app: &App, url: &str, ca: &str, cert: &str, key: &str) -> ApiResult<(String, String, String, String)> {
    let url = url.trim();
    crate::docker::tls_host::host_port(url).map_err(|e| ApiError::bad(e.to_string()).field("url"))?;
    crate::docker::tls_host::client_config(ca, cert, key).map_err(field_error)?;
    Ok((url.to_string(), ca.trim().to_string(), cert.trim().to_string(), app.sealer.seal(key.trim())?))
}

/// A new join token: the token to hand out, and the hash of its secret, which is all that is kept.
fn new_token(app: &App) -> (String, String) {
    let mut s = [0u8; 32];
    rand::rng().fill_bytes(&mut s);
    let t = JoinToken { ca_sha256: app.agents.ca.sha256(), secret: hex::encode(s) };
    (t.to_string(), t.secret_hash())
}

/// What an administrator needs to start an agent: the token, until when it is good, and the `docker run` line.
fn join_answer(app: &App, headers: &HeaderMap, token: &str, expires_at: i64) -> Value {
    let address = app.agents.address_for(headers.get(header::HOST).and_then(|v| v.to_str().ok()));
    let command = address.as_deref().map(|a| crate::agent::run_command(&app.agents.image, a, token));
    json!({ "token": token, "expires_at": expires_at, "address": address, "image": app.agents.image, "command": command })
}

fn unique(e: anyhow::Error) -> ApiError {
    if format!("{e:#}").contains("UNIQUE constraint failed: environments.name") {
        return ApiError::conflict("There is an environment of that name.").field("name");
    }
    e.into()
}

/// `POST /api/environments`: `{name, kind: "agent"}` answers the join token and the agent's `docker run` line once;
/// `{name, kind: "tls", url, tls_ca, tls_cert, tls_key}` a host on Docker's TLS port.
async fn create(State(app): State<App>, admin: Admin, headers: HeaderMap, Json(f): Json<Fields>) -> ApiResult<Response> {
    let name = check_name(f.name.as_deref().unwrap_or(""))?;
    let kind = f.kind.as_deref().unwrap_or("");
    let mut place = f.placement();
    let t = now();
    let (id, join) = match kind {
        "agent" => {
            if app.agents.address.is_none() && app.agents.port.is_none() {
                return Err(ApiError::conflict("The agent endpoint is off (FREIGHTDOCK_AGENT_BIND=off) and no FREIGHTDOCK_AGENT_ADDRESS is set, so an agent has nowhere to dial."));
            }
            place.check(&app).await?;
            let (token, hash) = new_token(&app);
            let n = name.clone();
            let id = app
                .db
                .call(move |c| {
                    let tx = c.transaction()?;
                    tx.execute(
                        "INSERT INTO environments (name, kind, url, created_at, join_token_hash, join_token_expires_at) VALUES (?1, 'agent', '', ?2, ?3, ?4)",
                        params![n, t, hash, t + JOIN_FOR_S],
                    )?;
                    let id = tx.last_insert_rowid();
                    let made = place.apply(&tx, id)?;
                    tx.commit()?;
                    Ok((id, made))
                })
                .await
                .map_err(unique)?;
            (id, Some(join_answer(&app, &headers, &token, t + JOIN_FOR_S)))
        }
        "tls" => {
            let (url, ca, cert, key) = tls_settings(&app, f.url.as_deref().unwrap_or(""), f.tls_ca.as_deref().unwrap_or(""), f.tls_cert.as_deref().unwrap_or(""), f.tls_key.as_deref().unwrap_or(""))?;
            place.check(&app).await?;
            let n = name.clone();
            let id = app
                .db
                .call(move |c| {
                    let tx = c.transaction()?;
                    tx.execute("INSERT INTO environments (name, kind, url, created_at, tls_ca, tls_cert, tls_key) VALUES (?1, 'tls', ?2, ?3, ?4, ?5, ?6)", params![n, url, t, ca, cert, key])?;
                    let id = tx.last_insert_rowid();
                    let made = place.apply(&tx, id)?;
                    tx.commit()?;
                    Ok((id, made))
                })
                .await
                .map_err(unique)?;
            (id, None)
        }
        "local" => return Err(ApiError::bad("The local environment is made at setup.").field("kind")),
        _ => return Err(ApiError::bad("An environment is an agent or a TLS host.").field("kind")),
    };
    let ((id, made), join) = (id, join);
    crate::groups::log_made(&app, &admin, made).await;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "environment.create", target: Some(name), detail: Some(kind.to_string()) }).await;
    if kind == "tls" {
        let app = app.clone();
        tokio::spawn(async move { check(&app, id).await });
    }
    let env = fetch(&app, Some(id)).await?.pop();
    Ok((StatusCode::CREATED, Json(json!({ "environment": env, "join": join }))).into_response())
}

/// The kind and name of environment `id`.
async fn kind_of(app: &App, id: i64) -> ApiResult<(String, String)> {
    app.db
        .call(move |c| Ok(c.query_row("SELECT kind, name FROM environments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?))
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))
}

/// `PUT /api/environments/{id}`: a new name; for a TLS host also its address and certificates (an empty key keeps
/// the one there is).
async fn update(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<Fields>) -> ApiResult<Json<Value>> {
    let (kind, old) = kind_of(&app, id).await?;
    let mut place = f.placement();
    let placed = place.check(&app).await?;
    let name = match &f.name {
        Some(n) => check_name(n)?,
        None => old.clone(),
    };
    let tls = if kind == "tls" && (f.url.is_some() || f.tls_ca.is_some() || f.tls_cert.is_some() || f.tls_key.as_deref().is_some_and(|k| !k.trim().is_empty())) {
        let (url0, ca0, cert0, key0) = app
            .db
            .call(move |c| Ok(c.query_row("SELECT url, tls_ca, tls_cert, tls_key FROM environments WHERE id = ?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?)))?))
            .await?;
        let key = match f.tls_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => k.to_string(),
            None => app.sealer.open(&key0.unwrap_or_default())?,
        };
        Some(tls_settings(
            &app,
            f.url.as_deref().unwrap_or(&url0),
            f.tls_ca.as_deref().unwrap_or(ca0.as_deref().unwrap_or("")),
            f.tls_cert.as_deref().unwrap_or(cert0.as_deref().unwrap_or("")),
            &key,
        )?)
    } else {
        None
    };
    let n = name.clone();
    let changed_tls = tls.is_some();
    let made = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE environments SET name = ?1 WHERE id = ?2", params![n, id])?;
            if let Some((url, ca, cert, key)) = tls {
                tx.execute("UPDATE environments SET url = ?1, tls_ca = ?2, tls_cert = ?3, tls_key = ?4, status = 'unknown' WHERE id = ?5", params![url, ca, cert, key, id])?;
            }
            let made = place.apply(&tx, id)?;
            tx.commit()?;
            Ok(made)
        })
        .await
        .map_err(unique)?;
    crate::groups::log_made(&app, &admin, made).await;
    if changed_tls {
        // The next request makes the bridge again with what was saved.
        app.sockets.close(id);
    }
    let a = &admin.0;
    let said = [(old != name).then(|| format!("was {old}")), changed_tls.then(|| "address or certificates changed".to_string()), placed];
    let detail = said.into_iter().flatten().collect::<Vec<_>>().join("; ");
    let detail = (!detail.is_empty()).then_some(detail);
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "environment.update", target: Some(name), detail }).await;
    fetch(&app, Some(id)).await?.pop().map(Json).ok_or_else(|| ApiError::not_found("environment"))
}

/// `DELETE /api/environments/{id}`: forgets the host; an agent's tunnel is cut and its certificate is good for nothing
/// more. The local environment stays.
async fn remove(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let (kind, name) = kind_of(&app, id).await?;
    if kind == "local" {
        return Err(ApiError::conflict("The local environment stays."));
    }
    // Its stacks' rows go by cascade; their folders are taken off the disk here, as removing each stack would.
    let stacks = app
        .db
        .call(move |c| {
            let ids = c.prepare("SELECT id FROM stacks WHERE env_id = ?1")?.query_map([id], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
            c.execute("DELETE FROM environments WHERE id = ?1", [id])?;
            Ok(ids)
        })
        .await?;
    crate::stacks::forget_folders(&app, stacks).await;
    app.sockets.close(id);
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "environment.remove", target: Some(name), detail: Some(kind) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/environments/{id}/join`: a new join token for an agent. Its certificate is revoked and its tunnel cut
/// at once: the agent must enrol again with the new token.
async fn join(State(app): State<App>, admin: Admin, headers: HeaderMap, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (kind, name) = kind_of(&app, id).await?;
    if kind != "agent" {
        return Err(ApiError::bad("Only an agent environment has a join token."));
    }
    let (token, hash) = new_token(&app);
    let expires = now() + JOIN_FOR_S;
    app.db
        .call(move |c| Ok(c.execute("UPDATE environments SET join_token_hash = ?1, join_token_expires_at = ?2, agent_cert_sha256 = NULL WHERE id = ?3", params![hash, expires, id])?))
        .await?;
    app.sockets.close(id);
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "environment.join", target: Some(name), detail: Some("new join token; the old certificate revoked".into()) }).await;
    Ok(Json(join_answer(&app, &headers, &token, expires)))
}

/// Asks environment `id`'s Docker for its version and writes down what came of it.
pub async fn check(app: &App, id: i64) {
    let answer = match crate::docker::endpoint(app, id).await {
        Ok(d) => match tokio::time::timeout(Duration::from_secs(10), d.client().version()).await {
            Ok(Ok(v)) => Ok(v.version.unwrap_or_default()),
            Ok(Err(e)) => Err(crate::docker::api_error(e).message),
            Err(_) => Err("Docker took too long to answer.".to_string()),
        },
        Err(e) if e.status == StatusCode::NOT_FOUND => return,
        Err(e) => Err(e.message),
    };
    let up = answer.is_ok();
    let r = app
        .db
        .call(move |c| {
            let was: Option<(String, String)> = c.query_row("SELECT status, name FROM environments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let down = match &answer {
                Ok(v) => {
                    c.execute("UPDATE environments SET status = 'up', last_seen_at = ?1, last_error = NULL, docker_version = ?2 WHERE id = ?3", params![now(), v, id])?;
                    None
                }
                Err(e) => {
                    c.execute("UPDATE environments SET status = 'down', last_error = ?1 WHERE id = ?2", params![e, id])?;
                    was.filter(|(s, _)| s == "up").map(|(_, name)| (name, e.clone()))
                }
            };
            Ok(down)
        })
        .await;
    match r {
        Ok(Some((name, why))) => went_down(app, id, name, why),
        Ok(None) => {}
        Err(e) => tracing::warn!("could not write down environment {id}'s health: {e:#}"),
    }
    // Up: what it holds now, kept for the home page and for when it is down. Failing that, the last stays.
    if up {
        let _ = crate::docker::summary::snapshot(app, id).await;
    }
}

/// Tells that environment `id`, which answered before, does not now.
pub fn went_down(app: &App, id: i64, name: String, why: String) {
    crate::notify::notify(app, crate::notify::Event { kind: "environment.down", title: format!("{name} is down"), message: why, env: Some((id, name.clone())), target: Some(name) });
}

/// Checks every environment every `CHECK_EVERY`, until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    loop {
        let ids = app.db.call(|c| Ok(c.prepare("SELECT id FROM environments")?.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap_or_default();
        futures_util::future::join_all(ids.into_iter().map(|id| {
            let app = app.clone();
            async move { check(&app, id).await }
        }))
        .await;
        tokio::select! {
            _ = tokio::time::sleep(CHECK_EVERY) => {}
            _ = stop.cancelled() => return,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::api::tests::{app, call, json_body, signed_in};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use serde_json::{Value, json};

    /// A request as the signed-in browser makes it: the cookie, the CSRF token, this server's origin.
    pub fn req(method: Method, path: &str, cookie: &str, csrf: &str, body: Option<Value>) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "freightdock.test:7443")
            .header(header::ORIGIN, "https://freightdock.test:7443")
            .header(header::COOKIE, cookie)
            .header("x-csrf-token", csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map(json_body).unwrap_or_else(Body::empty))
            .unwrap()
    }

    async fn audited(app: &crate::state::App, action: &str) -> Vec<(String, Option<String>)> {
        let a = action.to_string();
        app.db
            .call(move |c| Ok(c.prepare("SELECT target, detail FROM audit WHERE action = ?1 ORDER BY id")?.query_map([a], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_agent_environment_comes_with_its_join_token_and_run_line_once() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": " vm-1 ", "kind": "agent" })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!(b["environment"]["name"], "vm-1");
        assert_eq!(b["environment"]["kind"], "agent");
        assert_eq!(b["environment"]["status"], "unknown");
        assert_eq!(b["environment"]["agent"]["enrolled"], false);
        let token = b["join"]["token"].as_str().unwrap();
        let t = freightdock_tunnel::token::JoinToken::parse(token).expect("a join token");
        assert_eq!(t.ca_sha256, app.agents.ca.sha256(), "the token pins this Freightdock's CA");
        assert_eq!(b["join"]["address"], "freightdock.test:7444");
        assert!(b["join"]["command"].as_str().unwrap().contains(token));

        // Only the hash of the secret is kept.
        let id = b["environment"]["id"].as_i64().unwrap();
        let kept: String = app.db.call(move |c| Ok(c.query_row("SELECT join_token_hash FROM environments WHERE id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert_eq!(kept, t.secret_hash());
        let (_, _, list) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert!(!list.to_string().contains(&t.secret), "the token is never shown again");
        assert!(list[1]["agent"]["join_expires_at"].as_i64().unwrap() > crate::db::now());
        assert_eq!(audited(&app, "environment.create").await, vec![("vm-1".into(), Some("agent".into()))]);

        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "VM-1", "kind": "agent" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")), "names are unique, case-blind");
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "x", "kind": "swarm" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("kind")));
        let (s, _, _) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "", "kind": "agent" })))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn only_an_administrator_changes_environments_and_only_with_the_csrf_token() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let (s, _, _) = call(&app, req(Method::POST, "/api/environments", &cookie, "wrong", Some(json!({ "name": "a", "kind": "agent" })))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let cookie = format!("freightdock_session={token}");
        let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
        let (s, _, b) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!((s, b.as_array().map(Vec::len)), (StatusCode::OK, Some(0)), "a host somebody has no role on is not shown to them");
        let (s, _, _) = call(&app, req(Method::GET, "/api/environments/1", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        app.db.call(|c| Ok(c.execute("INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'operator')", [])?)).await.unwrap();
        let (_, _, b) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!((b[0]["kind"].as_str(), b[0]["role"].as_str()), (Some("local"), Some("operator")));
        assert_eq!(b[0]["actions"], json!(["read", "operate", "console"]), "what the role may do, for the UI to hide the rest");
        let (s, _, _) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "a", "kind": "agent" })))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, req(Method::DELETE, "/api/environments/1", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn renaming_removing_and_a_new_token_are_logged_and_the_local_one_stays() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "vm", "kind": "agent" })))).await;
        let id = b["environment"]["id"].as_i64().unwrap();
        let first = b["join"]["token"].as_str().unwrap().to_string();
        app.db.call(move |c| Ok(c.execute("UPDATE environments SET agent_cert_sha256 = 'abc' WHERE id = ?1", [id])?)).await.unwrap();

        let (s, _, b) = call(&app, req(Method::PUT, &format!("/api/environments/{id}"), &cookie, &csrf, Some(json!({ "name": "vm-renamed" })))).await;
        assert_eq!((s, b["name"].as_str()), (StatusCode::OK, Some("vm-renamed")));
        let (s, _, b) = call(&app, req(Method::POST, &format!("/api/environments/{id}/join"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK);
        assert_ne!(b["token"].as_str().unwrap(), first);
        let cert: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT agent_cert_sha256 FROM environments WHERE id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert_eq!(cert, None, "a new token revokes the certificate the agent had");
        let (s, _, _) = call(&app, req(Method::POST, "/api/environments/1/join", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "the local Docker has no token");

        let (s, _, _) = call(&app, req(Method::DELETE, "/api/environments/1", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(audited(&app, "environment.update").await, vec![("vm-renamed".into(), Some("was vm".into()))]);
        assert_eq!(audited(&app, "environment.join").await.len(), 1);
        assert_eq!(audited(&app, "environment.remove").await, vec![("vm-renamed".into(), Some("agent".into()))]);
    }

    #[tokio::test]
    async fn an_agent_that_is_not_connected_is_said_so() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "far", "kind": "agent" })))).await;
        let id = b["environment"]["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/envs/{id}/info"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(b["error"], "The agent for far is not connected.");
        let (s, _, _) = call(&app, req(Method::GET, &format!("/api/envs/{id}/summary"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "the home page hears at once, and keeps the last numbers");
        super::check(&app, id).await;
        let (_, _, b) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
        assert_eq!(b["status"], "down");
        assert_eq!(b["last_error"], "The agent for far is not connected.");
        let (s, _, _) = call(&app, req(Method::GET, "/api/envs/99/info", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[test]
    fn an_agent_older_than_freightdock_is_outdated() {
        assert!(super::outdated("0.0.9", "0.1.0"));
        assert!(super::outdated("0.1.0", "0.10.0"), "by number, not by text");
        assert!(super::outdated("0.1", "0.1.1"));
        assert!(!super::outdated("0.1.0", "0.1.0"));
        assert!(!super::outdated("0.2.0", "0.1.0"));
        assert!(!super::outdated("0.1.0-dev", "0.1.0"), "a build of the same version is not behind");
        assert!(!super::outdated("junk", "0.1.0"), "what cannot be read is not called old");
    }

    #[tokio::test]
    async fn an_agent_says_its_version_and_where_it_last_dialled_from_and_a_host_its_last_numbers() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "old", "kind": "agent" })))).await;
        let id = b["environment"]["id"].as_i64().unwrap();
        assert_eq!(b["environment"]["agent"]["version"], json!(null));
        assert_eq!(b["environment"]["agent"]["outdated"], false);
        assert_eq!(b["environment"]["snapshot"], json!(null));
        app.db
            .call(move |c| Ok(c.execute("UPDATE environments SET agent_version = '0.0.1', agent_peer = '192.0.2.5', snapshot = '{\"cpus\":4}', snapshot_at = 1700000000 WHERE id = ?1", [id])?))
            .await
            .unwrap();
        let (_, _, e) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
        assert_eq!(e["agent"]["version"], "0.0.1");
        assert_eq!(e["agent"]["outdated"], true, "older than this Freightdock");
        assert_eq!(e["agent"]["peer"], json!(null), "not connected now");
        assert_eq!(e["agent"]["last_peer"], "192.0.2.5");
        assert_eq!(e["snapshot"], json!({ "cpus": 4, "at": 1700000000 }));
        assert_eq!(e["group"], json!(null));
        assert_eq!(e["tags"], json!([]));
    }

    #[tokio::test]
    async fn a_tls_host_is_checked_when_saved_and_its_key_is_sealed() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (ca, cert, key) = crate::docker::tls_host::tests::pki("freightdock", true);
        let body = |url: &str, key: &str| json!({ "name": "tls-host", "kind": "tls", "url": url, "tls_ca": ca, "tls_cert": cert, "tls_key": key });
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(body("ftp://x", &key)))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("url")));
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(body("tcp://127.0.0.1:1", "not a key")))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("tls_key")));
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(body("tcp://127.0.0.1:1", &key)))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!(b["environment"]["tls"]["has_cert"], true);
        assert!(!b.to_string().contains("PRIVATE KEY"));
        let id = b["environment"]["id"].as_i64().unwrap();
        let kept: String = app.db.call(move |c| Ok(c.query_row("SELECT tls_key FROM environments WHERE id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert!(kept.starts_with("sealed1:"));
        assert_eq!(app.sealer.open(&kept).unwrap(), key.trim());
        // An edit without a key keeps the key.
        let (s, _, _) = call(&app, req(Method::PUT, &format!("/api/environments/{id}"), &cookie, &csrf, Some(json!({ "url": "tcp://127.0.0.1:2" })))).await;
        assert_eq!(s, StatusCode::OK);
        let (url, kept2): (String, String) = app.db.call(move |c| Ok(c.query_row("SELECT url, tls_key FROM environments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!(url, "tcp://127.0.0.1:2");
        assert_eq!(app.sealer.open(&kept2).unwrap(), key.trim());
    }
}
