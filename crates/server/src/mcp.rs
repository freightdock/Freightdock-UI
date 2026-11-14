//! `/mcp`: Freightdock as a Model Context Protocol server, so an AI agent (Claude Code and the rest) can look after the
//! cluster through it ("MCP" in DECISIONS.md). The Streamable HTTP transport: JSON-RPC 2.0, one message per POST, each
//! answered as plain JSON (nothing here needs a stream), a session id in `Mcp-Session-Id` from `initialize` on.
//!
//! Signed in by API token only. Every tool is a request to Freightdock's own HTTP API, made with the caller's token
//! through the same router, so it meets the same guard, the same calls to the managers and the same activity log as the
//! UI: a read-only token or a person who is not an Administrator is refused a change here exactly as there.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::delete, routing::get, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::auth::User;
use crate::db::{now, rusqlite::OptionalExtension};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The protocol versions this server speaks, newest first. A client asking for another is answered with the newest.
pub const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
pub const SESSION_HEADER: &str = "mcp-session-id";
pub const VERSION_HEADER: &str = "mcp-protocol-version";
/// The longest text a tool answers; past it the text is cut and says so.
pub const MAX_TEXT: usize = 256 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub fn router() -> Router<App> {
    Router::new()
        .route("/mcp", post(rpc).get(no_stream).delete(end))
        .route("/api/settings/mcp", get(get_settings).put(put_settings))
        .route("/api/settings/mcp/tokens/{id}", delete(revoke))
        .route("/api/mcp/status", get(status))
}

const SETTINGS_KEY: &str = "mcp";
/// What `/mcp` answers while it is off.
const OFF: &str = "The MCP server is turned off in this Freightdock. An Administrator turns it on in Settings, AI agents (MCP).";

/// The switch in Settings, AI agents (MCP): one for the whole Freightdock ("MCP" 6).
#[derive(Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

impl Default for Settings {
    /// On: every request needs an API token all the same.
    fn default() -> Self {
        Settings { enabled: true }
    }
}

/// Whether `/mcp` answers.
pub async fn enabled(app: &App) -> bool {
    match app.db.setting::<Settings>(SETTINGS_KEY).await {
        Ok(s) => s.unwrap_or_default().enabled,
        Err(e) => {
            tracing::error!("the MCP setting could not be read: {e:#}");
            false
        }
    }
}

/// `GET /api/settings/mcp`: the switch, where the server is, and every API token there is with its owner (never the
/// token itself), the most lately used first.
async fn get_settings(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let t = now();
    let tokens = app
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT k.id, k.name, k.hint, k.read_only, k.created_at, k.expires_at, k.last_used_at, k.last_used_ip, u.id, u.name, u.disabled
                 FROM api_tokens k JOIN users u ON u.id = k.user_id ORDER BY k.last_used_at IS NULL, k.last_used_at DESC, k.id",
            )?;
            let rows = st.query_map([], |r| {
                let expires: Option<i64> = r.get(5)?;
                let disabled: bool = r.get(10)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "hint": r.get::<_, String>(2)?, "read_only": r.get::<_, bool>(3)?,
                    "created_at": r.get::<_, i64>(4)?, "expires_at": expires, "last_used_at": r.get::<_, Option<i64>>(6)?,
                    "last_used_ip": r.get::<_, Option<String>>(7)?,
                    "owner": { "id": r.get::<_, i64>(8)?, "name": r.get::<_, String>(9)?, "disabled": disabled },
                    "active": !disabled && expires.is_none_or(|e| e > t),
                }))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let active = tokens.iter().filter(|k| k["active"] == true).count();
    Ok(Json(json!({ "enabled": enabled(&app).await, "path": "/mcp", "ca": app.tls_ca.get().is_some(), "active": active, "tokens": tokens })))
}

/// `PUT /api/settings/mcp`: `{enabled}`. Turning it off ends every MCP session.
async fn put_settings(State(app): State<App>, admin: Admin, Json(s): Json<Settings>) -> ApiResult<StatusCode> {
    app.db.put_setting(SETTINGS_KEY, &s).await?;
    if !s.enabled {
        app.mcp.clear();
    }
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.mcp", target: None, detail: Some(if s.enabled { "on" } else { "off" }.into()) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/settings/mcp/tokens/{id}`: an Administrator revokes anybody's token, from a session (as a token cannot
/// revoke one).
async fn revoke(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let a = &admin.0;
    crate::tokens::session_only(a)?;
    let found = app
        .db
        .call(move |c| {
            let found: Option<(String, String)> = c.query_row("SELECT k.name, u.name FROM api_tokens k JOIN users u ON u.id = k.user_id WHERE k.id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            c.execute("DELETE FROM api_tokens WHERE id = ?1", [id])?;
            Ok(found)
        })
        .await?;
    let (name, owner) = found.ok_or_else(|| ApiError::not_found("token"))?;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "token.revoke", target: Some(name), detail: Some(format!("{owner}'s")) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/mcp/status`: for Account, whether the server is on and whether there is a CA to download.
async fn status(State(app): State<App>, _: Authed) -> Json<Value> {
    Json(json!({ "enabled": enabled(&app).await, "path": "/mcp", "ca": app.tls_ca.get().is_some() }))
}

/// The sessions `initialize` began: each is its token's, so another token cannot carry it on. In memory: after the UI
/// moves to another node a client is told to initialize again, which every client does.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, (i64, i64)>>,
}

impl Sessions {
    /// A session unused this long is forgotten.
    pub const IDLE_S: i64 = 24 * 3600;

    pub fn start(&self, token: i64, at: i64) -> String {
        let id = crate::auth::random_token();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, (_, seen)| at - *seen < Self::IDLE_S);
        }
        m.insert(id.clone(), (token, at));
        id
    }

    /// Whether `id` is a live session of `token`; using it keeps it alive.
    pub fn check(&self, id: &str, token: i64, at: i64) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(id) {
            Some((t, seen)) if *t == token && at - *seen < Self::IDLE_S => {
                *seen = at;
                true
            }
            _ => false,
        }
    }

    /// Every session ends (the server was turned off).
    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn end(&self, id: &str, token: i64) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(id).is_some_and(|(t, _)| *t == token) {
            m.remove(id);
            return true;
        }
        false
    }
}

/// The version to speak: the client's when it is one of ours, else the newest.
pub fn pick_version(asked: Option<&str>) -> &'static str {
    VERSIONS.iter().copied().find(|v| Some(*v) == asked).unwrap_or(VERSIONS[0])
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn refuse(status: StatusCode, message: &str) -> Response {
    let mut r = (status, Json(rpc_error(Value::Null, INVALID_REQUEST, message))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"Freightdock\""));
    }
    r
}

/// Who is calling: the token's owner, the token itself (for the requests made on their behalf), and where from.
pub struct Caller {
    app: App,
    token: String,
    user: User,
    host: Option<HeaderValue>,
    peer: Option<ConnectInfo<SocketAddr>>,
}

/// An answer from Freightdock's own API.
struct Answer {
    status: StatusCode,
    body: Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The sentence Freightdock said, for an answer that is not a success.
    fn refusal(&self) -> String {
        let said = self.json()["error"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(&self.body).trim().to_string());
        format!("Freightdock answered {}: {said}", self.status.as_u16())
    }
}

/// `%`-escapes for a query.
fn query(pairs: &[(&str, String)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    let q = s.finish();
    if q.is_empty() { q } else { format!("?{q}") }
}

impl Caller {
    /// A request to Freightdock's API as this caller, through the very router the browser's requests go through.
    async fn api(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Answer, String> {
        let mut req = Request::builder().method(method).uri(path).header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(h) = &self.host {
            req = req.header(header::HOST, h);
        }
        let body = match body {
            Some(b) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let mut req = req.body(body).map_err(|e| e.to_string())?;
        if let Some(p) = self.peer {
            req.extensions_mut().insert(p);
        }
        let mut router = crate::api::router(self.app.clone());
        let r = match tower_service::Service::call(&mut router, req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let status = r.status();
        let limited = http_body_util::Limited::new(r.into_body(), 64 * 1024 * 1024);
        let body = http_body_util::BodyExt::collect(limited).await.map_err(|e| format!("Freightdock's answer could not be read: {e}"))?.to_bytes();
        Ok(Answer { status, body })
    }

    /// A GET or a change whose answer is JSON (or nothing): the JSON, or Freightdock's refusal.
    async fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let a = self.api(method, path, body).await?;
        if !a.status.is_success() {
            return Err(a.refusal());
        }
        Ok(if a.body.is_empty() { json!({ "ok": true }) } else { a.json() })
    }

    /// The node a tool's `node` names: its id, or its name as Freightdock shows it, its hostname or its `name` label.
    async fn node(&self, args: &Value) -> Result<String, String> {
        let asked = args["node"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("Say which node: node is required.")?;
        let list = self.json(Method::GET, "/api/cluster/nodes", None).await?;
        let list = list.as_array().cloned().unwrap_or_default();
        let names = |n: &Value| -> Vec<String> {
            let l = if n["all_labels"].is_object() { &n["all_labels"] } else { &n["labels"] };
            [&n["id"], &n["name"], &n["hostname"], &l["name"]].iter().filter_map(|v| v.as_str()).map(str::to_string).collect()
        };
        if let Some(n) = list.iter().find(|n| names(n).iter().any(|x| x.eq_ignore_ascii_case(asked))) {
            return n["id"].as_str().map(str::to_string).ok_or_else(|| "The managers gave a node without an id.".into());
        }
        let known: Vec<String> = list.iter().map(|n| names(n).get(1).cloned().unwrap_or_else(|| n["id"].as_str().unwrap_or("").to_string())).collect();
        Err(format!("No node {asked}. There are: {}.", if known.is_empty() { "none".to_string() } else { known.join(", ") }))
    }
}

/// A stack's name, as the managers take one.
fn stack_name(args: &Value) -> Result<String, String> {
    let s = args["stack"].as_str().map(str::trim).unwrap_or_default();
    if s.is_empty() {
        return Err("Say which stack: stack is required.".into());
    }
    if s.len() > 40 || !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(format!("\"{s}\" is not a stack's name: lowercase letters, digits and -, at most 40."));
    }
    Ok(s.to_string())
}

/// The container `asked` names on a node, from the node's list: a workload (`web.nginx.1`), a container's name or the
/// start of its id, so an agent can name it as the stacks show it. Not in the list, it is passed on as it was.
fn container_ref(list: &Value, asked: &str) -> String {
    let found = list.as_array().into_iter().flatten().find(|c| {
        c["workload"].as_str() == Some(asked)
            || c["names"].as_array().into_iter().flatten().any(|n| n.as_str().map(|n| n.trim_start_matches('/')) == Some(asked))
            || (asked.len() >= 12 && c["id"].as_str().is_some_and(|id| id.starts_with(asked)))
    });
    found.and_then(|c| c["id"].as_str()).unwrap_or(asked).to_string()
}

/// Text past `max` bytes, cut on a character and saying so.
pub fn capped(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let more = s.len() - end;
    s.truncate(end);
    s.push_str(&format!("\n[cut here: {more} more bytes. Ask for less.]"));
    s
}

/// What a tool's annotations say it does: whether it only reads, may destroy, and is the same done twice.
#[derive(Clone, Copy)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

const READ: Hints = Hints { read_only: true, destructive: false, idempotent: true };
/// A change that can be undone, and done again to no further effect (start, stop, deploy the same file).
const CHANGE: Hints = Hints { read_only: false, destructive: false, idempotent: true };
/// A change that loses something (remove).
const DESTROY: Hints = Hints { read_only: false, destructive: true, idempotent: true };

fn tool(name: &str, title: &str, description: &str, props: Value, required: &[&str], hints: Hints) -> Value {
    let Hints { read_only, destructive, idempotent } = hints;
    json!({
        "name": name, "title": title, "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
        "annotations": { "title": title, "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": false },
    })
}

fn node_prop() -> Value {
    json!({ "type": "string", "description": "The node: its id, or its name, hostname or name label, as list_nodes shows them." })
}

fn stack_prop() -> Value {
    json!({ "type": "string", "description": "The stack's name (lowercase letters, digits and -)." })
}

/// What a deploy or a check takes beside the stack's name: its Compose file and the options the UI gives.
fn deploy_props() -> Value {
    json!({
        "stack": stack_prop(),
        "compose": { "type": "string", "description": "The whole Compose file (YAML) of the stack." },
        "nodes": { "type": "array", "items": { "type": "string" }, "description": "Node ids it may run on; empty for any. Left out, as it was." },
        "selector": { "type": "string", "description": "Labels the nodes must have, such as \"zone=a, disk!=hdd\". Left out, as it was." },
        "description": { "type": "string", "description": "A line saying what the stack is." },
        "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Values for the file's ${VARIABLES}." },
    })
}

/// Every tool there is. Reads are any signed-in token's (some, such as a node's Docker, an Administrator's); changes
/// need an Administrator's token that is not read-only, as the API has it.
pub fn tools() -> Vec<Value> {
    let n = node_prop;
    let s = stack_prop;
    let action = |verb: &str, what: &str| tool(&format!("{verb}_stack"), &format!("{} a stack", capitalise(verb)), what, json!({ "stack": s() }), &["stack"], CHANGE);
    vec![
        tool("cluster_overview", "The cluster", "The cluster as its managers see it: its id and name, whether it has quorum, each manager (address, leader, online) and how many nodes there are.", json!({}), &[], READ),
        tool("list_nodes", "List nodes", "Every node (worker) of the cluster: id, name, hostname, labels, health (healthy, suspect, unreachable), fencing state, what it reports (CPU, memory, Docker) and when it was last heard.", json!({}), &[], READ),
        tool("get_node", "A node", "One node in full: its labels, health, fencing, report and the containers it runs.", json!({ "node": n() }), &["node"], READ),
        tool("list_stacks", "List stacks", "Every stack the cluster runs: name, description, options and each workload with the node it is on and how it is (wanted and actual state, last error).", json!({}), &[], READ),
        tool("get_stack", "A stack", "One stack in full: its Compose file, options, exposures and workloads.", json!({ "stack": s() }), &["stack"], READ),
        tool("check_stack", "Check a stack", "What deploying a Compose file would do, changing nothing: its services and ports, warnings, and where each workload would go or why it could not. Use before deploy_stack.", deploy_props(), &["stack", "compose"], READ),
        tool("deploy_stack", "Deploy a stack", "Makes a stack from a Compose file, or changes one to it: the managers place its workloads on the nodes and start them. Answers what was placed where.", deploy_props(), &["stack", "compose"], CHANGE),
        action("start", "Starts every workload of a stopped stack again."),
        action("stop", "Stops every workload of a stack; it stays defined and can be started again."),
        action("restart", "Stops and starts every workload of a stack (for a new image under the same tag, say)."),
        tool("remove_stack", "Remove a stack", "Removes a stack for good: its workloads are stopped and removed on their nodes. Volumes on shared storage stay.", json!({ "stack": s() }), &["stack"], DESTROY),
        tool("cluster_events", "Cluster events", "What happened in the cluster, oldest first: nodes heard or lost, fencing, workloads started, failed or moved. after gives only the newer ones.",
            json!({ "after": { "type": "integer", "minimum": 0, "description": "Only events after this seq." }, "node": n() }), &[], READ),
        tool("recoveries", "Recoveries", "Workloads the managers moved off a failed (fenced) node and where to, or left there and why, newest first.", json!({ "stack": s() }), &[], READ),
        tool("list_storage", "List storage", "The cluster's shared storage (NFS exports and the like): its source, the volumes on it, and its health on each node that uses it.", json!({}), &[], READ),
        tool("node_containers", "A node's containers", "Every container on one node's Docker, the cluster's and any other, as its worker sees them. An Administrator's.", json!({ "node": n() }), &["node"], READ),
        tool("container_logs", "A container's logs", "The last lines a container on a node wrote, standard output and error together. An Administrator's.",
            json!({
                "node": n(),
                "container": { "type": "string", "description": "The workload (such as web.nginx.1), or the container's name or id on that node, as node_containers shows them." },
                "tail": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 200, "description": "Lines from the end." },
                "timestamps": { "type": "boolean", "default": false, "description": "Docker's time before each line." },
            }), &["node", "container"], READ),
        tool("authorize_node", "Let a node back in", "Lets a node that came back fenced run workloads again. Only once it is known to be sound: it was cut off, and its workloads may have been started elsewhere.", json!({ "node": n() }), &["node"], CHANGE),
        tool("activity_log", "Activity log", "Freightdock's activity log, newest first: who did what to which thing, when (Unix time), from where, and with which API token (via). An Administrator's.",
            json!({
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50, "description": "How many entries." },
                "action": { "type": "string", "description": "Only this action or family, such as cluster.stack.deploy, or cluster for all of them." },
                "user": { "type": "string", "description": "Only this user's." },
            }), &[], READ),
    ]
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Runs a tool: `Ok` with what to answer, `Err` with why it went wrong (a tool error, said to the agent), and a protocol
/// error for a tool that does not exist.
async fn call_tool(caller: &Caller, name: &str, args: &Value) -> Result<Result<Value, String>, (i64, String)> {
    if !tools().iter().any(|t| t["name"] == name) {
        return Err((INVALID_PARAMS, format!("There is no tool {name}.")));
    }
    if !args.is_object() && !args.is_null() {
        return Err((INVALID_PARAMS, "A tool's arguments are an object.".into()));
    }
    Ok(run(caller, name, args).await)
}

/// The body of a deploy or a check, as `/api/cluster/stacks` takes it.
fn deploy_body(args: &Value) -> Result<Value, String> {
    let name = stack_name(args)?;
    let compose = args["compose"].as_str().filter(|c| !c.trim().is_empty()).ok_or("compose is the stack's Compose file, as text.")?;
    let mut b = json!({ "name": name, "compose": compose });
    for k in ["nodes", "selector", "description", "env"] {
        if !args[k].is_null() {
            b[k] = args[k].clone();
        }
    }
    Ok(b)
}

async fn run(caller: &Caller, name: &str, args: &Value) -> Result<Value, String> {
    let get = |p: String| async move { caller.json(Method::GET, &p, None).await };
    match name {
        "cluster_overview" => get("/api/cluster".into()).await,
        "list_nodes" => get("/api/cluster/nodes".into()).await,
        "get_node" => get(format!("/api/cluster/nodes/{}", caller.node(args).await?)).await,
        "list_stacks" => get("/api/cluster/stacks".into()).await,
        "get_stack" => get(format!("/api/cluster/stacks/{}", stack_name(args)?)).await,
        "check_stack" => caller.json(Method::POST, "/api/cluster/stack-check", Some(&deploy_body(args)?)).await,
        "deploy_stack" => caller.json(Method::POST, "/api/cluster/stacks", Some(&deploy_body(args)?)).await,
        "start_stack" | "stop_stack" | "restart_stack" => {
            let stack = stack_name(args)?;
            let verb = name.trim_end_matches("_stack");
            caller.json(Method::POST, &format!("/api/cluster/stacks/{stack}/{verb}"), None).await
        }
        "remove_stack" => {
            let stack = stack_name(args)?;
            caller.json(Method::DELETE, &format!("/api/cluster/stacks/{stack}"), None).await?;
            Ok(json!({ "ok": true, "removed": stack }))
        }
        "cluster_events" => {
            let mut q = vec![];
            if let Some(a) = args["after"].as_u64() {
                q.push(("after", a.to_string()));
            }
            if args["node"].as_str().is_some_and(|n| !n.trim().is_empty()) {
                q.push(("node", caller.node(args).await?));
            }
            get(format!("/api/cluster/events{}", query(&q))).await
        }
        "recoveries" => {
            let q: Vec<(&str, String)> = if args["stack"].is_null() { vec![] } else { vec![("stack", stack_name(args)?)] };
            get(format!("/api/cluster/recoveries{}", query(&q))).await
        }
        "list_storage" => get("/api/cluster/storage".into()).await,
        "node_containers" => get(format!("/api/cluster/nodes/{}/docker{}", caller.node(args).await?, query(&[("action", "containers".into())]))).await,
        "container_logs" => {
            let node = caller.node(args).await?;
            let asked = args["container"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("Say which container: container is required.")?;
            let list = get(format!("/api/cluster/nodes/{node}/docker{}", query(&[("action", "containers".into())]))).await.unwrap_or(Value::Null);
            let target = container_ref(&list, asked);
            let tail = args["tail"].as_u64().unwrap_or(200).clamp(1, 5000);
            let mut q = vec![("action", "logs".to_string()), ("target", target.to_string()), ("tail", tail.to_string())];
            if args["timestamps"].as_bool() == Some(true) {
                q.push(("timestamps", "true".into()));
            }
            get(format!("/api/cluster/nodes/{node}/docker{}", query(&q))).await
        }
        "authorize_node" => caller.json(Method::POST, &format!("/api/cluster/nodes/{}/authorize", caller.node(args).await?), None).await,
        "activity_log" => {
            let mut q = vec![("limit", args["limit"].as_i64().unwrap_or(50).clamp(1, 500).to_string())];
            for k in ["action", "user"] {
                if let Some(v) = args[k].as_str().filter(|v| !v.is_empty()) {
                    q.push((k, v.to_string()));
                }
            }
            let a = get(format!("/api/audit{}", query(&q))).await?;
            Ok(a["entries"].clone())
        }
        _ => Err(format!("There is no tool {name}.")),
    }
}

/// What `initialize` answers.
fn initialized(asked: Option<&str>) -> Value {
    json!({
        "protocolVersion": pick_version(asked),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "freightdock", "title": "Freightdock", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Freightdock is the UI of a Freightdock cluster: managers that place stacks (Compose files) as workloads on worker nodes, move them off a node that fails, and fence it. cluster_overview and list_nodes show the cluster; list_stacks its stacks. A node is named by its id, name or hostname; a stack by its name. Use check_stack before deploy_stack. You may do what the token's owner may: reads for anybody, changes only with an Administrator's token that is not read-only. Every change is written to Freightdock's activity log under the token's name.",
    })
}

/// One JSON-RPC message in, its answer out: `None` for a notification or a response, which are not answered.
pub async fn handle(caller: &Caller, msg: &Value) -> Option<Value> {
    let Some(obj) = msg.as_object() else { return Some(rpc_error(Value::Null, INVALID_REQUEST, "A JSON-RPC message is an object.")) };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response (to a request this server never makes) is taken and dropped.
        return if obj.contains_key("result") || obj.contains_key("error") { None } else { Some(rpc_error(id.unwrap_or(Value::Null), INVALID_REQUEST, "A request names its method.")) };
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_error(id.unwrap_or(Value::Null), INVALID_REQUEST, "This is JSON-RPC 2.0: jsonrpc is \"2.0\"."));
    }
    let id = id?;
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return Some(rpc_error(Value::Null, INVALID_REQUEST, "A request's id is a string or an integer."));
    }
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => rpc_result(id, initialized(params["protocolVersion"].as_str())),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tools() })),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else { return Some(rpc_error(id, INVALID_PARAMS, "tools/call names its tool.")) };
            match call_tool(caller, name, &params["arguments"]).await {
                Err((code, m)) => rpc_error(id, code, m),
                Ok(Ok(v)) => rpc_result(id, json!({ "content": [{ "type": "text", "text": capped(v.to_string(), MAX_TEXT) }], "isError": false })),
                Ok(Err(m)) => rpc_result(id, json!({ "content": [{ "type": "text", "text": m }], "isError": true })),
            }
        }
        other => rpc_error(id, METHOD_NOT_FOUND, format!("This server has no method {other}.")),
    })
}

/// The caller, from the request's API token; `/mcp` takes no cookie. A browser page elsewhere is refused, as the spec
/// asks against DNS rebinding.
async fn caller(app: &App, headers: &HeaderMap, peer: Option<ConnectInfo<SocketAddr>>) -> Result<Caller, (StatusCode, String)> {
    if !crate::api::same_origin(headers) {
        return Err((StatusCode::FORBIDDEN, "A page on another site may not use /mcp.".into()));
    }
    let Some(Ok(token)) = crate::tokens::bearer(headers) else {
        return Err((StatusCode::UNAUTHORIZED, "/mcp wants an API token: Authorization: Bearer fdk_... (make one in Freightdock's Account page).".into()));
    };
    let ip = peer.map(|p| p.0.ip().to_string());
    let user = match crate::tokens::lookup(&app.db, token, ip).await {
        Ok(Some(u)) => u,
        Ok(None) => return Err((StatusCode::UNAUTHORIZED, "That API token is not good: revoked, expired, or never made.".into())),
        Err(e) => {
            tracing::error!("{e:#}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server. Its log says what.".into()));
        }
    };
    if let Some(v) = headers.get(VERSION_HEADER).and_then(|v| v.to_str().ok())
        && !VERSIONS.contains(&v)
    {
        return Err((StatusCode::BAD_REQUEST, format!("This server speaks MCP {}, not {v}.", VERSIONS.join(", "))));
    }
    Ok(Caller { app: app.clone(), token: token.to_string(), user, host: headers.get(header::HOST).cloned(), peer })
}

fn token_id(c: &Caller) -> i64 {
    c.user.token.as_ref().map_or(0, |t| t.id)
}

/// `POST /mcp`: one JSON-RPC message.
async fn rpc(State(app): State<App>, req: Request<Body>) -> Response {
    if !enabled(&app).await {
        return refuse(StatusCode::FORBIDDEN, OFF);
    }
    let (parts, body) = req.into_parts();
    let headers = parts.headers;
    let c = match caller(&app, &headers, parts.extensions.get::<ConnectInfo<SocketAddr>>().copied()).await {
        Ok(c) => c,
        Err((status, m)) => return refuse(status, &m),
    };
    let Ok(body) = axum::body::to_bytes(body, 4 * 1024 * 1024).await else { return refuse(StatusCode::PAYLOAD_TOO_LARGE, "A message is at most 4 MiB.") };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(rpc_error(Value::Null, PARSE_ERROR, format!("That is not JSON: {e}")))).into_response(),
    };
    if msg.is_array() {
        return (StatusCode::BAD_REQUEST, Json(rpc_error(Value::Null, INVALID_REQUEST, "One message per request: batches are not taken."))).into_response();
    }
    let initialize = msg["method"] == "initialize";
    let session = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());
    if !initialize
        && let Some(s) = session
        && !app.mcp.check(s, token_id(&c), now())
    {
        return refuse(StatusCode::NOT_FOUND, "No such session: initialize again.");
    }
    let Some(answer) = handle(&c, &msg).await else { return StatusCode::ACCEPTED.into_response() };
    let mut r = Json(answer.clone()).into_response();
    if initialize && answer.get("result").is_some() {
        let id = app.mcp.start(token_id(&c), now());
        if let Ok(v) = HeaderValue::from_str(&id) {
            r.headers_mut().insert(SESSION_HEADER, v);
        }
    }
    r
}

/// `GET /mcp`: a stream for what the server would say unasked. This one never does.
async fn no_stream(State(app): State<App>) -> Response {
    if !enabled(&app).await {
        return refuse(StatusCode::FORBIDDEN, OFF);
    }
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST, DELETE")]).into_response()
}

/// `DELETE /mcp`: the client ends its session.
async fn end(State(app): State<App>, req: Request<Body>) -> Response {
    if !enabled(&app).await {
        return refuse(StatusCode::FORBIDDEN, OFF);
    }
    let headers = req.headers();
    let c = match caller(&app, headers, req.extensions().get::<ConnectInfo<SocketAddr>>().copied()).await {
        Ok(c) => c,
        Err((status, m)) => return refuse(status, &m),
    };
    match headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        Some(s) if app.mcp.end(s, token_id(&c)) => StatusCode::NO_CONTENT.into_response(),
        _ => refuse(StatusCode::NOT_FOUND, "No such session."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, json_body, signed_in};
    use crate::tokens::tests::token;

    #[test]
    fn the_version_is_the_client_s_when_it_is_ours_and_else_the_newest() {
        assert_eq!(pick_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(pick_version(Some("2024-11-05")), VERSIONS[0]);
        assert_eq!(pick_version(None), VERSIONS[0]);
    }

    #[test]
    fn long_text_is_cut_on_a_character_and_says_so() {
        assert_eq!(capped("short".into(), 10), "short");
        let c = capped("ééééé".into(), 3);
        assert!(c.starts_with("é\n[cut here: 8 more bytes"), "{c}");
    }

    #[test]
    fn every_tool_has_a_schema_a_description_and_says_whether_it_changes_anything() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for want in ["cluster_overview", "list_nodes", "get_node", "list_stacks", "get_stack", "check_stack", "deploy_stack", "start_stack", "stop_stack", "restart_stack", "remove_stack", "cluster_events", "recoveries", "list_storage", "node_containers", "container_logs", "authorize_node", "activity_log"] {
            assert!(names.contains(&want), "no {want}");
        }
        let mut seen = std::collections::HashSet::new();
        for t in &tools {
            let n = t["name"].as_str().unwrap();
            assert!(seen.insert(n), "{n} twice");
            assert!(t["description"].as_str().unwrap().len() > 30, "{n} says too little");
            assert_eq!(t["inputSchema"]["type"], "object", "{n}");
            assert!(t["annotations"]["readOnlyHint"].is_boolean() && t["annotations"]["destructiveHint"].is_boolean(), "{n}");
            for r in t["inputSchema"]["required"].as_array().into_iter().flatten() {
                assert!(t["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(), "{n} requires {r}, which it does not describe");
            }
        }
        let ro = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["annotations"]["readOnlyHint"] == true;
        assert!(ro("list_nodes") && ro("container_logs") && ro("activity_log") && ro("check_stack"));
        assert!(!ro("deploy_stack") && !ro("remove_stack") && !ro("authorize_node"));
        assert_eq!(tools.iter().find(|t| t["name"] == "remove_stack").unwrap()["annotations"]["destructiveHint"], true);
    }

    #[test]
    fn a_session_is_its_token_s_and_ends() {
        let s = Sessions::default();
        let id = s.start(1, 100);
        assert!(s.check(&id, 1, 101));
        assert!(!s.check(&id, 2, 101), "another token cannot carry it on");
        assert!(!s.check("nonsense", 1, 101));
        assert!(!s.check(&id, 1, 101 + Sessions::IDLE_S), "forgotten when idle");
        let id = s.start(1, 100);
        assert!(!s.end(&id, 2));
        assert!(s.end(&id, 1));
        assert!(!s.check(&id, 1, 101));
    }

    #[test]
    fn a_deploy_takes_the_file_and_only_the_options_given() {
        assert_eq!(deploy_body(&json!({ "stack": "web", "compose": "services: {}" })).unwrap(), json!({ "name": "web", "compose": "services: {}" }));
        let b = deploy_body(&json!({ "stack": "web", "compose": "x", "selector": "zone=a", "nodes": [] })).unwrap();
        assert_eq!((b["selector"].clone(), b["nodes"].clone()), (json!("zone=a"), json!([])));
        assert!(deploy_body(&json!({ "stack": "Web", "compose": "x" })).unwrap_err().contains("not a stack's name"));
        assert!(deploy_body(&json!({ "stack": "web" })).unwrap_err().contains("compose"));
    }

    #[test]
    fn a_container_is_named_by_its_workload_its_name_or_its_id() {
        let list = json!([{ "id": "3a8979504093c0c553dfa9311ad7681ea74ee35d", "names": ["/freightdock-ui-1"], "workload": "freightdock.ui.1" }, { "id": "e3774c00", "names": ["other"] }]);
        let id = "3a8979504093c0c553dfa9311ad7681ea74ee35d";
        assert_eq!(container_ref(&list, "freightdock.ui.1"), id);
        assert_eq!(container_ref(&list, "freightdock-ui-1"), id);
        assert_eq!(container_ref(&list, "3a8979504093"), id);
        assert_eq!(container_ref(&list, "3a89"), "3a89", "too short to be sure: passed on");
        assert_eq!(container_ref(&list, "web"), "web", "not there: passed on, the worker says so");
        assert_eq!(container_ref(&Value::Null, "web"), "web");
    }

    fn mcp(token: Option<&str>, session: Option<&str>, body: Value) -> Request<Body> {
        let mut r = Request::post("/mcp").header(header::HOST, "freightdock.test").header(header::CONTENT_TYPE, "application/json").header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(t) = token {
            r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(s) = session {
            r = r.header(SESSION_HEADER, s);
        }
        r.body(json_body(body)).unwrap()
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    async fn setup(app: App, read_only: bool) -> (App, String, String) {
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "claude", "read_only": read_only })).await;
        (app, cookie, t)
    }

    fn text(b: &Value) -> &str {
        b["result"]["content"][0]["text"].as_str().unwrap_or_default()
    }

    async fn tool_call(app: &App, t: &str, name: &str, args: Value) -> (bool, String) {
        let (_, _, b) = call(app, mcp(Some(t), None, req(1, "tools/call", json!({ "name": name, "arguments": args })))).await;
        (b["result"]["isError"] == true, text(&b).to_string())
    }

    #[tokio::test]
    async fn mcp_wants_a_token_and_no_cookie_will_do() {
        let (app, cookie, _) = setup(app(), false).await;
        let (s, h, b) = call(&app, mcp(None, None, req(1, "initialize", json!({})))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(h[header::WWW_AUTHENTICATE].to_str().unwrap().starts_with("Bearer"));
        assert!(b["error"]["message"].as_str().unwrap().contains("API token"));
        let mut r = mcp(None, None, req(1, "initialize", json!({})));
        r.headers_mut().insert(header::COOKIE, cookie.parse().unwrap());
        assert_eq!(call(&app, r).await.0, StatusCode::UNAUTHORIZED, "a browser's cookie is not a way in");
        let (s, _, _) = call(&app, mcp(Some(&format!("fdk_{}", "1".repeat(64))), None, req(1, "initialize", json!({})))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn initialize_answers_the_version_and_a_session_and_tools_are_listed() {
        let (app, _, t) = setup(app(), false).await;
        let init = req(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }));
        let (s, h, b) = call(&app, mcp(Some(&t), None, init)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!((b["jsonrpc"].clone(), b["id"].clone()), (json!("2.0"), json!(1)));
        assert_eq!(b["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(b["result"]["serverInfo"]["name"], "freightdock");
        assert!(b["result"]["instructions"].as_str().unwrap().contains("cluster"));
        let session = h[SESSION_HEADER].to_str().unwrap().to_string();

        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(call(&app, mcp(Some(&t), Some(&session), note)).await.0, StatusCode::ACCEPTED, "a notification is taken, not answered");
        let (s, _, b) = call(&app, mcp(Some(&t), Some(&session), req(2, "tools/list", json!({})))).await;
        assert_eq!((s, b["result"]["tools"].as_array().unwrap().len()), (StatusCode::OK, tools().len()));
        let (s, _, b) = call(&app, mcp(Some(&t), Some(&session), req(3, "ping", json!({})))).await;
        assert_eq!((s, &b["result"]), (StatusCode::OK, &json!({})));
        assert_eq!(call(&app, mcp(Some(&t), Some("not-a-session"), req(4, "tools/list", json!({})))).await.0, StatusCode::NOT_FOUND, "a session nobody began");

        let end = |s: &str| Request::delete("/mcp").header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {t}")).header(SESSION_HEADER, s).body(Body::empty()).unwrap();
        assert_eq!(call(&app, end(&session)).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&app, mcp(Some(&t), Some(&session), req(5, "tools/list", json!({})))).await.0, StatusCode::NOT_FOUND, "ended");
    }

    #[tokio::test]
    async fn what_is_not_json_rpc_gets_json_rpc_s_error_codes() {
        let (app, _, t) = setup(app(), false).await;
        let raw = Request::post("/mcp").header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::from("{not json")).unwrap();
        let (s, _, b) = call(&app, raw).await;
        assert_eq!((s, b["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(PARSE_ERROR)));
        let (s, _, b) = call(&app, mcp(Some(&t), None, json!([req(1, "ping", json!({}))]))).await;
        assert_eq!((s, b["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(INVALID_REQUEST)));
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "resources/list", json!({})))).await;
        assert_eq!((b["error"]["code"].clone(), b["id"].clone()), (json!(METHOD_NOT_FOUND), json!(1)));
        let (_, _, b) = call(&app, mcp(Some(&t), None, json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }))).await;
        assert_eq!(b["error"]["code"], INVALID_REQUEST);
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": "format_the_disk", "arguments": {} })))).await;
        assert_eq!(b["error"]["code"], INVALID_PARAMS, "an unknown tool is the protocol's error");
        assert_eq!(call(&app, mcp(Some(&t), None, json!({ "jsonrpc": "2.0", "id": 9, "result": {} }))).await.0, StatusCode::ACCEPTED, "a response is taken");
        let mut r = mcp(Some(&t), None, req(1, "ping", json!({})));
        r.headers_mut().insert(VERSION_HEADER, "1999-01-01".parse().unwrap());
        assert_eq!(call(&app, r).await.0, StatusCode::BAD_REQUEST, "a protocol version we do not speak");
        let mut r = mcp(Some(&t), None, req(1, "ping", json!({})));
        r.headers_mut().insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert_eq!(call(&app, r).await.0, StatusCode::FORBIDDEN, "a page elsewhere");
        let (s, _, _) = call(&app, Request::get("/mcp").header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "no stream of its own");
    }

    const NODE: &str = "6f1c7c2e-0000-4000-8000-000000000001";

    #[tokio::test]
    async fn a_tool_goes_through_the_api_to_the_managers_and_a_failure_is_the_tool_s() {
        let (app, asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let (app, _, t) = setup(app, false).await;
        let (err, nodes) = tool_call(&app, &t, "list_nodes", json!({})).await;
        assert!(!err && nodes.contains(NODE), "{nodes}");
        let (err, logs) = tool_call(&app, &t, "container_logs", json!({ "node": NODE, "container": "web", "tail": 50 })).await;
        assert!(!err, "{logs}");
        assert!(asked.lock().unwrap().iter().any(|l| l == &format!("GET /v1/nodes/{NODE}/docker?action=logs&target=web&tail=50")), "{:?}", asked.lock().unwrap());
        let (err, said) = tool_call(&app, &t, "get_node", json!({ "node": "nowhere" })).await;
        assert!(err && said.contains("No node nowhere") && said.contains(NODE), "{said}");
        let (err, _) = tool_call(&app, &t, "restart_stack", json!({ "stack": "web" })).await;
        assert!(!err);
        assert!(asked.lock().unwrap().iter().any(|l| l == "POST /v1/stacks/web/restart"));
        let (err, said) = tool_call(&app, &t, "remove_stack", json!({})).await;
        assert!(err && said.contains("stack is required"), "{said}");
        let (err, log) = tool_call(&app, &t, "activity_log", json!({ "action": "cluster" })).await;
        assert!(!err && log.contains("cluster.stack.restart") && log.contains("claude"), "under the token's name: {log}");
    }

    #[tokio::test]
    async fn a_read_only_token_reads_through_mcp_and_is_refused_a_change_as_the_api_refuses_it() {
        let (app, asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let (app, _, t) = setup(app, true).await;
        assert!(!tool_call(&app, &t, "list_stacks", json!({})).await.0);
        for (name, args) in [("stop_stack", json!({ "stack": "web" })), ("remove_stack", json!({ "stack": "web" })), ("deploy_stack", json!({ "stack": "web", "compose": "services: {}" }))] {
            let (err, said) = tool_call(&app, &t, name, args).await;
            assert!(err && said.contains("403") && said.contains("may only read"), "{name}: {said}");
        }
        assert!(!asked.lock().unwrap().iter().any(|l| l.starts_with("POST") || l.starts_with("DELETE")), "the managers were asked nothing: {:?}", asked.lock().unwrap());
    }

    async fn initialize(app: &App, t: &str) -> (StatusCode, Value) {
        let (s, _, b) = call(app, mcp(Some(t), None, req(1, "initialize", json!({ "protocolVersion": "2025-06-18" })))).await;
        (s, b)
    }

    /// The switch in Settings ("MCP" 6): on until an Administrator turns it off, and then nobody's token reaches a tool.
    #[tokio::test]
    async fn the_switch_turns_mcp_off_for_everybody_and_on_again() {
        let w = crate::access::api_tests::World::new().await;
        let (_, cookie, csrf) = &w.people[0];
        let t = token(&w.app, cookie, csrf, json!({ "name": "claude" })).await;
        let (s, card) = w.as_("admin", "GET", "/api/settings/mcp", None).await;
        assert_eq!((s, card["enabled"].clone()), (StatusCode::OK, json!(true)), "{card}");
        assert_eq!(initialize(&w.app, &t).await.0, StatusCode::OK);

        assert_eq!(w.as_("admin", "PUT", "/api/settings/mcp", Some(json!({ "enabled": false }))).await.0, StatusCode::NO_CONTENT);
        let (s, b) = initialize(&w.app, &t).await;
        assert!(s == StatusCode::FORBIDDEN && b["error"]["message"].as_str().unwrap().contains("turned off"), "{b}");
        assert_eq!(call(&w.app, mcp(None, None, req(2, "tools/list", json!({})))).await.0, StatusCode::FORBIDDEN, "off says so before it asks for a token");
        let (s, st) = w.as_("deckhand", "GET", "/api/mcp/status", None).await;
        assert_eq!((s, &st["enabled"]), (StatusCode::OK, &json!(false)), "anybody reads whether it is on, for Account");

        assert_eq!(w.as_("admin", "PUT", "/api/settings/mcp", Some(json!({ "enabled": true }))).await.0, StatusCode::NO_CONTENT);
        assert_eq!(initialize(&w.app, &t).await.0, StatusCode::OK);
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=settings.mcp", None).await;
        let details: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["detail"].as_str().unwrap()).collect();
        assert_eq!(details, vec!["on", "off"], "both changes logged, newest first");
    }

    /// The card's tokens are everybody's, with their owners; only an Administrator sees them or revokes one there.
    #[tokio::test]
    async fn the_card_lists_everybody_s_tokens_and_is_an_administrator_s() {
        let w = crate::access::api_tests::World::new().await;
        let (_, cookie, csrf) = &w.people[0];
        let mine = token(&w.app, cookie, csrf, json!({ "name": "admin-agent" })).await;
        let (_, cookie, csrf) = w.people.iter().find(|p| p.0 == "deckhand").unwrap();
        let theirs = token(&w.app, cookie, csrf, json!({ "name": "deckhand-agent", "read_only": true })).await;
        initialize(&w.app, &theirs).await;

        let (s, card) = w.as_("admin", "GET", "/api/settings/mcp", None).await;
        assert_eq!((s, card["path"].clone(), card["active"].clone()), (StatusCode::OK, json!("/mcp"), json!(2)), "{card}");
        let row = card["tokens"].as_array().unwrap().iter().find(|k| k["name"] == "deckhand-agent").unwrap().clone();
        assert_eq!((row["owner"]["name"].clone(), row["read_only"].clone()), (json!("deckhand"), json!(true)));
        assert!(row["last_used_at"].is_i64(), "its last use: {row}");
        assert!(!card.to_string().contains(&theirs[4..]), "never the token itself");
        let id = row["id"].as_i64().unwrap();
        for who in ["deckhand", "nobody"] {
            assert_eq!(w.as_(who, "GET", "/api/settings/mcp", None).await.0, StatusCode::FORBIDDEN, "{who} reads the card");
            assert_eq!(w.as_(who, "DELETE", &format!("/api/settings/mcp/tokens/{id}"), None).await.0, StatusCode::FORBIDDEN, "{who} revokes");
        }
        let by_token = Request::delete(format!("/api/settings/mcp/tokens/{id}")).header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {mine}")).body(Body::empty()).unwrap();
        assert_eq!(call(&w.app, by_token).await.0, StatusCode::FORBIDDEN, "a token does not revoke tokens");
        assert_eq!(w.as_("admin", "DELETE", &format!("/api/settings/mcp/tokens/{id}"), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(initialize(&w.app, &theirs).await.0, StatusCode::UNAUTHORIZED, "revoked at once");
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=token.revoke", None).await;
        assert_eq!((log["entries"][0]["target"].clone(), log["entries"][0]["detail"].clone()), (json!("deckhand-agent"), json!("deckhand's")));
    }
}
