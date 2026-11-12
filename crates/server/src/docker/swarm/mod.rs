//! Swarm, on an environment whose Docker is a swarm manager: nodes, services and their tasks and logs, secrets and
//! configs, and stacks deployed with `docker stack deploy`. Reads need a session; changes an administrator, the CSRF
//! token, and an entry in the activity log ("Swarm" in DECISIONS.md). Everything is spoken to Docker as JSON
//! (`docker::raw`), so a spec read and sent back keeps every field ("Swarm" 2).

pub mod nodes;
pub mod objects;
pub mod services;
pub mod stacks;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use hyper::Method;
use serde_json::{Value, json};

use super::{Docker, endpoint, raw};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub fn router() -> Router<App> {
    let e = |p: &str| format!("/api/envs/{{env}}/swarm{p}");
    Router::new()
        .route(&e(""), get(status))
        .route(&e("/nodes"), get(nodes::list))
        .route(&e("/nodes/{id}"), get(nodes::inspect).put(nodes::update).delete(nodes::remove))
        .route(&e("/services"), get(services::list).post(services::create))
        .route(&e("/services/{id}"), get(services::inspect).put(services::update).delete(services::remove))
        .route(&e("/services/{id}/scale"), post(services::scale))
        .route(&e("/services/{id}/rollback"), post(services::rollback))
        .route(&e("/services/{id}/logs/ws"), get(services::logs_ws))
        .route(&e("/tasks"), get(services::tasks))
        .route(&e("/secrets"), get(objects::list_secrets).post(objects::create_secret))
        .route(&e("/secrets/{id}"), get(objects::inspect_secret).delete(objects::remove_secret))
        .route(&e("/configs"), get(objects::list_configs).post(objects::create_config))
        .route(&e("/configs/{id}"), get(objects::inspect_config).delete(objects::remove_config))
        .route(&e("/stacks"), get(stacks::list).post(stacks::deploy))
        .route(&e("/stacks/{name}"), get(stacks::inspect).delete(stacks::remove))
}

/// What this Docker is to a swarm, from its `/info`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Role {
    /// Not in a swarm (or joining, or left with an error).
    None,
    /// A worker: it runs tasks but cannot be asked about the swarm.
    Worker,
    /// A manager: the Swarm pages are for it.
    Manager,
}

impl Role {
    pub fn of(info: &Value) -> Role {
        let s = &info["Swarm"];
        if s["LocalNodeState"] != json!("active") {
            Role::None
        } else if s["ControlAvailable"] == json!(true) {
            Role::Manager
        } else {
            Role::Worker
        }
    }

    fn word(self) -> &'static str {
        match self {
            Role::None => "none",
            Role::Worker => "worker",
            Role::Manager => "manager",
        }
    }
}

/// Docker's "This node is not a swarm manager" is a 503 from Docker, which `api_error` makes a 502 (Docker failed us);
/// here it is no failure but what the host is, said as a 409.
pub fn not_manager(e: ApiError) -> ApiError {
    if e.status == StatusCode::BAD_GATEWAY && (e.message.contains("not a swarm manager") || e.message.contains("not part of a swarm")) {
        ApiError::conflict(format!("This host is not a swarm manager. {}", e.message))
    } else {
        e
    }
}

/// One call to Docker for a Swarm route.
pub async fn call(d: &Docker, method: Method, path: &str, body: Option<&Value>) -> ApiResult<Value> {
    raw::call(d, method, path, body).await.map_err(not_manager)
}

/// How often a change is sent again when Swarm moved the object's version meanwhile.
const TRIES: u32 = 5;

/// Docker's refusal of an update sent at a version the object has since moved past.
fn out_of_sequence(e: &ApiError) -> bool {
    e.message.to_ascii_lowercase().contains("update out of sequence")
}

/// A change to a service or a node (`kind`: `services`, `nodes`): read it, make the spec to send from what was read
/// (`make` says the spec and whether it is a service's roll back), send it at the version read, as Docker wants. Swarm
/// moves the version by itself (a service's tasks rolling over, a node's status), so a change can meet "update out of
/// sequence" with nobody else involved: it is then read and made again, up to `TRIES` times ("Swarm" 9). Returns
/// Docker's answer and the object as last read.
pub async fn versioned_change(d: &Docker, kind: &str, id: &str, mut make: impl FnMut(&Value) -> ApiResult<(Value, bool)>) -> ApiResult<(Value, Value)> {
    let mut tries = 1;
    loop {
        let o = call(d, Method::GET, &format!("/{kind}/{id}"), None).await?;
        let version = o["Version"]["Index"].as_u64().ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, "Docker gave it no version."))?;
        let (spec, rollback) = make(&o)?;
        let v = version.to_string();
        let mut q = vec![("version", v.as_str())];
        if rollback {
            q.push(("rollback", "previous"));
        }
        match call(d, Method::POST, &raw::path(&format!("/{kind}/{id}/update"), &q), Some(&spec)).await {
            Err(e) if out_of_sequence(&e) && tries < TRIES => {
                tokio::time::sleep(std::time::Duration::from_millis(100 * u64::from(tries))).await;
                tries += 1;
            }
            r => return Ok((r?, o)),
        }
    }
}

pub fn check(id: &str, what: &str) -> ApiResult<()> {
    if super::valid_ref(id) { Ok(()) } else { Err(ApiError::bad(format!("That is not a {what}'s id or name."))) }
}

/// A list Docker answered, as a list (Docker says `null` for none on some versions).
pub fn list_of(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        _ => vec![],
    }
}

pub async fn log(app: &App, admin: &Allowed, action: &'static str, target: impl Into<String>, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target.into()), detail }).await;
}

/// `GET .../swarm`: what this host is to a swarm (`role`: none, worker or manager), and for a manager the swarm itself:
/// its id, when it was made, how many nodes and managers.
pub async fn status(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let info = raw::call(&d, Method::GET, "/info", None).await?;
    let role = Role::of(&info);
    let s = &info["Swarm"];
    let mut out = json!({
        "role": role.word(),
        "state": s["LocalNodeState"],
        "node_id": s["NodeID"],
        "node_addr": s["NodeAddr"],
        "nodes": s["Nodes"],
        "managers": s["Managers"],
        "error": s["Error"],
    });
    if role == Role::Manager {
        let sw = call(&d, Method::GET, "/swarm", None).await?;
        out["swarm"] = json!({ "id": sw["ID"], "created": sw["CreatedAt"], "updated": sw["UpdatedAt"], "spec": sw["Spec"] });
    }
    Ok(Json(out))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::tests::{app, call as http, json_body, post, signed_in};
    use axum::body::Body;
    use axum::http::{Request, header};

    /// A Swarm manager with one service or node (`kind` is `services` or `nodes`), `web`, as Docker keeps it: an
    /// update is taken only at the version it is at now. Swarm moves that version by itself (a service's tasks rolling
    /// over, a node's status); here it does so once, right after the first read, and `others` (when set) is another
    /// person's change landing at the same moment.
    pub struct Swarm {
        pub version: u64,
        pub spec: Value,
        moved: bool,
        others: Option<Value>,
        pub updates: Vec<(u64, bool)>,
    }

    pub async fn versioned(kind: &'static str, others: Option<Value>) -> (std::sync::Arc<std::sync::Mutex<Swarm>>, App) {
        use axum::extract::{RawQuery, State as S};
        use axum::response::IntoResponse;
        type Shared = std::sync::Arc<std::sync::Mutex<Swarm>>;
        async fn answer(S((sw, kind)): S<(Shared, &'static str)>, method: axum::http::Method, uri: axum::http::Uri, RawQuery(q): RawQuery, body: axum::body::Bytes) -> axum::response::Response {
            let path = uri.path().trim_start_matches('/');
            let path = path.split_once('/').filter(|(v, _)| v.starts_with('v')).map_or(path, |(_, rest)| rest);
            let mut sw = sw.lock().unwrap();
            match (method.as_str(), path) {
                ("GET", p) if p == format!("{kind}/web") => {
                    let now = json!({ "ID": "web", "Version": { "Index": sw.version }, "Spec": sw.spec, "PreviousSpec": { "Name": "web" } });
                    if !sw.moved {
                        sw.moved = true;
                        sw.version += 1;
                        if let Some(o) = sw.others.take() {
                            sw.spec = o;
                        }
                    }
                    axum::Json(now).into_response()
                }
                ("POST", p) if p == format!("{kind}/web/update") => {
                    let q = q.unwrap_or_default();
                    let at: u64 = q.split('&').find_map(|kv| kv.strip_prefix("version=")).and_then(|v| v.parse().ok()).unwrap_or(0);
                    sw.updates.push((at, q.contains("rollback=previous")));
                    if at != sw.version {
                        let refused = json!({ "message": "rpc error: code = Unknown desc = update out of sequence" });
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(refused)).into_response();
                    }
                    sw.version += 1;
                    sw.spec = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    axum::Json(json!({ "Warnings": [] })).into_response()
                }
                _ => axum::Json(json!({})).into_response(),
            }
        }
        let spec = json!({ "Name": "web", "Mode": { "Replicated": { "Replicas": 1 } }, "TaskTemplate": { "ContainerSpec": { "Image": "nginx:1" } } });
        let sw: Shared = std::sync::Arc::new(std::sync::Mutex::new(Swarm { version: 10, spec, moved: false, others, updates: vec![] }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("tcp://{}", listener.local_addr().unwrap());
        let router = axum::Router::new().fallback(answer).with_state((sw.clone(), kind));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let stacks = crate::stacks::Stacks::new(std::env::temp_dir().join(format!("freightdock-test-{}", crate::auth::random_token())), crate::stacks::compose::Compose::none());
        let app = crate::state::AppState::new(
            crate::db::Db::open_in_memory().unwrap(),
            false,
            crate::docker::Docker::connect(&url).unwrap(),
            crate::secret::Sealer::for_test(),
            crate::docker::sockets::Sockets::for_test(),
            crate::agent::Agents::for_test(),
            stacks,
        );
        (sw, app)
    }

    #[test]
    fn a_manager_a_worker_and_a_lone_docker_are_told_apart() {
        assert_eq!(Role::of(&json!({ "Swarm": { "LocalNodeState": "active", "ControlAvailable": true } })), Role::Manager);
        assert_eq!(Role::of(&json!({ "Swarm": { "LocalNodeState": "active", "ControlAvailable": false } })), Role::Worker);
        assert_eq!(Role::of(&json!({ "Swarm": { "LocalNodeState": "inactive", "ControlAvailable": false } })), Role::None);
        assert_eq!(Role::of(&json!({ "Swarm": { "LocalNodeState": "pending" } })), Role::None);
        assert_eq!(Role::of(&json!({})), Role::None);
    }

    #[test]
    fn not_a_manager_is_a_conflict_and_a_docker_that_fails_stays_a_502() {
        let e = not_manager(ApiError::new(StatusCode::BAD_GATEWAY, "This node is not a swarm manager. Use \"docker swarm init\"."));
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert!(e.message.starts_with("This host is not a swarm manager."));
        let e = not_manager(ApiError::new(StatusCode::BAD_GATEWAY, "Docker did not answer: refused"));
        assert_eq!(e.status, StatusCode::BAD_GATEWAY);
    }

    /// A signed-in administrator, a user who is not one, and the local environment's id.
    pub async fn people(a: &App) -> (String, String, String, i64) {
        let (cookie, csrf) = signed_in(a).await;
        a.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let token = crate::auth::start(&a.db, 9, None, None).await.unwrap();
        let env: i64 = a.db.call(|c| Ok(c.query_row("SELECT id FROM environments WHERE name = 'local'", [], |r| r.get(0))?)).await.unwrap();
        (cookie, csrf, format!("freightdock_session={token}"), env)
    }

    pub fn change(method: &str, path: &str, cookie: &str, csrf: Option<&str>, body: Value) -> Request<Body> {
        let mut r = post(path).method(method).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, cookie);
        if let Some(t) = csrf {
            r = r.header("x-csrf-token", t);
        }
        r.body(json_body(body)).unwrap()
    }

    /// Every change under `/swarm` is an administrator's and needs the CSRF token; every read needs a session.
    #[tokio::test]
    async fn swarm_changes_are_an_administrator_s_with_the_csrf_token_and_reads_want_a_session() {
        let a = app();
        let (admin, csrf, user, env) = people(&a).await;
        let changes = [
            ("PUT", "nodes/n1", json!({ "availability": "drain" })),
            ("DELETE", "nodes/n1", json!({})),
            ("POST", "services", json!({ "Name": "web", "TaskTemplate": { "ContainerSpec": { "Image": "nginx" } } })),
            ("PUT", "services/web", json!({ "Name": "web", "TaskTemplate": { "ContainerSpec": { "Image": "nginx" } } })),
            ("POST", "services/web/scale", json!({ "replicas": 3 })),
            ("POST", "services/web/rollback", json!({})),
            ("DELETE", "services/web", json!({})),
            ("POST", "secrets", json!({ "name": "s", "data": "x" })),
            ("DELETE", "secrets/s", json!({})),
            ("POST", "configs", json!({ "name": "c", "data": "x" })),
            ("DELETE", "configs/c", json!({})),
            ("POST", "stacks", json!({ "name": "app", "compose": "services: {}" })),
            ("DELETE", "stacks/app", json!({})),
        ];
        for (m, p, body) in changes {
            let path = format!("/api/envs/{env}/swarm/{p}");
            let (s, _, _) = http(&a, change(m, &path, &user, Some(&user_csrf(&a).await), body.clone())).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p} by a user who is not an administrator");
            let (s, _, _) = http(&a, change(m, &path, &admin, None, body.clone())).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p} without the CSRF token");
            let (s, _, b) = http(&a, change(m, &path, &admin, Some(&csrf), body)).await;
            assert_eq!(s, StatusCode::BAD_GATEWAY, "{m} {p} by the administrator gets as far as Docker (none here): {b}");
        }
        for p in ["", "/nodes", "/nodes/n1", "/services", "/services/web", "/tasks", "/secrets", "/secrets/s", "/configs", "/configs/c", "/stacks", "/stacks/app"] {
            let path = format!("/api/envs/{env}/swarm{p}");
            let (s, _, _) = http(&a, Request::get(&path).body(Body::empty()).unwrap()).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "GET {p} without a session");
            let (s, _, b) = http(&a, Request::get(&path).header(header::COOKIE, &user).body(Body::empty()).unwrap()).await;
            assert_eq!(s, StatusCode::BAD_GATEWAY, "GET {p} for anybody signed in gets as far as Docker: {b}");
        }
    }

    async fn user_csrf(a: &App) -> String {
        a.db.call(|c| Ok(c.query_row("SELECT csrf FROM sessions WHERE user_id = 9", [], |r| r.get(0))?)).await.unwrap()
    }
}
