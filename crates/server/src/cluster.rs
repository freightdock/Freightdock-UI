//! The cluster: Freightdock as the UI of a Freightdock cluster (Freightdock-Manager, Freightdock-Worker). When
//! `FREIGHTDOCK_CLUSTER_API` names a manager's admin API and `FREIGHTDOCK_CLUSTER_TOKEN_FILE` its token, the routes
//! under `/api/cluster` ask that manager: the cluster, its managers, its nodes and their health, events, commands and
//! join tokens. The manager holds every bit of the cluster's state; Freightdock keeps none of it ("Cluster" 1).
//!
//! The manager's admin API is on its own host's loopback (`http://127.0.0.1:7451`, with the admin token), or on the
//! network over TLS (`https://<manager>:7452`, with an API token): then several managers may be named, comma
//! separated, and a call goes to the next when one does not answer; the managers' certificate is checked against the
//! cluster CA (`FREIGHTDOCK_CLUSTER_CA_FILE`) for the managers' fixed name ("Cluster" 8). Reading needs a session; a
//! command or a join token, an administrator, and it goes to the activity log ("Cluster" 2).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Authed;
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The name every manager's API certificate is for (Freightdock-Contracts' `MANAGER_SERVER_NAME`).
const MANAGER_NAME: &str = "manager.freightdock.internal";

/// One manager's API.
pub struct Endpoint {
    /// As given, for the log.
    pub given: String,
    /// What a request's URL starts with: the address itself over HTTP, the managers' name over HTTPS.
    pub base: String,
    http: reqwest::Client,
}

/// A token or a certificate: in a file (read when needed), or itself in the environment, as the managers start the
/// UI they run (G8.5). A value is never shown in a log.
#[derive(Clone, PartialEq)]
pub enum Given {
    File(PathBuf),
    Value(String),
}

impl std::fmt::Debug for Given {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Given::File(p) => write!(f, "File({})", p.display()),
            Given::Value(_) => f.write_str("Value(..)"),
        }
    }
}

impl From<PathBuf> for Given {
    fn from(p: PathBuf) -> Self {
        Given::File(p)
    }
}

/// Where the managers are and how Freightdock signs in to them.
pub struct Cluster {
    /// The managers' APIs as given, comma separated.
    pub api: String,
    /// The admin token (`<manager data>/admin.token`) or an API token; a file is read at each call, so a new one is
    /// taken at once.
    pub token: Given,
    pub endpoints: Vec<Endpoint>,
    /// The endpoint that answered last, tried first.
    current: AtomicUsize,
}

fn endpoint(given: &str, ca: Option<&reqwest::Certificate>) -> anyhow::Result<Endpoint> {
    let b = reqwest::Client::builder().timeout(Duration::from_secs(15)).connect_timeout(Duration::from_secs(3)).redirect(reqwest::redirect::Policy::none());
    if given.starts_with("http://") {
        return Ok(Endpoint { given: given.into(), base: given.into(), http: b.build()? });
    }
    anyhow::ensure!(given.starts_with("https://"), "FREIGHTDOCK_CLUSTER_API is an address like http://127.0.0.1:7451 or https://manager:7452, not \"{given}\"");
    let ca = ca.ok_or_else(|| anyhow::anyhow!("FREIGHTDOCK_CLUSTER_CA_FILE is needed for {given}: the cluster CA's certificate (`freightdock-manager ca`)"))?;
    let url = reqwest::Url::parse(given)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{given} names no host"))?;
    let port = url.port().unwrap_or(7452);
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(host.trim_matches(['[', ']']), port))?.next().ok_or_else(|| anyhow::anyhow!("{host} has no address"))?;
    // The managers' certificate is for their fixed name, whatever address reaches them, and only the cluster CA counts.
    let http = b.use_rustls_tls().tls_built_in_root_certs(false).add_root_certificate(ca.clone()).https_only(true).resolve(MANAGER_NAME, addr).build()?;
    Ok(Endpoint { given: given.into(), base: format!("https://{MANAGER_NAME}:{port}"), http })
}

impl Cluster {
    pub fn new(api: &str, token: impl Into<Given>, ca: Option<Given>) -> anyhow::Result<Self> {
        let token = token.into();
        let ca = match &ca {
            Some(Given::File(f)) => Some(reqwest::Certificate::from_pem(&std::fs::read(f).map_err(|e| anyhow::anyhow!("FREIGHTDOCK_CLUSTER_CA_FILE {}: {e}", f.display()))?)?),
            Some(Given::Value(pem)) => {
                anyhow::ensure!(pem.contains("-----BEGIN CERTIFICATE-----"), "FREIGHTDOCK_CLUSTER_CA is the cluster CA's certificate (PEM)");
                Some(reqwest::Certificate::from_pem(pem.as_bytes())?)
            }
            None => None,
        };
        let endpoints = api.split(',').map(|a| a.trim().trim_end_matches('/')).filter(|a| !a.is_empty()).map(|a| endpoint(a, ca.as_ref())).collect::<anyhow::Result<Vec<_>>>()?;
        anyhow::ensure!(!endpoints.is_empty(), "FREIGHTDOCK_CLUSTER_API names no manager");
        let api = endpoints.iter().map(|e| e.given.as_str()).collect::<Vec<_>>().join(",");
        Ok(Cluster { api, token, endpoints, current: AtomicUsize::new(0) })
    }

    /// One call to a manager: the one that answered last, else the next. Only a connection that could not be made
    /// goes on to the next, so a change is never sent twice. Its answer comes back as it is; its refusals as the same
    /// status with its sentence.
    /// A WebSocket to the manager (`path`, a GET), opened with the browser's own key so the manager's answer is the
    /// browser's: the manager's 101 with its headers, and the upgraded connection to splice to the browser's.
    pub(crate) async fn upgrade(&self, path: &str, key: &str) -> ApiResult<(reqwest::header::HeaderMap, reqwest::Upgraded)> {
        let token = self.token_now().await?;
        let n = self.endpoints.len();
        let at = self.current.load(Ordering::Relaxed) % n;
        let e = &self.endpoints[at];
        let r = e.http.get(format!("{}{path}", e.base)).bearer_auth(token.trim())
            .header(reqwest::header::CONNECTION, "Upgrade").header(reqwest::header::UPGRADE, "websocket")
            .header("sec-websocket-version", "13").header("sec-websocket-key", key)
            .send().await.map_err(|err| {
                tracing::warn!("the cluster manager at {}: {err}", e.given);
                ApiError::new(StatusCode::BAD_GATEWAY, "the cluster's manager does not answer")
            })?;
        if r.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let said = r.json::<Value>().await.ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| format!("the manager answered {status}"));
            return Err(ApiError::new(if status == StatusCode::UNAUTHORIZED { StatusCode::BAD_GATEWAY } else { status }, said));
        }
        let headers = r.headers().clone();
        let up = r.upgrade().await.map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, format!("the manager's shell: {err}")))?;
        Ok((headers, up))
    }

    async fn token_now(&self) -> ApiResult<String> {
        Ok(match &self.token {
            Given::Value(t) => t.clone(),
            Given::File(f) => tokio::fs::read_to_string(f).await.map_err(|e| {
                tracing::warn!("the cluster manager's token in {}: {e}", f.display());
                ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Freightdock cannot read the manager's token")
            })?,
        })
    }

    pub(crate) async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> ApiResult<(StatusCode, Value)> {
        let (status, bytes) = self.call_bytes(method, path, body).await?;
        Ok((status, if bytes.trim_ascii().is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) }))
    }

    /// As `call`, the answer as bytes: a backup is not JSON.
    pub(crate) async fn call_bytes(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> ApiResult<(StatusCode, Vec<u8>)> {
        self.send(method, path, body.map(Payload::Json)).await
    }

    /// As `call`, with a body of raw bytes (a release's binary).
    pub(crate) async fn call_raw(&self, method: reqwest::Method, path: &str, bytes: axum::body::Bytes) -> ApiResult<(StatusCode, Value)> {
        let (status, bytes) = self.send(method, path, Some(Payload::Raw(bytes))).await?;
        Ok((status, if bytes.trim_ascii().is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) }))
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Payload>) -> ApiResult<(StatusCode, Vec<u8>)> {
        let token = self.token_now().await?;
        let n = self.endpoints.len();
        let first = self.current.load(Ordering::Relaxed) % n;
        let mut answered = None;
        for i in 0..n {
            let at = (first + i) % n;
            let e = &self.endpoints[at];
            let mut req = e.http.request(method.clone(), format!("{}{path}", e.base)).bearer_auth(token.trim());
            match &body {
                Some(Payload::Json(b)) => req = req.json(b),
                Some(Payload::Raw(b)) => req = req.header(reqwest::header::CONTENT_TYPE, "application/octet-stream").body(b.clone()),
                None => {}
            }
            match req.send().await {
                Ok(r) => {
                    self.current.store(at, Ordering::Relaxed);
                    answered = Some(r);
                    break;
                }
                Err(err) if err.is_connect() && i + 1 < n => tracing::warn!("the cluster manager at {}: {err}; trying the next", e.given),
                Err(err) => {
                    tracing::warn!("the cluster manager at {}: {err}", e.given);
                    break;
                }
            }
        }
        let r = answered.ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, "the cluster's manager does not answer"))?;
        let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let bytes = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            // The manager's sentence, for a person; a 401 from it is Freightdock's token, not the person's sign-in.
            // The manager answers `{"error": "..."}`; its sentence is what a person reads.
            let said = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| text.trim().to_string());
            let message = if status == StatusCode::UNAUTHORIZED { "the manager refused Freightdock's token".to_string() } else if said.is_empty() { format!("the manager answered {status}") } else { said };
            let status = if status == StatusCode::UNAUTHORIZED { StatusCode::BAD_GATEWAY } else { status };
            return Err(ApiError::new(status, message));
        }
        Ok((status, bytes))
    }
}

/// What a call to the managers carries.
enum Payload {
    Json(Value),
    Raw(axum::body::Bytes),
}

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/cluster", get(overview))
        .route("/api/cluster/nodes", get(nodes))
        .route("/api/cluster/nodes/{id}", get(node).delete(remove_node))
        .route("/api/cluster/nodes/{id}/name", axum::routing::put(rename_node))
        .route("/api/cluster/managers/{id}/name", axum::routing::put(rename_manager))
        .route("/api/cluster/name", axum::routing::put(rename_cluster))
        .route("/api/cluster/nodes/{id}/commands", post(command))
        .route("/api/cluster/nodes/{id}/labels", axum::routing::put(set_labels))
        .route("/api/cluster/nodes/{id}/authorize", post(authorize))
        .route("/api/cluster/nodes/{id}/docker", get(docker_read).post(docker_change))
        .route("/api/cluster/grants", get(crate::grants::list).post(crate::grants::add))
        .route("/api/cluster/grants/{id}", axum::routing::delete(crate::grants::remove))
        .route("/api/cluster/nodes/{id}/shell", get(shell))
        .route("/api/cluster/commands/{id}", get(command_status))
        .route("/api/cluster/events", get(events))
        .route("/api/cluster/recoveries", get(recoveries))
        .route("/api/cluster/join-tokens", post(join_token))
        .route("/api/cluster/stacks", get(stacks).post(deploy_stack))
        .route("/api/cluster/stack-check", post(check_stack))
        .route("/api/cluster/stacks/{name}", get(stack).delete(remove_stack))
        .route("/api/cluster/stacks/{name}/{action}", post(stack_action))
        .route("/api/cluster/stacks/{name}/exposures", axum::routing::put(set_exposures))
        .route("/api/cluster/floating", get(floating).post(add_floating))
        .route("/api/cluster/traffic", get(traffic))
        .route("/api/cluster/floating/{address}", axum::routing::delete(remove_floating))
        .route("/api/cluster/storage", get(storage).post(add_storage))
        .route("/api/cluster/usage", get(usage))
        .route("/api/cluster/storage/{name}", axum::routing::delete(remove_storage))
        .route("/api/cluster/storage/{name}/keys", axum::routing::put(set_storage_keys))
        .route("/api/cluster/storage/{name}/password", axum::routing::put(set_storage_password))
        .route("/api/cluster/ceph/setup", get(ceph_setup).post(start_ceph_setup).delete(clear_ceph_setup))
        .route("/api/cluster/ceph/setup/retry", post(retry_ceph_setup))
        .route("/api/cluster/release", get(release))
        .route("/api/cluster/release/fetch", post(fetch_release))
        // A binary is larger than any other body Freightdock takes (the managers' own limit).
        .route("/api/cluster/release/{part}", axum::routing::put(upload_release).layer(axum::extract::DefaultBodyLimit::max(RELEASE_MAX)))
        .route("/api/cluster/upgrade", post(start_upgrade))
        .route("/api/cluster/upgrade/stop", post(stop_upgrade))
        .route("/api/cluster/settings", get(settings).put(set_settings))
        .route("/api/cluster/settings/proxmox/fingerprint", post(proxmox_fingerprint))
        .route("/api/cluster/settings/proxmox/test", post(proxmox_test))
        .route("/api/cluster/settings/redfish/fingerprint", post(redfish_fingerprint))
        .route("/api/cluster/settings/redfish/test", post(redfish_test))
        .route("/api/cluster/backup", post(take_backup))
        .route("/api/cluster/registries", get(registries))
        .route("/api/cluster/registries/{host}", axum::routing::put(set_registry).delete(remove_registry))
        .route("/api/cluster/registries/{host}/test", post(test_registry))
        .route("/api/cluster/notifications", get(notifications))
        .route("/api/cluster/notifications/{name}", axum::routing::put(set_channel).delete(remove_channel))
        .route("/api/cluster/notifications/{name}/test", post(test_channel))
        .route("/api/cluster/backups", get(backups))
        .route("/api/cluster/backups/{name}", get(backup_file))
}

pub(crate) fn cluster(app: &App) -> ApiResult<&Cluster> {
    app.cluster.get().map(|c| c.as_ref()).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "this Freightdock is not connected to a cluster"))
}

/// Whether there is a cluster, and when there is, the manager's view of it.
async fn overview(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    let Some(c) = app.cluster.get() else { return Ok(Json(json!({ "connected": false }))) };
    let (_, mut v) = c.call(reqwest::Method::GET, "/v1/cluster", None).await?;
    let names = names_in(&app, "manager_names").await?;
    for m in v["raft"].as_array_mut().into_iter().flatten() {
        if let Some(name) = m["raft_id"].as_str().and_then(|id| names.get(id)) {
            m["name"] = json!(name);
        }
    }
    // The cluster's name, given here by its id ("Cluster" 18).
    let name = match v["cluster_id"].as_str() {
        Some(id) => names_in(&app, "cluster_names").await?.remove(id),
        None => None,
    };
    Ok(Json(json!({ "connected": true, "cluster": v, "name": name })))
}

async fn nodes(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    let mut v = cluster(&app)?.call(reqwest::Method::GET, "/v1/nodes", None).await?.1;
    let names = node_names(&app).await?;
    for n in v.as_array_mut().into_iter().flatten() {
        if let Some(name) = n["id"].as_str().and_then(|id| names.get(id)) {
            n["name"] = json!(name);
        }
    }
    Ok(Json(v))
}

/// One node, with the containers it reports and the name given it here.
async fn node(State(app): State<App>, _a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let mut v = cluster(&app)?.call(reqwest::Method::GET, &format!("/v1/nodes/{id}"), None).await?.1;
    if let Some(name) = node_names(&app).await?.remove(&id) {
        v["name"] = json!(name);
    }
    Ok(Json(v))
}

/// The names nodes were given in Freightdock ("Cluster" 12), by node id.
async fn node_names(app: &App) -> ApiResult<std::collections::HashMap<String, String>> {
    names_in(app, "node_names").await
}

/// A table of names given here: `node_names` by node id or `manager_names` by raft id (its first column).
async fn names_in(app: &App, table: &'static str) -> ApiResult<std::collections::HashMap<String, String>> {
    Ok(app.db.call(move |c| Ok(c.prepare(&format!("SELECT * FROM {table}"))?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await?)
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

/// Gives a node a name to be shown by ("Cluster" 12), and the managers the same one as its `name` label, so their
/// notifications, CLI and metrics call it so too ("Cluster" 30); its other labels are kept. An empty name takes both
/// away. The managers are told first: a name they refuse is not given here either.
async fn rename_node(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let name = b.name.trim().to_string();
    if name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("A name is at most 64 characters, on one line.").field("name"));
    }
    let c = cluster(&app)?;
    let (_, node) = c.call(reqwest::Method::GET, &format!("/v1/nodes/{id}"), None).await?;
    let mut labels: std::collections::BTreeMap<String, String> = serde_json::from_value(node["labels"].clone()).unwrap_or_default();
    if name.is_empty() {
        labels.remove("name");
    } else {
        labels.insert("name".into(), name.clone());
    }
    c.call(reqwest::Method::PUT, &format!("/v1/nodes/{id}/labels"), Some(json!({ "labels": labels }))).await?;
    give_name(&app, a, "node_names", "node_id", "cluster.node.rename", id, name).await
}

/// The same for a manager, by its raft id (a number): shown instead of its address ("Cluster" 14).
/// Gives the cluster a name to be shown by, here only; an empty name takes it away.
async fn rename_cluster(State(app): State<App>, a: Authed, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    let (_, v) = cluster(&app)?.call(reqwest::Method::GET, "/v1/cluster", None).await?;
    let Some(id) = v["cluster_id"].as_str().map(str::to_string) else { return Err(ApiError::not_found("cluster")) };
    give_name(&app, a, "cluster_names", "cluster_id", "cluster.rename", id, b.name).await
}

async fn rename_manager(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    if id.is_empty() || id.len() > 20 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::not_found("manager"));
    }
    give_name(&app, a, "manager_names", "raft_id", "cluster.manager.rename", id, b.name).await
}

async fn give_name(app: &App, a: Authed, table: &'static str, key: &'static str, action: &'static str, id: String, name: String) -> ApiResult<Json<Value>> {
    let name = name.trim().to_string();
    if name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("A name is at most 64 characters, on one line.").field("name"));
    }
    let (k, n) = (id.clone(), name.clone());
    app.db
        .call(move |c| {
            if n.is_empty() {
                c.execute(&format!("DELETE FROM {table} WHERE {key} = ?1"), [&k])?;
            } else {
                c.execute(&format!("INSERT INTO {table} ({key}, name) VALUES (?1, ?2) ON CONFLICT ({key}) DO UPDATE SET name = excluded.name"), [&k, &n])?;
            }
            Ok(())
        })
        .await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action, target: Some(id.clone()), detail: Some(name.clone()) }).await;
    Ok(Json(json!({ "id": id, "name": if name.is_empty() { Value::Null } else { json!(name) } })))
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<u64>,
    node: Option<String>,
}

async fn events(State(app): State<App>, _a: Authed, Query(q): Query<EventsQuery>) -> ApiResult<Json<Value>> {
    let mut path = "/v1/events".to_string();
    let mut sep = '?';
    if let Some(a) = q.after {
        path.push_str(&format!("{sep}after={a}"));
        sep = '&';
    }
    if let Some(n) = q.node.filter(|n| valid_id(n)) {
        path.push_str(&format!("{sep}node={n}"));
    }
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, &path, None).await?.1))
}

/// A node's or a command's id, as the manager makes them (UUIDs): nothing else goes into the manager's path.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn id_or_404(id: &str, what: &str) -> ApiResult<()> {
    if valid_id(id) { Ok(()) } else { Err(ApiError::not_found(what)) }
}

/// The keys a look at a node's Docker passes on to the manager, in this order; any other is dropped.
const DOCKER_KEYS: [&str; 4] = ["action", "target", "tail", "timestamps"];

/// A look at the node's Docker, through its worker ("Cluster" 25): an Administrator's, since a container's inspect and
/// logs may hold its secrets.
async fn docker_read(State(app): State<App>, _a: Authed, Path(id): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let query = {
        let mut url = url::form_urlencoded::Serializer::new(String::new());
        for k in DOCKER_KEYS {
            if let Some(v) = q.get(k) {
                url.append_pair(k, v);
            }
        }
        url.finish()
    };
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, &format!("/v1/nodes/{id}/docker?{query}"), None).await?.1))
}

#[derive(Deserialize)]
struct ShellQuery {
    #[serde(default)]
    container: String,
    #[serde(default)]
    cols: u32,
    #[serde(default)]
    rows: u32,
}

/// A shell on a node or in one of its containers ("Cluster" 35). Asked without an upgrade, only whether it may be
/// opened (204, or the manager's why not: a browser is never told why a WebSocket failed). Asked as a WebSocket from
/// this origin, the browser's handshake goes to the manager and, once it switches, the two connections are spliced:
/// Freightdock passes the bytes and keeps none. Opening one is on record; nothing typed is.
async fn shell(State(app): State<App>, a: Authed, peer: crate::api::Peer, Path(id): Path<String>, Query(q): Query<ShellQuery>, req: axum::extract::Request) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    id_or_404(&id, "node")?;
    crate::signin::confirm::recent(&a)?;
    let c = q.container.trim();
    if !(c.is_empty() || (c.len() <= 128 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || "_.-".contains(ch)) && !c.starts_with(['.', '-']))) {
        return Err(ApiError::not_found("container"));
    }
    let h = req.headers();
    let upgrading = h.get(axum::http::header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !upgrading {
        cluster(&app)?.call(reqwest::Method::GET, &format!("/v1/nodes/{id}/shell?container={c}&check=1"), None).await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    // A WebSocket is not held to CORS: only a page of this origin may open one, and it must say where it is from.
    if !h.contains_key(axum::http::header::ORIGIN) || !peer.same_origin {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "a shell is opened from Freightdock's own pages only"));
    }
    let key = h.get("sec-websocket-key").and_then(|v| v.to_str().ok()).filter(|k| k.len() <= 64).ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "a WebSocket's key is needed"))?.to_string();
    let path = format!("/v1/nodes/{id}/shell?container={c}&cols={}&rows={}", q.cols.min(1000), q.rows.min(1000));
    let (headers, manager) = cluster(&app)?.upgrade(&path, &key).await?;
    let on = if c.is_empty() { "the host".to_string() } else { format!("container {c}") };
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action: "cluster.shell", target: Some(id.clone()), detail: Some(on) }).await;
    let browser = hyper::upgrade::on(req);
    tokio::spawn(async move {
        match browser.await {
            Ok(b) => {
                let mut b = hyper_util::rt::TokioIo::new(b);
                let mut m = manager;
                let _ = tokio::io::copy_bidirectional(&mut b, &mut m).await;
            }
            Err(e) => tracing::warn!(node = %id, "the browser's side of a shell: {e}"),
        }
    });
    let mut res = axum::response::Response::builder().status(StatusCode::SWITCHING_PROTOCOLS).header(axum::http::header::CONNECTION, "Upgrade").header(axum::http::header::UPGRADE, "websocket");
    if let Some(accept) = headers.get("sec-websocket-accept").and_then(|v| v.to_str().ok()) {
        res = res.header("sec-websocket-accept", accept);
    }
    res.body(axum::body::Body::empty()).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// A change to the node's Docker (the worker refuses the cluster's own); on record with what and on what.
async fn docker_change(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let action = b["action"].as_str().unwrap_or_default().to_string();
    // Granted the node, not an administrator: its containers started, stopped and restarted, nothing more ("Access" 24).
    if !a.session.user.is_admin() && !crate::grants::NODE_CHANGES.contains(&action.as_str()) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Only an administrator may do that on a node."));
    }
    let on = b["target"].as_str().filter(|t| !t.is_empty()).or_else(|| b["params"]["image"].as_str()).unwrap_or_default().to_string();
    let body = json!({ "action": action, "target": b["target"].as_str().unwrap_or_default(), "params": b.get("params").cloned().unwrap_or(json!({})) });
    let detail = if on.is_empty() { action } else { format!("{action} {on}") };
    cluster(&app)?;
    // In a task of its own: a browser that leaves before the answer does not leave a change done and not on record.
    let task = tokio::spawn(async move {
        let (_, v) = cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/nodes/{id}/docker"), Some(body)).await?;
        audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(detail), ..audit::entry("cluster.docker", id.clone()) }).await;
        Ok::<_, ApiError>(v)
    });
    Ok(Json(task.await.map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??))
}

async fn command(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    id_or_404(&id, "node")?;
    let op = body.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/nodes/{id}/commands"), Some(body)).await?;
    let detail = Some(format!("{op} ({})", v["id"].as_str().unwrap_or("")));
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action: "cluster.command", target: Some(id), detail }).await;
    Ok((s, Json(v)))
}

/// Lets a node that came back fenced run workloads again ("Cluster" 6).
async fn authorize(State(app): State<App>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let (_, v) = cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/nodes/{id}/authorize"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.node.authorize", id)).await;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct LabelsBody {
    labels: std::collections::BTreeMap<String, String>,
}

/// The node's labels, all of them; the manager checks them ("Cluster" 5).
async fn set_labels(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(b): Json<LabelsBody>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    let detail = b.labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ");
    let (_, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/nodes/{id}/labels"), Some(json!({ "labels": b.labels }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action: "cluster.node.labels", target: Some(id), detail: Some(detail) }).await;
    Ok(Json(v))
}

/// The cluster's traffic now (G9.8): each node's latest flows, their ends named by the managers.
async fn traffic(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/traffic", None).await?.1))
}

/// The cluster's shared storage (G11), each with the volumes on it and who holds them.
/// The Storage page's numbers ("Cluster" 43): each worker's disk and each shared storage, by stack.
async fn usage(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/usage", None).await?.1))
}

async fn storage(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/storage", None).await?.1))
}

#[derive(Deserialize)]
struct StorageBody {
    name: String,
    driver: String,
    source: String,
    #[serde(default)]
    options: String,
    /// cephfs: its keys, passed on and never kept here nor put on record ("Cluster" 34).
    #[serde(flatten)]
    keys: CephKeysBody,
    /// smb: the password, passed on as typed and never kept here nor put on record ("Cluster" 37).
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize)]
struct PasswordBody {
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize, Default)]
struct CephKeysBody {
    secret: Option<String>,
    fence_client: Option<String>,
    fence_secret: Option<String>,
    image: Option<String>,
}

impl CephKeysBody {
    /// What the managers get: a key left empty is left out (the one set stays), a fence client or image as given
    /// (empty takes eviction, or the image chosen, away).
    fn into_json(self, mut body: Value) -> Value {
        let t = |s: Option<String>| s.map(|s| s.trim().to_string());
        for (k, v) in [("secret", t(self.secret)), ("fence_secret", t(self.fence_secret))] {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                body[k] = json!(v);
            }
        }
        for (k, v) in [("fence_client", t(self.fence_client)), ("image", t(self.image))] {
            if let Some(v) = v {
                body[k] = json!(v);
            }
        }
        body
    }
}

/// Storage given to the cluster; the managers check it.
async fn add_storage(State(app): State<App>, a: Authed, Json(b): Json<StorageBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = b.name.trim().to_string();
    let mut body = json!({ "name": name, "driver": b.driver.trim(), "source": b.source.trim(), "options": b.options.trim() });
    if b.driver.trim() == "cephfs" {
        body = b.keys.into_json(body);
    }
    if b.driver.trim() == "smb" && let Some(p) = b.password.filter(|p| !p.is_empty()) {
        body["password"] = json!(p);
    }
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/storage", Some(body)).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.add", name)).await;
    Ok((s, Json(v)))
}

async fn remove_storage(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<StatusCode> {
    storage_or_404(&name)?;
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/storage/{name}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.remove", name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A storage name (a-z, 0-9 and -): nothing else goes into a manager's path.
fn storage_or_404(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(()) } else { Err(ApiError::not_found("storage")) }
}

/// A CephFS storage's keys set anew (G12.6); a key left empty stays as it was. On record without any key.
async fn set_storage_keys(State(app): State<App>, a: Authed, Path(name): Path<String>, Json(b): Json<CephKeysBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    storage_or_404(&name)?;
    let (s, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/storage/{name}/keys"), Some(b.into_json(json!({})))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.keys", name)).await;
    Ok((s, Json(v)))
}

/// An SMB share's password set anew (G11.9); empty for a guest share. On record without it.
async fn set_storage_password(State(app): State<App>, a: Authed, Path(name): Path<String>, Json(b): Json<PasswordBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    storage_or_404(&name)?;
    let (s, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/storage/{name}/password"), Some(json!({ "password": b.password.unwrap_or_default() }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.password", name)).await;
    Ok((s, Json(v)))
}

/// Ceph made by the cluster (G12.6): the setup being made or the last one, each step with how it went; null for none.
/// The largest binary passed on, as the managers take it.
const RELEASE_MAX: usize = 256 << 20;

async fn release(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/release", None).await?.1))
}

#[derive(Deserialize)]
struct UploadAsk {
    version: String,
}

/// A binary of the cluster's release, passed on to the managers as it came ("Cluster" 36).
async fn upload_release(State(app): State<App>, a: Authed, Path(part): Path<String>, Query(q): Query<UploadAsk>, body: axum::body::Bytes) -> ApiResult<Json<Value>> {
    crate::signin::confirm::recent(&a)?;
    if !matches!(part.as_str(), "worker" | "manager") {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "a release has a worker and a manager binary"));
    }
    let version = q.version.trim().to_string();
    if version.is_empty() || version.len() > 32 || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "the version is x.y.z"));
    }
    let path = format!("/v1/release/{part}?version={version}");
    let size = body.len();
    let (_, v) = cluster(&app)?.call_raw(reqwest::Method::PUT, &path, body).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(format!("{part} {version}, {size} bytes")), ..audit::entry("cluster.release.upload", Some(part.clone())) }).await;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct FetchAsk {
    tag: String,
}

async fn fetch_release(State(app): State<App>, a: Authed, Json(b): Json<FetchAsk>) -> ApiResult<Json<Value>> {
    crate::signin::confirm::recent(&a)?;
    let tag = b.tag.trim().to_string();
    let (_, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/release/fetch", Some(json!({ "tag": tag }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.release.fetch", Some(tag.clone()))).await;
    Ok(Json(v))
}

async fn start_upgrade(State(app): State<App>, a: Authed) -> ApiResult<(StatusCode, Json<Value>)> {
    crate::signin::confirm::recent(&a)?;
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/upgrade", Some(json!({ "by": a.session.user.name }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(format!("{} steps", v["steps"])), ..audit::entry("cluster.upgrade.start", None) }).await;
    Ok((s, Json(v)))
}

async fn stop_upgrade(State(app): State<App>, a: Authed) -> ApiResult<StatusCode> {
    cluster(&app)?.call(reqwest::Method::POST, "/v1/upgrade/stop", Some(json!({ "by": a.session.user.name }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.upgrade.stop", None)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn ceph_setup(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/ceph/setup", None).await?.1))
}

#[derive(Deserialize)]
struct CephSetupBody {
    storage: String,
    fs: String,
    #[serde(default)]
    image: String,
    hosts: Vec<CephHostBody>,
}

#[derive(Deserialize, serde::Serialize)]
struct CephHostBody {
    node_id: String,
    disks: Vec<String>,
}

/// Starts Ceph on the nodes and disks chosen (the first bootstraps); the managers check that each disk is free.
async fn start_ceph_setup(State(app): State<App>, a: Authed, Json(b): Json<CephSetupBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (storage, fs) = (b.storage.trim().to_string(), b.fs.trim().to_string());
    let detail = format!("filesystem {fs} on {}", b.hosts.iter().map(|h| format!("{}={}", h.node_id, h.disks.join(","))).collect::<Vec<_>>().join(" "));
    let body = json!({ "storage": storage, "fs": fs, "image": b.image.trim(), "hosts": b.hosts, "by": a.session.user.name });
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/ceph/setup", Some(body)).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(detail), ..audit::entry("cluster.ceph.setup", storage) }).await;
    Ok((s, Json(v)))
}

/// The failed step run again, and on from there.
async fn retry_ceph_setup(State(app): State<App>, a: Authed) -> ApiResult<StatusCode> {
    cluster(&app)?.call(reqwest::Method::POST, "/v1/ceph/setup/retry", None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.ceph.retry", None)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// The setup forgotten; what it made (Ceph on the nodes, the storage) stays.
async fn clear_ceph_setup(State(app): State<App>, a: Authed) -> ApiResult<StatusCode> {
    cluster(&app)?.call(reqwest::Method::DELETE, "/v1/ceph/setup", None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.ceph.clear", None)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A registry as images name it (`ghcr.io`, `registry.example:5000`): nothing else goes into a manager's path.
fn registry_or_404(host: &str) -> ApiResult<()> {
    let ok = !host.is_empty() && host.len() <= 260 && !host.contains("..") && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.:".contains(&b));
    if ok { Ok(()) } else { Err(ApiError::not_found("registry")) }
}

/// The private registries the cluster has a login for ("Cluster" 26), never with a password.
async fn registries(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/registries", None).await?.1))
}

/// A registry's login, passed on whole (the password may be left out to keep the one set); on record without it.
async fn set_registry(State(app): State<App>, a: Authed, Path(host): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    registry_or_404(&host)?;
    let mut body = json!({ "username": b["username"].as_str().unwrap_or_default() });
    if let Some(p) = b["password"].as_str().filter(|p| !p.is_empty()) {
        body["password"] = json!(p);
    }
    let (_, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/registries/{host}"), Some(body)).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: b["username"].as_str().map(str::to_string), ..audit::entry("cluster.registry.set", host) }).await;
    Ok(Json(v))
}

async fn remove_registry(State(app): State<App>, a: Authed, Path(host): Path<String>) -> ApiResult<StatusCode> {
    registry_or_404(&host)?;
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/registries/{host}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.registry.remove", host)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Signs in to the registry from a manager, with the login given or the one set. Changes nothing.
async fn test_registry(State(app): State<App>, _a: Authed, Path(host): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    registry_or_404(&host)?;
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/registries/{host}/test"), Some(b)).await?.1))
}

/// A notification channel's name: nothing else goes into a manager's path.
fn channel_or_404(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok { Ok(()) } else { Err(ApiError::not_found("notification channel")) }
}

/// The notification channels ("Cluster" 27), never with a secret, and the events and kinds there are.
async fn notifications(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/notifications", None).await?.1))
}

/// A channel, passed on whole (the manager checks it); on record with its kind, never its secret.
async fn set_channel(State(app): State<App>, a: Authed, Path(name): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    channel_or_404(&name)?;
    let (_, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/notifications/{name}"), Some(b.clone())).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: b["kind"].as_str().map(str::to_string), ..audit::entry("cluster.notification.set", name) }).await;
    Ok(Json(v))
}

async fn remove_channel(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<StatusCode> {
    channel_or_404(&name)?;
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/notifications/{name}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.notification.remove", name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends a test through the channel from a manager; on record whether it went through, as it reaches outside.
async fn test_channel(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    channel_or_404(&name)?;
    let r = cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/notifications/{name}/test"), None).await;
    let outcome = if r.is_ok() { "sent" } else { "failed" };
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(outcome.into()), ..audit::entry("cluster.notification.test", name) }).await;
    Ok(Json(r?.1))
}

/// The cluster's settings (Integrations): each value, where it comes from, and what each manager's environment sets.
async fn settings(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/settings", None).await?.1))
}

/// Changes some of the cluster's settings: passed on whole, the manager checks them. The log names the settings,
/// never a value (the Proxmox token is one).
async fn set_settings(State(app): State<App>, a: Authed, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let Some(keys) = b.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")) else { return Err(ApiError::bad("the settings are a JSON object")) };
    let (_, v) = cluster(&app)?.call(reqwest::Method::PUT, "/v1/settings", Some(b)).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::Entry { detail: Some(keys), ..audit::entry("cluster.settings.change", "settings".to_string()) }).await;
    Ok(Json(v))
}

/// A backup as a download: never cached, never kept by Freightdock.
fn download(name: &str, bytes: Vec<u8>) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    ([(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")), (header::CACHE_CONTROL, "no-store".to_string())], bytes).into_response()
}

/// A backup's file name as the managers make it: nothing else goes into a manager's path.
fn backup_name(name: &str) -> bool {
    name.starts_with("freightdock-") && name.ends_with(".fdbk") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') && !name.contains("..")
}

/// A backup of the cluster now, sealed by a manager with the passphrase given. The log says it was taken, never
/// with what.
async fn take_backup(State(app): State<App>, a: Authed, Json(b): Json<Value>) -> ApiResult<axum::response::Response> {
    let passphrase = b["passphrase"].as_str().ok_or_else(|| ApiError::bad("a backup needs a passphrase"))?;
    let (_, bytes) = cluster(&app)?.call_bytes(reqwest::Method::POST, "/v1/backup", Some(json!({ "passphrase": passphrase }))).await?;
    let t = time::OffsetDateTime::now_utc();
    let name = format!("freightdock-{:04}{:02}{:02}-{:02}{:02}{:02}.fdbk", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second());
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.backup.download", "now".to_string())).await;
    Ok(download(&name, bytes))
}

/// The scheduled backups of the manager that answers.
async fn backups(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/backups", None).await?.1))
}

async fn backup_file(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<axum::response::Response> {
    if !backup_name(&name) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such backup"));
    }
    let (_, bytes) = cluster(&app)?.call_bytes(reqwest::Method::GET, &format!("/v1/backups/{name}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.backup.download", name.clone())).await;
    Ok(download(&name, bytes))
}

/// The certificate the Proxmox API shows, read by a manager (it is the one that will talk to Proxmox).
async fn proxmox_fingerprint(State(app): State<App>, _a: Authed, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let url = b["url"].as_str().unwrap_or("").to_string();
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, "/v1/settings/proxmox/fingerprint", Some(json!({ "url": url }))).await?.1))
}

/// Tries the Proxmox provider from a manager: the settings given, or the ones in use. Changes nothing.
async fn proxmox_test(State(app): State<App>, _a: Authed, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, "/v1/settings/proxmox/test", Some(b)).await?.1))
}

/// The certificate a node's BMC shows, read by a manager (the managers are the ones that will talk to it).
async fn redfish_fingerprint(State(app): State<App>, _a: Authed, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let host = b["host"].as_str().unwrap_or("").to_string();
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, "/v1/settings/redfish/fingerprint", Some(json!({ "host": host }))).await?.1))
}

/// Tries the Redfish account from a manager against each redfish node's BMC: the account given, or the one in use.
/// Changes nothing.
async fn redfish_test(State(app): State<App>, _a: Authed, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, "/v1/settings/redfish/test", Some(b)).await?.1))
}

/// The floating addresses (G10.3) and each one's holder.
async fn floating(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/floating", None).await?.1))
}

#[derive(Deserialize)]
struct FloatingBody {
    address: String,
}

/// One address or a range of them; answers how many were new, as the manager counted.
async fn add_floating(State(app): State<App>, a: Authed, Json(b): Json<FloatingBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let address = b.address.trim().to_string();
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/floating", Some(json!({ "address": address }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.floating.add", address)).await;
    Ok((s, Json(v)))
}

async fn remove_floating(State(app): State<App>, a: Authed, Path(address): Path<String>) -> ApiResult<StatusCode> {
    // Only an IPv4 address goes into the manager's path.
    let Ok(ip) = address.parse::<std::net::Ipv4Addr>() else { return Err(ApiError::not_found("floating address")) };
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/floating/{ip}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.floating.remove", ip.to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn command_status(State(app): State<App>, _a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "command")?;
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, &format!("/v1/commands/{id}"), None).await?.1))
}

async fn remove_node(State(app): State<App>, a: Authed, Path(id): Path<String>) -> ApiResult<StatusCode> {
    id_or_404(&id, "node")?;
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/nodes/{id}"), None).await?;
    crate::grants::forget(&app, "node", &id).await;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.node.remove", id)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TokenBody {
    /// `worker` (the default) or `manager`.
    kind: Option<String>,
    ttl_s: Option<i64>,
}

/// A join token, to add a worker or a manager. Shown once, to the administrator who asked; never logged.
async fn join_token(State(app): State<App>, a: Authed, Json(b): Json<TokenBody>) -> ApiResult<Json<Value>> {
    let (path, kind) = match b.kind.as_deref().unwrap_or("worker") {
        "worker" => ("/v1/join-tokens", "worker"),
        "manager" => ("/v1/manager-tokens", "manager"),
        _ => return Err(ApiError::bad("a join token is for a worker or a manager").field("kind")),
    };
    let (_, v) = cluster(&app)?.call(reqwest::Method::POST, path, Some(json!({ "ttl_s": b.ttl_s }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.join_token", kind.to_string())).await;
    Ok(Json(v))
}

/// A stack's name as the manager allows it: nothing else goes into the manager's path.
fn stack_or_404(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty() && name.len() <= 40 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(ApiError::not_found("stack")) }
}

#[derive(Deserialize)]
struct RecoveriesQuery {
    stack: Option<String>,
}

/// Workloads the managers moved off fenced nodes, or left there and why ("Cluster" 7), newest first.
async fn recoveries(State(app): State<App>, _a: Authed, Query(q): Query<RecoveriesQuery>) -> ApiResult<Json<Value>> {
    let path = match q.stack {
        Some(name) => {
            stack_or_404(&name)?;
            format!("/v1/recoveries?stack={name}")
        }
        None => "/v1/recoveries".into(),
    };
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, &path, None).await?.1))
}

async fn stacks(State(app): State<App>, _a: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, "/v1/stacks", None).await?.1))
}

async fn stack(State(app): State<App>, _a: Authed, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    stack_or_404(&name)?;
    Ok(Json(cluster(&app)?.call(reqwest::Method::GET, &format!("/v1/stacks/{name}"), None).await?.1))
}

#[derive(Deserialize)]
struct DeployBody {
    name: String,
    compose: String,
    /// The nodes chosen for it (ids; empty: any) and its exposures on the pool; each left out stays as it was.
    #[serde(default)]
    nodes: Option<Vec<String>>,
    #[serde(default)]
    exposures: Option<Value>,
    /// The stack's options ("Cluster" 22): each passed on as given (`mode` null too), each left out kept as it was.
    #[serde(flatten)]
    rest: serde_json::Map<String, Value>,
}

/// The options a deploy may give beside its file; `stopped` makes it without starting it ("Cluster" 31).
const OPTIONS: [&str; 6] = ["description", "env", "selector", "mode", "names", "stopped"];

impl DeployBody {
    /// As the manager takes it, with only what was given.
    fn to_manager(&self) -> Value {
        let mut v = json!({ "name": self.name.trim(), "compose": self.compose });
        for k in OPTIONS {
            if let Some(x) = self.rest.get(k) {
                v[k] = x.clone();
            }
        }
        if let Some(n) = &self.nodes {
            v["nodes"] = json!(n);
        }
        if let Some(e) = &self.exposures {
            v["exposures"] = e.clone();
        }
        v
    }
}

/// What a deploy would do, changing nothing: the services and the ports their file asks for, the warnings, where the
/// workloads would go or why not ("Cluster" 19).
async fn check_stack(State(app): State<App>, _a: Authed, Json(b): Json<DeployBody>) -> ApiResult<Json<Value>> {
    Ok(Json(cluster(&app)?.call(reqwest::Method::POST, "/v1/stack-check", Some(b.to_manager())).await?.1))
}

/// Makes or changes a stack from a Compose file, on the nodes chosen for it and with its exposures; the manager reads
/// the file and places the workloads ("Cluster" 4, 19).
async fn deploy_stack(State(app): State<App>, a: Authed, Json(b): Json<DeployBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = b.name.trim().to_string();
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/stacks", Some(b.to_manager())).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.stack.deploy", name)).await;
    Ok((s, Json(v)))
}

async fn remove_stack(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<StatusCode> {
    stack_or_404(&name)?;
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/stacks/{name}"), None).await?;
    crate::grants::forget(&app, "stack", &name).await;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.stack.remove", name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What a stack exposes on the pool's floating addresses (G10.5), all of it; the managers check it.
async fn set_exposures(State(app): State<App>, a: Authed, Path(name): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    stack_or_404(&name)?;
    let list = b.get("exposures").cloned().unwrap_or_else(|| json!([]));
    let detail = list.as_array().into_iter().flatten().map(|e| format!("{}:{} to {}:{}/{}", e["address"].as_str().unwrap_or(""), e["port"], e["service"].as_str().unwrap_or(""), e["container_port"], e["protocol"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ");
    let (_, v) = cluster(&app)?.call(reqwest::Method::PUT, &format!("/v1/stacks/{name}/exposures"), Some(json!({ "exposures": list }))).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, Entry { action: "cluster.stack.expose", target: Some(name), detail: Some(if detail.is_empty() { "nothing".into() } else { detail }) }).await;
    Ok(Json(v))
}

async fn stack_action(State(app): State<App>, a: Authed, Path((name, action)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    stack_or_404(&name)?;
    let what = match action.as_str() {
        "start" => "cluster.stack.start",
        "stop" => "cluster.stack.stop",
        "restart" => "cluster.stack.restart",
        _ => return Err(ApiError::not_found("action")),
    };
    let (_, v) = cluster(&app)?.call(reqwest::Method::POST, &format!("/v1/stacks/{name}/{action}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry(what, name)).await;
    Ok(Json(v))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};

    use super::*;
    use crate::api::tests::{app, call, json_body, post, signed_in};

    /// A stand-in for a manager's admin API: it wants the token `t0k3n` and keeps what it was asked.
    async fn fake_manager() -> (String, Arc<Mutex<Vec<String>>>) {
        let asked: Arc<Mutex<Vec<String>>> = Default::default();
        let seen = asked.clone();
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let seen = seen.clone();
            async move {
                let ok = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some("Bearer t0k3n");
                let line = format!("{} {}", req.method(), req.uri());
                seen.lock().unwrap().push(line.clone());
                if !ok {
                    return (StatusCode::UNAUTHORIZED, "no".to_string());
                }
                // What was sent, given back by the two that echo it.
                let ct = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let raw = axum::body::to_bytes(req.into_body(), 8 << 20).await.unwrap_or_default();
                let got: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                // A binary is kept as its size and type, for a test to read.
                if line.starts_with("PUT /v1/release/") {
                    *seen.lock().unwrap().last_mut().unwrap() = format!("{line} {} bytes {ct}", raw.len());
                }
                // Labels are kept with what was sent, for a test to read.
                if line.ends_with("/labels") {
                    *seen.lock().unwrap().last_mut().unwrap() = format!("{line} {got}");
                }
                // A slow change, for a caller that goes before the answer.
                if got["action"] == "container.restart" {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                match line.as_str() {
                    "GET /v1/cluster" => (StatusCode::OK, json!({ "cluster_id": "c1", "raft": [{ "raft_id": "5271529257891035005", "addr": "192.0.2.1:7450", "leader": true }] }).to_string()),
                    "GET /v1/nodes" => (StatusCode::OK, json!([{ "id": "6f1c7c2e-0000-4000-8000-000000000001", "liveness": { "health": "healthy" } }]).to_string()),
                    "GET /v1/events?after=4" => (StatusCode::OK, json!([{ "seq": 5, "kind": "connected" }]).to_string()),
                    "POST /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/commands" => (StatusCode::ACCEPTED, json!({ "id": "cmd-1", "status": "pending" }).to_string()),
                    "POST /v1/join-tokens" => (StatusCode::OK, json!({ "token": "FDCT1-x" }).to_string()),
                    "PUT /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/labels" => (StatusCode::OK, json!({ "all_labels": got["labels"] }).to_string()),
                    "POST /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/authorize" => (StatusCode::OK, json!({ "fencing": { "state": "authorized" } }).to_string()),
                    "GET /v1/recoveries?stack=web" => (StatusCode::OK, json!([{ "workload_id": "web.nginx.1", "to": null, "reason": "pinned" }]).to_string()),
                    "GET /v1/stacks" => (StatusCode::OK, json!([{ "name": "web", "workloads": [] }]).to_string()),
                    "POST /v1/stacks" => (StatusCode::CREATED, json!({ "stack": { "name": "web" }, "warnings": [], "got": got }).to_string()),
                    "POST /v1/stack-check" => (StatusCode::OK, json!({ "services": { "web": { "ports": [{ "host_port": 8080, "container_port": 80 }] } }, "got": got }).to_string()),
                    "POST /v1/stacks/web/stop" => (StatusCode::CONFLICT, json!({ "error": "the cluster says no" }).to_string()),
                    "POST /v1/stacks/web/restart" => (StatusCode::OK, json!({ "restarting": 1, "busy": 0 }).to_string()),
                    "DELETE /v1/stacks/web" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001" => (StatusCode::OK, json!({ "id": "6f1c7c2e-0000-4000-8000-000000000001", "hostname": "docker", "labels": { "zone": "a", "name": "old" }, "containers": [] }).to_string()),
                    "PUT /v1/stacks/web/exposures" => (StatusCode::OK, json!({ "name": "web", "exposures": [{ "service": "nginx", "container_port": 80, "protocol": "tcp", "address": "192.0.2.10", "port": 443 }] }).to_string()),
                    "GET /v1/traffic" => (StatusCode::OK, json!({ "reporting": ["6f1c7c2e-0000-4000-8000-000000000001"], "workloads": [], "flows": [{ "from": { "kind": "outside" }, "to": { "kind": "workload", "id": "web.nginx.1" }, "port": 80, "out_bps": 10 }] }).to_string()),
                    "GET /v1/floating" => (StatusCode::OK, json!([{ "address": "192.0.2.10", "holder": "6f1c7c2e-0000-4000-8000-000000000001", "since": 1 }]).to_string()),
                    "POST /v1/floating" => (StatusCode::CREATED, json!({ "added": 3 }).to_string()),
                    "DELETE /v1/floating/192.0.2.10" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/storage" => (StatusCode::OK, json!([{ "name": "nas", "driver": "nfs", "source": "192.0.2.50:/export", "volumes": [] }]).to_string()),
                    "POST /v1/storage" => (StatusCode::CREATED, json!({ "got": got }).to_string()),
                    "DELETE /v1/storage/nas" => (StatusCode::NO_CONTENT, String::new()),
                    "PUT /v1/storage/ceph/keys" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "PUT /v1/storage/files/password" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "GET /v1/release" => (StatusCode::OK, json!({ "release": null, "upgrade": null, "nodes": [], "managers": [], "version": "1.0.0" }).to_string()),
                    "PUT /v1/release/worker?version=1.1.0" => (StatusCode::OK, json!({ "release": { "version": "1.1.0" } }).to_string()),
                    "POST /v1/release/fetch" => (StatusCode::OK, json!({ "release": { "version": "1.1.0" }, "got": got }).to_string()),
                    "POST /v1/upgrade" => (StatusCode::ACCEPTED, json!({ "steps": 4, "got": got }).to_string()),
                    "POST /v1/upgrade/stop" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/ceph/setup" => (StatusCode::OK, json!({ "state": "running", "storage": "ceph", "fs": "fdfs", "steps": [{ "kind": "prepare", "state": "done" }] }).to_string()),
                    "POST /v1/ceph/setup" => (StatusCode::ACCEPTED, json!({ "steps": 9, "got": got }).to_string()),
                    "POST /v1/ceph/setup/retry" | "DELETE /v1/ceph/setup" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/settings" => (StatusCode::OK, json!({ "settings": { "fence_stop_s": { "value": 30, "source": "default" }, "proxmox": null }, "manager": {} }).to_string()),
                    "PUT /v1/settings" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "POST /v1/settings/proxmox/fingerprint" => (StatusCode::OK, json!({ "fingerprint": "AB:CD", "got": got }).to_string()),
                    "POST /v1/settings/redfish/fingerprint" => (StatusCode::OK, json!({ "host": "192.0.2.31:443", "fingerprint": "AB:CD", "got": got }).to_string()),
                    "POST /v1/settings/redfish/test" => (StatusCode::OK, json!({ "systems": [{ "name": "w1", "bmc": "192.0.2.31:443", "system": "/redfish/v1/Systems/1", "power": "On" }], "got": got }).to_string()),
                    "POST /v1/settings/proxmox/test" => (StatusCode::BAD_GATEWAY, json!({ "error": "the Proxmox API at https://pve.example:8006: no answer" }).to_string()),
                    "POST /v1/backup" if got["passphrase"] == "correct horse battery" => (StatusCode::OK, "FDBK1sealed".to_string()),
                    "POST /v1/backup" => (StatusCode::BAD_REQUEST, json!({ "error": "a backup's passphrase is at least 12 characters" }).to_string()),
                    "GET /v1/backups" => (StatusCode::OK, json!({ "backups": [{ "name": "freightdock-20261113-200000.fdbk", "taken_at": 1, "size": 11, "manager": "m1" }] }).to_string()),
                    "GET /v1/backups/freightdock-20261113-200000.fdbk" => (StatusCode::OK, "FDBK1stored".to_string()),
                    "GET /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/docker?action=logs&target=web&tail=50" => (StatusCode::OK, json!({ "lines": ["hi"], "cut": false }).to_string()),
                    "GET /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/shell?container=web&check=1" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/shell?container=&check=1" => (StatusCode::FORBIDDEN, json!({ "error": "the host's shell is turned off on this node (FREIGHTDOCK_WORKER_HOST_SHELL)" }).to_string()),
                    "POST /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/docker" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "GET /v1/usage" => (StatusCode::OK, json!({ "nodes": [{ "id": "n1", "name": "w1", "total_bytes": 100, "used_bytes": 40, "stacks": [{ "stack": "web", "bytes": 10 }] }], "storage": [] }).to_string()),
                    "GET /v1/registries" => (StatusCode::OK, json!([{ "host": "ghcr.io", "username": "bot", "at": 1 }]).to_string()),
                    "PUT /v1/registries/ghcr.io" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "DELETE /v1/registries/ghcr.io" => (StatusCode::NO_CONTENT, String::new()),
                    "POST /v1/registries/ghcr.io/test" => (StatusCode::OK, json!({ "ok": true, "message": "signed in", "got": got }).to_string()),
                    "GET /v1/notifications" => (StatusCode::OK, json!({ "channels": [{ "name": "ops", "kind": "ntfy", "has_secret": true }], "events": [{ "event": "node.down", "says": "a node" }], "kinds": ["webhook"] }).to_string()),
                    "PUT /v1/notifications/ops" => (StatusCode::OK, json!({ "got": got }).to_string()),
                    "DELETE /v1/notifications/ops" => (StatusCode::NO_CONTENT, String::new()),
                    "POST /v1/notifications/ops/test" => (StatusCode::BAD_GATEWAY, json!({ "error": "https://ntfy.example answered 401" }).to_string()),
                    "GET /v1/nodes/broken" => (StatusCode::SERVICE_UNAVAILABLE, "the cluster cannot take changes now".to_string()),
                    _ => (StatusCode::NOT_FOUND, "unknown".to_string()),
                }
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("http://{addr}"), asked)
    }

    pub(crate) async fn connected(token: &str) -> (App, Arc<Mutex<Vec<String>>>, tempfile::TempDir) {
        let (api, asked) = fake_manager().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("admin.token");
        std::fs::write(&file, format!("{token}\n")).unwrap();
        let app = app();
        assert!(app.cluster.set(Arc::new(Cluster::new(&api, file, None).unwrap())).is_ok());
        (app, asked, dir)
    }

    fn get(path: &str, cookie: &str) -> Request<Body> {
        Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn without_a_cluster_freightdock_says_so_and_asks_nobody() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!((s, b["connected"].clone()), (StatusCode::OK, json!(false)));
        let (s, _, _) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_signed_in_person_sees_the_cluster_its_nodes_and_events() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (s, _, _) = call(&app, Request::get("/api/cluster").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a session first");
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((b["connected"].clone(), b["cluster"]["cluster_id"].clone()), (json!(true), json!("c1")));
        let (_, _, b) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(b[0]["liveness"]["health"], "healthy");
        let (_, _, b) = call(&app, get("/api/cluster/events?after=4", &cookie)).await;
        assert_eq!(b[0]["seq"], 5);
        assert_eq!(asked.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_command_is_an_administrator_s_and_goes_to_the_activity_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let req = |cookie: &str, csrf: &str| post(&format!("/api/cluster/nodes/{node}/commands")).header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(json!({ "op": "ping" }))).unwrap();
        let (s, _, b) = call(&app, req(&cookie, &csrf)).await;
        assert_eq!(s, StatusCode::ACCEPTED);
        assert_eq!(b["id"], "cmd-1");
        let (action, target, detail): (String, String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, target, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), target.as_str(), detail.as_str()), ("cluster.command", node, "ping (cmd-1)"));

        // Not without the CSRF token, and not for somebody who is not an administrator.
        let (s, _, _) = call(&app, req(&cookie, "wrong")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let user = format!("freightdock_session={token}");
        let (_, _, me) = call(&app, get("/api/me", &user)).await;
        let (s, _, _) = call(&app, req(&user, me["csrf"].as_str().unwrap())).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(asked.lock().unwrap().iter().filter(|l| l.starts_with("POST")).count(), 1, "the manager was asked once");
    }

    #[tokio::test]
    async fn the_manager_s_refusals_come_through_and_a_bad_token_is_freightdock_s() {
        let (app, _, _d) = connected("not-it").await;
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "{b}");
        assert!(b.to_string().contains("refused Freightdock's token"), "{b}");

        let (app, _, _d) = connected("t0k3n").await;
        let (cookie, _) = signed_in(&app).await;
        // Only ids go into the manager's paths.
        let (s, _, _) = call(&app, get("/api/cluster/commands/..%2Fcluster", &cookie)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let c = app.cluster.get().unwrap();
        let e = c.call(reqwest::Method::GET, "/v1/nodes/broken", None).await.unwrap_err();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_stack_s_exposures_are_an_administrator_s_and_go_to_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let body = json!({ "exposures": [{ "service": "nginx", "container_port": 80, "protocol": "tcp", "address": "192.0.2.10", "port": 443 }] });
        let put = |name: &str| Request::put(format!("/api/cluster/stacks/{name}/exposures")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap();
        let (s, _, b) = call(&app, put("web")).await;
        assert_eq!((s, b["exposures"][0]["port"].clone()), (StatusCode::OK, json!(443)));
        assert_eq!(call(&app, put("..%2Fnodes")).await.0, StatusCode::NOT_FOUND, "only a stack's name goes into the path");
        let (action, target, detail): (String, String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, target, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), target.as_str(), detail.as_str()), ("cluster.stack.expose", "web", "192.0.2.10:443 to nginx:80/tcp"));
        assert_eq!(asked.lock().unwrap().iter().filter(|l| l.starts_with("PUT")).count(), 1);
    }

    #[tokio::test]
    async fn the_cluster_is_named_by_an_administrator_and_shown_with_it() {
        let (app, _, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let put = |name: &str| Request::put("/api/cluster/name").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json").body(Body::from(json!({ "name": name }).to_string())).unwrap();
        let (_, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(b["name"], Value::Null, "no name yet");
        assert_eq!(call(&app, put("  Home lab  ")).await.0, StatusCode::OK);
        let (_, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(b["name"], "Home lab");
        assert_eq!(call(&app, put(&"x".repeat(65))).await.0, StatusCode::BAD_REQUEST);
        let (action, target): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, target FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), target.as_str()), ("cluster.rename", "c1"), "by the cluster's id");
        assert_eq!(call(&app, put("")).await.0, StatusCode::OK);
        let (_, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(b["name"], Value::Null, "an empty name takes it away");
    }

    #[tokio::test]
    async fn the_traffic_is_anybody_s_who_is_signed_in() {
        let (app, _, _d) = connected("t0k3n").await;
        assert_eq!(call(&app, Request::get("/api/cluster/traffic").body(Body::empty()).unwrap()).await.0, StatusCode::UNAUTHORIZED);
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/traffic", &cookie)).await;
        assert_eq!((s, b["flows"][0]["to"]["id"].clone()), (StatusCode::OK, json!("web.nginx.1")));
    }

    #[tokio::test]
    async fn floating_addresses_are_read_by_anybody_and_changed_by_an_administrator_into_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/floating", &cookie)).await;
        assert_eq!((s, b[0]["address"].clone()), (StatusCode::OK, json!("192.0.2.10")));
        let add = post("/api/cluster/floating").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "address": "192.0.2.10" }))).unwrap();
        let (s, _, b) = call(&app, add).await;
        assert_eq!((s, b["added"].clone()), (StatusCode::CREATED, json!(3)), "how many a range added, as the manager counted");
        let del = |a: &str| Request::delete(format!("/api/cluster/floating/{a}")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        assert_eq!(call(&app, del("192.0.2.10")).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&app, del("..%2Fnodes")).await.0, StatusCode::NOT_FOUND, "only an address goes into the manager's path");
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, target FROM audit WHERE action LIKE 'cluster.floating.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log, vec![("cluster.floating.add".to_string(), "192.0.2.10".to_string()), ("cluster.floating.remove".to_string(), "192.0.2.10".to_string())]);
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("nodes") && l.starts_with("DELETE")));
    }

    #[tokio::test]
    async fn the_storage_page_s_numbers_are_the_managers() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/usage", &cookie)).await;
        assert_eq!((s, b["nodes"][0]["stacks"][0]["bytes"].clone()), (StatusCode::OK, json!(10)));
        assert!(asked.lock().unwrap().iter().any(|l| l.starts_with("GET /v1/usage")));
        assert_eq!(call(&app, Request::get("/api/cluster/usage").body(Body::empty()).unwrap()).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn storage_is_read_by_anybody_and_given_or_taken_away_by_an_administrator_into_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/storage", &cookie)).await;
        assert_eq!((s, b[0]["name"].clone()), (StatusCode::OK, json!("nas")));
        let add = post("/api/cluster/storage").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "name": " nas ", "driver": "nfs", "source": "192.0.2.50:/export", "options": "vers=4.2" }))).unwrap();
        let (s, _, b) = call(&app, add).await;
        assert_eq!((s, b["got"]["name"].clone(), b["got"]["options"].clone()), (StatusCode::CREATED, json!("nas"), json!("vers=4.2")), "trimmed, as given");
        let del = |n: &str| Request::delete(format!("/api/cluster/storage/{n}")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        assert_eq!(call(&app, del("nas")).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&app, del("..%2Fnodes")).await.0, StatusCode::NOT_FOUND, "only a storage name goes into the manager's path");
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, target FROM audit WHERE action LIKE 'cluster.storage.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log, vec![("cluster.storage.add".to_string(), "nas".to_string()), ("cluster.storage.remove".to_string(), "nas".to_string())]);
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("nodes")), "{:?}", asked.lock().unwrap());
    }

    #[tokio::test]
    async fn a_shell_is_an_administrator_s_asked_first_and_opened_from_this_origin_only() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, _csrf) = signed_in(&app).await;
        let n = "6f1c7c2e-0000-4000-8000-000000000001";
        let ask = |q: &str| Request::get(format!("/api/cluster/nodes/{n}/shell{q}")).header(header::COOKIE, &cookie).header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        assert_eq!(call(&app, ask("?container=web")).await.0, StatusCode::NO_CONTENT, "may be opened");
        let (s, _, b) = call(&app, ask("")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(b["error"].as_str().unwrap().contains("FREIGHTDOCK_WORKER_HOST_SHELL"), "the manager's sentence: {b}");
        assert_eq!(call(&app, ask("?container=-it")).await.0, StatusCode::NOT_FOUND, "only a container's name reaches the manager");
        let upgrade = |origin: Option<&str>| {
            let mut r = Request::get(format!("/api/cluster/nodes/{n}/shell?container=web")).header(header::COOKIE, &cookie).header(header::HOST, "freightdock.test")
                .header(header::CONNECTION, "Upgrade").header(header::UPGRADE, "websocket").header("sec-websocket-version", "13").header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
            if let Some(o) = origin {
                r = r.header(header::ORIGIN, o);
            }
            r.body(Body::empty()).unwrap()
        };
        let before = asked.lock().unwrap().len();
        assert_eq!(call(&app, upgrade(Some("https://evil.example"))).await.0, StatusCode::FORBIDDEN, "another site's page opens no shell");
        assert_eq!(call(&app, upgrade(None)).await.0, StatusCode::FORBIDDEN, "nor a page that does not say where it is from");
        assert_eq!(asked.lock().unwrap().len(), before, "the manager is not asked");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("-it")));
    }

    #[tokio::test]
    async fn a_cephfs_storage_and_its_keys_go_to_the_managers_and_no_key_into_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let add = post("/api/cluster/storage").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "name": "ceph", "driver": "cephfs", "source": "192.0.2.1,192.0.2.2:/", "options": "name=freightdock,fs=fdfs", "secret": " AQBzZWNyZXQ= ", "fence_client": "freightdock-fence", "fence_secret": "AQBmZW5jZQ==" }))).unwrap();
        let (s, _, b) = call(&app, add).await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!((b["got"]["secret"].clone(), b["got"]["fence_client"].clone(), b["got"]["fence_secret"].clone()), (json!("AQBzZWNyZXQ="), json!("freightdock-fence"), json!("AQBmZW5jZQ==")));
        let nfs = post("/api/cluster/storage").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "name": "nas", "driver": "nfs", "source": "192.0.2.50:/export" }))).unwrap();
        let (_, _, b) = call(&app, nfs).await;
        assert!(b["got"].get("secret").is_none(), "no keys sent for storage that has none: {b}");
        let keys = |n: &str, body: Value| Request::put(format!("/api/cluster/storage/{n}/keys")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap();
        let (s, _, b) = call(&app, keys("ceph", json!({ "secret": "", "fence_client": "", "image": "quay.io/ceph/ceph:v20" }))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b["got"].get("secret").is_none(), "an empty key keeps the one set: {b}");
        assert_eq!((b["got"]["fence_client"].clone(), b["got"]["image"].clone()), (json!(""), json!("quay.io/ceph/ceph:v20")), "an empty fence client takes eviction away");
        assert_eq!(call(&app, keys("..%2Fnodes", json!({}))).await.0, StatusCode::NOT_FOUND);
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, target || ' ' || coalesce(detail, '') FROM audit WHERE action LIKE 'cluster.storage.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log[0].0, "cluster.storage.add");
        assert_eq!(log.last().unwrap().0, "cluster.storage.keys");
        assert!(!log.iter().any(|(_, t)| t.contains("AQB")), "no key on record: {log:?}");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("AQB")));
    }

    #[tokio::test]
    async fn an_smb_share_and_its_password_go_to_the_managers_and_no_password_into_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let add = post("/api/cluster/storage").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "name": "files", "driver": "smb", "source": "//192.0.2.60/fd", "options": "username=fd", "password": "p4ss word" }))).unwrap();
        let (s, _, b) = call(&app, add).await;
        assert_eq!((s, b["got"]["password"].clone()), (StatusCode::CREATED, json!("p4ss word")), "a password as it was typed, spaces and all");
        let nfs = post("/api/cluster/storage").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "name": "nas", "driver": "nfs", "source": "192.0.2.50:/export", "password": "x" }))).unwrap();
        let (_, _, b) = call(&app, nfs).await;
        assert!(b["got"].get("password").is_none(), "no password for storage that takes none: {b}");
        let pw = |n: &str, body: Value| Request::put(format!("/api/cluster/storage/{n}/password")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap();
        let (s, _, b) = call(&app, pw("files", json!({ "password": "n3w" }))).await;
        assert_eq!((s, b["got"]["password"].clone()), (StatusCode::OK, json!("n3w")));
        assert_eq!(call(&app, pw("..%2Fnodes", json!({}))).await.0, StatusCode::NOT_FOUND);
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, target || ' ' || coalesce(detail, '') FROM audit WHERE action LIKE 'cluster.storage.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.last().unwrap().0, "cluster.storage.password");
        assert!(!log.iter().any(|(_, t)| t.contains("p4ss") || t.contains("n3w")), "no password on record: {log:?}");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("p4ss")));
    }

    #[tokio::test]
    async fn an_administrator_gives_the_cluster_its_release_and_upgrades_it_into_the_log() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/release", &cookie)).await;
        assert_eq!((s, b["version"].clone()), (StatusCode::OK, json!("1.0.0")));
        // A binary larger than any JSON body, passed on as it came.
        let bin: Vec<u8> = [b"\x7fELF".as_slice(), &vec![7u8; 3 << 20]].concat();
        let up = Request::put("/api/cluster/release/worker?version=1.1.0").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/octet-stream").body(Body::from(bin.clone())).unwrap();
        let (s, _, b) = call(&app, up).await;
        assert_eq!((s, b["release"]["version"].clone()), (StatusCode::OK, json!("1.1.0")), "{b}");
        assert!(asked.lock().unwrap().iter().any(|l| l == &format!("PUT /v1/release/worker?version=1.1.0 {} bytes application/octet-stream", bin.len())), "{:?}", asked.lock().unwrap());
        let bad = Request::put("/api/cluster/release/other?version=1.1.0").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").body(Body::from(bin.clone())).unwrap();
        assert_eq!(call(&app, bad).await.0, StatusCode::NOT_FOUND, "a worker or a manager binary, nothing else");
        let fetch = post("/api/cluster/release/fetch").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "tag": "v1.1.0" }))).unwrap();
        let (s, _, b) = call(&app, fetch).await;
        assert_eq!((s, b["got"]["tag"].clone()), (StatusCode::OK, json!("v1.1.0")));
        let start = post("/api/cluster/upgrade").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        let (s, _, b) = call(&app, start).await;
        assert_eq!((s, b["steps"].clone(), b["got"]["by"].clone()), (StatusCode::ACCEPTED, json!(4), json!("admin")));
        let stop = post("/api/cluster/upgrade/stop").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        assert_eq!(call(&app, stop).await.0, StatusCode::NO_CONTENT);
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, coalesce(target, '') || ' ' || coalesce(detail, '') FROM audit WHERE action LIKE 'cluster.release.%' OR action LIKE 'cluster.upgrade.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        let actions: Vec<&str> = log.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(actions, ["cluster.release.upload", "cluster.release.fetch", "cluster.upgrade.start", "cluster.upgrade.stop"]);
        assert!(log[0].1.contains("worker") && log[0].1.contains("1.1.0"), "{log:?}");
    }

    #[tokio::test]
    async fn ceph_is_set_up_by_an_administrator_into_the_log_and_followed_by_anybody() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/ceph/setup", &cookie)).await;
        assert_eq!((s, b["state"].clone()), (StatusCode::OK, json!("running")));
        let body = json!({ "storage": " ceph ", "fs": "fdfs", "hosts": [{ "node_id": "6f1c7c2e-0000-4000-8000-000000000001", "disks": ["/dev/vdb"] }] });
        let start = post("/api/cluster/ceph/setup").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(&app, start).await;
        assert_eq!((s, b["steps"].clone(), b["got"]["storage"].clone()), (StatusCode::ACCEPTED, json!(9), json!("ceph")));
        assert_eq!(b["got"]["hosts"][0]["disks"], json!(["/dev/vdb"]));
        assert_eq!(b["got"]["by"], json!("admin"), "the manager is told who started it");
        let retry = post("/api/cluster/ceph/setup/retry").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        assert_eq!(call(&app, retry).await.0, StatusCode::NO_CONTENT);
        let clear = Request::delete("/api/cluster/ceph/setup").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap();
        assert_eq!(call(&app, clear).await.0, StatusCode::NO_CONTENT);
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, coalesce(detail, '') FROM audit WHERE action LIKE 'cluster.ceph.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["cluster.ceph.setup", "cluster.ceph.retry", "cluster.ceph.clear"]);
        assert!(log[0].1.contains("fdfs") && log[0].1.contains("/dev/vdb"), "{log:?}");
        assert!(asked.lock().unwrap().iter().any(|l| l == "DELETE /v1/ceph/setup"));
    }

    #[tokio::test]
    async fn the_cluster_s_settings_are_changed_by_an_administrator_into_the_log_without_the_token() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/settings", &cookie)).await;
        assert_eq!((s, b["settings"]["fence_stop_s"]["value"].clone()), (StatusCode::OK, json!(30)));
        let put = |body: Value| Request::put("/api/cluster/settings").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::CONTENT_TYPE, "application/json").body(json_body(body)).unwrap();
        let (s, _, b) = call(&app, put(json!({ "fence_stop_s": 45, "proxmox": { "url": "https://pve.example", "fingerprint": "AB", "token": "fd@pve!fence=s3cret" } }))).await;
        assert_eq!((s, b["got"]["fence_stop_s"].clone(), b["got"]["proxmox"]["token"].clone()), (StatusCode::OK, json!(45), json!("fd@pve!fence=s3cret")), "passed on whole");
        assert_eq!(call(&app, put(json!([1]))).await.0, StatusCode::BAD_REQUEST, "an object only");
        let probe = post("/api/cluster/settings/proxmox/fingerprint").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "url": "https://pve.example" }))).unwrap();
        let (s, _, b) = call(&app, probe).await;
        assert_eq!((s, b["fingerprint"].clone(), b["got"]["url"].clone()), (StatusCode::OK, json!("AB:CD"), json!("https://pve.example")));
        let test = post("/api/cluster/settings/proxmox/test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({}))).unwrap();
        let (s, _, b) = call(&app, test).await;
        assert!(s == StatusCode::BAD_GATEWAY && b["error"].as_str().unwrap().contains("pve.example"), "the manager's sentence: {s} {b}");
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, target || ' ' || COALESCE(detail, '') FROM audit WHERE action LIKE 'cluster.settings%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(log[0].0, "cluster.settings.change");
        assert!(log[0].1.contains("fence_stop_s") && log[0].1.contains("proxmox") && !log[0].1.contains("s3cret"), "{log:?}");
        assert!(asked.lock().unwrap().iter().any(|l| l == "PUT /v1/settings"));
    }

    #[tokio::test]
    async fn a_bmc_s_certificate_is_read_and_the_redfish_account_tried_through_a_manager() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let probe = post("/api/cluster/settings/redfish/fingerprint").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "host": "192.0.2.31" }))).unwrap();
        let (s, _, b) = call(&app, probe).await;
        assert_eq!((s, b["fingerprint"].clone(), b["got"].clone()), (StatusCode::OK, json!("AB:CD"), json!({ "host": "192.0.2.31" })), "the host alone is passed on");
        let test = post("/api/cluster/settings/redfish/test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "username": "fence", "password": "s3cret" }))).unwrap();
        let (s, _, b) = call(&app, test).await;
        assert_eq!((s, b["systems"][0]["power"].clone(), b["got"]["password"].clone()), (StatusCode::OK, json!("On"), json!("s3cret")));
        assert!(asked.lock().unwrap().iter().any(|l| l == "POST /v1/settings/redfish/test"));
    }

    #[tokio::test]
    async fn a_backup_is_downloaded_by_an_administrator_and_logged_without_its_passphrase() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let raw = |req: Request<Body>| {
            let app = app.clone();
            async move {
                let r = tower::ServiceExt::oneshot(crate::api::router(app), req).await.unwrap();
                let (s, h) = (r.status(), r.headers().clone());
                (s, h, http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes().to_vec())
            }
        };
        let take = |p: &str| post("/api/cluster/backup").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "passphrase": p }))).unwrap();
        let (s, h, b) = raw(take("correct horse battery")).await;
        assert_eq!((s, b.as_slice()), (StatusCode::OK, &b"FDBK1sealed"[..]));
        assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().contains(".fdbk"), "{h:?}");
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        let (s, _, b) = call(&app, take("short")).await;
        assert!(s == StatusCode::BAD_REQUEST && b["error"].as_str().unwrap().contains("12"), "{s} {b}");
        let (s, _, b) = call(&app, get("/api/cluster/backups", &cookie)).await;
        assert_eq!((s, b["backups"][0]["manager"].clone()), (StatusCode::OK, json!("m1")));
        let (s, _, b) = raw(get("/api/cluster/backups/freightdock-20261113-200000.fdbk", &cookie)).await;
        assert_eq!((s, b.as_slice()), (StatusCode::OK, &b"FDBK1stored"[..]));
        assert_eq!(raw(get("/api/cluster/backups/..%2Fadmin.token", &cookie)).await.0, StatusCode::NOT_FOUND, "only a backup's name goes into the manager's path");
        let log: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action || ' ' || target || ' ' || COALESCE(detail, '') FROM audit WHERE action LIKE 'cluster.backup%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.len(), 2, "the download and the stored one: {log:?}");
        assert!(log.iter().all(|l| !l.contains("horse")), "{log:?}");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("admin.token")));
    }

    #[tokio::test]
    async fn a_node_s_docker_is_an_administrator_s_and_a_change_is_on_record() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let (s, _, b) = call(&app, get(&format!("/api/cluster/nodes/{node}/docker?action=logs&target=web&tail=50&evil=1"), &cookie)).await;
        assert_eq!((s, b["lines"][0].clone()), (StatusCode::OK, json!("hi")), "only the keys known go on: {b}");
        let change = |cookie: &str, csrf: &str, body: Value| post(&format!("/api/cluster/nodes/{node}/docker")).header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(&app, change(&cookie, &csrf, json!({ "action": "container.stop", "target": "web" }))).await;
        assert_eq!((s, b["got"]["action"].clone(), b["got"]["target"].clone()), (StatusCode::OK, json!("container.stop"), json!("web")));
        let (action, target, detail): (String, String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, target, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), target.as_str(), detail.as_str()), ("cluster.docker", node, "container.stop web"));
        assert_eq!(call(&app, get("/api/cluster/nodes/..%2Fstacks/docker?action=containers", &cookie)).await.0, StatusCode::NOT_FOUND);
        // The browser leaves before the answer: the change is on record all the same, since it was done all the same.
        assert!(tokio::time::timeout(Duration::from_millis(100), call(&app, change(&cookie, &csrf, json!({ "action": "container.restart", "target": "web" })))).await.is_err());
        let mut recorded = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let n: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit WHERE detail = 'container.restart web'", [], |r| r.get(0))?)).await.unwrap();
            if n == 1 {
                recorded = true;
                break;
            }
        }
        assert!(recorded, "the restart is on record");

        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let user = format!("freightdock_session={token}");
        let (_, _, me) = call(&app, get("/api/me", &user)).await;
        assert_eq!(call(&app, get(&format!("/api/cluster/nodes/{node}/docker?action=containers"), &user)).await.0, StatusCode::FORBIDDEN, "a container's environment and logs may hold secrets");
        assert_eq!(call(&app, change(&user, me["csrf"].as_str().unwrap(), json!({ "action": "container.stop", "target": "web" }))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(asked.lock().unwrap().iter().filter(|l| l.contains("/docker")).count(), 3);
    }

    #[tokio::test]
    async fn a_registry_s_login_is_an_administrator_s_and_on_record_without_its_password() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/registries", &cookie)).await;
        assert_eq!((s, b[0]["host"].clone()), (StatusCode::OK, json!("ghcr.io")));
        let put = Request::put("/api/cluster/registries/ghcr.io").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(json!({ "username": "bot", "password": "s3cret" }))).unwrap();
        let (s, _, b) = call(&app, put).await;
        assert_eq!((s, b["got"]["password"].clone()), (StatusCode::OK, json!("s3cret")), "passed on whole");
        let test = post("/api/cluster/registries/ghcr.io/test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({}))).unwrap();
        assert_eq!(call(&app, test).await.0, StatusCode::OK);
        let del = Request::delete("/api/cluster/registries/ghcr.io").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        assert_eq!(call(&app, del).await.0, StatusCode::NO_CONTENT);
        let bad = Request::delete("/api/cluster/registries/..%2Fstorage%2Fnas").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        assert_eq!(call(&app, bad).await.0, StatusCode::NOT_FOUND, "only a registry's name goes into the manager's path");
        let log: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action || ' ' || target || ' ' || COALESCE(detail, '') FROM audit WHERE action LIKE 'cluster.registry%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[0].starts_with("cluster.registry.set ghcr.io") && log.iter().all(|l| !l.contains("s3cret")), "{log:?}");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("storage")));
    }

    #[tokio::test]
    async fn a_notification_channel_is_an_administrator_s_and_on_record_without_its_secret() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/notifications", &cookie)).await;
        assert_eq!((s, b["channels"][0]["name"].clone()), (StatusCode::OK, json!("ops")));
        let body = json!({ "kind": "ntfy", "events": ["node.down"], "config": { "topic": "alerts" }, "secret": "tk_s3cret" });
        let put = Request::put("/api/cluster/notifications/ops").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(body)).unwrap();
        let (s, _, b) = call(&app, put).await;
        assert_eq!((s, b["got"]["secret"].clone(), b["got"]["config"]["topic"].clone()), (StatusCode::OK, json!("tk_s3cret"), json!("alerts")), "passed on whole");
        let test = post("/api/cluster/notifications/ops/test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({}))).unwrap();
        let (s, _, b) = call(&app, test).await;
        assert!(s == StatusCode::BAD_GATEWAY && b["error"].as_str().unwrap_or("").contains("401"), "the manager's why kept: {s} {b}");
        let del = Request::delete("/api/cluster/notifications/ops").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        assert_eq!(call(&app, del).await.0, StatusCode::NO_CONTENT);
        let bad = Request::delete("/api/cluster/notifications/..%2Fstorage%2Fnas").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
        assert_eq!(call(&app, bad).await.0, StatusCode::NOT_FOUND, "only a channel's name goes into the manager's path");
        let log: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action || ' ' || target || ' ' || COALESCE(detail, '') FROM audit WHERE action LIKE 'cluster.notification%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.len(), 3, "set, test and remove: {log:?}");
        assert!(log[0].starts_with("cluster.notification.set ops ntfy") && log.iter().all(|l| !l.contains("s3cret")), "{log:?}");
        assert!(!asked.lock().unwrap().iter().any(|l| l.contains("storage")));

        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        assert_eq!(call(&app, get("/api/cluster/notifications", &format!("freightdock_session={token}"))).await.0, StatusCode::FORBIDDEN, "where the cluster's news goes is an administrator's");
    }

    #[tokio::test]
    async fn a_join_token_is_for_a_worker_or_a_manager() {
        let (app, _, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let req = |kind: &str| post("/api/cluster/join-tokens").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({ "kind": kind }))).unwrap();
        let (s, _, b) = call(&app, req("worker")).await;
        assert_eq!((s, b["token"].clone()), (StatusCode::OK, json!("FDCT1-x")));
        let (s, _, _) = call(&app, req("captain")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_recovery_history_is_read_by_anybody_and_only_a_stack_name_reaches_the_manager() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, _) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/recoveries?stack=web", &cookie)).await;
        assert_eq!((s, b[0]["reason"].clone()), (StatusCode::OK, json!("pinned")));
        let (s, _, _) = call(&app, get("/api/cluster/recoveries?stack=web%26x=1", &cookie)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(*asked.lock().unwrap(), vec!["GET /v1/recoveries?stack=web".to_string()]);
    }

    #[tokio::test]
    async fn stacks_are_read_by_anybody_and_changed_by_an_administrator_on_record() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, get("/api/cluster/stacks", &cookie)).await;
        assert_eq!((s, b[0]["name"].clone()), (StatusCode::OK, json!("web")));
        let change = |method: &str, path: &str, body: Value| Request::builder().method(method).uri(path).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap();
        let (s, _, b) = call(&app, change("POST", "/api/cluster/stacks", json!({ "name": "web", "compose": "services: {}" }))).await;
        assert_eq!((s, b["stack"]["name"].clone()), (StatusCode::CREATED, json!("web")));
        assert_eq!((b["got"].get("nodes"), b["got"].get("exposures")), (None, None), "left out: the manager keeps them as they were");
        let chosen = json!({ "name": "web", "compose": "services: {}", "nodes": ["n1"], "exposures": [{ "service": "web", "container_port": 80, "protocol": "tcp", "address": "192.0.2.10", "port": 80 }] });
        let (_, _, b) = call(&app, change("POST", "/api/cluster/stacks", chosen.clone())).await;
        assert_eq!((&b["got"]["nodes"], &b["got"]["exposures"][0]["address"]), (&json!(["n1"]), &json!("192.0.2.10")), "the nodes and exposures chosen go with the deploy");
        let options = json!({ "name": "web", "compose": "services: {}", "description": "The shop", "env": "TAG=1\n", "selector": ["zone=a"], "mode": null, "names": { "web": "shop-web" } });
        let (_, _, b) = call(&app, change("POST", "/api/cluster/stacks", options)).await;
        assert_eq!((&b["got"]["description"], &b["got"]["env"], &b["got"]["selector"], &b["got"]["names"]["web"]), (&json!("The shop"), &json!("TAG=1\n"), &json!(["zone=a"]), &json!("shop-web")), "the options go with the deploy");
        assert_eq!(b["got"].get("mode"), Some(&Value::Null), "null (as the file says) is passed on, not left out");
        let (_, _, b) = call(&app, change("POST", "/api/cluster/stacks", json!({ "name": "web", "compose": "services: {}", "stopped": true }))).await;
        assert_eq!(b["got"]["stopped"], json!(true), "a stack can be made without being started");
        let (s, _, b) = call(&app, change("POST", "/api/cluster/stack-check", chosen)).await;
        assert_eq!((s, &b["services"]["web"]["ports"][0]["host_port"], &b["got"]["nodes"]), (StatusCode::OK, &json!(8080), &json!(["n1"])), "a check is passed on whole");
        let (s, _, b) = call(&app, change("POST", "/api/cluster/stacks/web/restart", json!({}))).await;
        assert_eq!((s, b["restarting"].clone()), (StatusCode::OK, json!(1)));
        let (s, _, _) = call(&app, change("DELETE", "/api/cluster/stacks/web", Value::Null)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        // Only a stack's name and the three actions go into the manager's paths.
        let (s, _, _) = call(&app, change("POST", "/api/cluster/stacks/web/explode", json!({}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = call(&app, change("DELETE", "/api/cluster/stacks/..%2Fnodes", Value::Null)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let actions: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action || ' ' || COALESCE(target, '') FROM audit ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(actions.iter().filter(|a| a.starts_with("cluster.stack")).cloned().collect::<Vec<_>>(), vec!["cluster.stack.deploy web", "cluster.stack.deploy web", "cluster.stack.deploy web", "cluster.stack.deploy web", "cluster.stack.restart web", "cluster.stack.remove web"]);
        assert_eq!(asked.lock().unwrap().len(), 8, "a check changes nothing, so it is not on record");
    }

    #[tokio::test]
    async fn a_node_s_name_is_an_administrator_s_to_give_on_record_and_the_managers_name_it_so_too() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let rename = |name: &str| Request::put(format!("/api/cluster/nodes/{node}/name")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(json!({ "name": name }))).unwrap();
        let (s, _, b) = call(&app, rename("  web box  ")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, json!("web box")));
        let put = |asked: &Mutex<Vec<String>>| -> Value {
            let all = asked.lock().unwrap();
            let line = all.iter().rev().find(|l| l.starts_with("PUT ")).expect("the managers are told").clone();
            serde_json::from_str::<Value>(line.split_once(' ').unwrap().1.split_once(' ').unwrap().1).unwrap()["labels"].clone()
        };
        assert_eq!(put(&asked), json!({ "zone": "a", "name": "web box" }), "its name label, the other labels kept");
        let (_, _, b) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(b[0]["name"], "web box", "shown with the managers' nodes");
        let (s, _, b) = call(&app, get(&format!("/api/cluster/nodes/{node}"), &cookie)).await;
        assert_eq!((s, b["name"].clone(), b["hostname"].clone()), (StatusCode::OK, json!("web box"), json!("docker")));
        let (action, detail): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), detail.as_str()), ("cluster.node.rename", "web box"));

        assert_eq!(call(&app, rename(&"x".repeat(65))).await.0, StatusCode::BAD_REQUEST, "64 characters at most");
        let (s, _, b) = call(&app, rename("")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, Value::Null), "an empty name gives the hostname back");
        assert_eq!(put(&asked), json!({ "zone": "a" }), "and the managers' name label goes");
        let (_, _, b) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(b[0].get("name"), None);
    }

    #[tokio::test]
    async fn a_manager_s_name_is_freightdock_s_own_too_given_by_its_raft_id() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let rename = |id: &str, name: &str| Request::put(format!("/api/cluster/managers/{id}/name")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(json!({ "name": name }))).unwrap();
        let (s, _, b) = call(&app, rename("5271529257891035005", " core 1 ")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, json!("core 1")));
        assert_eq!(asked.lock().unwrap().len(), 0, "the managers are not told");
        let (_, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(b["cluster"]["raft"][0]["name"], "core 1", "shown with the managers' members");
        let (action, detail): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), detail.as_str()), ("cluster.manager.rename", "core 1"));

        assert_eq!(call(&app, rename("5271529257891035005", &"x".repeat(65))).await.0, StatusCode::BAD_REQUEST, "64 characters at most");
        assert_eq!(call(&app, rename("not-a-raft-id", "x")).await.0, StatusCode::NOT_FOUND, "a raft id is a number");
        let (s, _, b) = call(&app, rename("5271529257891035005", "")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, Value::Null), "an empty name gives the address back");
        let (_, _, b) = call(&app, get("/api/cluster", &cookie)).await;
        assert_eq!(b["cluster"]["raft"][0].get("name"), None);
    }

    #[tokio::test]
    async fn an_administrator_sets_a_node_s_labels_on_record() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let req = Request::put(format!("/api/cluster/nodes/{node}/labels")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(json!({ "labels": { "zone": "a" } }))).unwrap();
        let (s, _, b) = call(&app, req).await;
        assert_eq!((s, b["all_labels"]["zone"].clone()), (StatusCode::OK, json!("a")));
        let (action, detail): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), detail.as_str()), ("cluster.node.labels", "zone=a"));
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_administrator_authorizes_a_fenced_node_on_record() {
        let (app, _, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let req = post(&format!("/api/cluster/nodes/{node}/authorize")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(json!({}))).unwrap();
        let (s, _, b) = call(&app, req).await;
        assert_eq!((s, b["fencing"]["state"].clone()), (StatusCode::OK, json!("authorized")));
        let action: String = app.db.call(|c| Ok(c.query_row("SELECT action FROM audit ORDER BY id DESC LIMIT 1", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(action, "cluster.node.authorize");
    }

    #[test]
    fn the_address_is_http() {
        assert!(Cluster::new("http://127.0.0.1:7451/", PathBuf::from("/x"), None).unwrap().api == "http://127.0.0.1:7451");
        assert!(Cluster::new("127.0.0.1:7451", PathBuf::from("/x"), None).is_err());
    }

    #[test]
    fn over_https_the_cluster_ca_is_needed_and_the_only_one_trusted() {
        assert!(Cluster::new("https://127.0.0.1:7452", PathBuf::from("/x"), None).is_err(), "no CA: nothing to check the managers against");
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        assert!(Cluster::new("https://127.0.0.1:7452", PathBuf::from("/x"), Some(Given::File(ca.clone()))).is_err(), "a CA file that is not there");
        let pem = rcgen::generate_simple_self_signed(vec!["x".to_string()]).unwrap().cert.pem();
        std::fs::write(&ca, pem).unwrap();
        let c = Cluster::new("https://127.0.0.1:7452, https://localhost:7452", PathBuf::from("/x"), Some(Given::File(ca))).unwrap();
        assert_eq!(c.endpoints.iter().map(|e| e.base.as_str()).collect::<Vec<_>>(), vec!["https://manager.freightdock.internal:7452"; 2], "checked for the managers' name, whatever address reaches them");
    }

    #[tokio::test]
    async fn a_manager_s_refusal_comes_through_as_its_sentence() {
        let (api, _) = fake_manager().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("admin.token");
        std::fs::write(&file, "t0k3n").unwrap();
        let e = Cluster::new(&api, file, None).unwrap().call(reqwest::Method::POST, "/v1/stacks/web/stop", None).await.unwrap_err();
        assert_eq!((e.status, e.message.as_str()), (StatusCode::CONFLICT, "the cluster says no"));
    }

    #[tokio::test]
    async fn the_token_and_the_ca_can_be_given_as_they_are_rather_than_in_files() {
        let (api, _) = fake_manager().await;
        let c = Cluster::new(&api, Given::Value("t0k3n\n".into()), None).unwrap();
        assert_eq!(c.call(reqwest::Method::GET, "/v1/cluster", None).await.unwrap().0, StatusCode::OK, "the managers hand the UI its token so");
        let pem = rcgen::generate_simple_self_signed(vec!["x".to_string()]).unwrap().cert.pem();
        assert!(Cluster::new("https://127.0.0.1:7452", Given::Value("t".into()), Some(Given::Value(pem))).is_ok());
        assert!(Cluster::new("https://127.0.0.1:7452", Given::Value("t".into()), Some(Given::Value("not a PEM".into()))).is_err());
        assert!(!format!("{:?}", Given::Value("t0k3n".into())).contains("t0k3n"), "never in a log");
    }

    #[tokio::test]
    async fn a_manager_that_does_not_answer_is_passed_over_for_the_next() {
        let (api, asked) = fake_manager().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("admin.token");
        std::fs::write(&file, "t0k3n").unwrap();
        let dead = format!("http://{}", std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap());
        let c = Cluster::new(&format!("{dead},{api}"), file, None).unwrap();
        assert_eq!(c.call(reqwest::Method::GET, "/v1/cluster", None).await.unwrap().0, StatusCode::OK);
        assert_eq!(c.call(reqwest::Method::GET, "/v1/cluster", None).await.unwrap().0, StatusCode::OK);
        assert_eq!(asked.lock().unwrap().len(), 2);
    }
}
