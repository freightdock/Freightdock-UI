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
    pub(crate) async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> ApiResult<(StatusCode, Value)> {
        let token = match &self.token {
            Given::Value(t) => t.clone(),
            Given::File(f) => tokio::fs::read_to_string(f).await.map_err(|e| {
                tracing::warn!("the cluster manager's token in {}: {e}", f.display());
                ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Freightdock cannot read the manager's token")
            })?,
        };
        let n = self.endpoints.len();
        let first = self.current.load(Ordering::Relaxed) % n;
        let mut answered = None;
        for i in 0..n {
            let at = (first + i) % n;
            let e = &self.endpoints[at];
            let mut req = e.http.request(method.clone(), format!("{}{path}", e.base)).bearer_auth(token.trim());
            if let Some(b) = &body {
                req = req.json(b);
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
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            // The manager's sentence, for a person; a 401 from it is Freightdock's token, not the person's sign-in.
            // The manager answers `{"error": "..."}`; its sentence is what a person reads.
            let said = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| text.trim().to_string());
            let message = if status == StatusCode::UNAUTHORIZED { "the manager refused Freightdock's token".to_string() } else if said.is_empty() { format!("the manager answered {status}") } else { said };
            let status = if status == StatusCode::UNAUTHORIZED { StatusCode::BAD_GATEWAY } else { status };
            return Err(ApiError::new(status, message));
        }
        Ok((status, if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) }))
    }
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
        .route("/api/cluster/storage/{name}", axum::routing::delete(remove_storage))
}

fn cluster(app: &App) -> ApiResult<&Cluster> {
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

/// Gives a node a name to be shown by, here only: the managers and the node keep its hostname ("Cluster" 12). An
/// empty name takes it away.
async fn rename_node(State(app): State<App>, a: Authed, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    id_or_404(&id, "node")?;
    give_name(&app, a, "node_names", "node_id", "cluster.node.rename", id, b.name).await
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
}

/// Storage given to the cluster; the managers check it.
async fn add_storage(State(app): State<App>, a: Authed, Json(b): Json<StorageBody>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = b.name.trim().to_string();
    let body = json!({ "name": name, "driver": b.driver.trim(), "source": b.source.trim(), "options": b.options.trim() });
    let (s, v) = cluster(&app)?.call(reqwest::Method::POST, "/v1/storage", Some(body)).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.add", name)).await;
    Ok((s, Json(v)))
}

async fn remove_storage(State(app): State<App>, a: Authed, Path(name): Path<String>) -> ApiResult<StatusCode> {
    // Only a storage name (a-z, 0-9 and -) goes into the manager's path.
    if name.is_empty() || name.len() > 40 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ApiError::not_found("storage"));
    }
    cluster(&app)?.call(reqwest::Method::DELETE, &format!("/v1/storage/{name}"), None).await?;
    audit::record(&app.db, Some(&a.session.user), a.ip, audit::entry("cluster.storage.remove", name)).await;
    Ok(StatusCode::NO_CONTENT)
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
}

impl DeployBody {
    /// As the manager takes it, with only what was given.
    fn to_manager(&self) -> Value {
        let mut v = json!({ "name": self.name.trim(), "compose": self.compose });
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
mod tests {
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
                let got: Value = serde_json::from_slice(&axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap_or_default()).unwrap_or(Value::Null);
                match line.as_str() {
                    "GET /v1/cluster" => (StatusCode::OK, json!({ "cluster_id": "c1", "raft": [{ "raft_id": "5271529257891035005", "addr": "192.0.2.1:7450", "leader": true }] }).to_string()),
                    "GET /v1/nodes" => (StatusCode::OK, json!([{ "id": "6f1c7c2e-0000-4000-8000-000000000001", "liveness": { "health": "healthy" } }]).to_string()),
                    "GET /v1/events?after=4" => (StatusCode::OK, json!([{ "seq": 5, "kind": "connected" }]).to_string()),
                    "POST /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/commands" => (StatusCode::ACCEPTED, json!({ "id": "cmd-1", "status": "pending" }).to_string()),
                    "POST /v1/join-tokens" => (StatusCode::OK, json!({ "token": "FDCT1-x" }).to_string()),
                    "PUT /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/labels" => (StatusCode::OK, json!({ "all_labels": { "zone": "a" } }).to_string()),
                    "POST /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001/authorize" => (StatusCode::OK, json!({ "fencing": { "state": "authorized" } }).to_string()),
                    "GET /v1/recoveries?stack=web" => (StatusCode::OK, json!([{ "workload_id": "web.nginx.1", "to": null, "reason": "pinned" }]).to_string()),
                    "GET /v1/stacks" => (StatusCode::OK, json!([{ "name": "web", "workloads": [] }]).to_string()),
                    "POST /v1/stacks" => (StatusCode::CREATED, json!({ "stack": { "name": "web" }, "warnings": [], "got": got }).to_string()),
                    "POST /v1/stack-check" => (StatusCode::OK, json!({ "services": { "web": { "ports": [{ "host_port": 8080, "container_port": 80 }] } }, "got": got }).to_string()),
                    "POST /v1/stacks/web/stop" => (StatusCode::CONFLICT, json!({ "error": "the cluster says no" }).to_string()),
                    "POST /v1/stacks/web/restart" => (StatusCode::OK, json!({ "restarting": 1, "busy": 0 }).to_string()),
                    "DELETE /v1/stacks/web" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/nodes/6f1c7c2e-0000-4000-8000-000000000001" => (StatusCode::OK, json!({ "id": "6f1c7c2e-0000-4000-8000-000000000001", "hostname": "docker", "containers": [] }).to_string()),
                    "PUT /v1/stacks/web/exposures" => (StatusCode::OK, json!({ "name": "web", "exposures": [{ "service": "nginx", "container_port": 80, "protocol": "tcp", "address": "192.0.2.10", "port": 443 }] }).to_string()),
                    "GET /v1/traffic" => (StatusCode::OK, json!({ "reporting": ["6f1c7c2e-0000-4000-8000-000000000001"], "workloads": [], "flows": [{ "from": { "kind": "outside" }, "to": { "kind": "workload", "id": "web.nginx.1" }, "port": 80, "out_bps": 10 }] }).to_string()),
                    "GET /v1/floating" => (StatusCode::OK, json!([{ "address": "192.0.2.10", "holder": "6f1c7c2e-0000-4000-8000-000000000001", "since": 1 }]).to_string()),
                    "POST /v1/floating" => (StatusCode::CREATED, json!({ "added": 3 }).to_string()),
                    "DELETE /v1/floating/192.0.2.10" => (StatusCode::NO_CONTENT, String::new()),
                    "GET /v1/storage" => (StatusCode::OK, json!([{ "name": "nas", "driver": "nfs", "source": "192.0.2.50:/export", "volumes": [] }]).to_string()),
                    "POST /v1/storage" => (StatusCode::CREATED, json!({ "got": got }).to_string()),
                    "DELETE /v1/storage/nas" => (StatusCode::NO_CONTENT, String::new()),
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

    async fn connected(token: &str) -> (App, Arc<Mutex<Vec<String>>>, tempfile::TempDir) {
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
        assert_eq!(actions.iter().filter(|a| a.starts_with("cluster.stack")).cloned().collect::<Vec<_>>(), vec!["cluster.stack.deploy web", "cluster.stack.deploy web", "cluster.stack.restart web", "cluster.stack.remove web"]);
        assert_eq!(asked.lock().unwrap().len(), 6, "a check changes nothing, so it is not on record");
    }

    #[tokio::test]
    async fn a_node_s_name_is_freightdock_s_own_an_administrator_s_to_give_and_on_record() {
        let (app, asked, _d) = connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let node = "6f1c7c2e-0000-4000-8000-000000000001";
        let rename = |name: &str| Request::put(format!("/api/cluster/nodes/{node}/name")).header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").body(json_body(json!({ "name": name }))).unwrap();
        let (s, _, b) = call(&app, rename("  web box  ")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, json!("web box")));
        assert_eq!(asked.lock().unwrap().len(), 0, "the managers are not told");
        let (_, _, b) = call(&app, get("/api/cluster/nodes", &cookie)).await;
        assert_eq!(b[0]["name"], "web box", "shown with the managers' nodes");
        let (s, _, b) = call(&app, get(&format!("/api/cluster/nodes/{node}"), &cookie)).await;
        assert_eq!((s, b["name"].clone(), b["hostname"].clone()), (StatusCode::OK, json!("web box"), json!("docker")));
        let (action, detail): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT action, detail FROM audit ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((action.as_str(), detail.as_str()), ("cluster.node.rename", "web box"));

        assert_eq!(call(&app, rename(&"x".repeat(65))).await.0, StatusCode::BAD_REQUEST, "64 characters at most");
        let (s, _, b) = call(&app, rename("")).await;
        assert_eq!((s, b["name"].clone()), (StatusCode::OK, Value::Null), "an empty name gives the hostname back");
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
