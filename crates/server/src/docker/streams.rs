//! What flows: a container's logs, its live stats, a console inside it or attached to its main process, and the host's
//! events, each over a WebSocket that ends when the page leaves. A WebSocket is a GET, so the `Authed` extractor holds
//! it to the same-origin rule ("Sign-in" 4); a console and an attach are an administrator's.

use std::collections::HashMap;

use axum::Json;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use bollard::container::LogOutput;
use bollard::exec::{ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerStatsResponse, ExecConfig};
use bollard::query_parameters as q;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::{api_error, endpoint, valid_ref};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

fn check(id: &str) -> ApiResult<()> {
    if valid_ref(id) { Ok(()) } else { Err(ApiError::bad("That is not a container's id or name.")) }
}

#[derive(Deserialize, Default)]
pub struct LogQuery {
    /// How many lines from the end to start with: a number, or `all`.
    tail: Option<String>,
    /// Docker's time before each line.
    #[serde(default)]
    timestamps: bool,
    /// Lines from this Unix time on.
    since: Option<i32>,
}

impl LogQuery {
    fn options(&self, follow: bool) -> q::LogsOptions {
        let tail = match self.tail.as_deref() {
            Some("all") => "all".to_string(),
            Some(n) if n.parse::<u32>().is_ok() => n.to_string(),
            _ => "500".to_string(),
        };
        q::LogsOptions { follow, stdout: true, stderr: true, since: self.since.unwrap_or(0), until: 0, timestamps: self.timestamps, tail }
    }
}

fn chunk(o: LogOutput) -> Option<(&'static str, axum::body::Bytes)> {
    match o {
        LogOutput::StdOut { message } | LogOutput::Console { message } => Some(("out", message)),
        LogOutput::StdErr { message } => Some(("err", message)),
        LogOutput::StdIn { .. } => None,
    }
}

/// `GET .../containers/{id}/logs/ws`: the last lines, then every new one as it is written. Each message is JSON:
/// `{"s":"out"|"err","t":"text"}`, text as Docker sent it (lines may come split or together).
pub async fn logs_ws(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>, Query(lq): Query<LogQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    check(&id)?;
    let d = endpoint(&app, env).await?;
    Ok(ws.on_upgrade(move |mut socket| async move {
        let s = d.client().logs(&id, Some(lq.options(true)));
        futures_util::pin_mut!(s);
        loop {
            tokio::select! {
                next = s.next() => match next {
                    Some(Ok(o)) => {
                        let Some((stream, bytes)) = chunk(o) else { continue };
                        let msg = json!({ "s": stream, "t": String::from_utf8_lossy(&bytes) }).to_string();
                        if socket.send(Message::Text(msg.into())).await.is_err() { break; }
                    }
                    Some(Err(e)) => { let _ = socket.send(Message::Text(json!({ "error": api_error(e).message }).to_string().into())).await; break; }
                    None => { let _ = socket.send(Message::Text(json!({ "end": true }).to_string().into())).await; break; }
                },
                incoming = socket.recv() => if !matches!(incoming, Some(Ok(_))) { break },
            }
        }
    }))
}

/// `GET .../containers/{id}/logs`: the logs as a file to keep, stdout and stderr together, as `docker logs` shows them.
pub async fn logs_download(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>, Query(lq): Query<LogQuery>) -> ApiResult<Response> {
    check(&id)?;
    let d = endpoint(&app, env).await?;
    let client = d.client().clone();
    let opts = lq.options(false);
    let target = id.clone();
    let s = futures_util::stream::once(async move {
        client.logs(&target, Some(opts)).filter_map(|r| async move { r.ok().and_then(chunk).map(|(_, b)| Ok::<_, std::io::Error>(b)) })
    })
    .flatten();
    let file: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.').collect();
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}.log\""))], Body::from_stream(s)).into_response())
}

/// One sample of a container's use, as the page draws it.
#[derive(serde::Serialize, Debug, PartialEq, Default)]
pub struct Sample {
    /// Percent of one CPU (200 is two CPUs flat out), as `docker stats` counts it.
    pub cpu: f64,
    pub cpus: u32,
    /// Bytes in use, without the page cache the kernel can take back.
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub pids: u64,
}

/// `docker stats`' arithmetic: the container's CPU time since the last sample over the whole host's, times its CPUs.
pub fn sample(s: &ContainerStatsResponse) -> Sample {
    let cpu = s.cpu_stats.as_ref();
    let pre = s.precpu_stats.as_ref();
    let total = |c: Option<&bollard::models::ContainerCpuStats>| c.and_then(|c| c.cpu_usage.as_ref()).and_then(|u| u.total_usage).unwrap_or(0);
    let system = |c: Option<&bollard::models::ContainerCpuStats>| c.and_then(|c| c.system_cpu_usage).unwrap_or(0);
    let cpus = cpu
        .and_then(|c| c.online_cpus)
        .filter(|n| *n > 0)
        .or_else(|| cpu.and_then(|c| c.cpu_usage.as_ref()).and_then(|u| u.percpu_usage.as_ref()).map(|p| p.len() as u32))
        .unwrap_or(1);
    let dc = total(cpu).saturating_sub(total(pre)) as f64;
    let ds = system(cpu).saturating_sub(system(pre)) as f64;
    let cpu_pct = if dc > 0.0 && ds > 0.0 { dc / ds * cpus as f64 * 100.0 } else { 0.0 };
    let m = s.memory_stats.as_ref();
    let usage = m.and_then(|m| m.usage).unwrap_or(0);
    // cgroup v2 names it inactive_file, v1 total_inactive_file: the cache docker stats leaves out.
    let cache = m.and_then(|m| m.stats.as_ref()).and_then(|st| st.get("inactive_file").or_else(|| st.get("total_inactive_file")).copied()).unwrap_or(0);
    let (mut rx, mut tx) = (0, 0);
    for n in s.networks.iter().flat_map(|n| n.values()) {
        rx += n.rx_bytes.unwrap_or(0);
        tx += n.tx_bytes.unwrap_or(0);
    }
    let (mut read, mut write) = (0, 0);
    for e in s.blkio_stats.as_ref().and_then(|b| b.io_service_bytes_recursive.as_ref()).into_iter().flatten() {
        match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("read") => read += e.value.unwrap_or(0),
            Some("write") => write += e.value.unwrap_or(0),
            _ => {}
        }
    }
    Sample {
        cpu: (cpu_pct * 10.0).round() / 10.0,
        cpus,
        mem: usage.saturating_sub(cache),
        mem_limit: m.and_then(|m| m.limit).unwrap_or(0),
        net_rx: rx,
        net_tx: tx,
        disk_read: read,
        disk_write: write,
        pids: s.pids_stats.as_ref().and_then(|p| p.current).unwrap_or(0),
    }
}

/// The seconds a stats page may ask for between samples ("Streams" 5); five when it does not say.
pub const STATS_EVERY: [u64; 4] = [1, 5, 10, 30];

/// How often the stats socket sends: one of [`STATS_EVERY`], five seconds by default.
pub fn stats_every(asked: Option<u64>) -> Result<std::time::Duration, ApiError> {
    let s = asked.unwrap_or(5);
    if STATS_EVERY.contains(&s) { Ok(std::time::Duration::from_secs(s)) } else { Err(ApiError::bad(format!("A sample every {s} seconds is not offered: 1, 5, 10 or 30."))) }
}

#[derive(Deserialize, Default)]
pub struct StatsQuery {
    /// Seconds between samples.
    every: Option<u64>,
}

/// `GET .../containers/{id}/stats/ws?every=5`: the latest sample every few seconds while the page listens. Docker
/// sends one a second; the first goes out at once and then the newest at each tick, so a slower page asks Docker for
/// nothing more and is only sent less.
pub async fn stats_ws(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>, Query(sq): Query<StatsQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    check(&id)?;
    let every = stats_every(sq.every)?;
    let d = endpoint(&app, env).await?;
    let s = super::raw::follow::<ContainerStatsResponse>(&d, &stats_path(&id, true)).await?;
    Ok(ws.on_upgrade(move |mut socket| async move {
        futures_util::pin_mut!(s);
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (mut latest, mut sent_any) = (None::<String>, false);
        loop {
            tokio::select! {
                next = s.next() => match next {
                    Some(Ok(st)) => {
                        let msg = json!(sample(&st)).to_string();
                        if sent_any { latest = Some(msg); } else {
                            sent_any = true;
                            if socket.send(Message::Text(msg.into())).await.is_err() { break; }
                            tick.reset();
                        }
                    }
                    Some(Err(e)) => { let _ = socket.send(Message::Text(json!({ "error": e.message }).to_string().into())).await; break; }
                    None => break,
                },
                _ = tick.tick() => if let Some(msg) = latest.take() && socket.send(Message::Text(msg.into())).await.is_err() { break },
                incoming = socket.recv() => if !matches!(incoming, Some(Ok(_))) { break },
            }
        }
    }))
}

#[derive(Deserialize, Default)]
pub struct ExecQuery {
    /// The command, as words: `/bin/sh` by default.
    cmd: Option<String>,
    user: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

/// A command line as words: spaces part them; single or double quotes keep a word whole.
pub fn words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut any) = (vec![], String::new(), None::<char>, false);
    for c in s.chars() {
        match (quote, c) {
            (Some(qc), c) if c == qc => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What the browser's console sends as text: a resize. Its keystrokes come as binary messages.
#[derive(Deserialize)]
struct Control {
    resize: Option<[u16; 2]>,
}

/// `GET .../containers/{id}/exec/ws`: a terminal inside the container. Binary messages both ways are the terminal's
/// bytes; a text message from the browser is `{"resize":[cols,rows]}`.
pub async fn exec_ws(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Query(eq): Query<ExecQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    check(&id)?;
    let cmd = words(eq.cmd.as_deref().unwrap_or("/bin/sh"));
    if cmd.is_empty() {
        return Err(ApiError::bad("Say which command to run.").field("cmd"));
    }
    let d = endpoint(&app, env).await?;
    let user = eq.user.clone().filter(|u| !u.is_empty());
    let size = match (eq.cols, eq.rows) {
        (Some(c), Some(r)) if c > 0 && r > 0 => Some(vec![r as usize, c as usize]),
        _ => None,
    };
    let exec = d
        .client()
        .create_exec(
            &id,
            ExecConfig { attach_stdin: Some(true), attach_stdout: Some(true), attach_stderr: Some(true), tty: Some(true), cmd: Some(cmd.clone()), user: user.clone(), console_size: size, env: Some(vec!["TERM=xterm-256color".into()]), ..Default::default() },
        )
        .await
        .map_err(api_error)?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "container.exec", target: Some(id.clone()), detail: Some(cmd.join(" ") + &user.map(|u| format!(" as {u}")).unwrap_or_default()) }).await;
    Ok(ws.on_upgrade(move |socket| async move {
        let c = d.client();
        let started = c.start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: true, output_capacity: Some(64 * 1024) })).await;
        let (mut output, mut input) = match started {
            Ok(StartExecResults::Attached { output, input }) => (output, input),
            Ok(StartExecResults::Detached) => return,
            Err(e) => {
                let mut socket = socket;
                let _ = socket.send(Message::Text(json!({ "error": api_error(e).message }).to_string().into())).await;
                return;
            }
        };
        let (mut tx, mut rx) = socket.split();
        loop {
            tokio::select! {
                out = output.next() => match out {
                    Some(Ok(o)) => {
                        let bytes = o.into_bytes();
                        if tx.send(Message::Binary(bytes)).await.is_err() { break; }
                    }
                    _ => { let _ = tx.send(Message::Text(json!({ "end": true }).to_string().into())).await; break; }
                },
                incoming = rx.next() => match incoming {
                    Some(Ok(Message::Binary(b))) => { if input.write_all(&b).await.is_err() { break; } let _ = input.flush().await; }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(Control { resize: Some([cols, rows]) }) = serde_json::from_str::<Control>(&t)
                            && cols > 0 && rows > 0 {
                                let _ = c.resize_exec(&exec.id, ResizeExecOptions { width: cols, height: rows }).await;
                            }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                },
            }
        }
        let _ = tx.close().await;
    }))
}

/// How an attach goes, from what the container is: whether its output is a terminal's, and whether what is typed
/// reaches its main process.
#[derive(Debug, PartialEq)]
pub struct AttachPlan {
    pub tty: bool,
    pub stdin: bool,
}

/// An attach is to a running container. Typing goes in only when the container keeps its input open (`-i`) and does
/// not close it when an attach lets go (`StdinOnce`, which `docker run -i` without `-d` sets): there, leaving the page
/// would end the input, and a shell reading it would exit and stop the container. Such a container is attached to
/// watch only ("Streams" 3).
pub fn attach_plan(c: &bollard::models::ContainerInspectResponse) -> ApiResult<AttachPlan> {
    if !c.state.as_ref().and_then(|s| s.running).unwrap_or(false) {
        return Err(ApiError::new(axum::http::StatusCode::CONFLICT, "It is not running: attaching is to a running container."));
    }
    let cfg = c.config.as_ref();
    let tty = cfg.and_then(|c| c.tty).unwrap_or(false);
    let open = cfg.and_then(|c| c.open_stdin).unwrap_or(false);
    let once = cfg.and_then(|c| c.stdin_once).unwrap_or(false);
    Ok(AttachPlan { tty, stdin: open && !once })
}

/// Output without a TTY ends its lines with a bare `\n`; a terminal needs `\r\n` to go back to the start of the line.
pub fn crlf(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() + b.len() / 16);
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' && (i == 0 || b[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(c);
    }
    out
}

#[derive(Deserialize, Default)]
pub struct AttachQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

/// `GET .../containers/{id}/attach/ws`: the container's main process, as `docker attach` gives it: what it prints from
/// now on and, when it reads its input, what is typed. The protocol is the console's (binary both ways, text
/// `{"resize":[cols,rows]}`), and the first text message says `{"attached":{"tty":..,"stdin":..}}`. Leaving never
/// stops the container: the stream is dropped, its input is not closed, and nothing is sent to the process. Ctrl-C
/// typed into a TTY does reach it, as with `docker attach`; Docker's own detach keys (Ctrl-P Ctrl-Q) end the attach.
pub async fn attach_ws(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Query(aq): Query<AttachQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    check(&id)?;
    let d = endpoint(&app, env).await?;
    let c = d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
    let plan = attach_plan(&c)?;
    let a = &admin.0;
    let detail = if plan.stdin { "with input" } else { "watching only" };
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "container.attach", target: Some(id.clone()), detail: Some(detail.into()) }).await;
    Ok(ws.on_upgrade(move |socket| async move {
        let c = d.client();
        let opts = q::AttachContainerOptions { stream: true, stdin: plan.stdin, stdout: true, stderr: true, logs: false, detach_keys: None };
        let (mut output, mut input) = match c.attach_container(&id, Some(opts)).await {
            Ok(r) => (r.output, r.input),
            Err(e) => {
                let mut socket = socket;
                let _ = socket.send(Message::Text(json!({ "error": api_error(e).message }).to_string().into())).await;
                return;
            }
        };
        let resize = |cols: u16, rows: u16| {
            let c = c.clone();
            let id = id.clone();
            async move {
                if plan.tty && cols > 0 && rows > 0 {
                    let _ = c.resize_container_tty(&id, q::ResizeContainerTTYOptions { w: cols as i32, h: rows as i32 }).await;
                }
            }
        };
        if let (Some(cols), Some(rows)) = (aq.cols, aq.rows) {
            resize(cols, rows).await;
        }
        let (mut tx, mut rx) = socket.split();
        if tx.send(Message::Text(json!({ "attached": { "tty": plan.tty, "stdin": plan.stdin } }).to_string().into())).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                out = output.next() => match out {
                    Some(Ok(o)) => {
                        let bytes = o.into_bytes();
                        let bytes = if plan.tty { bytes } else { crlf(&bytes).into() };
                        if tx.send(Message::Binary(bytes)).await.is_err() { break; }
                    }
                    _ => { let _ = tx.send(Message::Text(json!({ "end": true }).to_string().into())).await; break; }
                },
                incoming = rx.next() => match incoming {
                    Some(Ok(Message::Binary(b))) if plan.stdin => { if input.write_all(&b).await.is_err() { break; } let _ = input.flush().await; }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(Control { resize: Some([cols, rows]) }) = serde_json::from_str::<Control>(&t) {
                            resize(cols, rows).await;
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                },
            }
        }
        // Dropped, not shut down: a shutdown of the write half would be an end of input to the process.
        drop(input);
        let _ = tx.close().await;
    }))
}

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    /// Only these kinds: container, image, volume, network…
    #[serde(rename = "type")]
    typ: Option<String>,
    /// From this Unix time on (the events before the page opened).
    since: Option<i64>,
}

/// `GET .../events/ws`: Docker's events as they happen, each a JSON message as Docker words it.
/// A Standard or Read-only user gets no event about a container, a volume, a network or an image they cannot see
/// ("Access" 4, 18), as they could when the events began.
pub async fn events_ws(State(app): State<App>, access: crate::access::Access, Path(env): Path<i64>, Query(eq): Query<EventsQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    let d = endpoint(&app, env).await?;
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    let shown = move |e: &Value| -> bool {
        if !access.limited() {
            return true;
        }
        let a = &e["Actor"]["Attributes"];
        let id = e["Actor"]["ID"].as_str().unwrap_or_default();
        let name = a["name"].as_str().unwrap_or_default();
        let project = a[crate::access::ownership::PROJECT].as_str();
        match e["Type"].as_str().unwrap_or_default() {
            "container" => access.sees(owners.container(name, project)),
            "volume" => seen.volume(&access, &owners, id, project),
            "network" => seen.network(&access, &owners, name, id, project),
            "image" => seen.image(&access, &owners, id),
            _ => true,
        }
    };
    let filters = eq.typ.filter(|t| !t.is_empty()).map(|t| HashMap::from([("type".to_string(), vec![t])]));
    let s = super::raw::follow::<bollard::models::EventMessage>(&d, &events_path(eq.since, filters.as_ref())).await?;
    Ok(ws.on_upgrade(move |mut socket| async move {
        futures_util::pin_mut!(s);
        loop {
            tokio::select! {
                next = s.next() => match next {
                    Some(Ok(e)) => {
                        let e = json!(e);
                        if shown(&e) && socket.send(Message::Text(e.to_string().into())).await.is_err() { break; }
                    }
                    Some(Err(e)) => { let _ = socket.send(Message::Text(json!({ "error": e.message }).to_string().into())).await; break; }
                    None => break,
                },
                incoming = socket.recv() => if !matches!(incoming, Some(Ok(_))) { break },
            }
        }
    }))
}

/// Docker's events from `since` on, of the kinds `filters` names, followed. Read as JSON lines by `raw::follow`, not
/// by bollard, so a line that never ends is refused at `raw::MAX_LINE` ("Endpoints" 7).
pub fn events_path(since: Option<i64>, filters: Option<&HashMap<String, Vec<String>>>) -> String {
    let since = since.map(|s| s.to_string());
    let filters = filters.map(|f| json!(f).to_string());
    let q: Vec<(&str, &str)> = [("since", since.as_deref()), ("filters", filters.as_deref())].into_iter().filter_map(|(k, v)| Some((k, v?))).collect();
    super::raw::path("/events", &q)
}

/// A container's stats: a sample a second while `stream`, else one (Docker waits for a second reading to count CPU).
pub fn stats_path(id: &str, stream: bool) -> String {
    super::raw::path(&format!("/containers/{id}/stats"), &[("stream", if stream { "true" } else { "false" })])
}

/// One sample of container `id`, read as `raw::follow` reads a stream.
async fn one_sample(d: &super::Docker, id: &str) -> ApiResult<ContainerStatsResponse> {
    let s = super::raw::follow::<ContainerStatsResponse>(d, &stats_path(id, false)).await?;
    futures_util::pin_mut!(s);
    s.next().await.unwrap_or_else(|| Err(ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "Docker sent no sample.")))
}

/// The last `max` bytes of `b` as text, cut where a character starts, and whether anything was left out.
pub fn last_bytes(b: &[u8], max: usize) -> (String, bool) {
    if b.len() <= max {
        return (String::from_utf8_lossy(b).into_owned(), false);
    }
    let mut from = b.len() - max;
    // A UTF-8 continuation byte is 10xxxxxx: step past them to the next character's start.
    while from < b.len() && b[from] & 0xC0 == 0x80 {
        from += 1;
    }
    (String::from_utf8_lossy(&b[from..]).into_owned(), true)
}

/// Output kept to its last `max` bytes as it comes, so a command that prints for ever holds no more than twice that.
pub struct Tail {
    buf: Vec<u8>,
    max: usize,
    cut: bool,
}

impl Tail {
    pub fn new(max: usize) -> Self {
        Tail { buf: Vec::new(), max, cut: false }
    }

    pub fn push(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
        if self.buf.len() > self.max * 2 {
            let drop = self.buf.len() - self.max;
            self.buf.drain(..drop);
            self.cut = true;
        }
    }

    pub fn finish(self) -> (String, bool) {
        let (s, cut) = last_bytes(&self.buf, self.max);
        (s, cut || self.cut)
    }
}

/// How long a command run with `exec` may take by default, and at most.
pub const RUN_TIMEOUT_S: u64 = 30;
pub const RUN_TIMEOUT_MAX_S: u64 = 600;
/// How much of each of its outputs is kept by default, and at most: the end, where errors are.
pub const RUN_KEEP: usize = 64 * 1024;
pub const RUN_KEEP_MAX: usize = 1024 * 1024;

/// A command to run inside a container without a terminal.
#[derive(Deserialize)]
pub struct Run {
    /// The command as words: `["sh", "-c", "ls /"]`.
    cmd: Vec<String>,
    user: Option<String>,
    workdir: Option<String>,
    /// More environment, `KEY=value`.
    env: Option<Vec<String>>,
    timeout_s: Option<u64>,
    /// Bytes of each of standard output and standard error kept, from the end.
    max_bytes: Option<usize>,
}

/// `POST .../containers/{id}/exec`: runs a command to its end (or its time limit) and answers what it printed and how
/// it ended: `{exit_code, stdout, stderr, timed_out, truncated}`. For programs (and `/mcp`); people use the console.
/// A command past its time is left running: Docker has no way to stop one exec, and the answer says it timed out.
pub async fn exec_run(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(r): Json<Run>) -> ApiResult<Json<Value>> {
    check(&id)?;
    if r.cmd.is_empty() || r.cmd[0].trim().is_empty() {
        return Err(ApiError::bad("Say which command to run, as words: [\"ls\", \"-l\"].").field("cmd"));
    }
    let timeout = r.timeout_s.unwrap_or(RUN_TIMEOUT_S);
    if !(1..=RUN_TIMEOUT_MAX_S).contains(&timeout) {
        return Err(ApiError::bad(format!("A command may take 1 to {RUN_TIMEOUT_MAX_S} seconds.")).field("timeout_s"));
    }
    let keep = r.max_bytes.unwrap_or(RUN_KEEP).clamp(1, RUN_KEEP_MAX);
    let d = endpoint(&app, env).await?;
    let user = r.user.filter(|u| !u.is_empty());
    let config = ExecConfig { attach_stdout: Some(true), attach_stderr: Some(true), tty: Some(false), cmd: Some(r.cmd.clone()), user: user.clone(), working_dir: r.workdir.filter(|w| !w.is_empty()), env: r.env, ..Default::default() };
    let exec = d.client().create_exec(&id, config).await.map_err(api_error)?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "container.exec", target: Some(id.clone()), detail: Some(r.cmd.join(" ") + &user.map(|u| format!(" as {u}")).unwrap_or_default()) }).await;
    let started = d.client().start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: false, output_capacity: Some(64 * 1024) })).await.map_err(api_error)?;
    let (mut out, mut err) = (Tail::new(keep), Tail::new(keep));
    let mut timed_out = false;
    if let StartExecResults::Attached { mut output, .. } = started {
        let read = async {
            while let Some(next) = output.next().await {
                match next {
                    Ok(LogOutput::StdErr { message }) => err.push(&message),
                    Ok(LogOutput::StdOut { message } | LogOutput::Console { message }) => out.push(&message),
                    Ok(LogOutput::StdIn { .. }) => {}
                    Err(e) => return Err(api_error(e)),
                }
            }
            Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(timeout), read).await {
            Ok(r) => r?,
            Err(_) => timed_out = true,
        }
    }
    let exit_code = if timed_out { None } else { d.client().inspect_exec(&exec.id).await.map_err(api_error)?.exit_code };
    let ((stdout, cut_out), (stderr, cut_err)) = (out.finish(), err.finish());
    Ok(Json(json!({ "exit_code": exit_code, "stdout": stdout, "stderr": stderr, "timed_out": timed_out, "truncated": cut_out || cut_err })))
}

/// `GET .../containers/{id}/stats`: one sample of one container, as the stats WebSocket sends them.
pub async fn stats_one(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check(&id)?;
    let d = endpoint(&app, env).await?;
    Ok(Json(json!(sample(&one_sample(&d, &id).await?))))
}

/// One sample of every running container on `d`: its id, its name and what it uses. Docker takes a second or two to
/// answer each (it waits for a second reading to count CPU by), so they are asked all at once.
pub async fn sample_running(d: &super::Docker) -> ApiResult<Vec<(String, String, Sample)>> {
    let list: Vec<bollard::models::ContainerSummary> = super::raw::get(d, "/containers/json").await?;
    let running: Vec<(String, String)> = list
        .into_iter()
        .filter_map(|c| Some((c.id?, c.names.and_then(|n| n.into_iter().next()).unwrap_or_default().trim_start_matches('/').to_string())))
        .collect();
    let samples = futures_util::future::join_all(running.into_iter().map(|(id, name)| async move { one_sample(d, &id).await.ok().map(|s| (id, name, sample(&s))) }))
    .await;
    Ok(samples.into_iter().flatten().collect())
}

/// `GET .../stats`: one sample of every running container at once, for the containers list.
/// A Standard or Read-only user gets only the containers they can see ("Access" 4).
pub async fn all_stats(State(app): State<App>, access: crate::access::Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let seen: Option<std::collections::HashSet<String>> = if access.limited() {
        let list = d.client().list_containers(None::<q::ListContainersOptions>).await.map_err(api_error)?;
        let owners = crate::access::ownership::Owners::load(&app.db, env).await?;
        Some(
            list.into_iter()
                .filter(|c| {
                    let (name, project) = crate::access::ownership::container_keys(&json!(c));
                    access.sees(owners.container(&name, project.as_deref()))
                })
                .filter_map(|c| c.id)
                .collect(),
        )
    } else {
        None
    };
    let out: HashMap<String, Sample> = sample_running(&d)
        .await?
        .into_iter()
        .filter(|(id, _, _)| seen.as_ref().is_none_or(|s| s.contains(id)))
        .map(|(id, _, s)| (id, s))
        .collect();
    Ok(Json(json!(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_stats_are_asked_as_docker_takes_them() {
        assert_eq!(events_path(None, None), "/events");
        let f = HashMap::from([("type".to_string(), vec!["container".to_string()])]);
        assert_eq!(events_path(Some(17), Some(&f)), "/events?since=17&filters=%7B%22type%22%3A%5B%22container%22%5D%7D");
        assert_eq!(stats_path("web", true), "/containers/web/stats?stream=true");
        assert_eq!(stats_path("web", false), "/containers/web/stats?stream=false");
    }
    use bollard::models::{ContainerBlkioStatEntry, ContainerBlkioStats, ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats, ContainerNetworkStats};

    #[test]
    fn stats_come_every_five_seconds_unless_one_of_the_offered_intervals_is_asked_for() {
        assert_eq!(stats_every(None).unwrap(), std::time::Duration::from_secs(5));
        for s in [1, 5, 10, 30] {
            assert_eq!(stats_every(Some(s)).unwrap(), std::time::Duration::from_secs(s));
        }
        for s in [0, 2, 60, 3600] {
            assert!(stats_every(Some(s)).is_err(), "{s} is not offered");
        }
    }

    #[test]
    fn cpu_is_counted_as_docker_stats_counts_it_and_memory_without_the_cache() {
        let cpu = |total, system| ContainerCpuStats { cpu_usage: Some(ContainerCpuUsage { total_usage: Some(total), ..Default::default() }), system_cpu_usage: Some(system), online_cpus: Some(4), ..Default::default() };
        let s = ContainerStatsResponse {
            cpu_stats: Some(cpu(2_000, 10_000)),
            precpu_stats: Some(cpu(1_000, 6_000)),
            memory_stats: Some(ContainerMemoryStats { usage: Some(500), limit: Some(1000), stats: Some(HashMap::from([("inactive_file".into(), 100)])), ..Default::default() }),
            networks: Some(HashMap::from([
                ("eth0".into(), ContainerNetworkStats { rx_bytes: Some(10), tx_bytes: Some(20), ..Default::default() }),
                ("eth1".into(), ContainerNetworkStats { rx_bytes: Some(1), tx_bytes: Some(2), ..Default::default() }),
            ])),
            blkio_stats: Some(ContainerBlkioStats {
                io_service_bytes_recursive: Some(vec![
                    ContainerBlkioStatEntry { op: Some("read".into()), value: Some(7), ..Default::default() },
                    ContainerBlkioStatEntry { op: Some("Write".into()), value: Some(9), ..Default::default() },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let x = sample(&s);
        assert_eq!(x.cpu, 100.0, "1000 of 4000 over four CPUs is one CPU's worth");
        assert_eq!(x.mem, 400);
        assert_eq!(x.mem_limit, 1000);
        assert_eq!((x.net_rx, x.net_tx), (11, 22));
        assert_eq!((x.disk_read, x.disk_write), (7, 9));
    }

    #[test]
    fn the_first_sample_has_nothing_to_compare_with_and_says_zero() {
        assert_eq!(sample(&ContainerStatsResponse::default()).cpu, 0.0);
    }

    #[test]
    fn a_command_line_splits_into_words_and_quotes_keep_them_whole() {
        assert_eq!(words("/bin/sh"), vec!["/bin/sh"]);
        assert_eq!(words("  bash -l "), vec!["bash", "-l"]);
        assert_eq!(words(r#"sh -c "echo hi there""#), vec!["sh", "-c", "echo hi there"]);
        assert_eq!(words("psql -c ''"), vec!["psql", "-c", ""]);
        assert!(words("   ").is_empty());
    }

    fn inspected(running: bool, tty: bool, open_stdin: bool, stdin_once: bool) -> bollard::models::ContainerInspectResponse {
        bollard::models::ContainerInspectResponse {
            state: Some(bollard::models::ContainerState { running: Some(running), ..Default::default() }),
            config: Some(bollard::models::ContainerConfig { tty: Some(tty), open_stdin: Some(open_stdin), stdin_once: Some(stdin_once), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn attaching_needs_a_running_container() {
        let e = attach_plan(&inspected(false, true, true, false)).unwrap_err();
        assert_eq!(e.status, axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn attaching_types_into_the_main_process_only_when_its_input_stays_open_after() {
        let p = attach_plan(&inspected(true, true, true, false)).unwrap();
        assert_eq!(p, AttachPlan { tty: true, stdin: true });
        // StdinOnce: Docker closes the process's input when this attach lets go, and a shell would end with it.
        let p = attach_plan(&inspected(true, true, true, true)).unwrap();
        assert_eq!(p, AttachPlan { tty: true, stdin: false });
        // Made without -i: there is no input to type into.
        let p = attach_plan(&inspected(true, false, false, false)).unwrap();
        assert_eq!(p, AttachPlan { tty: false, stdin: false });
    }

    #[test]
    fn output_without_a_tty_gets_the_carriage_returns_a_terminal_needs() {
        assert_eq!(crlf(b"a\nb\n"), b"a\r\nb\r\n".to_vec());
        assert_eq!(crlf(b"a\r\nb"), b"a\r\nb".to_vec(), "a line already ended right is left alone");
        assert_eq!(crlf(b""), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn a_console_or_an_attach_is_an_administrator_s() {
        use crate::api::tests::{app, call};
        use axum::http::{Request, StatusCode, header};
        let app = app();
        let (cookie, _) = crate::api::tests::signed_in(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let env: i64 = app.db.call(|c| Ok(c.query_row("SELECT id FROM environments WHERE name = 'local'", [], |r| r.get(0))?)).await.unwrap();
        for path in ["attach/ws", "exec/ws"] {
            let req = |cookie: Option<&str>| {
                let mut r = Request::get(format!("/api/envs/{env}/containers/web/{path}")).header(header::HOST, "freightdock.test");
                if let Some(c) = cookie { r = r.header(header::COOKIE, c); }
                r.body(axum::body::Body::empty()).unwrap()
            };
            let (s, _, _) = call(&app, req(None)).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{path} without a session");
            let (s, _, _) = call(&app, req(Some(&format!("freightdock_session={token}")))).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{path} for a user who is not an administrator");
            let (s, _, _) = call(&app, req(Some(&cookie))).await;
            assert!(s != StatusCode::NOT_FOUND && s != StatusCode::METHOD_NOT_ALLOWED && s.is_client_error(), "{path} for the administrator is there but wants an upgrade: {s}");
        }
    }

    #[test]
    fn a_tail_is_a_number_or_all_and_anything_else_is_500() {
        let t = |s: Option<&str>| LogQuery { tail: s.map(str::to_string), ..Default::default() }.options(true).tail;
        assert_eq!(t(Some("50")), "50");
        assert_eq!(t(Some("all")), "all");
        assert_eq!(t(Some("-1; rm")), "500");
        assert_eq!(t(None), "500");
    }

    #[test]
    fn output_kept_to_its_last_bytes_is_cut_on_a_character_and_says_so() {
        assert_eq!(last_bytes(b"hello", 10), ("hello".to_string(), false));
        assert_eq!(last_bytes(b"hello world", 5), ("world".to_string(), true));
        let (s, cut) = last_bytes("aé".as_bytes(), 1);
        assert!(cut && s.is_empty(), "half a character is not kept: {s:?}");
        let mut b = Tail::new(4);
        b.push(b"abc");
        b.push(b"defgh");
        assert_eq!(b.finish(), ("efgh".to_string(), true));
    }

    #[tokio::test]
    async fn running_a_command_is_an_administrator_s_and_says_what_is_wrong_with_it() {
        use crate::api::tests::{app, call, json_body, post};
        use axum::http::{StatusCode, header};
        let app = app();
        let (cookie, csrf) = crate::api::tests::signed_in(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let user = format!("freightdock_session={}", crate::auth::start(&app.db, 9, None, None).await.unwrap());
        let (_, _, me) = call(&app, axum::http::Request::get("/api/me").header(header::COOKIE, &user).body(axum::body::Body::empty()).unwrap()).await;
        let run = |cookie: &str, csrf: &str, body: Value| post("/api/envs/1/containers/web/exec").header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap();
        let (s, _, _) = call(&app, run(&user, me["csrf"].as_str().unwrap(), json!({ "cmd": ["true"] }))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, b) = call(&app, run(&cookie, &csrf, json!({ "cmd": [] }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("cmd")));
        let (s, _, b) = call(&app, run(&cookie, &csrf, json!({ "cmd": ["true"], "timeout_s": 0 }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("timeout_s")));
        let (s, _, b) = call(&app, run(&cookie, &csrf, json!({ "cmd": ["true"] }))).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "the test Docker is not there: {b}");
    }

    #[tokio::test]
    async fn one_container_s_sample_is_a_read() {
        use crate::api::tests::{app, call};
        use axum::http::{Request, StatusCode, header};
        let app = app();
        let (cookie, _) = crate::api::tests::signed_in(&app).await;
        let (s, _, _) = call(&app, Request::get("/api/envs/1/containers/web/stats").body(axum::body::Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _, _) = call(&app, Request::get("/api/envs/1/containers/web/stats").header(header::COOKIE, &cookie).body(axum::body::Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY, "there, and asking the Docker that is not there");
    }
}
