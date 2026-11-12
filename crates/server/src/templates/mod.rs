//! App templates: lists in Portainer's template JSON (v2 and v3, and v1's bare list), read from the addresses in the
//! settings so the community's lists work as they are, and custom templates of Freightdock's own (a compose file and the
//! variables it asks for). A container template becomes a container; a stack template a stack: from its repository
//! (a Git stack, as Portainer deploys it), from its compose file's address, or from the custom template's file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{Admin, Allowed, Authed};
use crate::audit::{self, Entry, Who};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Portainer's own list, the one most people know: what Freightdock reads until the settings say otherwise.
pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/portainer/templates/v3/templates.json";

/// A list or a compose file fetched by its address may be this big; Portainer's whole list is about 100 KB.
const MAX_FETCH: usize = 8 * 1024 * 1024;
/// A list is asked for again after this long.
const KEEP: Duration = Duration::from_secs(10 * 60);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Container,
    Stack,
    /// A Swarm stack: listed, not deployable until Swarm comes (M6).
    Swarm,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Choice {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub default: bool,
}

/// A variable a template asks for: Portainer's `env` entry.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Var {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: String,
    /// Set by the template, not asked.
    #[serde(default)]
    pub preset: bool,
    #[serde(default)]
    pub select: Vec<Choice>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Volume {
    pub container: String,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Repository {
    pub url: String,
    #[serde(default)]
    pub stackfile: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Label {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// One template, whichever list it came from, as the page shows it and a deploy reads it.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Template {
    pub kind: Kind,
    pub title: String,
    pub description: String,
    /// Portainer's notes are HTML; the page shows them as text ("UI" 2).
    pub note: String,
    pub categories: Vec<String>,
    pub platform: String,
    /// An http(s) address only.
    pub logo: Option<String>,
    pub image: Option<String>,
    pub name: Option<String>,
    pub command: Option<String>,
    pub interactive: bool,
    pub privileged: bool,
    pub restart_policy: Option<String>,
    pub hostname: Option<String>,
    pub network: Option<String>,
    pub ports: Vec<String>,
    pub volumes: Vec<Volume>,
    pub labels: Vec<Label>,
    pub env: Vec<Var>,
    pub repository: Option<Repository>,
    /// A compose file by its address (v2's type 4).
    pub stackfile: Option<String>,
    /// A custom template's own compose file.
    pub compose: Option<String>,
    pub deployable: bool,
}

/// A template entry as Portainer writes it; every field may be missing.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Raw {
    #[serde(rename = "type")]
    ty: i64,
    title: String,
    description: String,
    note: String,
    categories: Vec<String>,
    platform: String,
    logo: String,
    image: String,
    name: String,
    command: String,
    interactive: bool,
    privileged: bool,
    restart_policy: String,
    hostname: String,
    network: String,
    ports: Vec<String>,
    volumes: Vec<Volume>,
    labels: Vec<Label>,
    env: Vec<Var>,
    repository: Option<Repository>,
    stackfile: String,
}

fn some(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn http_only(s: String) -> Option<String> {
    some(s).filter(|l| l.starts_with("https://") || l.starts_with("http://"))
}

impl From<Raw> for Template {
    fn from(r: Raw) -> Self {
        let repository = r.repository.filter(|g| !g.url.trim().is_empty() && !g.stackfile.trim().is_empty());
        let stackfile = http_only(r.stackfile);
        let kind = match r.ty {
            2 => Kind::Swarm,
            3 | 4 => Kind::Stack,
            _ => Kind::Container,
        };
        let image = some(r.image);
        let deployable = match kind {
            Kind::Container => image.is_some(),
            Kind::Stack => repository.is_some() || stackfile.is_some(),
            Kind::Swarm => false,
        };
        Template {
            kind,
            title: r.title.trim().to_string(),
            description: r.description,
            note: r.note,
            categories: r.categories,
            platform: r.platform,
            logo: http_only(r.logo),
            image,
            name: some(r.name),
            command: some(r.command),
            interactive: r.interactive,
            privileged: r.privileged,
            restart_policy: some(r.restart_policy),
            hostname: some(r.hostname),
            network: some(r.network),
            ports: r.ports,
            volumes: r.volumes.into_iter().filter(|v| !v.container.trim().is_empty()).collect(),
            labels: r.labels,
            env: r.env.into_iter().filter(|v| !v.name.trim().is_empty()).collect(),
            repository,
            stackfile,
            compose: None,
            deployable,
        }
    }
}

/// A template list in Portainer's JSON: `{"version": "2" or "3", "templates": [...]}`, or v1's bare list. An entry that
/// cannot be read (or has no title) is left out, not the whole list.
pub fn parse(bytes: &[u8]) -> Result<Vec<Template>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("That is not a template list (not JSON: {e})."))?;
    let entries = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => match o.get("templates") {
            Some(Value::Array(a)) => a.clone(),
            _ => return Err("That is not a template list: it has no \"templates\".".into()),
        },
        _ => return Err("That is not a template list.".into()),
    };
    Ok(entries.into_iter().filter_map(|e| serde_json::from_value::<Raw>(e).ok()).map(Template::from).filter(|t| !t.title.is_empty()).collect())
}

/// What a variable is at deploy: the template's own when it is preset, else what was given, else its default (or the
/// default choice).
pub fn value_for(v: &Var, given: &HashMap<String, String>) -> String {
    let fallback = || if v.default.is_empty() { v.select.iter().find(|c| c.default).map(|c| c.value.clone()).unwrap_or_default() } else { v.default.clone() };
    if v.preset {
        return fallback();
    }
    given.get(&v.name).cloned().unwrap_or_else(fallback)
}

/// A command line as words, as a shell would split it: spaces apart, quotes kept together.
pub fn split_command(cmd: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut any) = (Vec::new(), String::new(), None::<char>, false);
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A port as a template writes it, `80/tcp` or `8080:80/udp`: (the container's port with its protocol, the host's).
fn port(p: &str) -> Option<(String, String)> {
    let (ports, proto) = p.trim().split_once('/').unwrap_or((p.trim(), "tcp"));
    let (host, inside) = ports.rsplit_once(':').map_or(("", ports), |(h, c)| (h, c));
    (!inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit() || c == '-')).then(|| (format!("{inside}/{proto}"), host.to_string()))
}

/// Docker's create body for a container template with the values given: its image, command, variables, ports (the
/// host's port Docker picks when the template names none, as Portainer does), volumes (anonymous ones, or the binds it
/// names), labels and settings.
pub fn container_body(t: &Template, given: &HashMap<String, String>) -> ContainerCreateBody {
    let env: Vec<String> = t.env.iter().map(|v| format!("{}={}", v.name, value_for(v, given))).collect();
    let mut exposed = HashMap::new();
    let mut bindings = HashMap::new();
    for (inside, host) in t.ports.iter().filter_map(|p| port(p)) {
        exposed.insert(inside.clone(), HashMap::new());
        bindings.insert(inside, Some(vec![PortBinding { host_ip: None, host_port: Some(host) }]));
    }
    let binds: Vec<String> = t.volumes.iter().filter_map(|v| v.bind.as_ref().map(|b| format!("{b}:{}{}", v.container, if v.readonly { ":ro" } else { "" }))).collect();
    let anonymous: HashMap<String, HashMap<(), ()>> = t.volumes.iter().filter(|v| v.bind.is_none()).map(|v| (v.container.clone(), HashMap::new())).collect();
    let restart = match t.restart_policy.as_deref().unwrap_or("unless-stopped") {
        "always" => RestartPolicyNameEnum::ALWAYS,
        "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
        "no" => RestartPolicyNameEnum::NO,
        _ => RestartPolicyNameEnum::UNLESS_STOPPED,
    };
    ContainerCreateBody {
        image: t.image.clone(),
        cmd: t.command.as_deref().map(split_command).filter(|c| !c.is_empty()),
        env: (!env.is_empty()).then_some(env),
        exposed_ports: (!exposed.is_empty()).then_some(exposed),
        volumes: (!anonymous.is_empty()).then_some(anonymous),
        tty: t.interactive.then_some(true),
        open_stdin: t.interactive.then_some(true),
        hostname: t.hostname.clone(),
        labels: (!t.labels.is_empty()).then(|| t.labels.iter().map(|l| (l.name.clone(), l.value.clone())).collect()),
        host_config: Some(HostConfig {
            port_bindings: (!bindings.is_empty()).then_some(bindings),
            binds: (!binds.is_empty()).then_some(binds),
            privileged: t.privileged.then_some(true),
            network_mode: t.network.clone(),
            restart_policy: Some(RestartPolicy { name: Some(restart), maximum_retry_count: None }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A stack template's variables as its `.env`: a line each, in the template's order.
pub fn env_file(t: &Template, given: &HashMap<String, String>) -> String {
    t.env.iter().map(|v| format!("{}={}\n", v.name, value_for(v, given).replace('\n', " "))).collect()
}

/// A Compose project name from a template's title: "My App 2" is `my-app-2`.
pub fn project_name(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').chars().take(63).collect::<String>();
    if s.is_empty() { "app".into() } else { s }
}

fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("Freightdock/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default()
    })
    .clone()
}

/// What is at `url`, up to `MAX_FETCH` bytes; the reason in words when it cannot be had.
pub async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let mut r = http().get(url).send().await.map_err(|e| format!("{url} did not answer: {}", e.without_url()))?;
    if !r.status().is_success() {
        return Err(format!("{url} answered {}.", r.status()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|e| format!("{url} stopped answering: {}", e.without_url()))? {
        out.extend_from_slice(&chunk);
        if out.len() > MAX_FETCH {
            return Err(format!("{url} is bigger than {} MB.", MAX_FETCH / 1024 / 1024));
        }
    }
    Ok(out)
}

/// A list as fetched, and when.
type Fetched = (Instant, Arc<Vec<Template>>);

/// The lists fetched lately, by address.
#[derive(Default)]
pub struct Cache {
    lists: Mutex<HashMap<String, Fetched>>,
}

impl Cache {
    /// The list at `url`, from the cache when it is fresh and `fresh` is not asked for.
    pub async fn list(&self, url: &str, fresh: bool) -> Result<Arc<Vec<Template>>, String> {
        if !fresh
            && let Some((at, l)) = self.lists.lock().unwrap_or_else(|e| e.into_inner()).get(url)
            && at.elapsed() < KEEP
        {
            return Ok(l.clone());
        }
        let l = Arc::new(parse(&fetch(url).await?)?);
        self.lists.lock().unwrap_or_else(|e| e.into_inner()).insert(url.to_string(), (Instant::now(), l.clone()));
        Ok(l)
    }
}

/// The template list addresses in the settings; Portainer's own when nothing is set.
pub async fn urls(app: &App) -> ApiResult<Vec<String>> {
    let v: Option<String> = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'templates'", [], |r| r.get(0)).optional()?)).await?;
    Ok(match v.and_then(|v| serde_json::from_str::<Value>(&v).ok()) {
        Some(v) => v["urls"].as_array().map(|a| a.iter().filter_map(|u| u.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        None => vec![DEFAULT_URL.to_string()],
    })
}

/// `GET /api/settings/templates`
pub async fn get_settings(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "urls": urls(&app).await?, "default": DEFAULT_URL })))
}

#[derive(Deserialize)]
pub struct Settings {
    urls: Vec<String>,
}

/// `PUT /api/settings/templates`: the list addresses, http(s) only.
pub async fn put_settings(State(app): State<App>, admin: Admin, Json(s): Json<Settings>) -> ApiResult<StatusCode> {
    let urls: Vec<String> = s.urls.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
    if let Some(bad) = urls.iter().find(|u| http_only((*u).clone()).is_none() || url::Url::parse(u).is_err()) {
        return Err(ApiError::bad(format!("\"{bad}\" is not an http or https address.")).field("urls"));
    }
    if urls.len() > 20 {
        return Err(ApiError::bad("Twenty lists are plenty.").field("urls"));
    }
    let v = json!({ "urls": urls }).to_string();
    app.db.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('templates', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [v])?)).await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "settings.templates", target: None, detail: Some(urls.join(" ")) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A custom template as the database keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Custom {
    #[serde(default)]
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub logo: String,
    pub compose: String,
    #[serde(default)]
    pub env: Vec<Var>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

impl Custom {
    pub fn template(&self) -> Template {
        Template {
            kind: Kind::Stack,
            title: self.title.clone(),
            description: self.description.clone(),
            note: self.note.clone(),
            categories: self.categories.clone(),
            logo: http_only(self.logo.clone()),
            env: self.env.clone(),
            compose: Some(self.compose.clone()),
            deployable: true,
            ..Default::default()
        }
    }
}

const CUSTOM: &str = "id, title, description, note, categories, logo, compose, env, created_by, updated_at";

fn custom_row(r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Custom> {
    Ok(Custom {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        note: r.get(3)?,
        categories: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
        logo: r.get(5)?,
        compose: r.get(6)?,
        env: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        created_by: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

async fn customs(app: &App) -> ApiResult<Vec<Custom>> {
    Ok(app
        .db
        .call(|c| {
            let mut st = c.prepare(&format!("SELECT {CUSTOM} FROM templates ORDER BY title COLLATE NOCASE"))?;
            let rows = st.query_map([], custom_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}

async fn custom(app: &App, id: i64) -> ApiResult<Custom> {
    app.db.call(move |c| Ok(c.query_row(&format!("SELECT {CUSTOM} FROM templates WHERE id = ?1"), [id], custom_row).optional()?)).await?.ok_or_else(|| ApiError::not_found("template"))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// Ask every list again, not the copy kept for ten minutes.
    #[serde(default)]
    refresh: bool,
}

/// `GET /api/templates`: every list with its templates (or why it could not be read), and the custom templates.
pub async fn list(State(app): State<App>, _: Authed, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let mut lists = Vec::new();
    for url in urls(&app).await? {
        lists.push(match app.templates.list(&url, q.refresh).await {
            Ok(l) => json!({ "url": url, "templates": *l }),
            Err(e) => json!({ "url": url, "templates": [], "error": e }),
        });
    }
    Ok(Json(json!({ "lists": lists, "custom": customs(&app).await? })))
}

fn check_custom(c: &Custom) -> ApiResult<()> {
    if c.title.trim().is_empty() {
        return Err(ApiError::bad("A template needs a title.").field("title"));
    }
    if c.compose.trim().is_empty() {
        return Err(ApiError::bad("A template needs its compose file.").field("compose"));
    }
    if c.compose.len() > 1024 * 1024 {
        return Err(ApiError::bad("That file is over a megabyte; a compose file is not.").field("compose"));
    }
    if let Some(v) = c.env.iter().find(|v| crate::stacks::compose::check_env(&format!("{}=", v.name)).is_err() || v.name.contains('=')) {
        return Err(ApiError::bad(format!("\"{}\" is not a variable name.", v.name)).field("env"));
    }
    Ok(())
}

/// `GET /api/templates/custom/{id}`
pub async fn get_custom(State(app): State<App>, _: Authed, Path(id): Path<i64>) -> ApiResult<Json<Custom>> {
    Ok(Json(custom(&app, id).await?))
}

/// `POST /api/templates/custom`
pub async fn create_custom(State(app): State<App>, admin: Admin, Json(c): Json<Custom>) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = insert_custom(&app, &Who::from(&admin), c, None).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// Saves a new custom template, checked; `detail` says in the activity log where it came from.
pub async fn insert_custom(app: &App, who: &Who, c: Custom, detail: Option<String>) -> ApiResult<i64> {
    check_custom(&c)?;
    let by = who.user.as_ref().map(|u| u.name.clone());
    let title = c.title.trim().to_string();
    let t = title.clone();
    let id = app
        .db
        .call(move |db| {
            let at = now();
            db.execute(
                "INSERT INTO templates (title, description, note, categories, logo, compose, env, created_by, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![t, c.description, c.note, serde_json::to_string(&c.categories)?, c.logo.trim(), c.compose, serde_json::to_string(&c.env)?, by, at],
            )?;
            Ok(db.last_insert_rowid())
        })
        .await?;
    audit::record_by(&app.db, who, Entry { action: "template.create", target: Some(title), detail }).await;
    Ok(id)
}

/// `PUT /api/templates/custom/{id}`
pub async fn update_custom(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(c): Json<Custom>) -> ApiResult<StatusCode> {
    check_custom(&c)?;
    custom(&app, id).await?;
    let title = c.title.trim().to_string();
    let t = title.clone();
    app.db
        .call(move |db| {
            db.execute(
                "UPDATE templates SET title = ?1, description = ?2, note = ?3, categories = ?4, logo = ?5, compose = ?6, env = ?7, updated_at = ?8 WHERE id = ?9",
                params![t, c.description, c.note, serde_json::to_string(&c.categories)?, c.logo.trim(), c.compose, serde_json::to_string(&c.env)?, now(), id],
            )?;
            Ok(())
        })
        .await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "template.update", target: Some(title), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/templates/custom/{id}`
pub async fn remove_custom(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let c = custom(&app, id).await?;
    app.db.call(move |db| Ok(db.execute("DELETE FROM templates WHERE id = ?1", [id])?)).await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "template.remove", target: Some(c.title), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Deploy {
    /// A template from a list: the list's address and the template's place in it.
    list: Option<String>,
    index: Option<usize>,
    /// Or a custom template, by its id.
    custom: Option<i64>,
    /// The container's or the stack's name.
    #[serde(default)]
    name: String,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// `POST /api/envs/{env}/templates/deploy`: a container template becomes a container (pulled and started), a stack
/// template a stack (not yet deployed: the page deploys it next, with Compose's output).
pub async fn deploy(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(d): Json<Deploy>) -> ApiResult<(StatusCode, Json<Value>)> {
    let t = match (&d.list, d.index, d.custom) {
        (_, _, Some(id)) => custom(&app, id).await?.template(),
        (Some(url), Some(i), None) => {
            if !urls(&app).await?.contains(url) {
                return Err(ApiError::bad("That list is not one of the template lists in the settings."));
            }
            let l = app.templates.list(url, false).await.map_err(|m| ApiError::new(StatusCode::BAD_GATEWAY, m))?;
            l.get(i).cloned().ok_or_else(|| ApiError::not_found("template"))?
        }
        _ => return Err(ApiError::bad("Say which template.")),
    };
    if !t.deployable {
        return Err(ApiError::bad(match t.kind {
            Kind::Swarm => "That is a Swarm stack; Freightdock does not manage Swarm yet.",
            _ => "That template says nothing Freightdock can deploy.",
        }));
    }
    let name = d.name.trim().to_string();
    match t.kind {
        Kind::Container => {
            let name = if name.is_empty() { t.name.clone() } else { Some(name) };
            let body = container_body(&t, &d.env);
            let (_, Json(made)) = crate::docker::containers::create(State(app), admin, Path(env), Json(crate::docker::containers::Create::new(name, body))).await?;
            Ok((StatusCode::CREATED, Json(json!({ "container": made["id"] }))))
        }
        _ => {
            let who = Who::from(&admin);
            let name = if name.is_empty() { project_name(&t.title) } else { name };
            crate::stacks::unclaimed(&app, &admin.1, env, &name).await?;
            let env_text = env_file(&t, &d.env);
            let id = if let Some(compose) = &t.compose {
                crate::stacks::make(&app, &who, env, &name, compose, &env_text, "stack.create").await?
            } else if let Some(r) = &t.repository {
                crate::stacks::make_git(&app, &who, env, &crate::stacks::GitForm::for_template(&name, &r.url, &r.stackfile, &env_text)).await?
            } else if let Some(url) = &t.stackfile {
                let bytes = fetch(url).await.map_err(|m| ApiError::new(StatusCode::BAD_GATEWAY, m))?;
                let compose = String::from_utf8(bytes).map_err(|_| ApiError::bad("Its compose file is not text."))?;
                crate::stacks::make(&app, &who, env, &name, &compose, &env_text, "stack.create").await?
            } else {
                return Err(ApiError::bad("That template says nothing Freightdock can deploy."));
            };
            crate::stacks::own(&app, env, id, crate::access::ownership::Wanted::default().owner(&admin.1)?).await?;
            Ok((StatusCode::CREATED, Json(json!({ "stack": id }))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2: &[u8] = include_bytes!("fixtures/v2.json");
    const V3: &[u8] = include_bytes!("fixtures/v3.json");

    #[test]
    fn a_v2_list_reads_every_kind_and_leaves_out_what_it_cannot_read() {
        let l = parse(V2).unwrap();
        let titles: Vec<&str> = l.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Registry", "Ubuntu", "Monitor", "Swarm monitoring", "WordPress", "Edge thing"], "the broken and the untitled are left out");
        assert_eq!(l[0].kind, Kind::Container);
        assert_eq!(l[0].logo.as_deref(), Some("https://example.com/logos/registry.png"));
        assert_eq!(l[1].logo, None, "only http(s) logos");
        assert!(l[1].interactive);
        assert_eq!((l[3].kind, l[3].deployable), (Kind::Swarm, false));
        assert_eq!(l[4].kind, Kind::Stack);
        assert_eq!(l[4].repository.as_ref().unwrap().stackfile, "stacks/wordpress/docker-compose.yml");
        assert_eq!((l[5].kind, l[5].stackfile.as_deref()), (Kind::Stack, Some("https://example.com/edge/docker-compose.yml")));
        assert!(l[5].deployable);
    }

    #[test]
    fn a_v3_list_reads_the_same_and_a_bare_list_is_v1() {
        let l = parse(V3).unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].env[1].name, "DB_HOST");
        assert_eq!(l[1].note, "Deploys a WordPress instance connected to a MySQL database.");
        let v1 = parse(br#"[{"type": 1, "title": "Nginx", "image": "nginx"}]"#).unwrap();
        assert_eq!(v1[0].image.as_deref(), Some("nginx"));
        assert!(parse(b"<html>").unwrap_err().contains("not JSON"));
        assert!(parse(br#"{"version": "3"}"#).is_err());
    }

    #[test]
    fn a_container_template_becomes_docker_s_create_body() {
        let l = parse(V2).unwrap();
        let mon = &l[2];
        let b = container_body(mon, &HashMap::from([("TOKEN".to_string(), "abc".to_string())]));
        assert_eq!(b.image.as_deref(), Some("example/monitor:2"));
        assert_eq!(b.env, Some(vec!["LEVEL=info".to_string(), "TOKEN=abc".to_string()]), "a choice's default, and what was given");
        let hc = b.host_config.unwrap();
        let ports = hc.port_bindings.unwrap();
        assert_eq!(ports["80/tcp"].as_ref().unwrap()[0].host_port.as_deref(), Some("8080"));
        assert_eq!(ports["53/udp"].as_ref().unwrap()[0].host_port.as_deref(), Some(""), "Docker picks the host's port");
        assert_eq!(hc.binds, Some(vec!["/var/run/docker.sock:/var/run/docker.sock:ro".to_string(), "/srv/monitor:/data".to_string()]));
        assert_eq!(hc.privileged, Some(true));
        assert_eq!(hc.network_mode.as_deref(), Some("host"));
        assert_eq!(hc.restart_policy.unwrap().name, Some(RestartPolicyNameEnum::ALWAYS));
        assert_eq!(b.hostname.as_deref(), Some("mon"));
        assert_eq!(b.labels.unwrap()["com.example.role"], "monitor");

        let reg = container_body(&l[0], &HashMap::from([("REGISTRY_PROXY_REMOTEURL".to_string(), "https://evil".to_string())]));
        assert_eq!(reg.env, Some(vec!["REGISTRY_PROXY_REMOTEURL=https://registry-1.docker.io".to_string()]), "a preset is the template's");
        assert!(reg.volumes.unwrap().contains_key("/var/lib/registry"), "an anonymous volume");
        assert_eq!(reg.host_config.unwrap().restart_policy.unwrap().name, Some(RestartPolicyNameEnum::UNLESS_STOPPED));

        let ub = container_body(&l[1], &HashMap::new());
        assert_eq!(ub.cmd, Some(vec!["/bin/bash".to_string(), "-c".to_string(), "echo hi; sleep infinity".to_string()]));
        assert_eq!((ub.tty, ub.open_stdin), (Some(true), Some(true)));
    }

    #[test]
    fn commands_split_as_a_shell_would() {
        assert_eq!(split_command("a  b"), ["a", "b"]);
        assert_eq!(split_command(r#"sh -c "echo \"hi\" there" ''"#), ["sh", "-c", "echo \"hi\" there", ""]);
        assert_eq!(split_command(r"a\ b c"), ["a b", "c"]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn a_stack_template_s_variables_are_its_env_file_and_its_title_its_name() {
        let l = parse(V3).unwrap();
        assert_eq!(env_file(&l[1], &HashMap::from([("MYSQL_DATABASE_PASSWORD".into(), "pw".into())])), "MYSQL_DATABASE_PASSWORD=pw\nDB_HOST=\n");
        assert_eq!(project_name("WordPress"), "wordpress");
        assert_eq!(project_name("  My App (2)!"), "my-app-2");
        assert_eq!(project_name("!!!"), "app");
    }

    mod api {
        use super::*;
        use crate::api::tests::{call, signed_in};
        use crate::stacks::tests::{app_with_compose, send};
        use axum::body::Body;
        use axum::http::{Request, header};

        /// Serves `body` at `/list.json` on a port of its own; answers the address.
        async fn serve(body: &'static [u8]) -> String {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            let r = axum::Router::new().route("/list.json", axum::routing::get(move || async move { body }));
            tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
            format!("http://{addr}/list.json")
        }

        #[tokio::test]
        async fn lists_come_from_the_settings_and_custom_templates_deploy_as_stacks() {
            let dir = tempfile::tempdir().unwrap();
            let app = app_with_compose(dir.path());
            let (cookie, csrf) = signed_in(&app).await;
            let url = serve(V3).await;
            let get = |p: &str| Request::get(p).header(header::COOKIE, &cookie).body(Body::empty()).unwrap();
            let (_, _, s) = call(&app, get("/api/settings/templates")).await;
            assert_eq!(s["urls"], json!([DEFAULT_URL]), "Portainer's list until told otherwise");
            let (st, b) = send(&app, &cookie, &csrf, "PUT", "/api/settings/templates", json!({ "urls": ["file:///etc/passwd"] })).await;
            assert_eq!((st, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("urls")));
            let (st, _) = send(&app, &cookie, &csrf, "PUT", "/api/settings/templates", json!({ "urls": [url, "http://127.0.0.1:1/nothing.json"] })).await;
            assert_eq!(st, StatusCode::NO_CONTENT);
            let (_, _, l) = call(&app, get("/api/templates")).await;
            assert_eq!(l["lists"][0]["templates"][1]["title"], "WordPress");
            assert!(l["lists"][1]["error"].as_str().unwrap().contains("did not answer"), "a list that does not answer says so: {l}");

            let (st, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/templates/deploy", json!({ "list": url, "index": 2, "name": "swarmy" })).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "Swarm is not here yet: {b}");
            let (st, _) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/templates/deploy", json!({ "list": "http://elsewhere/list.json", "index": 0 })).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "only the lists in the settings");

            let (st, b) = send(&app, &cookie, &csrf, "POST", "/api/templates/custom", json!({ "title": "Greeter", "compose": "services:\n  app: {image: alpine}\n", "env": [{ "name": "GREETING", "default": "hello" }] })).await;
            assert_eq!(st, StatusCode::CREATED, "{b}");
            let tid = b["id"].as_i64().unwrap();
            let (st, b) = send(&app, &cookie, &csrf, "POST", "/api/templates/custom", json!({ "title": "Bad", "compose": "x", "env": [{ "name": "1X" }] })).await;
            assert_eq!((st, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("env")));
            let (st, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/templates/deploy", json!({ "custom": tid, "env": { "GREETING": "hi there" } })).await;
            assert_eq!(st, StatusCode::CREATED, "{b}");
            let sid = b["stack"].as_i64().unwrap();
            let s = crate::stacks::load(&app, 1, sid).await.unwrap();
            assert_eq!(s.name, "greeter", "named after its title");
            assert_eq!(std::fs::read_to_string(dir.path().join(format!("stacks/{sid}/.env"))).unwrap(), "GREETING=hi there\n");
            let (st, _) = send(&app, &cookie, &csrf, "DELETE", &format!("/api/templates/custom/{tid}"), json!({})).await;
            assert_eq!(st, StatusCode::NO_CONTENT);
        }
    }
}
