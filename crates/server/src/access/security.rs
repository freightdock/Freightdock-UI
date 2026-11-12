//! Per-environment security settings ("Access" 12 to 17): switches that stop everybody below Environment administrator
//! on a host from making containers that reach past their own walls (privileged mode, host paths, the host's
//! namespaces, devices, extra capabilities, security options, sysctls), and from opening consoles. Set on an
//! environment or on its group; the environment's own, when it has one, wins whole. Everything is allowed until an
//! administrator says otherwise, as before there were settings.
//!
//! The rules are judged on one shape, [`Spec`], made from each way a container comes to be: Docker's create body (a
//! container, a template), the file Compose resolves (a stack), a Swarm service's spec and a Swarm stack's file.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Access, Level, Role};
use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::{Db, now, rusqlite::{self, OptionalExtension, params}};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// What non-administrators may not do on a host. `false` everywhere (the default) is everything allowed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub block_privileged: bool,
    /// Bind mounts of host paths, but those under `bind_allow`.
    pub block_bind_mounts: bool,
    /// Host path prefixes a bind mount may still use while bind mounts are blocked.
    pub bind_allow: Vec<String>,
    pub block_host_pid: bool,
    pub block_host_ipc: bool,
    pub block_host_network: bool,
    pub block_host_uts: bool,
    pub block_devices: bool,
    pub block_cap_add: bool,
    pub block_security_opt: bool,
    pub block_sysctls: bool,
    pub block_consoles: bool,
}

impl Rules {
    /// The recommended preset: nothing that reaches the host; consoles stay, an Operator's work is in them.
    pub fn locked_down() -> Rules {
        Rules {
            block_privileged: true,
            block_bind_mounts: true,
            bind_allow: vec![],
            block_host_pid: true,
            block_host_ipc: true,
            block_host_network: true,
            block_host_uts: true,
            block_devices: true,
            block_cap_add: true,
            block_security_opt: true,
            block_sysctls: true,
            block_consoles: false,
        }
    }

    /// Whether any rule is about what a container is made with (so a stack's file is worth resolving).
    pub fn judges_specs(&self) -> bool {
        let c = Rules { block_consoles: false, bind_allow: vec![], ..self.clone() };
        c != Rules::default()
    }

    /// The allow-list cleaned: absolute paths, no `..`, no trailing slash.
    fn checked(mut self) -> ApiResult<Rules> {
        let mut out = vec![];
        for p in &self.bind_allow {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            match normal(p) {
                Some(n) => out.push(n),
                None => return Err(ApiError::bad(format!("{p} is not an absolute host path: start it with / and leave out \"..\".")).field("bind_allow")),
            }
        }
        out.dedup();
        self.bind_allow = out;
        Ok(self)
    }
}

/// A host path as one: absolute, `.` and empty steps gone, without its trailing slash; `None` for a relative path or
/// one with `..` in it (it could climb out of any prefix).
pub fn normal(p: &str) -> Option<String> {
    if !p.starts_with('/') {
        return None;
    }
    let mut parts = vec![];
    for s in p.split('/') {
        match s {
            "" | "." => {}
            ".." => return None,
            s => parts.push(s),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// Whether host path `p` is at or under one of `allow`.
pub fn allowed_path(allow: &[String], p: &str) -> bool {
    let Some(p) = normal(p) else { return false };
    allow.iter().filter_map(|a| normal(a)).any(|a| a == "/" || p == a || p.starts_with(&format!("{a}/")))
}

/// What a rule is about, with the field of the container form it is shown on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Privileged,
    BindMount,
    HostPid,
    HostIpc,
    HostNetwork,
    HostUts,
    Devices,
    CapAdd,
    SecurityOpt,
    Sysctls,
}

impl Setting {
    pub fn field(self) -> &'static str {
        match self {
            Setting::Privileged => "privileged",
            Setting::BindMount => "binds",
            Setting::HostPid => "pid_mode",
            Setting::HostIpc => "ipc_mode",
            Setting::HostNetwork => "network_mode",
            Setting::HostUts => "uts_mode",
            Setting::Devices => "devices",
            Setting::CapAdd => "cap_add",
            Setting::SecurityOpt => "security_opt",
            Setting::Sysctls => "sysctls",
        }
    }

    fn words(self) -> &'static str {
        match self {
            Setting::Privileged => "Privileged mode",
            Setting::BindMount => "A bind mount of a host path",
            Setting::HostPid => "The host's process namespace",
            Setting::HostIpc => "The host's IPC namespace",
            Setting::HostNetwork => "The host's network",
            Setting::HostUts => "The host's UTS namespace",
            Setting::Devices => "A device",
            Setting::CapAdd => "An extra capability",
            Setting::SecurityOpt => "A security option",
            Setting::Sysctls => "A sysctl",
        }
    }
}

/// One thing a spec asks for that a rule refuses: the setting, what exactly (a path, a capability), and where (a
/// service, a volume) when there is more than one container in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breach {
    pub setting: Setting,
    pub what: String,
    pub place: Option<String>,
}

impl Breach {
    pub fn sentence(&self) -> String {
        let what = if self.what.is_empty() { String::new() } else { format!(" ({})", self.what) };
        let place = self.place.as_ref().map(|p| format!("{p}: ")).unwrap_or_default();
        format!("{place}{}{what} is turned off for non-administrators on this host.", self.setting.words())
    }
}

/// The refusal for these breaches: every one said, on the field of the first (or `field` when given, a stack's file).
pub fn refusal(breaches: &[Breach], field: Option<&'static str>) -> ApiError {
    let words: Vec<String> = breaches.iter().map(Breach::sentence).collect();
    ApiError::new(StatusCode::FORBIDDEN, words.join(" ")).field(field.unwrap_or_else(|| breaches[0].setting.field()))
}

/// What a container asks of its host, from whichever shape it came in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub place: Option<String>,
    pub privileged: bool,
    /// Host paths bind mounted (and named volumes that are host paths underneath).
    pub binds: Vec<String>,
    pub pid: Option<String>,
    pub ipc: Option<String>,
    pub network: Option<String>,
    pub uts: Option<String>,
    pub devices: Vec<String>,
    pub cap_add: Vec<String>,
    pub security_opt: Vec<String>,
    pub sysctls: Vec<String>,
}

/// Whether a path on the host is a device (or all of them): a bind of `/dev/...` is a device in all but name.
fn under_dev(p: &str) -> bool {
    normal(p).is_some_and(|p| p == "/dev" || p.starts_with("/dev/"))
}

/// What `r` refuses of `s`.
pub fn judge(r: &Rules, s: &Spec) -> Vec<Breach> {
    let mut out = vec![];
    let mut add = |setting, what: &str| out.push(Breach { setting, what: what.to_string(), place: s.place.clone() });
    if r.block_privileged && s.privileged {
        add(Setting::Privileged, "");
    }
    for b in &s.binds {
        if r.block_devices && under_dev(b) {
            add(Setting::Devices, b);
        } else if r.block_bind_mounts && !allowed_path(&r.bind_allow, b) {
            add(Setting::BindMount, b);
        }
    }
    let host = |m: &Option<String>| m.as_deref() == Some("host");
    if r.block_host_pid && host(&s.pid) {
        add(Setting::HostPid, "pid: host");
    }
    if r.block_host_ipc && host(&s.ipc) {
        add(Setting::HostIpc, "ipc: host");
    }
    if r.block_host_network && host(&s.network) {
        add(Setting::HostNetwork, "network: host");
    }
    if r.block_host_uts && host(&s.uts) {
        add(Setting::HostUts, "uts: host");
    }
    let lists = [
        (r.block_devices, &s.devices, Setting::Devices),
        (r.block_cap_add, &s.cap_add, Setting::CapAdd),
        (r.block_security_opt, &s.security_opt, Setting::SecurityOpt),
        (r.block_sysctls, &s.sysctls, Setting::Sysctls),
    ];
    for (on, items, setting) in lists {
        if on {
            for i in items {
                add(setting, i);
            }
        }
    }
    out
}

/// What `new` asks that `old` (the container an edit replaces) did not: an edit keeps what an administrator set.
pub fn added(new: Vec<Breach>, old: &[Breach]) -> Vec<Breach> {
    new.into_iter().filter(|b| !old.iter().any(|o| o.setting == b.setting && o.what == b.what)).collect()
}

// ---------------------------------------------------------------- the shapes

/// A string list from JSON that may be a list or (for sysctls) a map of name to value.
fn names(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Value::Object(m) => m.keys().cloned().collect(),
        _ => vec![],
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// The host path a volume's driver options bind (`type=none`, `o=bind`, `device=/path`), when they do.
pub fn bound_device(opts: &Value) -> Option<String> {
    let o = opts["o"].as_str().unwrap_or_default();
    let device = opts["device"].as_str()?;
    o.split(',').any(|x| matches!(x.trim(), "bind" | "rbind")).then(|| device.to_string())
}

/// Docker's create body (as the Engine API writes it) as a spec.
pub fn of_container(body: &Value) -> Spec {
    let hc = &body["HostConfig"];
    let mut binds = vec![];
    for b in hc["Binds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let src = b.split(':').next().unwrap_or_default();
        if src.starts_with('/') {
            binds.push(src.to_string());
        }
    }
    for m in hc["Mounts"].as_array().into_iter().flatten() {
        match m["Type"].as_str() {
            Some("bind") => binds.extend(text(&m["Source"])),
            Some("volume") => binds.extend(bound_device(&m["VolumeOptions"]["DriverConfig"]["Options"])),
            _ => {}
        }
    }
    let mut devices: Vec<String> = hc["Devices"].as_array().into_iter().flatten().filter_map(|d| text(&d["PathOnHost"])).collect();
    devices.extend(hc["DeviceRequests"].as_array().into_iter().flatten().map(|d| text(&d["Driver"]).unwrap_or_else(|| "a device request".into())));
    devices.extend(hc["DeviceCgroupRules"].as_array().into_iter().flatten().filter_map(text));
    Spec {
        place: None,
        privileged: hc["Privileged"].as_bool().unwrap_or(false),
        binds,
        pid: text(&hc["PidMode"]),
        ipc: text(&hc["IpcMode"]),
        network: text(&hc["NetworkMode"]),
        uts: text(&hc["UTSMode"]),
        devices,
        cap_add: names(&hc["CapAdd"]),
        security_opt: names(&hc["SecurityOpt"]),
        sysctls: names(&hc["Sysctls"]),
    }
}

/// Every service of a file Compose resolved (`config --format json`), and the top-level volumes that are host paths.
pub fn of_compose(config: &Value) -> Vec<Spec> {
    let mut out = vec![];
    // A named volume that is a host path underneath binds it into every service that mounts it.
    let bound: std::collections::HashMap<String, String> = config["volumes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| bound_device(&v["driver_opts"]).map(|d| (k.clone(), d)))
        .collect();
    for (name, s) in config["services"].as_object().into_iter().flatten() {
        let mut binds = vec![];
        for v in s["volumes"].as_array().into_iter().flatten() {
            match v["type"].as_str() {
                Some("bind") => binds.extend(text(&v["source"])),
                Some("volume") => binds.extend(v["source"].as_str().and_then(|n| bound.get(n).cloned())),
                _ => {}
            }
        }
        let mut devices = vec![];
        for d in s["devices"].as_array().into_iter().flatten() {
            match d {
                Value::String(x) => devices.push(x.split(':').next().unwrap_or_default().to_string()),
                d => devices.extend(text(&d["source"])),
            }
        }
        if s.get("gpus").is_some_and(|g| !g.is_null()) {
            devices.push("gpus".into());
        }
        for d in s["deploy"]["resources"]["reservations"]["devices"].as_array().into_iter().flatten() {
            let caps = names(&d["capabilities"]).join(",");
            devices.push(if caps.is_empty() { "a device reservation".into() } else { caps });
        }
        out.push(Spec {
            place: Some(format!("Service {name}")),
            privileged: s["privileged"].as_bool().unwrap_or(false),
            binds,
            pid: text(&s["pid"]),
            ipc: text(&s["ipc"]),
            network: text(&s["network_mode"]),
            uts: text(&s["uts"]),
            devices,
            cap_add: names(&s["cap_add"]),
            security_opt: names(&s["security_opt"]),
            sysctls: names(&s["sysctls"]),
        });
    }
    out
}

/// A Swarm service's spec (as the Engine API writes it).
pub fn of_service(spec: &Value) -> Spec {
    let t = &spec["TaskTemplate"];
    let c = &t["ContainerSpec"];
    let mut binds = vec![];
    for m in c["Mounts"].as_array().into_iter().flatten() {
        match m["Type"].as_str() {
            Some("bind") => binds.extend(text(&m["Source"])),
            Some("volume") => binds.extend(bound_device(&m["VolumeOptions"]["DriverConfig"]["Options"])),
            _ => {}
        }
    }
    let host_net = t["Networks"].as_array().into_iter().flatten().any(|n| n["Target"].as_str() == Some("host"));
    let p = &c["Privileges"];
    let mut security_opt = vec![];
    if let Some(l) = p["SELinuxContext"].as_object().filter(|o| !o.is_empty()) {
        security_opt.push(format!("SELinux {}", l.values().filter_map(Value::as_str).collect::<Vec<_>>().join(":")));
    }
    if let Some(m) = p["Seccomp"]["Mode"].as_str().filter(|m| *m != "default") {
        security_opt.push(format!("seccomp {m}"));
    }
    if let Some(m) = p["AppArmor"]["Mode"].as_str().filter(|m| *m != "default") {
        security_opt.push(format!("apparmor {m}"));
    }
    let devices = t["Resources"]["Reservations"]["GenericResources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| text(&g["DiscreteResourceSpec"]["Kind"]).or_else(|| text(&g["NamedResourceSpec"]["Kind"])).unwrap_or_else(|| "a generic resource".into()))
        .collect();
    Spec {
        place: text(&spec["Name"]).map(|n| format!("Service {n}")),
        privileged: false,
        binds,
        network: host_net.then(|| "host".to_string()),
        devices,
        cap_add: names(&c["CapabilityAdd"]),
        security_opt,
        sysctls: names(&c["Sysctls"]),
        ..Default::default()
    }
}

/// A volume's create body: a host path when its driver options bind one.
pub fn of_volume(body: &Value) -> Spec {
    Spec { place: text(&body["Name"]).map(|n| format!("Volume {n}")), binds: bound_device(&body["DriverOpts"]).into_iter().collect(), ..Default::default() }
}

// ---------------------------------------------------------------- who is held to them

/// Whether a level is above the rules: an Administrator and an Environment administrator.
pub fn exempt(level: Level) -> bool {
    matches!(level, Level::Admin | Level::Role(Role::EnvAdmin))
}

/// Whom a change is judged as when it is not a person at a page: a webhook as its owner, the Git poller as the stack's
/// maker, by their names; nobody (a name no longer there) is held to every rule.
#[derive(Clone, Debug)]
pub enum Judge {
    Exempt,
    Person(crate::auth::User),
    Named(Option<String>),
}

impl Judge {
    pub fn of(access: &Access) -> Judge {
        if exempt(access.level) { Judge::Exempt } else { Judge::Person(access.user.clone()) }
    }

    /// Whether this judge is above the rules on `env`.
    pub async fn exempt_on(&self, db: &Db, env: i64) -> anyhow::Result<bool> {
        let user = match self {
            Judge::Exempt => return Ok(true),
            Judge::Person(u) => u.clone(),
            Judge::Named(None) => return Ok(false),
            Judge::Named(Some(n)) => {
                let n = n.clone();
                let found = db
                    .call(move |c| {
                        Ok(c.query_row("SELECT id, name, role FROM users WHERE name = ?1 AND NOT disabled", params![n], |r| {
                            Ok(crate::auth::User { id: r.get(0)?, name: r.get(1)?, role: r.get(2)?, token: None })
                        })
                        .optional()?)
                    })
                    .await?;
                match found {
                    Some(u) => u,
                    None => return Ok(false),
                }
            }
        };
        Ok(super::load(db, &user, env).await?.is_some_and(|a| exempt(a.level)))
    }
}

// ---------------------------------------------------------------- kept

/// Where the rules on an environment come from.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub rules: Rules,
    pub own: Option<Rules>,
    pub group: Option<(i64, String, Option<Rules>)>,
}

fn parse(s: Option<String>) -> Option<Rules> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

pub fn found(c: &rusqlite::Connection, env: i64) -> rusqlite::Result<Found> {
    let own = parse(c.query_row("SELECT rules FROM security_rules WHERE env_id = ?1", params![env], |r| r.get(0)).optional()?);
    let group = c
        .query_row(
            "SELECT g.id, g.name, s.rules FROM environments e JOIN environment_groups g ON g.id = e.group_id LEFT JOIN security_rules s ON s.group_id = g.id WHERE e.id = ?1",
            params![env],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)),
        )
        .optional()?
        .map(|(id, name, rules)| (id, name, parse(rules)));
    let rules = own.clone().or_else(|| group.as_ref().and_then(|g| g.2.clone())).unwrap_or_default();
    Ok(Found { rules, own, group })
}

/// The rules on environment `env`: its own, else its group's, else everything allowed.
pub async fn rules(db: &Db, env: i64) -> anyhow::Result<Rules> {
    Ok(db.call(move |c| Ok(found(c, env)?)).await?.rules)
}

/// The rules `by` is held to on `env`, or `None` when nothing there is about specs or they are above the rules.
pub async fn applies(db: &Db, by: &Judge, env: i64) -> anyhow::Result<Option<Rules>> {
    if matches!(by, Judge::Exempt) {
        return Ok(None);
    }
    let r = rules(db, env).await?;
    if !r.judges_specs() || by.exempt_on(db, env).await? {
        return Ok(None);
    }
    Ok(Some(r))
}

/// Holds these specs to `r` (for an edit, only what `old` did not already have), refusing on `field` when given.
pub fn check(r: &Rules, specs: &[Spec], old: &[Spec], field: Option<&'static str>) -> ApiResult<()> {
    let was: Vec<Breach> = old.iter().flat_map(|s| judge(r, s)).collect();
    let breaches = added(specs.iter().flat_map(|s| judge(r, s)).collect(), &was);
    if breaches.is_empty() { Ok(()) } else { Err(refusal(&breaches, field)) }
}

/// [`applies`] and [`check`] at once, for a spec there is no cost to make.
pub async fn hold(db: &Db, by: &Judge, env: i64, specs: &[Spec], field: Option<&'static str>) -> ApiResult<()> {
    match applies(db, by, env).await? {
        Some(r) => check(&r, specs, &[], field),
        None => Ok(()),
    }
}

/// Whether consoles are closed to this person on `env` (the guard asks, for the console action).
pub async fn consoles_closed(db: &Db, access: &Access) -> anyhow::Result<bool> {
    if exempt(access.level) {
        return Ok(false);
    }
    Ok(rules(db, access.env).await?.block_consoles)
}

// ---------------------------------------------------------------- routes

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/environments/{id}/security", get(env_get).put(env_put))
        .route("/api/environment-groups/{id}/security", get(group_get).put(group_put))
}

fn presets() -> Value {
    json!({ "open": Rules::default(), "locked_down": Rules::locked_down() })
}

/// `GET /api/environments/{id}/security`: what holds there and where it comes from: `own` (null: inherited), `group`
/// (`{id, name, rules}`, its `rules` null when it has none), `rules` (what holds), `source`, and the presets.
async fn env_get(State(app): State<App>, _: Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let f = app
        .db
        .call(move |c| {
            let exists: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM environments WHERE id = ?1)", params![id], |r| r.get(0))?;
            Ok(exists.then(|| found(c, id)).transpose()?)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))?;
    let source = if f.own.is_some() {
        "environment"
    } else if f.group.as_ref().is_some_and(|g| g.2.is_some()) {
        "group"
    } else {
        "default"
    };
    Ok(Json(json!({
        "rules": f.rules, "own": f.own, "source": source,
        "group": f.group.map(|(id, name, rules)| json!({ "id": id, "name": name, "rules": rules })),
        "presets": presets(),
    })))
}

#[derive(Deserialize)]
struct Put {
    /// `null`: none of its own (an environment then follows its group).
    rules: Option<Rules>,
}

async fn save(app: &App, admin: &Admin, column: &'static str, id: i64, p: Put) -> ApiResult<()> {
    let rules = p.rules.map(Rules::checked).transpose()?;
    let text = rules.as_ref().map(|r| serde_json::to_string(r).unwrap_or_default());
    let by = admin.0.session.user.name.clone();
    let table = if column == "env_id" { "environments" } else { "environment_groups" };
    let target = app
        .db
        .call(move |c| {
            let Some(name) = c.query_row(&format!("SELECT name FROM {table} WHERE id = ?1"), params![id], |r| r.get::<_, String>(0)).optional()? else {
                return Ok(None);
            };
            match &text {
                Some(t) => c.execute(
                    &format!("INSERT INTO security_rules ({column}, rules, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4) ON CONFLICT ({column}) DO UPDATE SET rules = excluded.rules, updated_at = excluded.updated_at, updated_by = excluded.updated_by"),
                    params![id, t, now(), by],
                )?,
                None => c.execute(&format!("DELETE FROM security_rules WHERE {column} = ?1"), params![id])?,
            };
            Ok(Some(name))
        })
        .await?;
    let target = target.ok_or_else(|| ApiError::not_found(if column == "env_id" { "environment" } else { "group" }))?;
    let detail = match &rules {
        None => "none of its own".to_string(),
        Some(r) if *r == Rules::default() => "everything allowed".to_string(),
        Some(r) if *r == Rules::locked_down() => "locked down".to_string(),
        Some(r) => describe(r),
    };
    let action = if column == "env_id" { "environment.security" } else { "group.security" };
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action, target: Some(target), detail: Some(detail) }).await;
    Ok(())
}

/// The switches that are on, in words for the activity log.
fn describe(r: &Rules) -> String {
    let on = [
        (r.block_privileged, "privileged"),
        (r.block_bind_mounts, "bind mounts"),
        (r.block_host_pid, "host PID"),
        (r.block_host_ipc, "host IPC"),
        (r.block_host_network, "host network"),
        (r.block_host_uts, "host UTS"),
        (r.block_devices, "devices"),
        (r.block_cap_add, "capabilities"),
        (r.block_security_opt, "security options"),
        (r.block_sysctls, "sysctls"),
        (r.block_consoles, "consoles"),
    ];
    let mut s = format!("blocked: {}", on.iter().filter(|(b, _)| *b).map(|(_, w)| *w).collect::<Vec<_>>().join(", "));
    if !r.bind_allow.is_empty() {
        s.push_str(&format!("; host paths allowed: {}", r.bind_allow.join(", ")));
    }
    s
}

async fn env_put(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(p): Json<Put>) -> ApiResult<StatusCode> {
    save(&app, &admin, "env_id", id, p).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/environment-groups/{id}/security`: the group's own rules (`rules`, null when none) and the presets.
async fn group_get(State(app): State<App>, _: Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let r = app
        .db
        .call(move |c| {
            let exists: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM environment_groups WHERE id = ?1)", params![id], |r| r.get(0))?;
            if !exists {
                return Ok(None);
            }
            Ok(Some(parse(c.query_row("SELECT rules FROM security_rules WHERE group_id = ?1", params![id], |r| r.get(0)).optional()?)))
        })
        .await?
        .ok_or_else(|| ApiError::not_found("group"))?;
    Ok(Json(json!({ "rules": r, "presets": presets() })))
}

async fn group_put(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(p): Json<Put>) -> ApiResult<StatusCode> {
    save(&app, &admin, "group_id", id, p).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod tests;
