//! Import from Portainer: an administrator connects Freightdock to a Portainer (its address and an access token, sealed),
//! Freightdock reads what is there through Portainer's API without changing anything (GET only), matches Portainer's
//! environments to its own, shows what an import would make, and makes what is ticked: stacks adopted in place under the
//! same Compose project (their containers kept), Git stacks as Git stacks, groups and tags on the matched environments,
//! registries without their passwords, custom templates, and Portainer's teams as Freightdock teams with the members Freightdock
//! has a user for. Each import is written down per thing, so importing twice makes nothing twice ("Portainer" in
//! DECISIONS.md).

pub mod client;
pub mod plan;

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use bollard::query_parameters as q;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry, Who};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use client::{Client, Conn};
use plan::{EnvSide, Match, PortEnv, ShipEnv, ShipTeam, StackPlan, TeamPlan};

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/portainer", get(get_conn).put(put_conn).delete(forget))
        .route("/api/portainer/scan", post(scan))
        .route("/api/portainer/import", post(import))
}

/// The connection as it is kept in the settings (`portainer`): the token sealed ("Secrets" 1).
#[derive(Serialize, Deserialize, Default, Clone)]
struct Saved {
    url: String,
    token: String,
    #[serde(default)]
    ca: String,
    #[serde(default)]
    skip_verify: bool,
    #[serde(default)]
    version: String,
    #[serde(default)]
    instance: String,
    #[serde(default)]
    at: i64,
}

impl Saved {
    fn trust(&self) -> &'static str {
        if self.skip_verify {
            "certificate not checked, as chosen"
        } else if !self.ca.trim().is_empty() {
            "certificate checked against the one pasted"
        } else {
            "certificate checked"
        }
    }
}

async fn saved(app: &App) -> ApiResult<Option<Saved>> {
    let v: Option<String> = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'portainer'", [], |r| r.get(0)).optional()?)).await?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
}

fn shown(s: Option<&Saved>) -> Value {
    match s {
        None => json!({ "connected": false }),
        Some(s) => json!({ "connected": true, "url": s.url, "has_token": !s.token.is_empty(), "ca": s.ca, "skip_verify": s.skip_verify, "version": s.version, "instance": s.instance, "at": s.at, "trust": s.trust() }),
    }
}

/// The client for the saved connection, or a 409 when there is none.
async fn connected(app: &App) -> ApiResult<(Client, Saved)> {
    let s = saved(app).await?.ok_or_else(|| ApiError::conflict("Connect to Portainer first."))?;
    let token = app.sealer.open(&s.token)?;
    let c = Client::new(&Conn { url: s.url.clone(), token, ca: s.ca.clone(), skip_verify: s.skip_verify }).map_err(|m| ApiError::new(StatusCode::BAD_GATEWAY, m))?;
    Ok((c, s))
}

/// `GET /api/portainer`: the connection, never its token.
async fn get_conn(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(shown(saved(&app).await?.as_ref())))
}

#[derive(Deserialize)]
struct ConnForm {
    url: String,
    /// Empty or left out keeps the one saved for the same address.
    token: Option<String>,
    #[serde(default)]
    ca: String,
    #[serde(default)]
    skip_verify: bool,
}

/// `PUT /api/portainer`: `{url, token, ca, skip_verify}`, tried first (Portainer's status, then its environments with
/// the token), then saved.
async fn put_conn(State(app): State<App>, admin: Admin, Json(f): Json<ConnForm>) -> ApiResult<Json<Value>> {
    let url = client::check_url(&f.url).map_err(|m| ApiError::bad(m).field("url"))?;
    let was = saved(&app).await?;
    let token = match f.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => match was.as_ref().filter(|w| w.url == url) {
            Some(w) => app.sealer.open(&w.token)?,
            None => return Err(ApiError::bad("Give an access token: in Portainer, My account, Access tokens, Add access token.").field("token")),
        },
    };
    let ca = f.ca.trim().to_string();
    if !ca.is_empty() {
        client::pem_certs(&ca).map_err(|m| ApiError::bad(m).field("ca"))?;
    }
    let c = Client::new(&Conn { url: url.clone(), token: token.clone(), ca: ca.clone(), skip_verify: f.skip_verify }).map_err(|m| ApiError::bad(m).field("url"))?;
    let status = c.status().await.map_err(|m| ApiError::bad(m).field("url"))?;
    c.endpoints().await.map_err(|m| ApiError::bad(m.clone()).field(if m.contains("token") { "token" } else { "url" }))?;
    let s = Saved { url: url.clone(), token: app.sealer.seal(&token)?, ca, skip_verify: f.skip_verify, version: status.version.clone(), instance: status.instance_id, at: now() };
    let v = serde_json::to_string(&s).map_err(anyhow::Error::from)?;
    app.db.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('portainer', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [v])?)).await?;
    let detail = format!("Portainer {}; {}", s.version, s.trust());
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "portainer.connect", target: Some(url), detail: Some(detail) }).await;
    Ok(Json(shown(Some(&s))))
}

/// `DELETE /api/portainer`: forgets the connection and its token (what was imported stays, and so does the record of it).
async fn forget(State(app): State<App>, admin: Admin) -> ApiResult<StatusCode> {
    let s = saved(&app).await?;
    app.db.call(|c| Ok(c.execute("DELETE FROM settings WHERE key = 'portainer'", [])?)).await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "portainer.forget", target: s.map(|s| s.url), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// One of Freightdock's environments, as the import sees it.
#[derive(Clone, Debug, Serialize)]
struct EnvRow {
    id: i64,
    name: String,
    kind: String,
    engine: Option<String>,
    hostname: Option<String>,
    /// Why its Docker could not be asked, when it could not.
    error: Option<String>,
    #[serde(skip)]
    addresses: Vec<String>,
}

/// Everything read, from both sides, and the plan made of it.
struct Seen {
    instance: String,
    url: String,
    version: String,
    endpoints: Vec<client::Endpoint>,
    infos: HashMap<i64, Result<client::Info, String>>,
    stacks: Vec<client::Stack>,
    templates: Result<Vec<client::CustomTemplate>, String>,
    registries: Result<Vec<client::Registry>, String>,
    groups: Result<Vec<client::Group>, String>,
    tags: Result<Vec<client::Tag>, String>,
    users: Result<Vec<client::User>, String>,
    teams: Result<Vec<client::Team>, String>,
    memberships: Result<Vec<client::Membership>, String>,
    envs: Vec<EnvRow>,
    auto: HashMap<i64, Match>,
    mapping: HashMap<i64, i64>,
    /// What earlier imports made: (kind, Portainer's id) to Freightdock's id.
    imported: HashMap<(String, i64), i64>,
    plan: Vec<StackPlan>,
    /// Registries Freightdock has, by host and by name (lower case).
    registry_hosts: HashMap<String, i64>,
    registry_names: HashMap<String, i64>,
    template_ids: BTreeSet<i64>,
    /// What each of Portainer's teams would be in Freightdock, and with whom ("Portainer" 10).
    team_plan: Vec<TeamPlan>,
}

/// How long one of Freightdock's environments has to say what it is.
const ASK_FOR: Duration = Duration::from_secs(8);

/// Freightdock's environments, each with its engine's ID and host name and its Compose projects, asked at once.
async fn ship_side(app: &App) -> ApiResult<(Vec<EnvRow>, HashMap<i64, EnvSide>)> {
    type Row = (i64, String, String, String, Option<String>);
    let rows: Vec<Row> = app
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, name, kind, url, agent_peer FROM environments ORDER BY id")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
        .await?;
    let stacks: Vec<(i64, String, i64)> = app
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT env_id, name, id FROM stacks")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
        .await?;
    let asked = futures_util::future::join_all(rows.iter().map(|(id, ..)| {
        let (app, id) = (app.clone(), *id);
        async move {
            let ask = async {
                let d = crate::docker::endpoint(&app, id).await.map_err(|e| e.message)?;
                let info = d.client().info().await.map_err(|e| crate::docker::api_error(e).message)?;
                let cs = d.client().list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })).await.map_err(|e| crate::docker::api_error(e).message)?;
                Ok::<_, String>((info, cs))
            };
            tokio::time::timeout(ASK_FOR, ask).await.unwrap_or_else(|_| Err("Its Docker took too long to answer.".into()))
        }
    }))
    .await;
    let mut envs = Vec::new();
    let mut sides: HashMap<i64, EnvSide> = HashMap::new();
    for ((id, name, kind, url, peer), a) in rows.into_iter().zip(asked) {
        let side = sides.entry(id).or_default();
        let mut row = EnvRow { id, name, kind, engine: None, hostname: None, error: None, addresses: [Some(url), peer].into_iter().flatten().filter(|a| !a.is_empty()).collect() };
        match a {
            Ok((info, cs)) => {
                row.engine = info.id.filter(|i| !i.is_empty());
                row.hostname = info.name.filter(|n| !n.is_empty());
                for (n, p) in crate::stacks::projects::group(&cs) {
                    side.projects.insert(n, (p.containers, p.running, p.working_dir));
                }
            }
            Err(e) => {
                side.error = Some(e.clone());
                row.error = Some(e);
            }
        }
        envs.push(row);
    }
    for (env, name, id) in stacks {
        sides.entry(env).or_default().stacks.insert(name, id);
    }
    Ok((envs, sides))
}

/// Reads Portainer (GET only) and Freightdock, matches the environments (`overrides` first: Portainer's id, as text, to one
/// of Freightdock's, or null for none), and plans the stacks.
async fn read(app: &App, c: &Client, saved: &Saved, overrides: &HashMap<String, Option<i64>>) -> ApiResult<Seen> {
    let bad = |m: String| ApiError::new(StatusCode::BAD_GATEWAY, m);
    let (status, endpoints, stacks) = tokio::join!(c.status(), c.endpoints(), c.stacks());
    let (status, endpoints, stacks) = (status.map_err(bad)?, endpoints.map_err(bad)?, stacks.map_err(bad)?);
    let docker = endpoints.iter().filter(|e| matches!(e.kind, 1 | 2 | 4));
    let infos = futures_util::future::join_all(docker.map(|e| async move {
        let r = if e.status == 1 { c.docker_info(e.id).await } else { Err("It is down in Portainer.".to_string()) };
        (e.id, r)
    }))
    .await
    .into_iter()
    .collect::<HashMap<_, _>>();
    let (templates, registries, groups, tags, users, teams, memberships) = tokio::join!(c.custom_templates(), c.registries(), c.groups(), c.tags(), c.users(), c.teams(), c.memberships());
    let (envs, sides) = ship_side(app).await?;

    let ports: Vec<PortEnv> = endpoints
        .iter()
        .filter(|e| matches!(e.kind, 1 | 2 | 4))
        .map(|e| {
            let i = infos.get(&e.id).and_then(|r| r.as_ref().ok());
            PortEnv { id: e.id, name: e.name.clone(), url: e.url.clone(), engine: i.map(|i| i.id.clone()).filter(|s| !s.is_empty()), hostname: i.map(|i| i.name.clone()).filter(|s| !s.is_empty()) }
        })
        .collect();
    let ships: Vec<ShipEnv> = envs.iter().map(|e| ShipEnv { id: e.id, name: e.name.clone(), engine: e.engine.clone(), hostname: e.hostname.clone(), addresses: e.addresses.clone() }).collect();
    let auto: HashMap<i64, Match> = ports.iter().zip(plan::map_all(&ports, &ships)).filter_map(|(p, m)| Some((p.id, m?))).collect();
    let mut mapping: HashMap<i64, i64> = auto.iter().map(|(p, m)| (*p, m.env)).collect();
    for (k, v) in overrides {
        let Ok(pid) = k.parse::<i64>() else { continue };
        match v {
            Some(env) if envs.iter().any(|e| e.id == *env) && ports.iter().any(|p| p.id == pid) => mapping.insert(pid, *env),
            _ => mapping.remove(&pid),
        };
    }

    let instance = status.instance_id.clone();
    let inst = instance.clone();
    let imported: HashMap<(String, i64), i64> = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT kind, portainer_id, freightdock_id FROM portainer_imports WHERE instance = ?1")?;
            let r = st.query_map([inst], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?.collect::<Result<HashMap<_, _>, _>>()?;
            Ok(r)
        })
        .await?;
    let (registry_hosts, registry_names, template_ids) = app
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, host, name FROM registries")?;
            let mut hosts = HashMap::new();
            let mut names = HashMap::new();
            for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
                let (id, h, n) = r?;
                hosts.insert(h, id);
                names.insert(n.to_lowercase(), id);
            }
            let mut st = c.prepare("SELECT id FROM templates")?;
            let t = st.query_map([], |r| r.get(0))?.collect::<Result<BTreeSet<i64>, _>>()?;
            Ok((hosts, names, t))
        })
        .await?;
    let known: Vec<i64> = endpoints.iter().map(|e| e.id).collect();
    let stack_imports: HashMap<i64, i64> = imported.iter().filter(|((k, _), _)| k == "stack").map(|((_, p), s)| (*p, *s)).collect();
    let plan = plan::stacks(&stacks, &known, &mapping, &sides, &stack_imports);
    let (ship_teams, ship_users) = app
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT t.id, t.name, m.user_id FROM teams t LEFT JOIN team_members m ON m.team_id = t.id ORDER BY t.id")?;
            let mut teams: Vec<ShipTeam> = Vec::new();
            for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))? {
                let (id, name, member) = r?;
                if teams.last().is_none_or(|t| t.id != id) {
                    teams.push(ShipTeam { id, name, members: BTreeSet::new() });
                }
                if let (Some(m), Some(t)) = (member, teams.last_mut()) {
                    t.members.insert(m);
                }
            }
            let mut st = c.prepare("SELECT id, name FROM users")?;
            let users = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok((teams, users))
        })
        .await?;
    let team_imports: HashMap<i64, i64> = imported.iter().filter(|((k, _), _)| k == "team").map(|((_, p), s)| (*p, *s)).collect();
    let team_plan = match (&teams, &users) {
        (Ok(t), Ok(u)) => plan::teams(t, u, memberships.as_deref().unwrap_or_default(), &ship_teams, &ship_users, &team_imports),
        _ => Vec::new(),
    };
    Ok(Seen {
        instance,
        url: saved.url.clone(),
        version: status.version,
        endpoints,
        infos,
        stacks,
        templates,
        registries,
        groups,
        tags,
        users,
        teams,
        memberships,
        envs,
        auto,
        mapping,
        imported,
        plan,
        registry_hosts,
        registry_names,
        template_ids,
        team_plan,
    })
}

fn endpoint_kind(t: i64) -> &'static str {
    match t {
        1 => "Docker",
        2 => "Docker agent",
        4 => "Edge agent",
        3 => "Azure",
        5..=7 => "Kubernetes",
        _ => "unknown",
    }
}

impl Seen {
    fn env_name(&self, id: i64) -> String {
        self.envs.iter().find(|e| e.id == id).map(|e| e.name.clone()).unwrap_or_else(|| format!("environment {id}"))
    }

    /// The Portainer environments that have tag `t`: as the tag lists them, and as they list it.
    fn tagged(&self, t: &client::Tag) -> Vec<i64> {
        let mut out: BTreeSet<i64> = t.endpoints.iter().filter(|(_, on)| **on).filter_map(|(k, _)| k.parse().ok()).collect();
        out.extend(self.endpoints.iter().filter(|e| e.tag_ids.contains(&t.id)).map(|e| e.id));
        out.into_iter().collect()
    }

    /// Whether what an earlier import made of `kind` `id` is still there.
    fn imported(&self, kind: &str, id: i64) -> Option<i64> {
        let s = *self.imported.get(&(kind.to_string(), id))?;
        let there = match kind {
            "registry" => self.registry_hosts.values().any(|r| *r == s),
            "template" => self.template_ids.contains(&s),
            _ => true,
        };
        there.then_some(s)
    }

    /// The registry's line: what it would be in Freightdock, and why not when it cannot be.
    fn registry_line(&self, r: &client::Registry) -> (Value, Option<String>) {
        let kind = plan::registry_kind(r.kind);
        let host = crate::registries::check_url(kind, &r.url).map(|(_, h)| h);
        let reason = if let Some(id) = self.imported("registry", r.id) {
            Some(format!("Imported already (registry {id})."))
        } else {
            match &host {
                Err(e) => Some(format!("Freightdock cannot take its address: {}", e.message)),
                Ok(h) if self.registry_hosts.contains_key(h) => Some(format!("Freightdock has a registry for {h} already.")),
                Ok(_) if self.registry_names.contains_key(&r.name.to_lowercase()) => Some(format!("Freightdock has a registry named {} already.", r.name)),
                Ok(_) => None,
            }
        };
        let v = json!({ "id": r.id, "name": r.name, "kind": kind, "url": r.url, "host": host.ok(), "username": r.username, "authentication": r.authentication,
                        "importable": reason.is_none(), "reason": reason, "needs_password": r.authentication && !r.username.is_empty() });
        (v, reason)
    }

    fn template_reason(&self, t: &client::CustomTemplate) -> Option<String> {
        if let Some(id) = self.imported("template", t.id) {
            return Some(format!("Imported already (template {id})."));
        }
        match t.kind {
            1 => Some("Not imported yet: Portainer's Swarm templates are not brought over yet.".into()),
            3 => Some("Kubernetes is not something Freightdock manages.".into()),
            _ => None,
        }
    }

    fn json(&self) -> Value {
        let group_name = |id: i64| self.groups.as_ref().ok().and_then(|g| g.iter().find(|g| g.id == id)).map(|g| g.name.clone());
        let tag_names = |ids: &[i64]| -> Vec<String> { self.tags.as_ref().map(|t| t.iter().filter(|t| ids.contains(&t.id)).map(|t| t.name.clone()).collect()).unwrap_or_default() };
        let environments: Vec<Value> = self
            .endpoints
            .iter()
            .map(|e| {
                let info = self.infos.get(&e.id);
                let tags = self.tags.as_ref().map(|ts| ts.iter().filter(|t| self.tagged(t).contains(&e.id)).map(|t| t.name.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| tag_names(&e.tag_ids));
                json!({
                    "id": e.id, "name": e.name, "type": e.kind, "kind": endpoint_kind(e.kind), "url": e.url, "status": if e.status == 1 { "up" } else { "down" },
                    "group": if e.group_id > 1 { group_name(e.group_id) } else { None }, "tags": tags,
                    "engine": info.and_then(|i| i.as_ref().ok()).map(|i| &i.id), "hostname": info.and_then(|i| i.as_ref().ok()).map(|i| &i.name),
                    "info_error": info.and_then(|i| i.as_ref().err()), "docker": matches!(e.kind, 1 | 2 | 4),
                    "match": self.auto.get(&e.id), "env": self.mapping.get(&e.id),
                })
            })
            .collect();
        let groups: Vec<Value> = self
            .groups
            .as_ref()
            .map(|gs| {
                gs.iter()
                    .filter(|g| g.id != 1)
                    .map(|g| {
                        let members: Vec<i64> = self.endpoints.iter().filter(|e| e.group_id == g.id).map(|e| e.id).collect();
                        json!({ "id": g.id, "name": g.name, "description": g.description, "environments": members, "imported": self.imported("group", g.id) })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tags: Vec<Value> = self.tags.as_ref().map(|ts| ts.iter().map(|t| json!({ "id": t.id, "name": t.name, "environments": self.tagged(t), "imported": self.imported("tag", t.id) })).collect()).unwrap_or_default();
        let registries: Vec<Value> = self.registries.as_ref().map(|rs| rs.iter().map(|r| self.registry_line(r).0).collect()).unwrap_or_default();
        let templates: Vec<Value> = self
            .templates
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        let reason = self.template_reason(t);
                        json!({ "id": t.id, "title": t.title, "description": t.description, "type": t.kind, "variables": t.variables.len(), "importable": reason.is_none(), "reason": reason })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut errors = serde_json::Map::new();
        for (k, e) in [
            ("templates", self.templates.as_ref().err()),
            ("registries", self.registries.as_ref().err()),
            ("groups", self.groups.as_ref().err()),
            ("tags", self.tags.as_ref().err()),
            ("users", self.users.as_ref().err()),
            ("teams", self.teams.as_ref().err()),
            ("team_memberships", self.memberships.as_ref().err()),
        ] {
            if let Some(e) = e {
                errors.insert(k.into(), json!(e));
            }
        }
        json!({
            "portainer": { "url": self.url, "version": self.version, "instance": self.instance },
            "environments": environments,
            "freightdock": self.envs,
            "stacks": self.plan,
            "groups": groups,
            "tags": tags,
            "registries": registries,
            "templates": templates,
            "people": { "users": self.users.as_ref().map(Vec::len).unwrap_or(0), "teams": self.teams.as_ref().map(Vec::len).unwrap_or(0) },
            "teams": self.team_plan,
            "errors": errors,
        })
    }
}

#[derive(Deserialize, Default)]
struct ScanBody {
    /// Portainer's environment id (as text) to one of Freightdock's, or null for none; the rest are matched by themselves.
    #[serde(default)]
    mapping: HashMap<String, Option<i64>>,
}

/// `POST /api/portainer/scan`: what Portainer has and what an import would make of it. Changes nothing on either side.
async fn scan(State(app): State<App>, _: Admin, body: Option<Json<ScanBody>>) -> ApiResult<Json<Value>> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let (c, s) = connected(&app).await?;
    Ok(Json(read(&app, &c, &s, &b.mapping).await?.json()))
}

#[derive(Deserialize)]
struct Pick {
    id: i64,
    /// A Git stack's token, which Portainer does not give out.
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize, Default)]
struct ImportBody {
    #[serde(default)]
    mapping: HashMap<String, Option<i64>>,
    #[serde(default)]
    stacks: Vec<Pick>,
    #[serde(default)]
    groups: Vec<i64>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    registries: Vec<i64>,
    #[serde(default)]
    templates: Vec<i64>,
    /// Keep a copy of Portainer's users, teams and memberships as Portainer has them (`portainer_people`).
    #[serde(default)]
    people: bool,
    /// Portainer's teams to make (or match) in Freightdock, with the members Freightdock has a user for.
    #[serde(default)]
    teams: Vec<i64>,
    /// Deploy each imported stack from Freightdock once it is made (off: the containers stay as they are).
    #[serde(default)]
    deploy: bool,
}

/// How one thing went.
#[derive(Serialize, Debug)]
struct Outcome {
    kind: &'static str,
    id: i64,
    name: String,
    /// `done`, `skipped` or `failed`.
    outcome: &'static str,
    reason: Option<String>,
    freightdock_id: Option<i64>,
}

impl Outcome {
    fn new(kind: &'static str, id: i64, name: &str) -> Self {
        Outcome { kind, id, name: name.into(), outcome: "done", reason: None, freightdock_id: None }
    }
    fn done(mut self, sid: i64, reason: Option<String>) -> Self {
        (self.freightdock_id, self.reason) = (Some(sid), reason);
        self
    }
    fn skipped(mut self, why: impl Into<String>) -> Self {
        (self.outcome, self.reason) = ("skipped", Some(why.into()));
        self
    }
    fn failed(mut self, why: impl Into<String>) -> Self {
        (self.outcome, self.reason) = ("failed", Some(why.into()));
        self
    }
}

async fn note(app: &App, instance: &str, kind: &'static str, pid: i64, sid: i64) -> ApiResult<()> {
    let inst = instance.to_string();
    app.db
        .call(move |c| {
            c.execute(
                "INSERT INTO portainer_imports (instance, kind, portainer_id, freightdock_id, at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (instance, kind, portainer_id) DO UPDATE SET freightdock_id = excluded.freightdock_id, at = excluded.at",
                params![inst, kind, pid, sid, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// `POST /api/portainer/import`: makes what is ticked, reading Portainer again rather than taking the page's word for
/// what is there, and answers how each thing went.
async fn import(State(app): State<App>, admin: Admin, Json(b): Json<ImportBody>) -> ApiResult<Json<Value>> {
    // One import at a time: two at once would race to make the same things.
    let _one = app.importing.try_lock().map_err(|_| ApiError::conflict("An import is running already."))?;
    let (c, s) = connected(&app).await?;
    let seen = read(&app, &c, &s, &b.mapping).await?;
    let who = Who::from(&admin);
    let mut out = Vec::new();
    for id in &b.groups {
        out.push(import_group(&app, &who, &seen, *id).await);
    }
    for id in &b.tags {
        out.push(import_tag(&app, &who, &seen, *id).await);
    }
    for id in &b.registries {
        out.push(import_registry(&app, &who, &seen, *id).await);
    }
    for id in &b.templates {
        out.push(import_template(&app, &who, &c, &seen, *id).await);
    }
    for p in &b.stacks {
        out.push(import_stack(&app, &who, &c, &seen, p, b.deploy).await);
    }
    if b.people {
        out.push(import_people(&app, &who, &seen).await);
    }
    for id in &b.teams {
        out.push(import_team(&app, &who, &seen, *id).await);
    }
    let count = |w: &str| out.iter().filter(|o| o.outcome == w).count();
    let detail = format!("{} done, {} skipped, {} failed", count("done"), count("skipped"), count("failed"));
    audit::record_by(&app.db, &who, Entry { action: "portainer.import", target: Some(seen.url.clone()), detail: Some(detail) }).await;
    Ok(Json(json!({ "results": out })))
}

fn names(seen: &Seen, envs: &[i64]) -> String {
    envs.iter().map(|e| seen.env_name(*e)).collect::<Vec<_>>().join(", ")
}

/// A group by its name (made when Freightdock has none of that name), and each matched environment of Portainer's group
/// in it, unless it is in another group already (that is left as it is).
async fn import_group(app: &App, who: &Who, seen: &Seen, id: i64) -> Outcome {
    let Some(g) = seen.groups.as_ref().ok().and_then(|gs| gs.iter().find(|g| g.id == id && g.id != 1)) else {
        return Outcome::new("group", id, "").failed("Portainer has no such group.");
    };
    let o = Outcome::new("group", id, &g.name);
    let envs: Vec<i64> = seen.endpoints.iter().filter(|e| e.group_id == id).filter_map(|e| seen.mapping.get(&e.id).copied()).collect();
    let (name, description) = (g.name.clone(), g.description.clone());
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let existing: Option<i64> = tx.query_row("SELECT id FROM environment_groups WHERE name = ?1", [&name], |r| r.get(0)).optional()?;
            let gid = match existing {
                Some(g) => g,
                None => {
                    tx.execute("INSERT INTO environment_groups (name, description, created_at) VALUES (?1, ?2, ?3)", params![name, description, now()])?;
                    tx.last_insert_rowid()
                }
            };
            let (mut placed, mut kept) = (Vec::new(), Vec::new());
            for e in envs {
                let cur: Option<i64> = tx.query_row("SELECT group_id FROM environments WHERE id = ?1", [e], |r| r.get(0))?;
                match cur {
                    None => {
                        tx.execute("UPDATE environments SET group_id = ?1 WHERE id = ?2", params![gid, e])?;
                        placed.push(e);
                    }
                    Some(c) if c == gid => {}
                    Some(_) => kept.push(e),
                }
            }
            tx.commit()?;
            Ok((gid, existing.is_none(), placed, kept))
        })
        .await;
    let (gid, made, placed, kept) = match r {
        Ok(r) => r,
        Err(e) => return o.failed(format!("{e:#}")),
    };
    if made {
        audit::record_by(&app.db, who, Entry { action: "environment-group.create", target: Some(g.name.clone()), detail: Some("from Portainer".into()) }).await;
    }
    if !placed.is_empty() {
        audit::record_by(&app.db, who, Entry { action: "environment-group.update", target: Some(g.name.clone()), detail: Some(format!("{} put in it; from Portainer", names(seen, &placed))) }).await;
    }
    if let Err(e) = note(app, &seen.instance, "group", id, gid).await {
        return o.failed(e.message);
    }
    let mut said = Vec::new();
    said.push(if made { "Made.".to_string() } else { "Freightdock had a group by that name: used.".to_string() });
    if !placed.is_empty() {
        said.push(format!("{} put in it.", names(seen, &placed)));
    }
    if !kept.is_empty() {
        said.push(format!("{} left in the group it is in.", names(seen, &kept)));
    }
    if !made && placed.is_empty() {
        return o.skipped(said.join(" "));
    }
    o.done(gid, Some(said.join(" ")))
}

/// A tag by its name (made when Freightdock has none of that name), put on each matched environment Portainer has it on.
async fn import_tag(app: &App, who: &Who, seen: &Seen, id: i64) -> Outcome {
    let Some(t) = seen.tags.as_ref().ok().and_then(|ts| ts.iter().find(|t| t.id == id)) else {
        return Outcome::new("tag", id, "").failed("Portainer has no such tag.");
    };
    let o = Outcome::new("tag", id, &t.name);
    if crate::groups::tag_list(std::slice::from_ref(&t.name)).is_err() {
        return o.failed("Freightdock's tags are 1 to 40 characters, without a comma.");
    }
    let envs: Vec<i64> = seen.tagged(t).into_iter().filter_map(|e| seen.mapping.get(&e).copied()).collect();
    let name = t.name.clone();
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let made = tx.execute("INSERT OR IGNORE INTO tags (name, created_at) VALUES (?1, ?2)", params![name, now()])? > 0;
            let tid: i64 = tx.query_row("SELECT id FROM tags WHERE name = ?1", [&name], |r| r.get(0))?;
            let mut put = Vec::new();
            for e in envs {
                if tx.execute("INSERT OR IGNORE INTO environment_tags (environment_id, tag_id) VALUES (?1, ?2)", params![e, tid])? > 0 {
                    put.push(e);
                }
            }
            tx.commit()?;
            Ok((tid, made, put))
        })
        .await;
    let (tid, made, put) = match r {
        Ok(r) => r,
        Err(e) => return o.failed(format!("{e:#}")),
    };
    if made {
        audit::record_by(&app.db, who, Entry { action: "tag.create", target: Some(t.name.clone()), detail: Some("from Portainer".into()) }).await;
    }
    for e in &put {
        audit::record_by(&app.db, who, Entry { action: "environment.update", target: Some(seen.env_name(*e)), detail: Some(format!("tag {}; from Portainer", t.name)) }).await;
    }
    if let Err(e) = note(app, &seen.instance, "tag", id, tid).await {
        return o.failed(e.message);
    }
    if !made && put.is_empty() {
        return o.skipped("Freightdock has it, on those environments, already.");
    }
    o.done(tid, (!put.is_empty()).then(|| format!("On {}.", names(seen, &put))))
}

async fn import_registry(app: &App, who: &Who, seen: &Seen, id: i64) -> Outcome {
    let Some(r) = seen.registries.as_ref().ok().and_then(|rs| rs.iter().find(|r| r.id == id)) else {
        return Outcome::new("registry", id, "").failed("Portainer has no such registry.");
    };
    let o = Outcome::new("registry", id, &r.name);
    let (_, reason) = seen.registry_line(r);
    if let Some(why) = reason {
        return o.skipped(why);
    }
    let kind = plan::registry_kind(r.kind);
    let user = if r.authentication { r.username.as_str() } else { "" };
    match crate::registries::import(app, who, &r.name, kind, &r.url, user, r.ecr.as_ref().map(|e| e.region.clone()), "from Portainer, without its password".into()).await {
        Ok(sid) => {
            if let Err(e) = note(app, &seen.instance, "registry", id, sid).await {
                return o.failed(e.message);
            }
            let why = (!user.is_empty()).then(|| "Saved without its password, which Portainer does not give out: give it on the registries page.".to_string());
            o.done(sid, why)
        }
        Err(e) if e.status == StatusCode::CONFLICT => o.skipped(e.message),
        Err(e) => o.failed(e.message),
    }
}

async fn import_template(app: &App, who: &Who, c: &Client, seen: &Seen, id: i64) -> Outcome {
    let Some(t) = seen.templates.as_ref().ok().and_then(|ts| ts.iter().find(|t| t.id == id)) else {
        return Outcome::new("template", id, "").failed("Portainer has no such template.");
    };
    let o = Outcome::new("template", id, &t.title);
    if let Some(why) = seen.template_reason(t) {
        return o.skipped(why);
    }
    let file = match c.custom_template_file(id).await {
        Ok(f) => f,
        Err(m) => return o.failed(m),
    };
    let custom = crate::templates::Custom {
        title: t.title.clone(),
        description: t.description.clone(),
        note: t.note.clone(),
        logo: t.logo.clone(),
        compose: file,
        env: t.variables.iter().map(|v| crate::templates::Var { name: v.name.clone(), label: v.label.clone(), description: v.description.clone(), default: v.default_value.clone(), ..Default::default() }).collect(),
        ..Default::default()
    };
    match crate::templates::insert_custom(app, who, custom, Some("from Portainer".into())).await {
        Ok(sid) => match note(app, &seen.instance, "template", id, sid).await {
            Ok(()) => o.done(sid, None),
            Err(e) => o.failed(e.message),
        },
        Err(e) => o.failed(e.message),
    }
}

/// A compose file as Compose merges it from where the project was started (`dir`): relative paths made absolute, so
/// the stack keeps mounting what it mounted under Portainer ("Stacks" 6); its `stack.env` and `.env` stay the stack's
/// own. Without Compose, or when Compose cannot, the file as Portainer has it, and why.
async fn merged(app: &App, name: &str, file: &str, dir: &str) -> (String, Option<String>) {
    let Some(bin) = app.stacks.compose.path() else { return (file.to_string(), None) };
    let scratch = app.stacks.scratch();
    let tmp = scratch.join(format!("portainer-{}.yaml", crate::auth::random_token()));
    if let Err(e) = std::fs::create_dir_all(&scratch).and_then(|_| std::fs::write(&tmp, file)) {
        return (file.to_string(), Some(format!("Kept as Portainer has it: the file could not be written to merge it ({e}).")));
    }
    let a = crate::stacks::compose::merge_args(name, &[tmp.to_string_lossy().to_string()], dir, None);
    let r = crate::stacks::compose::run(bin, &a, &[], &scratch, None).await;
    let _ = std::fs::remove_file(&tmp);
    match r {
        Ok(lines) => {
            let d = dir.trim_end_matches('/');
            let text = (lines.join("\n") + "\n").replace(&format!("{d}/stack.env"), "stack.env").replace(&format!("{d}/.env"), ".env");
            (text, Some(format!("Relative paths made absolute from {d}, where Portainer ran it.")))
        }
        Err(m) => (file.to_string(), Some(format!("Kept as Portainer has it: Compose could not merge it ({m})."))),
    }
}

async fn import_stack(app: &App, who: &Who, c: &Client, seen: &Seen, pick: &Pick, deploy: bool) -> Outcome {
    let (Some(s), Some(p)) = (seen.stacks.iter().find(|s| s.id == pick.id), seen.plan.iter().find(|p| p.id == pick.id)) else {
        return Outcome::new("stack", pick.id, "").failed("Portainer has no such stack.");
    };
    let o = Outcome::new("stack", s.id, &s.name);
    let (true, Some(env)) = (p.importable, p.env) else {
        return o.skipped(p.reason.clone().unwrap_or_else(|| "It cannot be imported.".into()));
    };
    let (env_text, left) = plan::env_text(&s.env);
    let mut said = Vec::new();
    if !left.is_empty() {
        said.push(format!("Variables Compose would not take were left out: {}.", left.join(", ")));
    }
    let made = match s.git_config.as_ref().filter(|_| p.kind == "git") {
        Some(g) => {
            let token = pick.token.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
            let user = token.as_ref().and(g.authentication.as_ref()).map(|a| a.username.clone()).filter(|u| !u.is_empty());
            let poll = s.auto_update.as_ref().map(|a| plan::poll_minutes(&a.interval)).unwrap_or(0);
            let path = if g.config_file_path.is_empty() { s.entry_point.clone() } else { g.config_file_path.clone() };
            let form = crate::stacks::GitForm::from_portainer(&s.name, &g.url, &plan::reference(&g.reference_name), &path, user, token, poll, &env_text);
            crate::stacks::make_git(app, who, env, &form).await.map_err(|e| if p.needs_token && pick.token.is_none() { format!("{} Portainer signs in to it: give its token.", e.message) } else { e.message })
        }
        None => match c.stack_file(s.id).await {
            Err(m) => Err(m),
            Ok(file) => {
                let dir = p.working_dir.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| s.project_path.clone());
                let (text, how) = merged(app, &s.name, &file, &dir).await;
                said.extend(how);
                let extra = crate::stacks::Extra { stack_env: true, detail: Some(format!("from Portainer stack {}", s.id)) };
                crate::stacks::make_with(app, who, env, &s.name, &text, &env_text, "stack.adopt", extra).await.map_err(|e| e.message)
            }
        },
    };
    let sid = match made {
        Ok(id) => id,
        Err(m) => return o.failed(m),
    };
    if let Err(e) = note(app, &seen.instance, "stack", s.id, sid).await {
        return o.failed(e.message);
    }
    said.push(format!("A stack on {}{}.", seen.env_name(env), if p.containers > 0 { ", its containers kept as they are" } else { "" }));
    if deploy {
        let r = match crate::stacks::load(app, env, sid).await {
            Ok(st) => crate::stacks::run(app, &st, crate::stacks::Act::Deploy { pull: false }, who, &crate::access::security::Judge::Exempt, Some("after the import from Portainer"), None).await,
            Err(e) => Err(e.message),
        };
        said.push(match r {
            Ok(()) => "Deployed from Freightdock.".into(),
            Err(m) => format!("Imported, but the deploy failed: {m}"),
        });
    }
    o.done(sid, Some(said.join(" ")))
}

/// Portainer's users, teams and who is in which, kept in the settings (`portainer_people`) as Portainer has them. Nobody
/// is made a Freightdock user; teams are made by `import_team`.
async fn import_people(app: &App, who: &Who, seen: &Seen) -> Outcome {
    let o = Outcome::new("people", 0, "Users and teams");
    let (Ok(users), Ok(teams)) = (&seen.users, &seen.teams) else {
        return o.failed(seen.users.as_ref().err().or(seen.teams.as_ref().err()).cloned().unwrap_or_default());
    };
    let memberships = seen.memberships.as_ref().map(|m| m.as_slice()).unwrap_or_default();
    let v = json!({
        "instance": seen.instance, "url": seen.url, "at": now(),
        "users": users.iter().map(|u| json!({ "id": u.id, "name": u.username, "role": if u.role == 1 { "admin" } else { "user" } })).collect::<Vec<_>>(),
        "teams": teams.iter().map(|t| json!({ "id": t.id, "name": t.name })).collect::<Vec<_>>(),
        "memberships": memberships.iter().map(|m| json!({ "user": m.user_id, "team": m.team_id, "leader": m.role == 1 })).collect::<Vec<_>>(),
    })
    .to_string();
    if let Err(e) = app.db.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('portainer_people', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [v])?)).await {
        return o.failed(format!("{e:#}"));
    }
    let said = format!("{} users and {} teams kept as Portainer has them; nobody was made a Freightdock user.", users.len(), teams.len());
    audit::record_by(&app.db, who, Entry { action: "portainer.people", target: Some(seen.url.clone()), detail: Some(format!("{} users, {} teams", users.len(), teams.len())) }).await;
    o.done(0, Some(said))
}

/// One of Portainer's teams as a Freightdock team ("Portainer" 10): the one an earlier import made, else Freightdock's of its
/// name, else a new one; then each member Freightdock has a user of that name for, unless in it already. Nobody is taken
/// out, and nobody is made a user.
async fn import_team(app: &App, who: &Who, seen: &Seen, id: i64) -> Outcome {
    let Some(t) = seen.team_plan.iter().find(|t| t.id == id) else {
        return Outcome::new("team", id, "").failed("Portainer has no such team.");
    };
    let o = Outcome::new("team", id, &t.name);
    if let Some(why) = &t.reason {
        return o.skipped(why.clone());
    }
    let (known, name) = (t.freightdock_id, t.name.trim().to_string());
    let wanted: Vec<(i64, String)> = t.members.iter().filter_map(|m| Some((m.freightdock_user_id?, m.name.clone()))).collect();
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            // The plan's team, if it is still there; else by name (case-blind, as the column is), else made.
            let there = match known {
                Some(k) => tx.query_row("SELECT id, name FROM teams WHERE id = ?1", [k], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).optional()?,
                None => None,
            };
            let there = match there {
                Some(x) => Some(x),
                None => tx.query_row("SELECT id, name FROM teams WHERE name = ?1", [&name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).optional()?,
            };
            let (tid, shown, made) = match there {
                Some((tid, n)) => (tid, n, false),
                None => {
                    tx.execute("INSERT INTO teams (name, created_at) VALUES (?1, ?2)", params![name, now()])?;
                    (tx.last_insert_rowid(), name, true)
                }
            };
            let mut added = Vec::new();
            for (uid, n) in wanted {
                if tx.execute("INSERT OR IGNORE INTO team_members (team_id, user_id) SELECT ?1, id FROM users WHERE id = ?2", params![tid, uid])? > 0 {
                    added.push(n);
                }
            }
            tx.commit()?;
            Ok((tid, shown, made, added))
        })
        .await;
    let (tid, shown, made, added) = match r {
        Ok(r) => r,
        Err(e) => return o.failed(format!("{e:#}")),
    };
    if made {
        audit::record_by(&app.db, who, Entry { action: "team.create", target: Some(shown.clone()), detail: Some("from Portainer".into()) }).await;
    }
    if !added.is_empty() {
        audit::record_by(&app.db, who, Entry { action: "team.update", target: Some(shown.clone()), detail: Some(format!("{} added; from Portainer", added.join(", "))) }).await;
    }
    if let Err(e) = note(app, &seen.instance, "team", id, tid).await {
        return o.failed(e.message);
    }
    let left: Vec<&str> = t.members.iter().filter(|m| m.freightdock_user_id.is_none()).map(|m| m.name.as_str()).collect();
    let mut said = vec![match (made, t.outcome) {
        (true, _) => "Made.".to_string(),
        (false, "imported") => "Imported already.".to_string(),
        (false, _) => "Freightdock had a team by that name: used.".to_string(),
    }];
    if !added.is_empty() {
        said.push(format!("{} added.", added.join(", ")));
    }
    if !left.is_empty() {
        said.push(format!("{} left out: Freightdock has no user of that name.", left.join(", ")));
    }
    if !made && added.is_empty() {
        return o.skipped(if left.is_empty() { "Freightdock has it, with those members, already.".to_string() } else { said.join(" ") });
    }
    o.done(tid, Some(said.join(" ")))
}

#[cfg(test)]
mod tests;
