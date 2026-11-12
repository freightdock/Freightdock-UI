//! Notifications: channels an administrator sets up (a webhook, e-mail over SMTP, an ntfy topic, a Gotify server), each
//! subscribed to some of the events Freightdock tells about: a container that died or turned unhealthy, a newer image for
//! a container, an environment that stopped answering, a deploy that failed. Each channel's one secret (a webhook's
//! signing key, an ntfy or Gotify token, an SMTP password) is sealed ("Notifications" in DECISIONS.md).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router, routing::get, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// What Freightdock tells about, and how the settings page says it.
pub const EVENTS: &[(&str, &str)] = &[
    ("container.died", "A container stops on its own: it crashed, or ran out of memory"),
    ("container.unhealthy", "A container's health check fails"),
    ("update.available", "A newer image is out for a container"),
    ("environment.down", "An environment stops answering: an agent's tunnel ends, a host goes away"),
    ("deploy.failed", "A stack's deploy fails"),
];

pub const KINDS: &[&str] = &["webhook", "email", "ntfy", "gotify"];

/// The same event about the same thing is told at most once in this long: a container that crashes in a loop is one
/// message, not one a minute.
pub const QUIET_S: i64 = 600;
/// How long one delivery may take.
const SEND_WITHIN: Duration = Duration::from_secs(20);

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/notifications", get(list).post(create))
        .route("/api/notifications/{id}", axum::routing::put(update).delete(remove))
        .route("/api/notifications/{id}/test", post(test))
}

/// Something to tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: &'static str,
    pub title: String,
    pub message: String,
    /// The environment it happened on: its id and name.
    pub env: Option<(i64, String)>,
    /// What it is about: a container's name, an image, a stack.
    pub target: Option<String>,
}

impl Event {
    fn payload(&self, at: i64) -> Value {
        json!({
            "event": self.kind, "title": self.title, "message": self.message,
            "environment": self.env.as_ref().map(|(id, name)| json!({ "id": id, "name": name })),
            "target": self.target, "at": at, "freightdock": env!("CARGO_PKG_VERSION"),
        })
    }
}

/// Who was told what lately, so the same news is not told twice within `QUIET_S`.
#[derive(Default)]
pub struct Notifier {
    told: Mutex<HashMap<String, i64>>,
}

impl Notifier {
    /// Whether `key` may be told at `at`; if so, it is counted as told.
    pub fn allow(&self, key: &str, at: i64) -> bool {
        let mut told = self.told.lock().unwrap_or_else(|p| p.into_inner());
        told.retain(|_, t| at - *t < QUIET_S);
        if told.contains_key(key) {
            return false;
        }
        told.insert(key.to_string(), at);
        true
    }
}

/// A channel's settings. One struct for every kind; each kind reads its own fields. `secret` is sealed at rest and
/// never sent back.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// webhook: where to POST.
    pub url: String,
    /// ntfy and Gotify: the server (`https://ntfy.sh` for ntfy when left empty).
    pub server: String,
    /// ntfy: the topic.
    pub topic: String,
    /// ntfy (1 to 5) and Gotify (0 to 10): how loud.
    pub priority: Option<u8>,
    /// email: the SMTP server, its port, `starttls` (the default), `tls` or `none`, who signs in, from and to whom.
    pub host: String,
    pub port: Option<u16>,
    pub security: String,
    pub username: String,
    pub from: String,
    pub to: Vec<String>,
    /// The webhook's signing key, the ntfy or Gotify token, the SMTP password.
    pub secret: String,
}

fn http_url(u: &str, field: &'static str) -> ApiResult<url::Url> {
    match url::Url::parse(u.trim()) {
        Ok(p) if matches!(p.scheme(), "http" | "https") && p.host_str().is_some() => Ok(p),
        _ => Err(ApiError::bad("An address that starts http:// or https://.").field(field)),
    }
}

fn mailbox(m: &str, field: &'static str) -> ApiResult<lettre::message::Mailbox> {
    m.trim().parse().map_err(|_| ApiError::bad(format!("\"{}\" is not an e-mail address.", m.trim())).field(field))
}

impl Config {
    /// The settings of a channel of `kind`, checked, trimmed and with their defaults. `had_secret`: an edit keeps the
    /// secret there is when none is given.
    fn checked(mut self, kind: &str, had_secret: bool) -> ApiResult<Config> {
        let has_secret = !self.secret.trim().is_empty() || had_secret;
        match kind {
            "webhook" => {
                self.url = http_url(&self.url, "url")?.to_string();
            }
            "ntfy" => {
                if self.server.trim().is_empty() {
                    self.server = "https://ntfy.sh".into();
                }
                self.server = http_url(&self.server, "server")?.to_string().trim_end_matches('/').to_string();
                self.topic = self.topic.trim().to_string();
                if self.topic.is_empty() || self.topic.len() > 64 || !self.topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    return Err(ApiError::bad("A topic is 1 to 64 letters, digits, - or _.").field("topic"));
                }
                if self.priority.is_some_and(|p| !(1..=5).contains(&p)) {
                    return Err(ApiError::bad("ntfy's priority is 1 to 5.").field("priority"));
                }
            }
            "gotify" => {
                self.server = http_url(&self.server, "server")?.to_string().trim_end_matches('/').to_string();
                if !has_secret {
                    return Err(ApiError::bad("Gotify needs an application's token.").field("secret"));
                }
                if self.priority.is_some_and(|p| p > 10) {
                    return Err(ApiError::bad("Gotify's priority is 0 to 10.").field("priority"));
                }
            }
            "email" => {
                self.host = self.host.trim().to_string();
                if self.host.is_empty() || self.host.contains(['/', ' ']) {
                    return Err(ApiError::bad("Name the SMTP server.").field("host"));
                }
                self.security = match self.security.trim() {
                    "" | "starttls" => "starttls".into(),
                    s @ ("tls" | "none") => s.into(),
                    _ => return Err(ApiError::bad("STARTTLS, TLS or none.").field("security")),
                };
                mailbox(&self.from, "from")?;
                self.to = self.to.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                if self.to.is_empty() {
                    return Err(ApiError::bad("Say who gets the mail.").field("to"));
                }
                for t in &self.to {
                    mailbox(t, "to")?;
                }
                self.username = self.username.trim().to_string();
            }
            _ => return Err(ApiError::bad("A channel is a webhook, e-mail, ntfy or Gotify.").field("kind")),
        }
        Ok(self)
    }

    fn port(&self) -> u16 {
        self.port.unwrap_or(match self.security.as_str() {
            "tls" => 465,
            "none" => 25,
            _ => 587,
        })
    }
}

/// An HTTP delivery: where, with which headers, what.
#[derive(Debug, PartialEq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

/// A webhook's signature: HMAC-SHA256 of the body under the secret, in hex.
pub fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    m.update(body);
    hex::encode(m.finalize().into_bytes())
}

/// What to send for an HTTP kind; `secret` is the channel's, opened.
pub fn http_request(kind: &str, c: &Config, secret: &str, e: &Event, at: i64) -> Option<Request> {
    let mut headers = vec![("content-type", "application/json".to_string())];
    match kind {
        "webhook" => {
            let body = e.payload(at).to_string();
            if !secret.is_empty() {
                headers.push(("x-freightdock-signature", format!("sha256={}", sign(secret, body.as_bytes()))));
            }
            headers.push(("x-freightdock-event", e.kind.to_string()));
            Some(Request { url: c.url.clone(), headers, body })
        }
        "ntfy" => {
            // Published as JSON to the server's root, so a title in any language needs no header encoding.
            if !secret.is_empty() {
                headers.push(("authorization", format!("Bearer {secret}")));
            }
            let tags = if e.kind == "update.available" { vec!["package"] } else { vec!["warning"] };
            let body = json!({ "topic": c.topic, "title": e.title, "message": e.message, "priority": c.priority.unwrap_or(4), "tags": tags }).to_string();
            Some(Request { url: format!("{}/", c.server.trim_end_matches('/')), headers, body })
        }
        "gotify" => {
            headers.push(("x-gotify-key", secret.to_string()));
            let body = json!({ "title": e.title, "message": e.message, "priority": c.priority.unwrap_or(5) }).to_string();
            Some(Request { url: format!("{}/message", c.server.trim_end_matches('/')), headers, body })
        }
        _ => None,
    }
}

/// The mail for an event.
pub fn mail(c: &Config, e: &Event, at: i64) -> anyhow::Result<lettre::Message> {
    let mut b = lettre::Message::builder().from(c.from.trim().parse()?).subject(format!("Freightdock: {}", e.title));
    for t in &c.to {
        b = b.to(t.parse()?);
    }
    let mut text = format!("{}\n\n", e.message);
    if let Some((_, name)) = &e.env {
        text.push_str(&format!("Environment: {name}\n"));
    }
    if let Some(t) = &e.target {
        text.push_str(&format!("About: {t}\n"));
    }
    text.push_str(&format!("Event: {}\nAt: {}\n", e.kind, utc_text(at)));
    Ok(b.header(lettre::message::header::ContentType::TEXT_PLAIN).body(text)?)
}

/// A Unix time as UTC, `2026-11-09 21:14:09 UTC`.
pub fn utc_text(at: i64) -> String {
    let (y, m, d, hh, mm, ss) = crate::db::utc(at);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

/// Sends `e` through one channel; the error is said in words.
async fn deliver(kind: &str, c: &Config, secret: &str, e: &Event) -> Result<(), String> {
    let at = now();
    if kind == "email" {
        use lettre::AsyncTransport;
        let msg = mail(c, e, at).map_err(|x| format!("The mail could not be written: {x}"))?;
        let builder = match c.security.as_str() {
            "tls" => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&c.host),
            "none" => Ok(lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&c.host)),
            _ => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&c.host),
        }
        .map_err(|x| format!("{}: {x}", c.host))?;
        let mut builder = builder.port(c.port()).timeout(Some(SEND_WITHIN));
        if !c.username.is_empty() {
            builder = builder.credentials(lettre::transport::smtp::authentication::Credentials::new(c.username.clone(), secret.to_string()));
        }
        return builder.build().send(msg).await.map(|_| ()).map_err(|x| format!("{}: {x}", c.host));
    }
    let Some(r) = http_request(kind, c, secret, e, at) else { return Err("Freightdock does not know that kind of channel.".into()) };
    let client = reqwest::Client::builder().timeout(SEND_WITHIN).build().map_err(|x| x.to_string())?;
    let mut req = client.post(&r.url).body(r.body);
    for (k, v) in r.headers {
        req = req.header(k, v);
    }
    let answer = req.send().await.map_err(|x| format!("{}: {}", r.url, without_url(&x)))?;
    if answer.status().is_success() {
        return Ok(());
    }
    let status = answer.status();
    let text: String = answer.text().await.unwrap_or_default().chars().take(200).collect();
    Err(format!("{} answered {status}{}", r.url, if text.trim().is_empty() { String::new() } else { format!(": {}", text.trim()) }))
}

/// reqwest's error without the address it repeats, which may carry a token in its query.
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s.replace(&format!(" ({u})"), "").replace(u.as_str(), "");
    }
    s
}

struct Channel {
    id: i64,
    kind: String,
    config: Config,
}

/// Tells `e` to every enabled channel that wants it, unless the same was told within `QUIET_S`. Returns at once; the
/// deliveries go on in the background, each channel's last success or error written down.
pub fn notify(app: &App, e: Event) {
    let key = format!("{}|{}|{}", e.kind, e.env.as_ref().map_or(0, |x| x.0), e.target.as_deref().unwrap_or(""));
    if !app.notifier.allow(&key, now()) {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        let kind = e.kind;
        let channels = app
            .db
            .call(move |c| {
                let mut st = c.prepare("SELECT id, kind, config, events FROM notification_channels WHERE enabled = 1")?;
                let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let wanted = channels.into_iter().filter_map(|(id, kind_, config, events)| {
            let events: Vec<String> = serde_json::from_str(&events).unwrap_or_default();
            events.iter().any(|x| x == kind).then(|| Some(Channel { id, kind: kind_, config: serde_json::from_str(&config).ok()? })).flatten()
        });
        futures_util::future::join_all(wanted.map(|ch| {
            let (app, e) = (app.clone(), e.clone());
            async move {
                let r = send(&app, &ch, &e).await;
                if let Err(why) = &r {
                    tracing::warn!("notification channel {} could not tell {}: {why}", ch.id, e.kind);
                }
            }
        }))
        .await;
    });
}

/// One channel's delivery, its outcome written down.
async fn send(app: &App, ch: &Channel, e: &Event) -> Result<(), String> {
    let secret = if ch.config.secret.is_empty() { Ok(String::new()) } else { app.sealer.open(&ch.config.secret).map_err(|x| x.to_string()) };
    let r = match secret {
        Ok(secret) => deliver(&ch.kind, &ch.config, &secret, e).await,
        Err(x) => Err(format!("its secret could not be opened: {x}")),
    };
    let (id, err) = (ch.id, r.as_ref().err().cloned());
    let _ = app
        .db
        .call(move |c| {
            match err {
                None => c.execute("UPDATE notification_channels SET last_sent_at = ?1, last_error = NULL WHERE id = ?2", params![now(), id])?,
                Some(x) => c.execute("UPDATE notification_channels SET last_error = ?1 WHERE id = ?2", params![x, id])?,
            };
            Ok(())
        })
        .await;
    r
}

/// What a stack's deploy that failed tells (`stacks::run`: by hand, a webhook or the Git poller).
pub fn deploy_failed(app: &App, env: (i64, String), stack: &str, reason: &str) {
    notify(app, Event { kind: "deploy.failed", title: format!("{stack} did not deploy on {}", env.1), message: reason.to_string(), env: Some(env), target: Some(stack.to_string()) });
}

/// What a container's events say about it: a death nobody asked for, or a failing health check.
#[derive(Default)]
pub struct Deaths {
    /// When each container was last sent a signal (a stop, a kill, a restart): its death after that was asked for.
    asked: HashMap<String, i64>,
    /// Which ran out of memory just now.
    oom: HashMap<String, i64>,
}

/// A death within this long of a signal was asked for.
const ASKED_WITHIN_S: i64 = 60;

impl Deaths {
    /// Reads one of Docker's events; answers the event kind to tell, the container's name and what to say.
    pub fn see(&mut self, action: &str, id: &str, attrs: &HashMap<String, String>, at: i64) -> Option<(&'static str, String, String)> {
        self.asked.retain(|_, t| at - *t < ASKED_WITHIN_S);
        self.oom.retain(|_, t| at - *t < ASKED_WITHIN_S);
        let name = attrs.get("name").cloned().unwrap_or_else(|| id.chars().take(12).collect());
        match action {
            "kill" => {
                self.asked.insert(id.to_string(), at);
                None
            }
            "oom" => {
                self.oom.insert(id.to_string(), at);
                None
            }
            "die" => {
                let code = attrs.get("exitCode").map(String::as_str).unwrap_or("?");
                let oom = self.oom.remove(id).is_some();
                if self.asked.remove(id).is_some() || (code == "0" && !oom) {
                    return None;
                }
                let why = if oom { format!("It ran out of memory (exit code {code}).") } else { format!("It exited with code {code}.") };
                Some(("container.died", name, why))
            }
            a if a.starts_with("health_status") && a.contains("unhealthy") => Some(("container.unhealthy", name, "Its health check fails.".to_string())),
            _ => None,
        }
    }
}

/// Follows one environment's container events, telling the deaths and failing health checks, until it is aborted.
async fn watch_env(app: App, env: i64) {
    use futures_util::StreamExt;
    let mut deaths = Deaths::default();
    let mut since = now();
    loop {
        let name: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT name FROM environments WHERE id = ?1", [env], |r| r.get(0)).optional()?)).await.unwrap_or(None);
        let Some(name) = name else { return };
        if let Ok(d) = crate::docker::endpoint(&app, env).await {
            let filters = HashMap::from([("type".to_string(), vec!["container".to_string()]), ("event".to_string(), ["die", "kill", "oom", "health_status"].map(String::from).to_vec())]);
            // Read by raw::follow, so a line that never ends is refused at its limit ("Endpoints" 7).
            let path = crate::docker::streams::events_path(Some(since), Some(&filters));
            let Ok(s) = crate::docker::raw::follow::<bollard::models::EventMessage>(&d, &path).await else {
                tokio::time::sleep(Duration::from_secs(15)).await;
                continue;
            };
            futures_util::pin_mut!(s);
            while let Some(Ok(ev)) = s.next().await {
                let at = ev.time.unwrap_or_else(now);
                since = since.max(at);
                let (Some(action), Some(actor)) = (ev.action, ev.actor) else { continue };
                let attrs = actor.attributes.unwrap_or_default();
                if let Some((kind, container, why)) = deaths.see(&action, actor.id.as_deref().unwrap_or(""), &attrs, at) {
                    let title = if kind == "container.died" { format!("{container} died on {name}") } else { format!("{container} is unhealthy on {name}") };
                    notify(&app, Event { kind, title, message: why, env: Some((env, name.clone())), target: Some(container) });
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

/// Follows every environment's events (one task each, started for a new environment and stopped for a removed one),
/// until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    let mut tasks: HashMap<i64, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        let ids: Vec<i64> = app.db.call(|c| Ok(c.prepare("SELECT id FROM environments")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap_or_default();
        tasks.retain(|id, t| {
            let keep = ids.contains(id) && !t.is_finished();
            if !keep {
                t.abort();
            }
            keep
        });
        for id in ids {
            tasks.entry(id).or_insert_with(|| tokio::spawn(watch_env(app.clone(), id)));
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = stop.cancelled() => break,
        }
    }
    for t in tasks.values() {
        t.abort();
    }
}

#[derive(Deserialize)]
struct Fields {
    name: String,
    kind: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    config: Config,
}

fn yes() -> bool {
    true
}

fn check_fields(f: &Fields) -> ApiResult<(String, String)> {
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("A name is 1 to 64 characters.").field("name"));
    }
    if !KINDS.contains(&f.kind.as_str()) {
        return Err(ApiError::bad("A channel is a webhook, e-mail, ntfy or Gotify.").field("kind"));
    }
    let mut events: Vec<&str> = Vec::new();
    for e in &f.events {
        let Some((known, _)) = EVENTS.iter().find(|(k, _)| k == e) else { return Err(ApiError::bad(format!("Freightdock does not tell about \"{e}\".")).field("events")) };
        if !events.contains(known) {
            events.push(known);
        }
    }
    Ok((name.to_string(), serde_json::to_string(&events).map_err(anyhow::Error::from)?))
}

/// A channel as the settings page sees it: its settings without the secret, and whether there is one.
fn shown(r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Value> {
    let mut config: Config = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default();
    let has_secret = !config.secret.is_empty();
    config.secret = String::new();
    let mut c = json!(config);
    c.as_object_mut().map(|o| o.remove("secret"));
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?, "config": c, "has_secret": has_secret,
        "events": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(json!([])), "enabled": r.get::<_, bool>(5)?,
        "last_sent_at": r.get::<_, Option<i64>>(6)?, "last_error": r.get::<_, Option<String>>(7)?,
    }))
}

const COLUMNS: &str = "id, name, kind, config, events, enabled, last_sent_at, last_error";

async fn list(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let channels = app
        .db
        .call(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM notification_channels ORDER BY id"))?;
            let rows = st.query_map([], shown)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let events: Vec<Value> = EVENTS.iter().map(|(k, l)| json!({ "key": k, "label": l })).collect();
    Ok(Json(json!({ "channels": channels, "events": events })))
}

async fn one(app: &App, id: i64) -> ApiResult<Value> {
    app.db
        .call(move |c| Ok(c.query_row(&format!("SELECT {COLUMNS} FROM notification_channels WHERE id = ?1"), [id], shown).optional()?))
        .await?
        .ok_or_else(|| ApiError::not_found("channel"))
}

fn sealed(app: &App, mut c: Config, kept: Option<String>) -> ApiResult<Config> {
    c.secret = match c.secret.trim() {
        "" => kept.unwrap_or_default(),
        s => app.sealer.seal(s)?,
    };
    Ok(c)
}

async fn log(app: &App, admin: &Admin, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

async fn create(State(app): State<App>, admin: Admin, Json(f): Json<Fields>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (name, events) = check_fields(&f)?;
    let config = sealed(&app, f.config.checked(&f.kind, false)?, None)?;
    let (n, kind, enabled, raw) = (name.clone(), f.kind.clone(), f.enabled, serde_json::to_string(&config).map_err(anyhow::Error::from)?);
    let id = app
        .db
        .call(move |c| {
            c.execute("INSERT INTO notification_channels (name, kind, config, events, enabled, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![n, kind, raw, events, enabled, now()])?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    log(&app, &admin, "notification.create", name, Some(f.kind)).await;
    Ok((StatusCode::CREATED, Json(one(&app, id).await?)))
}

async fn stored(app: &App, id: i64) -> ApiResult<(String, Config)> {
    let row: Option<(String, String)> = app.db.call(move |c| Ok(c.query_row("SELECT kind, config FROM notification_channels WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await?;
    let (kind, raw) = row.ok_or_else(|| ApiError::not_found("channel"))?;
    Ok((kind, serde_json::from_str(&raw).unwrap_or_default()))
}

async fn update(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<Fields>) -> ApiResult<Json<Value>> {
    let (kind, old) = stored(&app, id).await?;
    let (name, events) = check_fields(&f)?;
    if f.kind != kind {
        return Err(ApiError::bad("A channel keeps its kind; make a new one for another.").field("kind"));
    }
    let kept = (!old.secret.is_empty()).then_some(old.secret);
    let config = sealed(&app, f.config.checked(&kind, kept.is_some())?, kept)?;
    let (n, enabled, raw) = (name.clone(), f.enabled, serde_json::to_string(&config).map_err(anyhow::Error::from)?);
    app.db.call(move |c| Ok(c.execute("UPDATE notification_channels SET name = ?1, config = ?2, events = ?3, enabled = ?4 WHERE id = ?5", params![n, raw, events, enabled, id])?)).await?;
    log(&app, &admin, "notification.update", name, None).await;
    Ok(Json(one(&app, id).await?))
}

async fn remove(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let name: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT name FROM notification_channels WHERE id = ?1", [id], |r| r.get(0)).optional()?)).await?;
    let name = name.ok_or_else(|| ApiError::not_found("channel"))?;
    app.db.call(move |c| Ok(c.execute("DELETE FROM notification_channels WHERE id = ?1", [id])?)).await?;
    log(&app, &admin, "notification.remove", name, None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/{id}/test`: a message through the channel now, whatever its events; the answer says whether
/// it went, and why not.
async fn test(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let (kind, config) = stored(&app, id).await?;
    let e = Event { kind: "test", title: "A test from Freightdock".into(), message: "This channel works: Freightdock's notifications reach you here.".into(), env: None, target: None };
    let r = send(&app, &Channel { id, kind: kind.clone(), config }, &e).await;
    log(&app, &admin, "notification.test", format!("channel {id}"), Some(r.as_ref().err().cloned().unwrap_or_else(|| "sent".into()))).await;
    r.map(|_| StatusCode::NO_CONTENT).map_err(|why| ApiError::new(StatusCode::BAD_GATEWAY, why))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;

    fn event() -> Event {
        Event { kind: "container.died", title: "web died on local".into(), message: "It exited with code 1.".into(), env: Some((1, "local".into())), target: Some("web".into()) }
    }

    #[test]
    fn a_webhook_is_signed_with_hmac_sha256() {
        // RFC 4231, test case 2.
        assert_eq!(sign("Jefe", b"what do ya want for nothing?"), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        let c = Config { url: "https://hooks.example/x".into(), ..Default::default() };
        let r = http_request("webhook", &c, "s3cret", &event(), 1_700_000_000).unwrap();
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body["event"], "container.died");
        assert_eq!(body["environment"]["name"], "local");
        assert_eq!(body["target"], "web");
        let sig = r.headers.iter().find(|(k, _)| *k == "x-freightdock-signature").unwrap().1.clone();
        assert_eq!(sig, format!("sha256={}", sign("s3cret", r.body.as_bytes())));
        let unsigned = http_request("webhook", &c, "", &event(), 0).unwrap();
        assert!(unsigned.headers.iter().all(|(k, _)| *k != "x-freightdock-signature"));
    }

    #[test]
    fn ntfy_and_gotify_get_their_own_shapes() {
        let c = Config { server: "https://ntfy.example".into(), topic: "ops".into(), ..Default::default() };
        let r = http_request("ntfy", &c, "tk_1", &event(), 0).unwrap();
        assert_eq!(r.url, "https://ntfy.example/");
        let b: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!((b["topic"].as_str(), b["title"].as_str(), b["priority"].as_u64()), (Some("ops"), Some("web died on local"), Some(4)));
        assert!(r.headers.contains(&("authorization", "Bearer tk_1".into())));
        let g = http_request("gotify", &Config { server: "https://gotify.example".into(), priority: Some(8), ..Default::default() }, "AppTok", &event(), 0).unwrap();
        assert_eq!(g.url, "https://gotify.example/message");
        assert!(g.headers.contains(&("x-gotify-key", "AppTok".into())));
        assert_eq!(serde_json::from_str::<Value>(&g.body).unwrap()["priority"], 8);
    }

    #[test]
    fn a_mail_says_what_happened_where() {
        let c = Config { from: "Freightdock <freightdock@example.com>".into(), to: vec!["ops@example.com".into()], ..Default::default() };
        let m = String::from_utf8(mail(&c, &event(), 1_762_722_849).unwrap().formatted()).unwrap();
        assert!(m.contains("Subject: Freightdock: web died on local"), "{m}");
        assert!(m.contains("To: ops@example.com"));
        assert!(m.contains("Environment: local"));
        assert!(m.contains("2025-11-09 21:14:09 UTC"), "{m}");
    }

    #[test]
    fn each_kind_is_checked_and_given_its_defaults() {
        let ntfy = Config { topic: "ops".into(), ..Default::default() }.checked("ntfy", false).unwrap();
        assert_eq!(ntfy.server, "https://ntfy.sh");
        assert_eq!(Config { topic: "a b".into(), ..Default::default() }.checked("ntfy", false).unwrap_err().field, Some("topic"));
        assert_eq!(Config { url: "ftp://x".into(), ..Default::default() }.checked("webhook", false).unwrap_err().field, Some("url"));
        assert_eq!(Config { server: "https://g.example".into(), ..Default::default() }.checked("gotify", false).unwrap_err().field, Some("secret"));
        assert!(Config { server: "https://g.example".into(), ..Default::default() }.checked("gotify", true).is_ok(), "an edit keeps the token there is");
        let mail = Config { host: "smtp.example.com".into(), from: "a@example.com".into(), to: vec![" b@example.com ".into(), "".into()], ..Default::default() }.checked("email", false).unwrap();
        assert_eq!((mail.security.as_str(), mail.port(), mail.to.clone()), ("starttls", 587, vec!["b@example.com".to_string()]));
        assert_eq!(Config { host: "smtp.example.com".into(), from: "nobody".into(), to: vec!["b@example.com".into()], ..Default::default() }.checked("email", false).unwrap_err().field, Some("from"));
    }

    #[test]
    fn the_same_news_is_told_once_in_a_while() {
        let n = Notifier::default();
        assert!(n.allow("container.died|1|web", 1000));
        assert!(!n.allow("container.died|1|web", 1000 + QUIET_S - 1));
        assert!(n.allow("container.died|1|db", 1001), "another container is other news");
        assert!(n.allow("container.died|1|web", 1000 + QUIET_S));
    }

    fn attrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_death_nobody_asked_for_is_told_and_a_stop_is_not() {
        let mut d = Deaths::default();
        let web = attrs(&[("name", "web"), ("exitCode", "1")]);
        assert_eq!(d.see("die", "abc", &web, 100), Some(("container.died", "web".into(), "It exited with code 1.".into())));
        assert_eq!(d.see("kill", "abc", &attrs(&[("name", "web"), ("signal", "15")]), 200), None);
        assert_eq!(d.see("die", "abc", &attrs(&[("name", "web"), ("exitCode", "143")]), 201), None, "it was stopped");
        assert_eq!(d.see("die", "abc", &web, 300), Some(("container.died", "web".into(), "It exited with code 1.".into())), "the stop is spent");
        assert_eq!(d.see("die", "job", &attrs(&[("name", "job"), ("exitCode", "0")]), 400), None, "a job that finished did not die");
        assert_eq!(d.see("oom", "big", &attrs(&[("name", "big")]), 500), None);
        assert_eq!(d.see("die", "big", &attrs(&[("name", "big"), ("exitCode", "137")]), 500).unwrap().2, "It ran out of memory (exit code 137).");
        assert_eq!(d.see("health_status: unhealthy", "abc", &attrs(&[("name", "web")]), 600).map(|x| x.0), Some("container.unhealthy"));
        assert_eq!(d.see("health_status: healthy", "abc", &attrs(&[("name", "web")]), 601), None);
    }

    #[tokio::test]
    async fn channels_are_an_administrator_s_their_secret_sealed_and_never_shown() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let body = json!({ "name": "ops hook", "kind": "webhook", "events": ["container.died", "environment.down"], "config": { "url": "https://hooks.example/x", "secret": "s3cret" } });
        let (s, _, b) = call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(body))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!(b["has_secret"], true);
        assert!(!b.to_string().contains("s3cret"));
        let id = b["id"].as_i64().unwrap();
        let raw: String = app.db.call(move |c| Ok(c.query_row("SELECT config FROM notification_channels WHERE id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert!(!raw.contains("s3cret") && raw.contains("sealed1:"));

        // An edit without a secret keeps it.
        let edit = json!({ "name": "ops", "kind": "webhook", "events": ["container.died"], "config": { "url": "https://hooks.example/y" } });
        let (s, _, b) = call(&app, req(Method::PUT, &format!("/api/notifications/{id}"), &cookie, &csrf, Some(edit))).await;
        assert_eq!((s, b["has_secret"].as_bool(), b["config"]["url"].as_str()), (StatusCode::OK, Some(true), Some("https://hooks.example/y")));
        let (_, config) = stored(&app, id).await.unwrap();
        assert_eq!(app.sealer.open(&config.secret).unwrap(), "s3cret");

        let (s, _, b) = call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "x", "kind": "webhook", "events": ["nonsense"], "config": { "url": "https://h.example" } })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("events")));
        let (_, _, b) = call(&app, req(Method::GET, "/api/notifications", &cookie, &csrf, None)).await;
        assert_eq!(b["channels"].as_array().unwrap().len(), 1);
        assert_eq!(b["events"].as_array().unwrap().len(), EVENTS.len());
        let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/notifications/{id}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let actions: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action FROM audit WHERE action LIKE 'notification.%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap();
        assert_eq!(actions, vec!["notification.create", "notification.update", "notification.remove"]);

        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let (s, _, _) = call(&app, req(Method::GET, "/api/notifications", &format!("freightdock_session={token}"), "", None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    /// A one-request HTTP listener on loopback: answers 200 and hands over what came.
    /// A webhook receiver on loopback: its address, and the first request it gets, head and body.
    pub(crate) async fn listener() -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
            let _ = tx.send(String::from_utf8_lossy(&got).to_string());
        });
        (format!("http://{addr}/hook"), rx)
    }

    #[tokio::test]
    async fn the_test_button_sends_through_the_channel_and_says_when_it_cannot() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (url, got) = listener().await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "hook", "kind": "webhook", "events": [], "config": { "url": url, "secret": "k" } })))).await;
        let id = b["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::POST, &format!("/api/notifications/{id}/test"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let got = got.await.unwrap();
        assert!(got.starts_with("POST /hook"), "{got}");
        assert!(got.contains("x-freightdock-signature: sha256="));
        assert!(got.contains("\"event\":\"test\""));

        let (_, _, b) = call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "dead", "kind": "webhook", "events": [], "config": { "url": "http://127.0.0.1:1/x" } })))).await;
        let dead = b["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::POST, &format!("/api/notifications/{dead}/test"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert!(b["error"].as_str().unwrap().contains("127.0.0.1:1"), "{b}");
        let (_, _, b) = call(&app, req(Method::GET, "/api/notifications", &cookie, &csrf, None)).await;
        assert!(b["channels"][0]["last_sent_at"].is_i64());
        assert!(b["channels"][1]["last_error"].is_string());
    }

    #[tokio::test]
    async fn an_event_reaches_the_channels_that_want_it() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (url, got) = listener().await;
        call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "deaf", "kind": "webhook", "events": ["update.available"], "config": { "url": "http://127.0.0.1:1/x" } })))).await;
        call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "hook", "kind": "webhook", "events": ["deploy.failed"], "config": { "url": url } })))).await;
        deploy_failed(&app, (1, "local".into()), "blog", "compose said no");
        let got = tokio::time::timeout(Duration::from_secs(10), got).await.unwrap().unwrap();
        assert!(got.contains("\"event\":\"deploy.failed\"") && got.contains("blog did not deploy on local"), "{got}");
        let deaf_error: Option<String> = app.db.call(|c| Ok(c.query_row("SELECT last_error FROM notification_channels WHERE name = 'deaf'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(deaf_error, None, "a channel that does not want it is not asked");
    }

    #[tokio::test]
    async fn an_environment_that_stops_answering_is_told_once() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (url, got) = listener().await;
        call(&app, req(Method::POST, "/api/notifications", &cookie, &csrf, Some(json!({ "name": "hook", "kind": "webhook", "events": ["environment.down"], "config": { "url": url } })))).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "far", "kind": "agent" })))).await;
        let id = b["environment"]["id"].as_i64().unwrap();
        app.db.call(move |c| Ok(c.execute("UPDATE environments SET status = 'up' WHERE id = ?1", [id])?)).await.unwrap();
        crate::environments::check(&app, id).await;
        let got = tokio::time::timeout(Duration::from_secs(10), got).await.unwrap().unwrap();
        assert!(got.contains("\"event\":\"environment.down\"") && got.contains("far is down"), "{got}");
        assert!(got.contains("The agent for far is not connected."));
        // Down already: nothing more to tell.
        crate::environments::check(&app, id).await;
        assert!(!app.notifier.allow(&format!("environment.down|{id}|far"), now()), "told once, and quiet after");
    }
}
