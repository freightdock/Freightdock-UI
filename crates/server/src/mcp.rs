//! `/mcp`: Freightdock as a Model Context Protocol server, so an AI agent (Claude Code and the rest) can manage the Docker
//! hosts through it. The Streamable HTTP transport: JSON-RPC 2.0, one message per POST, each answered as plain JSON
//! (nothing here needs a stream), a session id in `Mcp-Session-Id` from `initialize` on ("MCP" in DECISIONS.md).
//!
//! Signed in by API token only. Every tool is a request to Freightdock's own HTTP API, made with the caller's token
//! through the same router, so it meets the same guards, the same Docker code and the same activity log as the UI: a
//! read-only token or a user who is not an administrator is refused a change here exactly as there ("MCP" 2).

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
/// A container's logs, kept to their last this many bytes unless asked for more (and never more than `MAX_TEXT`).
pub const LOG_BYTES: usize = 32 * 1024;

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
const OFF: &str = "The MCP server is turned off in this Freightdock. An administrator turns it on in Settings, AI agents (MCP).";

/// The switch in Settings, AI agents (MCP): one for the whole instance ("MCP" 6).
#[derive(Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

impl Default for Settings {
    /// On: an instance that had MCP before the switch keeps it, and a token is needed all the same.
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

/// `DELETE /api/settings/mcp/tokens/{id}`: an administrator revokes anybody's token, from a session (as a token cannot
/// revoke one, "Sign-in" 10).
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

/// The sessions `initialize` began: each is its token's, so another token cannot carry it on.
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

/// `%`-escapes for a query value.
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

    /// The environment a tool's `env` names: its id, or its name; the local one when there is none.
    async fn env(&self, args: &Value) -> Result<i64, String> {
        let asked = &args["env"];
        if let Some(id) = asked.as_i64() {
            return Ok(id);
        }
        if let Some(id) = asked.as_str().and_then(|s| s.trim().parse::<i64>().ok()) {
            return Ok(id);
        }
        let list = self.json(Method::GET, "/api/environments", None).await?;
        let list = list.as_array().cloned().unwrap_or_default();
        let found = match asked.as_str() {
            Some(name) => list.iter().find(|e| e["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name.trim()))),
            None if asked.is_null() => list.iter().find(|e| e["kind"] == "local").or(list.first()),
            None => return Err("env is an environment's id or name.".into()),
        };
        found.and_then(|e| e["id"].as_i64()).ok_or_else(|| {
            if list.is_empty() {
                return "You have no role on any environment. Ask an administrator for one.".to_string();
            }
            let names: Vec<&str> = list.iter().filter_map(|e| e["name"].as_str()).collect();
            format!("No environment {asked}. There are: {}.", names.join(", "))
        })
    }
}

/// A string argument that names a container, image or volume as a path segment of the API.
fn reference(args: &Value, key: &str) -> Result<String, String> {
    let r = args[key].as_str().map(str::trim).unwrap_or_default();
    if r.is_empty() {
        return Err(format!("Say which: {key} is required."));
    }
    if !crate::docker::valid_ref(r) {
        return Err(format!("\"{r}\" is not a {key}'s id or name."));
    }
    Ok(r.to_string())
}

/// When logs start from: Unix seconds, or a time ago such as `30s`, `10m`, `2h`, `1d`.
pub fn since(v: &Value, at: i64) -> Result<Option<i64>, String> {
    let bad = || format!("since is Unix seconds or a time ago such as 10m, 2h or 1d, not {v}.");
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n.as_i64().filter(|n| *n >= 0).map(Some).ok_or_else(bad),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(n) = s.parse::<i64>() {
                return Ok(Some(n.max(0)));
            }
            let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
            let n: i64 = n.parse().map_err(|_| bad())?;
            let per = match unit {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                "d" => 86_400,
                _ => return Err(bad()),
            };
            Ok(Some((at - n * per).max(0)))
        }
        _ => Err(bad()),
    }
}

/// A short id as `docker ps` shows one.
fn short(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(12).collect()
}

/// A container in a list, as a person reads `docker ps`: what it is, what it runs, how it is, what it publishes.
pub fn container_row(c: &Value) -> Value {
    let mut ports: Vec<String> = c["Ports"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let inner = format!("{}/{}", p["PrivatePort"], p["Type"].as_str().unwrap_or("tcp"));
            match p["PublicPort"].as_i64() {
                Some(public) => format!("{}:{public}->{inner}", p["IP"].as_str().unwrap_or("")),
                None => inner,
            }
        })
        .collect();
    ports.dedup();
    let name = c["Names"][0].as_str().unwrap_or_default().trim_start_matches('/');
    let mut row = json!({
        "id": short(c["Id"].as_str().unwrap_or_default()), "name": name, "image": c["Image"], "state": c["State"],
        "status": c["Status"], "created": c["Created"], "ports": ports,
    });
    if let Some(p) = c["Labels"]["com.docker.compose.project"].as_str() {
        row["compose_project"] = json!(p);
    }
    row
}

fn image_row(i: &Value) -> Value {
    json!({ "id": short(i["Id"].as_str().unwrap_or_default()), "tags": i["RepoTags"], "size": i["Size"], "created": i["Created"], "containers": i["containers"] })
}

fn volume_row(v: &Value) -> Value {
    json!({ "name": v["Name"], "driver": v["Driver"], "mountpoint": v["Mountpoint"], "created": v["CreatedAt"], "containers": v["containers"] })
}

fn network_row(n: &Value) -> Value {
    json!({ "id": short(n["Id"].as_str().unwrap_or_default()), "name": n["Name"], "driver": n["Driver"], "scope": n["Scope"], "internal": n["Internal"], "containers": n["container_count"] })
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

fn env_prop() -> Value {
    json!({ "type": ["integer", "string"], "description": "The environment (Docker host): its id or its name, as list_environments shows them. The local Docker when left out." })
}

fn container_prop() -> Value {
    json!({ "type": "string", "description": "The container's name or id (the 12-character short id will do)." })
}

/// One tool, as `tools/list` describes it.
/// What a tool's annotations say it does: whether it only reads, may destroy, and is the same done twice.
#[derive(Clone, Copy)]
struct Hints {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
}

const READ: Hints = Hints { read_only: true, destructive: false, idempotent: true };
/// A change that can be undone, and done again to no further effect (start, stop, pull).
const CHANGE: Hints = Hints { read_only: false, destructive: false, idempotent: true };
/// A change that makes something new each time (create).
const MAKE: Hints = Hints { read_only: false, destructive: false, idempotent: false };
/// A change that loses something (remove, kill).
const DESTROY: Hints = Hints { read_only: false, destructive: true, idempotent: true };
/// A command inside a container: it may do anything, each time.
const RUN: Hints = Hints { read_only: false, destructive: true, idempotent: false };

fn tool(name: &str, title: &str, description: &str, props: Value, required: &[&str], hints: Hints) -> Value {
    let Hints { read_only, destructive, idempotent } = hints;
    let mut properties = json!({ "env": env_prop() });
    for (k, v) in props.as_object().into_iter().flatten() {
        properties[k] = v.clone();
    }
    json!({
        "name": name, "title": title, "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "title": title, "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": name == "pull_image" },
    })
}

/// Every tool there is. Reads are any signed-in token's; changes need an administrator's token that is not read-only,
/// as the API has it.
pub fn tools() -> Vec<Value> {
    let c = container_prop;
    let action = |verb: &str, what: &str, destructive: bool| {
        tool(&format!("{verb}_container"), &format!("{} a container", capitalise(verb)), what, json!({ "container": c() }), &["container"], if destructive { DESTROY } else { CHANGE })
    };
    let mut list = vec![
        json!({
            "name": "list_environments", "title": "List environments",
            "description": "Every Docker host Freightdock manages, with its id, name, kind (local, agent, tls), health (status up/down/unknown, last_seen_at, last_error) and Docker version. Use an id or name as `env` in the other tools.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "title": "List environments", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
        }),
        tool("list_containers", "List containers", "The containers on a host, as `docker ps -a` shows them: short id, name, image, state, status, created (Unix time), published ports and compose project.",
            json!({ "all": { "type": "boolean", "default": true, "description": "Stopped ones too. false for running only." }, "name": { "type": "string", "description": "Only those whose name has this in it." } }), &[], READ),
        tool("inspect_container", "Inspect a container", "Everything Docker knows about one container (docker inspect, with its size): config, host config, state, mounts, networks.",
            json!({ "container": c() }), &["container"], READ),
        tool("container_logs", "A container's logs", "The last lines a container wrote, standard output and error together, as `docker logs --tail` gives them. Kept to their last max_bytes.",
            json!({
                "container": c(),
                "tail": { "type": ["integer", "string"], "default": 200, "description": "Lines from the end: a number (at most 5000), or \"all\"." },
                "since": { "type": ["integer", "string"], "description": "Only lines from then on: Unix seconds, or a time ago such as \"30s\", \"10m\", \"2h\", \"1d\"." },
                "timestamps": { "type": "boolean", "default": false, "description": "Docker's time before each line." },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_TEXT, "default": LOG_BYTES, "description": "Keep this many bytes from the end." },
            }), &["container"], READ),
        action("start", "Starts a stopped container. Starting one that runs is no error.", false),
        action("stop", "Stops a running container (SIGTERM, then SIGKILL after its stop timeout). Stopping one that is stopped is no error.", false),
        action("restart", "Stops and starts a container again.", false),
        action("kill", "Kills a container's main process at once (SIGKILL), without letting it finish.", true),
        tool("remove_container", "Remove a container", "Removes a container for good. A running one needs force. Its named volumes stay; its anonymous ones go only with volumes.",
            json!({ "container": c(), "force": { "type": "boolean", "default": false, "description": "Kill it first if it runs." }, "volumes": { "type": "boolean", "default": false, "description": "Its anonymous volumes too." } }), &["container"], DESTROY),
        tool("create_container", "Create a container", "Makes a container from Docker's own create body (POST /containers/create in the Docker Engine API), pulling the image first when it is not on the host, and starts it unless start is false. Answers its id.",
            json!({
                "name": { "type": "string", "description": "Its name: a letter or digit, then letters, digits, _ . or -. Docker makes one up when left out." },
                "config": { "type": "object", "additionalProperties": true, "required": ["Image"],
                    "description": "Docker's create body, keys as Docker spells them. For example {\"Image\":\"nginx:1.27\",\"Env\":[\"A=b\"],\"Cmd\":[\"sleep\",\"60\"],\"Labels\":{},\"ExposedPorts\":{\"80/tcp\":{}},\"HostConfig\":{\"PortBindings\":{\"80/tcp\":[{\"HostPort\":\"8080\"}]},\"Binds\":[\"data:/data\"],\"RestartPolicy\":{\"Name\":\"unless-stopped\"}}}.",
                    "properties": { "Image": { "type": "string", "description": "The image, such as nginx:1.27." } } },
                "pull": { "type": "boolean", "default": false, "description": "Pull the image even when it is here, for a newer one under the same tag." },
                "start": { "type": "boolean", "default": true, "description": "Start it once made." },
            }), &["config"], MAKE),
        tool("exec_command", "Run a command in a container", "Runs a command inside a running container without a terminal, to its end or its time limit, and answers exit_code, stdout and stderr (each kept to its last 64 KiB). A command past its time is left running and answered with timed_out. Not a shell: for pipes or globs use [\"sh\", \"-c\", \"...\"].",
            json!({
                "container": c(),
                "cmd": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "The command as words, such as [\"ls\", \"-l\", \"/data\"]." },
                "user": { "type": "string", "description": "Run as this user (name or uid[:gid]); the container's own when left out." },
                "workdir": { "type": "string", "description": "Run in this folder." },
                "env": { "type": "array", "items": { "type": "string" }, "description": "More environment, KEY=value." },
                "timeout_s": { "type": "integer", "minimum": 1, "maximum": 600, "default": 30, "description": "Seconds it may take." },
            }), &["container", "cmd"], RUN),
        tool("container_stats", "Container stats", "One sample of what containers use: CPU (percent of one CPU, so 200 is two flat out), memory without the page cache and its limit (bytes), network and disk bytes since start, and processes. One container, or every running one keyed by id.",
            json!({ "container": { "type": "string", "description": "One container's name or id; every running one when left out." } }), &[], READ),
        tool("list_images", "List images", "The images on a host: short id, tags, size (bytes), created (Unix time) and how many containers use each.", json!({}), &[], READ),
        tool("pull_image", "Pull an image", "Pulls an image from its registry and answers once it is done, with Docker's last status lines (or the error).",
            json!({ "image": { "type": "string", "description": "The image, such as nginx:1.27, ghcr.io/owner/app:2 or alpine (meaning alpine:latest)." } }), &["image"], CHANGE),
        tool("remove_image", "Remove an image", "Removes an image (or one of its tags, named by tag). One a container uses needs force.",
            json!({ "image": { "type": "string", "description": "The image's id, or a tag such as nginx:1.27." }, "force": { "type": "boolean", "default": false, "description": "Even if containers use it, or it has several tags." } }), &["image"], DESTROY),
        tool("list_volumes", "List volumes", "The volumes on a host: name, driver, mountpoint, created, and which containers use each.", json!({}), &[], READ),
        tool("list_networks", "List networks", "The networks on a host: short id, name, driver, scope, whether internal, and how many containers are on each.", json!({}), &[], READ),
        tool("disk_usage", "Disk usage", "What Docker uses on a host's disk, as `docker system df -v` counts it: images, containers, volumes and the build cache.", json!({}), &[], READ),
    ];
    list.push(json!({
        "name": "activity_log", "title": "Activity log",
        "description": "Freightdock's activity log, newest first: who did what to which thing, when (Unix time), from where, and with which API token (via). An administrator's only.",
        "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
            "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50, "description": "How many entries." },
            "action": { "type": "string", "description": "Only this action or family, such as container.stop, or container for all of them." },
            "user": { "type": "string", "description": "Only this user's." },
        } },
        "annotations": { "title": "Activity log", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
    }));
    list
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

async fn run(caller: &Caller, name: &str, args: &Value) -> Result<Value, String> {
    if name == "list_environments" {
        return caller.json(Method::GET, "/api/environments", None).await;
    }
    if name == "activity_log" {
        let mut q = vec![("limit", args["limit"].as_i64().unwrap_or(50).clamp(1, 500).to_string())];
        for k in ["action", "user"] {
            if let Some(v) = args[k].as_str().filter(|v| !v.is_empty()) {
                q.push((k, v.to_string()));
            }
        }
        let a = caller.json(Method::GET, &format!("/api/audit{}", query(&q)), None).await?;
        return Ok(a["entries"].clone());
    }
    let env = caller.env(args).await?;
    let e = |p: &str| format!("/api/envs/{env}{p}");
    match name {
        "list_containers" => {
            let all = args["all"].as_bool().unwrap_or(true);
            let list = caller.json(Method::GET, &e(&format!("/containers?all={all}")), None).await?;
            let filter = args["name"].as_str().unwrap_or_default().to_lowercase();
            let rows: Vec<Value> = list.as_array().into_iter().flatten().map(container_row).filter(|r| r["name"].as_str().unwrap_or_default().to_lowercase().contains(&filter)).collect();
            Ok(json!(rows))
        }
        "inspect_container" => caller.json(Method::GET, &e(&format!("/containers/{}", reference(args, "container")?)), None).await,
        "container_logs" => {
            let id = reference(args, "container")?;
            let tail = match &args["tail"] {
                Value::Null => "200".to_string(),
                Value::String(s) if s == "all" => s.clone(),
                v => v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).filter(|n| *n >= 0).map(|n| n.min(5000).to_string()).ok_or("tail is a number or \"all\".")?,
            };
            let mut q = vec![("tail", tail)];
            if let Some(s) = since(&args["since"], now())? {
                q.push(("since", s.min(i32::MAX as i64).to_string()));
            }
            if args["timestamps"].as_bool() == Some(true) {
                q.push(("timestamps", "true".into()));
            }
            let a = caller.api(Method::GET, &e(&format!("/containers/{id}/logs{}", query(&q))), None).await?;
            if !a.status.is_success() {
                return Err(a.refusal());
            }
            let keep = args["max_bytes"].as_u64().map_or(LOG_BYTES, |n| (n as usize).clamp(1, MAX_TEXT));
            let (logs, cut) = crate::docker::streams::last_bytes(&a.body, keep);
            Ok(json!({ "logs": logs, "truncated": cut }))
        }
        "start_container" | "stop_container" | "restart_container" | "kill_container" => {
            let id = reference(args, "container")?;
            let verb = name.trim_end_matches("_container");
            caller.json(Method::POST, &e(&format!("/containers/{id}/{verb}")), None).await?;
            Ok(json!({ "ok": true, "container": id, "done": verb }))
        }
        "remove_container" => {
            let id = reference(args, "container")?;
            let q = [("force", args["force"].as_bool().unwrap_or(false).to_string()), ("volumes", args["volumes"].as_bool().unwrap_or(false).to_string())];
            caller.json(Method::DELETE, &e(&format!("/containers/{id}{}", query(&q))), None).await?;
            Ok(json!({ "ok": true, "removed": id }))
        }
        "create_container" => {
            if !args["config"].is_object() {
                return Err("config is Docker's create body, an object with at least Image.".into());
            }
            let body = json!({ "name": args["name"], "config": args["config"], "pull": args["pull"].as_bool().unwrap_or(false), "start": args["start"].as_bool().unwrap_or(true) });
            caller.json(Method::POST, &e("/containers"), Some(&body)).await
        }
        "exec_command" => {
            let id = reference(args, "container")?;
            let body = json!({ "cmd": args["cmd"], "user": args["user"], "workdir": args["workdir"], "env": args["env"], "timeout_s": args["timeout_s"] });
            caller.json(Method::POST, &e(&format!("/containers/{id}/exec")), Some(&body)).await
        }
        "container_stats" => match args["container"].as_str().filter(|s| !s.is_empty()) {
            Some(_) => caller.json(Method::GET, &e(&format!("/containers/{}/stats", reference(args, "container")?)), None).await,
            None => caller.json(Method::GET, &e("/stats"), None).await,
        },
        "list_images" => Ok(json!(caller.json(Method::GET, &e("/images"), None).await?.as_array().into_iter().flatten().map(image_row).collect::<Vec<_>>())),
        "pull_image" => {
            let image = args["image"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("Say which image: image is required.")?;
            let a = caller.api(Method::POST, &e("/images/pull"), Some(&json!({ "image": image }))).await?;
            if !a.status.is_success() {
                return Err(a.refusal());
            }
            pulled(image, &a.body)
        }
        "remove_image" => {
            let image = args["image"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("Say which image: image is required.")?;
            // A tag with a registry or owner in it has slashes, which a path cannot carry: it goes by its id.
            let id = if crate::docker::valid_ref(image) { image.to_string() } else { image_id(caller, &e("/images"), image).await? };
            let q = [("force", args["force"].as_bool().unwrap_or(false).to_string())];
            caller.json(Method::DELETE, &e(&format!("/images/{id}{}", query(&q))), None).await
        }
        "list_volumes" => Ok(json!(caller.json(Method::GET, &e("/volumes"), None).await?.as_array().into_iter().flatten().map(volume_row).collect::<Vec<_>>())),
        "list_networks" => Ok(json!(caller.json(Method::GET, &e("/networks"), None).await?.as_array().into_iter().flatten().map(network_row).collect::<Vec<_>>())),
        "disk_usage" => caller.json(Method::GET, &e("/df"), None).await,
        _ => Err(format!("There is no tool {name}.")),
    }
}

/// The id of the image a tag names, when the tag cannot be a path segment.
async fn image_id(caller: &Caller, path: &str, tag: &str) -> Result<String, String> {
    let list = caller.json(Method::GET, path, None).await?;
    let want = if tag.rsplit('/').next().is_some_and(|last| last.contains(':') || last.contains('@')) { tag.to_string() } else { format!("{tag}:latest") };
    list.as_array()
        .into_iter()
        .flatten()
        .find(|i| i["RepoTags"].as_array().into_iter().flatten().any(|t| t.as_str() == Some(want.as_str())))
        .and_then(|i| i["Id"].as_str().map(str::to_string))
        .ok_or_else(|| format!("No image is tagged {want} on this host."))
}

/// A pull's NDJSON, read to its end: its error, or that it is done with Docker's last words about it.
pub fn pulled(image: &str, ndjson: &[u8]) -> Result<Value, String> {
    let lines: Vec<Value> = String::from_utf8_lossy(ndjson).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if let Some(e) = lines.iter().find_map(|l| l["error"].as_str()) {
        return Err(format!("The pull of {image} failed: {e}"));
    }
    let done = lines.last().is_some_and(|l| l["done"] == json!(true));
    // The lines without a layer are about the whole image: its digest and how the pull ended.
    let mut status: Vec<String> = lines.iter().filter(|l| l["id"].is_null()).filter_map(|l| l["status"].as_str().map(str::to_string)).collect();
    status.dedup();
    let from = status.len().saturating_sub(5);
    if !done {
        return Err(format!("The pull of {image} ended before it was done. Docker said: {}", status[from..].join(" / ")));
    }
    Ok(json!({ "image": image, "done": true, "status": status[from..] }))
}

/// What `initialize` answers.
fn initialized(asked: Option<&str>) -> Value {
    json!({
        "protocolVersion": pick_version(asked),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "freightdock", "title": "Freightdock", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Freightdock manages Docker hosts (environments). list_environments shows them; every other tool takes env, an environment's id or name, and works on the local Docker when it is left out. You may do what the token's owner may: reads for anybody, changes only with an administrator's token that is not read-only. Every change is written to Freightdock's activity log under the token's name. Containers are named by name or id; prefer list_containers before acting on one.",
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
    fn since_is_unix_seconds_or_a_time_ago() {
        assert_eq!(since(&Value::Null, 1000), Ok(None));
        assert_eq!(since(&json!(500), 1000), Ok(Some(500)));
        assert_eq!(since(&json!("500"), 1000), Ok(Some(500)));
        assert_eq!(since(&json!("10m"), 1000), Ok(Some(400)));
        assert_eq!(since(&json!("1d"), 1000), Ok(Some(0)), "never before the start of time");
        assert!(since(&json!("10 minutes"), 1000).is_err());
        assert!(since(&json!(-5), 1000).is_err());
        assert!(since(&json!(true), 1000).is_err());
    }

    #[test]
    fn long_text_is_cut_on_a_character_and_says_so() {
        assert_eq!(capped("short".into(), 10), "short");
        let c = capped("ééééé".into(), 3);
        assert!(c.starts_with("é\n[cut here: 8 more bytes"), "{c}");
    }

    #[test]
    fn a_container_reads_as_docker_ps_shows_it() {
        let c = json!({
            "Id": "3f2a9c1b7d4e5f60718293a4", "Names": ["/web"], "Image": "nginx:1.27", "State": "running", "Status": "Up 2 hours", "Created": 1,
            "Ports": [{ "IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp" }, { "IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp" }, { "PrivatePort": 443, "Type": "tcp" }],
            "Labels": { "com.docker.compose.project": "site" },
        });
        let r = container_row(&c);
        assert_eq!(r["id"], "3f2a9c1b7d4e");
        assert_eq!(r["name"], "web");
        assert_eq!(r["ports"], json!(["0.0.0.0:8080->80/tcp", "443/tcp"]));
        assert_eq!(r["compose_project"], "site");
    }

    #[test]
    fn a_pull_is_done_or_says_why_not() {
        let ok = b"{\"status\":\"Pulling from library/alpine\",\"id\":\"3.20\"}\n{\"status\":\"Digest: sha256:abc\"}\n{\"status\":\"Status: Downloaded newer image for alpine:3.20\"}\n{\"done\":true}\n";
        let v = pulled("alpine:3.20", ok).unwrap();
        assert_eq!(v["done"], true);
        assert_eq!(v["status"], json!(["Digest: sha256:abc", "Status: Downloaded newer image for alpine:3.20"]));
        let failed = b"{\"status\":\"Pulling\"}\n{\"error\":\"manifest unknown\"}\n";
        assert!(pulled("x", failed).unwrap_err().contains("manifest unknown"));
        assert!(pulled("x", b"{\"status\":\"Pulling\"}\n").unwrap_err().contains("before it was done"));
    }

    #[test]
    fn every_tool_has_a_schema_a_description_and_says_whether_it_changes_anything() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for want in ["list_environments", "list_containers", "inspect_container", "container_logs", "start_container", "stop_container", "restart_container", "kill_container", "remove_container", "create_container", "exec_command", "container_stats", "list_images", "pull_image", "remove_image", "list_volumes", "list_networks", "disk_usage", "activity_log"] {
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
        assert!(ro("list_containers") && ro("container_logs") && ro("activity_log"));
        assert!(!ro("stop_container") && !ro("exec_command") && !ro("pull_image"));
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

    async fn setup(read_only: bool) -> (App, String, String) {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = token(&app, &cookie, &csrf, json!({ "name": "claude", "read_only": read_only })).await;
        (app, cookie, t)
    }

    #[tokio::test]
    async fn mcp_wants_a_token_and_no_cookie_will_do() {
        let (app, cookie, _) = setup(false).await;
        let (s, h, b) = call(&app, mcp(None, None, req(1, "initialize", json!({})))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(h[header::WWW_AUTHENTICATE].to_str().unwrap().starts_with("Bearer"));
        assert!(b["error"]["message"].as_str().unwrap().contains("API token"));
        let mut r = mcp(None, None, req(1, "initialize", json!({})));
        r.headers_mut().insert(header::COOKIE, cookie.parse().unwrap());
        let (s, _, _) = call(&app, r).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a browser's cookie is not a way in");
        let (s, _, _) = call(&app, mcp(Some(&format!("fdk_{}", "1".repeat(64))), None, req(1, "initialize", json!({})))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn initialize_answers_the_version_and_a_session_and_tools_are_listed() {
        let (app, _, t) = setup(false).await;
        let init = req(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }));
        let (s, h, b) = call(&app, mcp(Some(&t), None, init)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["jsonrpc"], "2.0");
        assert_eq!(b["id"], 1);
        assert_eq!(b["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(b["result"]["serverInfo"]["name"], "freightdock");
        assert!(b["result"]["capabilities"]["tools"].is_object());
        let session = h[SESSION_HEADER].to_str().unwrap().to_string();

        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let (s, _, _) = call(&app, mcp(Some(&t), Some(&session), note)).await;
        assert_eq!(s, StatusCode::ACCEPTED, "a notification is taken, not answered");

        let (s, _, b) = call(&app, mcp(Some(&t), Some(&session), req(2, "tools/list", json!({})))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["id"], 2);
        assert!(b["result"]["tools"].as_array().unwrap().len() >= 19);

        let (s, _, b) = call(&app, mcp(Some(&t), Some(&session), req(3, "ping", json!({})))).await;
        assert_eq!((s, &b["result"]), (StatusCode::OK, &json!({})));

        let (s, _, _) = call(&app, mcp(Some(&t), Some("not-a-session"), req(4, "tools/list", json!({})))).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "a session nobody began");

        let end = |s: &str| Request::delete("/mcp").header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {t}")).header(SESSION_HEADER, s).body(Body::empty()).unwrap();
        let (s, _, _) = call(&app, end(&session)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = call(&app, mcp(Some(&t), Some(&session), req(5, "tools/list", json!({})))).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "ended");
    }

    #[tokio::test]
    async fn what_is_not_json_rpc_gets_json_rpc_s_error_codes() {
        let (app, _, t) = setup(false).await;
        let raw = Request::post("/mcp").header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::from("{not json")).unwrap();
        let (s, _, b) = call(&app, raw).await;
        assert_eq!((s, b["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(PARSE_ERROR)));
        let (s, _, b) = call(&app, mcp(Some(&t), None, json!([req(1, "ping", json!({}))]))).await;
        assert_eq!((s, b["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(INVALID_REQUEST)));
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "resources/list", json!({})))).await;
        assert_eq!(b["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(b["id"], 1);
        let (_, _, b) = call(&app, mcp(Some(&t), None, json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }))).await;
        assert_eq!(b["error"]["code"], INVALID_REQUEST);
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": "format_the_disk", "arguments": {} })))).await;
        assert_eq!(b["error"]["code"], INVALID_PARAMS, "an unknown tool is the protocol's error");
        let (s, _, _) = call(&app, mcp(Some(&t), None, json!({ "jsonrpc": "2.0", "id": 9, "result": {} }))).await;
        assert_eq!(s, StatusCode::ACCEPTED, "a response is taken");
        let mut r = mcp(Some(&t), None, req(1, "ping", json!({})));
        r.headers_mut().insert(VERSION_HEADER, "1999-01-01".parse().unwrap());
        let (s, _, _) = call(&app, r).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "a protocol version we do not speak");
        let mut r = mcp(Some(&t), None, req(1, "ping", json!({})));
        r.headers_mut().insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let (s, _, _) = call(&app, r).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "a page elsewhere");
        let (s, _, _) = call(&app, Request::get("/mcp").header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "no stream of its own");
    }

    fn text(b: &Value) -> &str {
        b["result"]["content"][0]["text"].as_str().unwrap_or_default()
    }

    #[tokio::test]
    async fn a_tool_goes_through_the_api_and_a_failure_is_the_tool_s_not_the_protocol_s() {
        let (app, _, t) = setup(false).await;
        let (s, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": "list_environments", "arguments": {} })))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["result"]["isError"], false);
        let envs: Value = serde_json::from_str(text(&b)).unwrap();
        assert_eq!(envs[0]["name"], "local");

        let (_, _, b) = call(&app, mcp(Some(&t), None, req(2, "tools/call", json!({ "name": "list_containers", "arguments": { "env": "local" } })))).await;
        assert_eq!(b["result"]["isError"], true, "{b}");
        assert!(text(&b).contains("Freightdock answered 502") && text(&b).contains("Docker"), "the test Docker is not there: {}", text(&b));

        let (_, _, b) = call(&app, mcp(Some(&t), None, req(3, "tools/call", json!({ "name": "list_containers", "arguments": { "env": "nowhere" } })))).await;
        assert_eq!(b["result"]["isError"], true);
        assert!(text(&b).contains("There are: local"), "{}", text(&b));

        let (_, _, b) = call(&app, mcp(Some(&t), None, req(4, "tools/call", json!({ "name": "activity_log", "arguments": { "limit": 5 } })))).await;
        assert_eq!(b["result"]["isError"], false);
        assert!(text(&b).contains("token.create"), "{}", text(&b));

        let (_, _, b) = call(&app, mcp(Some(&t), None, req(5, "tools/call", json!({ "name": "stop_container", "arguments": {} })))).await;
        assert_eq!(b["result"]["isError"], true);
        assert!(text(&b).contains("container is required"));
    }

    #[tokio::test]
    async fn a_read_only_token_reads_through_mcp_and_is_refused_a_change_as_the_api_refuses_it() {
        let (app, cookie, t) = setup(true).await;
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": "list_environments", "arguments": {} })))).await;
        assert_eq!(b["result"]["isError"], false);
        for (name, args) in [("stop_container", json!({ "container": "web" })), ("exec_command", json!({ "container": "web", "cmd": ["true"] })), ("pull_image", json!({ "image": "alpine" }))] {
            let (_, _, b) = call(&app, mcp(Some(&t), None, req(2, "tools/call", json!({ "name": name, "arguments": args })))).await;
            assert_eq!(b["result"]["isError"], true, "{name}");
            assert!(text(&b).contains("403") && text(&b).contains("may only read"), "{name}: {}", text(&b));
        }
        let (_, _, log) = call(&app, Request::get("/api/audit?action=container").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(log["entries"].as_array().unwrap().len(), 0, "nothing was done");
    }

    #[tokio::test]
    async fn a_user_s_token_is_refused_through_mcp_what_a_user_is_refused() {
        let app = app();
        signed_in(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let cookie = format!("freightdock_session={}", crate::auth::start(&app.db, 9, None, None).await.unwrap());
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        let t = token(&app, &cookie, me["csrf"].as_str().unwrap(), json!({ "name": "mine" })).await;
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": "kill_container", "arguments": { "container": "web" } })))).await;
        assert!(text(&b).contains("Freightdock answered 403"), "{}", text(&b));
        let (_, _, b) = call(&app, mcp(Some(&t), None, req(2, "tools/call", json!({ "name": "activity_log", "arguments": {} })))).await;
        assert!(text(&b).contains("Freightdock answered 403"), "the log is an administrator's: {}", text(&b));
    }

    /// The tools are the API's routes, so each role gets through MCP exactly what it gets through the API ("MCP" 2).
    #[tokio::test]
    async fn every_role_is_held_to_its_table_through_mcp_too() {
        let w = crate::access::api_tests::World::new().await;
        let tool = |t: String, name: &'static str, args: Value| {
            let app = w.app.clone();
            async move {
                let (_, _, b) = call(&app, mcp(Some(&t), None, req(1, "tools/call", json!({ "name": name, "arguments": args })))).await;
                (b["result"]["isError"] == json!(true), text(&b).to_string())
            }
        };
        let mut tokens = std::collections::HashMap::new();
        for (role, cookie, csrf) in &w.people {
            tokens.insert(*role, token(&w.app, cookie, csrf, json!({ "name": "agent" })).await);
        }
        let t = |r: &str| tokens[r].clone();
        for role in ["admin", "env_admin", "operator", "helpdesk", "standard", "read_only"] {
            let (err, out) = tool(t(role), "list_environments", json!({})).await;
            assert!(!err && out.contains("local"), "{role}: {out}");
            let (err, out) = tool(t(role), "list_containers", json!({})).await;
            assert!(!err, "{role}: {out}");
            assert_eq!(out.contains("secret"), !["standard", "read_only"].contains(&role), "{role} sees the administrators' container or not: {out}");
            let (err, out) = tool(t(role), "stop_container", json!({ "container": "web" })).await;
            assert_eq!(err, ["helpdesk", "read_only"].contains(&role), "{role} stop: {out}");
            let (err, out) = tool(t(role), "remove_container", json!({ "container": "web" })).await;
            assert_eq!(err, ["operator", "helpdesk", "read_only"].contains(&role), "{role} remove: {out}");
            w.seed().await;
            // The fake Docker has no exec stream: what counts is whether Freightdock refused it.
            let (_, out) = tool(t(role), "exec_command", json!({ "container": "web", "cmd": ["true"] })).await;
            assert_eq!(out.contains("answered 403"), ["helpdesk", "read_only"].contains(&role), "{role} exec: {out}");
            let (err, out) = tool(t(role), "activity_log", json!({})).await;
            assert_eq!(err, role != "admin", "{role} log: {out}");
        }
        let (err, out) = tool(t("read_only"), "inspect_container", json!({ "container": "mine" })).await;
        assert!(err && out.contains("404"), "another's private container is not there: {out}");
        let (err, out) = tool(t("nobody"), "list_environments", json!({})).await;
        assert!(!err && !out.contains("local"), "no role, no environment: {out}");
        let (err, out) = tool(t("nobody"), "list_containers", json!({})).await;
        assert!(err && out.contains("no role on any environment"), "{out}");
        let (err, out) = tool(t("nobody"), "list_containers", json!({ "env": 1 })).await;
        assert!(err && out.contains("404"), "{out}");
    }

    async fn initialize(app: &App, t: &str) -> (StatusCode, Value) {
        let (s, _, b) = call(app, mcp(Some(t), None, req(1, "initialize", json!({ "protocolVersion": "2025-06-18" })))).await;
        (s, b)
    }

    /// The switch in Settings ("MCP" 6): on until an administrator turns it off, and then nobody's token reaches a tool.
    #[tokio::test]
    async fn the_switch_turns_mcp_off_for_everybody_and_on_again() {
        let w = crate::access::api_tests::World::new().await;
        let (_, cookie, csrf) = &w.people[0];
        let t = token(&w.app, cookie, csrf, json!({ "name": "claude" })).await;
        let (s, card) = w.as_("admin", "GET", "/api/settings/mcp", None).await;
        assert_eq!(s, StatusCode::OK, "{card}");
        assert_eq!(card["enabled"], true, "on until somebody turns it off, so a connection that works keeps working");
        assert_eq!(initialize(&w.app, &t).await.0, StatusCode::OK);

        let (s, b) = w.as_("admin", "PUT", "/api/settings/mcp", Some(json!({ "enabled": false }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let (s, b) = initialize(&w.app, &t).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(b["error"]["message"].as_str().unwrap().contains("turned off"), "{b}");
        let (s, _, b) = call(&w.app, mcp(Some(&t), None, req(2, "tools/call", json!({ "name": "stop_container", "arguments": { "container": "web" } })))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "no tool is reached: {b}");
        let (s, _, _) = call(&w.app, mcp(None, None, req(2, "tools/list", json!({})))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "off says so before it asks for a token");
        let end = Request::delete("/mcp").header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::empty()).unwrap();
        assert_eq!(call(&w.app, end).await.0, StatusCode::FORBIDDEN);
        let (_, card) = w.as_("admin", "GET", "/api/settings/mcp", None).await;
        assert_eq!(card["enabled"], false);
        for role in ["env_admin", "standard", "read_only"] {
            let (s, st) = w.as_(role, "GET", "/api/mcp/status", None).await;
            assert_eq!((s, &st["enabled"]), (StatusCode::OK, &json!(false)), "{role} reads whether it is on, for Account");
        }

        let (s, _) = w.as_("admin", "PUT", "/api/settings/mcp", Some(json!({ "enabled": true }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(initialize(&w.app, &t).await.0, StatusCode::OK);
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=settings.mcp", None).await;
        let details: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["detail"].as_str().unwrap()).collect();
        assert_eq!(details, vec!["on", "off"], "both changes logged, newest first");
    }

    /// The card's tokens are everybody's, with their owners; only an administrator sees them or revokes one there.
    #[tokio::test]
    async fn the_card_lists_everybody_s_tokens_and_is_an_administrator_s() {
        let w = crate::access::api_tests::World::new().await;
        let mut made = Vec::new();
        for (role, cookie, csrf) in &w.people {
            if ["admin", "standard"].contains(role) {
                made.push(token(&w.app, cookie, csrf, json!({ "name": format!("{role}-agent"), "read_only": *role == "standard" })).await);
            }
        }
        // One that has run out: listed, not counted.
        let (_, cookie, csrf) = &w.people[0];
        token(&w.app, cookie, csrf, json!({ "name": "old", "expires_days": 1 })).await;
        w.app.db.call(|c| Ok(c.execute("UPDATE api_tokens SET expires_at = 1 WHERE name = 'old'", [])?)).await.unwrap();
        initialize(&w.app, &made[1]).await;

        let (s, card) = w.as_("admin", "GET", "/api/settings/mcp", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(card["path"], "/mcp");
        assert_eq!(card["active"], 2, "{card}");
        let list = card["tokens"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        let theirs = list.iter().find(|k| k["name"] == "standard-agent").unwrap();
        assert_eq!(theirs["owner"]["name"], "standard");
        assert_eq!(theirs["read_only"], true);
        assert!(theirs["last_used_at"].is_i64(), "its last use: {theirs}");
        assert_eq!(theirs["active"], true);
        assert_eq!(list.iter().find(|k| k["name"] == "old").unwrap()["active"], false);
        assert!(!card.to_string().contains(&made[1][4..]), "never the token itself");

        for role in ["env_admin", "operator", "helpdesk", "standard", "read_only", "nobody"] {
            assert_eq!(w.as_(role, "GET", "/api/settings/mcp", None).await.0, StatusCode::FORBIDDEN, "{role} reads the card");
            assert_eq!(w.as_(role, "PUT", "/api/settings/mcp", Some(json!({ "enabled": false }))).await.0, StatusCode::FORBIDDEN, "{role} turns it off");
            let id = theirs["id"].as_i64().unwrap();
            assert_eq!(w.as_(role, "DELETE", &format!("/api/settings/mcp/tokens/{id}"), None).await.0, StatusCode::FORBIDDEN, "{role} revokes");
        }
        let id = theirs["id"].as_i64().unwrap();
        let by_token = Request::delete(format!("/api/settings/mcp/tokens/{id}")).header(header::HOST, "freightdock.test").header(header::AUTHORIZATION, format!("Bearer {}", made[0])).body(Body::empty()).unwrap();
        assert_eq!(call(&w.app, by_token).await.0, StatusCode::FORBIDDEN, "a token does not revoke tokens, an administrator's neither");
        let (s, b) = w.as_("admin", "DELETE", &format!("/api/settings/mcp/tokens/{id}"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert_eq!(initialize(&w.app, &made[1]).await.0, StatusCode::UNAUTHORIZED, "revoked at once");
        assert_eq!(w.as_("admin", "DELETE", &format!("/api/settings/mcp/tokens/{id}"), None).await.0, StatusCode::NOT_FOUND);
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=token.revoke", None).await;
        assert_eq!(log["entries"][0]["target"], "standard-agent");
        assert_eq!(log["entries"][0]["detail"], "standard's");
    }
}
