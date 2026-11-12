//! Stacks: Compose projects Freightdock keeps, deployed, updated, stopped, started, pulled and removed with Docker's own
//! `docker compose` ("Endpoints" 2). A stack's files live in the data folder, `<data>/stacks/<id>/` (ARCHITECTURE §9);
//! the database keeps what the files do not say (its environment, where it came from, when it was deployed). Projects
//! started elsewhere are listed beside them as external, and can be adopted.
//!
//! Every compose job of a stack holds that stack's lock, so a deploy, a webhook and the Git poller never run over each
//! other, and runs to its end in a task of its own even when the page that started it is closed: a deploy stopped
//! halfway leaves a stack half old and half new ("Stacks" 2).

pub mod compose;
pub mod confine;
pub mod git;
pub mod projects;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bollard::query_parameters as q;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::Sender;

use crate::access::Access;
use crate::access::security::{self, Judge};
use crate::access::ownership::{self, Kind as Owned, Owners, Wanted};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry, Who};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::docker::images::Line;
use crate::docker::{api_error, endpoint};
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use compose::{Compose, Job, Project};

/// The largest compose file or variables file Freightdock takes: far past any real one, short of filling the disk.
const MAX_FILE: usize = 1024 * 1024;

pub struct Stacks {
    root: PathBuf,
    pub compose: Compose,
    locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl Stacks {
    pub fn new(root: PathBuf, compose: Compose) -> Self {
        Stacks { root, compose, locks: Mutex::default() }
    }

    /// The folder stack `id` keeps its files in.
    pub fn dir(&self, id: i64) -> PathBuf {
        self.root.join(id.to_string())
    }

    /// Where a compose job of stack `id` finds the registries' sign-ins while it runs: beside the stacks' folders,
    /// not in its own, which its compose file can reach.
    pub fn docker_config_dir(&self, id: i64) -> PathBuf {
        self.root.join(format!(".docker-{id}"))
    }

    /// Stack `id`'s lock: one compose job (or one change to its files) at a time.
    pub fn lock(&self, id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).entry(id).or_default().clone()
    }

    /// Where a stack's files are: its compose file, the folder relative paths in it are read from, and its variables.
    /// A Git stack's compose file is at its path in the clone, and its folder is the project's.
    pub fn files(&self, s: &Stack) -> Files {
        let dir = self.dir(s.id);
        let compose = match &s.git {
            Some(g) => dir.join("repo").join(&g.path),
            None => dir.join("compose.yaml"),
        };
        let project_dir = compose.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.clone());
        Files { compose, project_dir, env: dir.join(".env"), dir }
    }

    /// A folder for files on their way somewhere, beside the stacks' own (whose folders are numbers).
    pub fn scratch(&self) -> PathBuf {
        self.root.join(".scratch")
    }

    /// The folder every stack's folder is in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A Git stack's clone.
    pub fn repo(&self, id: i64) -> PathBuf {
        self.dir(id).join("repo")
    }

    /// Everything Freightdock keeps on disk for stack `id` goes: its folder and its registries' sign-ins. A stack id can be
    /// used again once the stack is gone, so what an old stack left must never reach the new one.
    pub fn clear(&self, id: i64) {
        for d in [self.dir(id), self.docker_config_dir(id)] {
            if d.symlink_metadata().is_ok()
                && let Err(e) = std::fs::remove_dir_all(&d)
            {
                tracing::warn!("removing {}: {e}", d.display());
            }
        }
    }
}

/// The stacks of environment `env`, taken off the disk once the environment is removed (its rows go by cascade), each
/// under its lock so no job writes there meanwhile.
pub async fn forget_folders(app: &App, ids: Vec<i64>) {
    for id in ids {
        let lock = app.stacks.lock(id);
        let _held = lock.lock().await;
        app.stacks.clear(id);
    }
}

pub struct Files {
    pub dir: PathBuf,
    pub compose: PathBuf,
    pub project_dir: PathBuf,
    pub env: PathBuf,
}

impl Files {
    /// Whether the compose file is a file in the stack's own folder (a Git stack's in its clone), every link followed.
    pub fn compose_inside(&self) -> bool {
        let root = if self.compose.starts_with(self.dir.join("repo")) { self.dir.join("repo") } else { self.dir.clone() };
        confine::inside(&root, &self.dir, &self.compose) && self.compose.is_file()
    }

    /// Whether Compose, run over the stack's files, reads only files in its own folder ("Stacks" 14, 15): the compose
    /// file is one, and every file it names. Asked before every run of Compose that reads the file.
    pub fn confined(&self) -> Result<(), String> {
        if !self.compose_inside() {
            return Err(OUT_OF_FOLDER.into());
        }
        confine::references(&self.dir, &self.compose, &self.project_dir)
    }

    /// The compose file's text, when it is in the stack's own folder; empty otherwise.
    pub fn compose_text(&self) -> String {
        if self.compose_inside() { read_or_empty(&self.compose) } else { String::new() }
    }

    /// The variables' text, when the file is in the stack's own folder; empty otherwise.
    pub fn env_text(&self) -> String {
        confine::read(&self.dir, &self.env)
    }
}

/// What is said when a stack's compose file leads out of its folder.
const OUT_OF_FOLDER: &str = "The compose file must be a file in the stack's own folder, not a link out of it.";

/// A stack as the database keeps it.
#[derive(Clone, Debug, Serialize)]
pub struct Stack {
    pub id: i64,
    pub env_id: i64,
    pub name: String,
    pub source: String,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deployed_at: Option<i64>,
    pub git: Option<GitSettings>,
}

/// Where a Git stack comes from, and how its polling went. The token is sealed and never leaves the server: the page is
/// told only whether there is one ("Secrets" 1).
#[derive(Clone, Debug, Serialize)]
pub struct GitSettings {
    pub url: String,
    pub reference: String,
    pub path: String,
    pub user: Option<String>,
    #[serde(skip)]
    pub token_sealed: Option<String>,
    pub has_token: bool,
    /// Seconds between checks; 0 is off.
    pub poll_s: i64,
    pub commit: Option<String>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
}

const COLUMNS: &str = "id, env_id, name, source, created_by, created_at, updated_at, deployed_at, git_url, git_ref, git_path, git_user, git_token, git_poll_s, git_commit, git_checked_at, git_error";

fn row(r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Stack> {
    Ok(Stack {
        id: r.get(0)?,
        env_id: r.get(1)?,
        name: r.get(2)?,
        source: r.get(3)?,
        created_by: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        deployed_at: r.get(7)?,
        git: match r.get::<_, Option<String>>(8)? {
            Some(url) => {
                let token_sealed: Option<String> = r.get(12)?;
                Some(GitSettings {
                    url,
                    reference: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    path: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    user: r.get(11)?,
                    has_token: token_sealed.is_some(),
                    token_sealed,
                    poll_s: r.get(13)?,
                    commit: r.get(14)?,
                    checked_at: r.get(15)?,
                    error: r.get(16)?,
                })
            }
            None => None,
        },
    })
}

/// A Git stack's remote, its token opened.
pub fn remote(app: &App, g: &GitSettings) -> Result<git::Remote, String> {
    let token = match &g.token_sealed {
        Some(t) => Some(app.sealer.open(t).map_err(|e| format!("The stack's Git token could not be opened: {e}"))?),
        None => None,
    };
    Ok(git::Remote { url: g.url.clone(), reference: g.reference.clone(), user: g.user.clone(), token })
}

/// Stack `id` of environment `env`, or a 404.
pub async fn load(app: &App, env: i64, id: i64) -> ApiResult<Stack> {
    app.db
        .call(move |c| Ok(c.query_row(&format!("SELECT {COLUMNS} FROM stacks WHERE id = ?1 AND env_id = ?2"), params![id, env], row).optional()?))
        .await?
        .ok_or_else(|| ApiError::not_found("stack"))
}

async fn all_of(app: &App, env: i64) -> ApiResult<Vec<Stack>> {
    Ok(app
        .db
        .call(move |c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM stacks WHERE env_id = ?1 ORDER BY name"))?;
            let rows = st.query_map([env], row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}

/// Writes `text` to `path` whole or not at all: to a file beside it first, then renamed over it.
fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("freightdock-new");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn read_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// A compose file and its variables as they came in, checked for what Freightdock can tell without Compose.
fn check_files(compose_text: &str, env_text: &str) -> ApiResult<()> {
    if compose_text.trim().is_empty() {
        return Err(ApiError::bad("A stack needs its compose file.").field("compose"));
    }
    if compose_text.len() > MAX_FILE || env_text.len() > MAX_FILE {
        return Err(ApiError::bad("That file is over a megabyte; a compose file is not.").field("compose"));
    }
    compose::check_env(env_text).map_err(|m| ApiError::bad(m).field("env"))
}

/// Whether Compose takes the stack's files as they now are (`config --quiet`); its own words when it does not. Without
/// the Compose binary there is nothing to ask, and deploying says so.
async fn compose_takes(app: &App, s: &Stack) -> ApiResult<()> {
    let f = app.stacks.files(s);
    f.confined().map_err(|m| ApiError::bad(m).field("compose"))?;
    let Some(bin) = app.stacks.compose.path() else { return Ok(()) };
    let p = Project { name: &s.name, file: &f.compose, dir: &f.project_dir, env_file: Some(&f.env) };
    compose::run(bin, &compose::args(&p, Job::Check), &[], &f.dir, None).await.map(|_| ()).map_err(|m| ApiError::bad(m).field("compose"))
}

/// The stack's files as Compose resolves them for the next deploy (`config --format json`): what reads a stack whole,
/// its Environment tab and the security settings.
async fn resolved(app: &App, s: &Stack) -> Result<Value, String> {
    let bin = app.stacks.compose.bin().map_err(|e| e.message)?;
    let f = app.stacks.files(s);
    f.confined()?;
    let p = Project { name: &s.name, file: &f.compose, dir: &f.project_dir, env_file: f.env.exists().then_some(f.env.as_path()) };
    let out = compose::run(bin, &compose::args(&p, Job::Resolved), &[], &f.dir, None).await?;
    serde_json::from_str(&out.join("\n")).map_err(|e| format!("Docker Compose's answer was not the JSON asked for: {e}"))
}

/// Holds a deploy of `s` to its environment's security settings, judged as `by`: every service of the file Compose
/// resolves, a refusal naming the service and the setting ("Access" 13).
pub async fn hold(app: &App, s: &Stack, by: &Judge) -> ApiResult<()> {
    let Some(r) = security::applies(&app.db, by, s.env_id).await? else { return Ok(()) };
    let config = resolved(app, s).await.map_err(|m| ApiError::bad(format!("The stack could not be checked against this host's security settings: {m}")).field("compose"))?;
    security::check(&r, &security::of_compose(&config), &[], Some("compose"))
}

/// A compose file that is not a stack's (a Swarm stack's) as Compose resolves it, in a scratch folder of its own.
pub async fn resolve_text(app: &App, text: &str) -> Result<Value, String> {
    let bin = app.stacks.compose.bin().map_err(|e| e.message)?;
    let dir = app.stacks.scratch().join(format!("check-{}", crate::auth::random_token().get(..16).unwrap_or("x")));
    std::fs::create_dir_all(&dir).map_err(|e| format!("A folder to check the file in could not be made: {e}"))?;
    let file = dir.join("compose.yaml");
    let r = async {
        std::fs::write(&file, text).map_err(|e| format!("The file could not be written to check: {e}"))?;
        let p = Project { name: "freightdock-check", file: &file, dir: &dir, env_file: None };
        let out = compose::run(bin, &compose::args(&p, Job::Resolved), &[], &dir, None).await?;
        serde_json::from_str(&out.join("\n")).map_err(|e| format!("Docker Compose's answer was not the JSON asked for: {e}"))
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// Every stack of environment `env` and every Compose project Docker has there: Freightdock's with their state, the others
/// as external.
/// Standard and Read-only users see only the stacks they can see, and no external project ("Access" 4); each stack
/// says whom it belongs to (`access`).
pub async fn list(State(app): State<App>, access: Access, UrlPath(env): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let (stacks, containers) = tokio::join!(all_of(&app, env), d.client().list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })));
    let (stacks, containers) = (stacks?, containers.map_err(api_error)?);
    let mut projects = projects::group(&containers);
    let owners = Owners::load(&app.db, env).await?;
    let mut out: Vec<Value> = stacks
        .into_iter()
        .filter_map(|s| {
            let p = projects.remove(&s.name).unwrap_or_default();
            let o = owners.stack(s.id);
            access.sees(o).then(|| json!({ "id": s.id, "name": s.name, "source": s.source, "external": false, "state": p.state(), "containers": p.containers, "running": p.running,
                    "services": p.services, "created_by": s.created_by, "created_at": s.created_at, "updated_at": s.updated_at, "deployed_at": s.deployed_at, "git": s.git,
                    "access": owners.show(o) }))
        })
        .collect();
    out.extend(projects.into_values().filter(|_| !access.limited()).map(|p| {
        json!({ "id": null, "name": p.name, "source": "external", "external": true, "state": p.state(), "containers": p.containers, "running": p.running,
                "services": p.services, "working_dir": p.working_dir, "config_files": p.config_files, "portainer": p.portainer })
    }));
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
pub struct NewStack {
    name: String,
    #[serde(default)]
    compose: String,
    #[serde(default)]
    env: String,
    /// Who it belongs to; private to its maker when left out ("Access" 4).
    #[serde(default)]
    access: Option<Wanted>,
}

/// A new stack's ownership, as asked (private to its maker when not), saved once it is made.
pub async fn own(app: &App, env: i64, id: i64, o: Option<ownership::Owner>) -> ApiResult<()> {
    Ok(ownership::save(&app.db, env, Owned::Stack, id.to_string(), o).await?)
}

/// Makes a stack row and its files; the files are checked by Compose, and a stack whose files it refuses is not kept.
pub async fn make(app: &App, who: &Who, env: i64, name: &str, compose_text: &str, env_text: &str, action: &'static str) -> ApiResult<i64> {
    make_with(app, who, env, name, compose_text, env_text, action, Extra::default()).await
}

/// What a stack made from somewhere else needs beside its files.
#[derive(Default)]
pub struct Extra {
    /// `stack.env` beside the compose file, a link to the stack's `.env`: Portainer writes its stacks' variables to a
    /// file by that name, and their compose files name it (`env_file: stack.env`).
    pub stack_env: bool,
    /// What the activity log says of where it came from.
    pub detail: Option<String>,
}

/// `stack.env` in `dir`, a link to the stack's variables (`env`), unless there is a file by that name already.
fn link_stack_env(dir: &Path, env: &Path) -> std::io::Result<()> {
    let link = dir.join("stack.env");
    if link.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    // By its whole path: the data folder may be given relative to where Freightdock was started.
    let target = std::fs::canonicalize(env).unwrap_or_else(|_| env.to_path_buf());
    std::os::unix::fs::symlink(target, link)
}

/// `make`, with what a stack from elsewhere needs (`Extra`).
#[allow(clippy::too_many_arguments)]
pub async fn make_with(app: &App, who: &Who, env: i64, name: &str, compose_text: &str, env_text: &str, action: &'static str, extra: Extra) -> ApiResult<i64> {
    if !compose::valid_project(name) {
        return Err(ApiError::bad("A stack's name is lower-case letters, digits, - and _, starting with a letter or digit.").field("name"));
    }
    check_files(compose_text, env_text)?;
    endpoint(app, env).await?;
    let (n, by) = (name.to_string(), who.user.as_ref().map(|u| u.name.clone()));
    let id = app
        .db
        .call(move |c| {
            let t = now();
            let r = c.execute("INSERT INTO stacks (env_id, name, source, created_by, created_at, updated_at) VALUES (?1, ?2, 'editor', ?3, ?4, ?4)", params![env, n, by, t]);
            match r {
                Ok(_) => Ok(Some(c.last_insert_rowid())),
                Err(crate::db::rusqlite::Error::SqliteFailure(e, _)) if e.code == crate::db::rusqlite::ErrorCode::ConstraintViolation => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await?
        .ok_or_else(|| ApiError::conflict("There is a stack by that name here already.").field("name"))?;
    // A fresh id's folder starts empty: what a removed stack of the same id left is not this one's.
    app.stacks.clear(id);
    let s = load(app, env, id).await?;
    let f = app.stacks.files(&s);
    let written = write_whole(&f.compose, compose_text).and_then(|_| write_whole(&f.env, env_text)).and_then(|_| if extra.stack_env { link_stack_env(&f.project_dir, &f.env) } else { Ok(()) });
    let checked = match written {
        Ok(()) => compose_takes(app, &s).await,
        Err(e) => Err(anyhow::Error::from(e).context("writing a stack's files").into()),
    };
    if let Err(e) = checked {
        forget(app, &s).await;
        return Err(e);
    }
    audit::record_by(&app.db, who, Entry { action, target: Some(name.to_string()), detail: extra.detail }).await;
    Ok(id)
}

/// The stack's row and its files go (once Compose has taken its containers down, or it never had any).
async fn forget(app: &App, s: &Stack) {
    let id = s.id;
    let env = s.env_id;
    if let Err(e) = app
        .db
        .call(move |c| {
            ownership::set(c, env, Owned::Stack, &id.to_string(), None)?;
            Ok(c.execute("DELETE FROM stacks WHERE id = ?1", [id])?)
        })
        .await
    {
        tracing::error!("forgetting stack {id}: {e:#}");
    }
    crate::webhooks::forget_stack(app, id).await;
    app.stacks.clear(id);
}

/// Refuses a new stack named after a Compose project Docker already runs here, for a person who sees only some of an
/// environment ("Access" 4): a container with no record of its own belongs as its project's stack does, so a stack by
/// that name would make the administrators' containers theirs. Taking over a project is adopting it, an
/// administer action ("Stacks" 17).
pub async fn unclaimed(app: &App, access: &Access, env: i64, name: &str) -> ApiResult<()> {
    if !access.limited() {
        return Ok(());
    }
    let d = endpoint(app, env).await?;
    let filters = HashMap::from([("label".to_string(), vec![format!("{}={name}", projects::PROJECT)])]);
    let found = d.client().list_containers(Some(q::ListContainersOptions { all: true, filters: Some(filters), ..Default::default() })).await.map_err(api_error)?;
    if projects::group(&found).contains_key(name) {
        return Err(ApiError::conflict("Docker already runs a Compose project by that name here; an administrator adopts it, or pick another name.").field("name"));
    }
    Ok(())
}

/// `POST .../stacks`: a new stack from the editor, not yet deployed (that is `POST .../stacks/{id}/deploy`).
pub async fn create(State(app): State<App>, admin: Allowed, UrlPath(env): UrlPath<i64>, Json(n): Json<NewStack>) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = n.access.clone().unwrap_or_default().owner(&admin.1)?;
    unclaimed(&app, &admin.1, env, n.name.trim()).await?;
    let id = make(&app, &Who::from(&admin), env, n.name.trim(), &n.compose, &n.env, "stack.create").await?;
    own(&app, env, id, o).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// The Compose project `name` in environment `env`, from its containers' labels; a 404 when Docker has none.
async fn project_of(app: &App, env: i64, name: &str) -> ApiResult<(projects::Project, Vec<bollard::models::ContainerSummary>)> {
    let d = endpoint(app, env).await?;
    let filters = HashMap::from([("label".to_string(), vec![format!("{}={name}", projects::PROJECT)])]);
    let containers = d.client().list_containers(Some(q::ListContainersOptions { all: true, filters: Some(filters), ..Default::default() })).await.map_err(api_error)?;
    let p = projects::group(&containers).remove(name).ok_or_else(|| ApiError::not_found("Compose project here"))?;
    Ok((p, containers))
}

/// A project's compose file as Freightdock can take it over, read from where it was started: one file, made by Compose
/// from all of the project's files with relative paths made absolute (`compose::merge_args`), or the one file as it is
/// when there is no Compose to ask. The reason, in words, when Freightdock cannot read them.
async fn read_project(app: &App, p: &projects::Project) -> Result<String, String> {
    let (Some(dir), Some(first)) = (p.working_dir.as_deref(), p.config_files.first()) else {
        return Err("Its containers do not say where its compose file is.".into());
    };
    for f in &p.config_files {
        if let Err(e) = std::fs::metadata(f) {
            let why = if p.portainer { " It was started by Portainer, whose files are in Portainer's own volume." } else { "" };
            return Err(format!("Freightdock cannot read {f} ({e}).{why}"));
        }
    }
    match app.stacks.compose.path() {
        Some(bin) => {
            let env_file = p.env_file.as_deref().filter(|e| Path::new(e).is_file());
            let a = compose::merge_args(&p.name, &p.config_files, dir, env_file);
            let out = compose::run(bin, &a, &[], Path::new(dir), None).await?;
            Ok(out.join("\n") + "\n")
        }
        None => std::fs::read_to_string(first).map_err(|e| format!("Freightdock cannot read {first} ({e}).")),
    }
}

/// A project's compose file and variables, read where its labels say, or why Freightdock will not ("Stacks" 16). Labels
/// are whatever made the containers says, so they are trusted as paths only from the local Docker (whose host is the one
/// Freightdock reads), only for a Freightdock administrator, and never into Freightdock's own data folder.
pub async fn project_files(app: &App, user: Option<&crate::auth::User>, env: i64, p: &projects::Project) -> Result<(String, String), String> {
    if !user.is_some_and(|u| u.role == "admin") {
        return Err("Only a Freightdock administrator has Freightdock read a project's files where it was started.".into());
    }
    let kind: Option<String> = app
        .db
        .call(move |c| Ok(c.query_row("SELECT kind FROM environments WHERE id = ?1", [env], |r| r.get(0)).optional()?))
        .await
        .map_err(|e| format!("{e:#}"))?;
    if kind.as_deref() != Some("local") {
        return Err("Its files are on another host: Freightdock reads a project's files only on the local Docker.".into());
    }
    let env_path = p.env_file.clone().or_else(|| p.working_dir.as_ref().map(|d| format!("{d}/.env")));
    let mut own = vec![app.stacks.root().to_path_buf()];
    own.extend(app.backups.data_dir.get().cloned());
    let named = p.working_dir.iter().chain(&p.config_files).chain(&env_path);
    for f in named {
        if own.iter().any(|o| confine::inside(o, o, Path::new(f))) {
            return Err(format!("{f} is Freightdock's own: its labels name a file in Freightdock's data folder."));
        }
    }
    let compose = read_project(app, p).await?;
    Ok((compose, env_path.map(|f| read_or_empty(Path::new(&f))).unwrap_or_default()))
}

/// `GET .../stacks/external/{name}`: a project started elsewhere, its containers, and its files when Freightdock can read
/// them (or why not), for adopting it.
pub async fn external(State(app): State<App>, who: Authed, UrlPath((env, name)): UrlPath<(i64, String)>) -> ApiResult<Json<Value>> {
    let (p, containers) = project_of(&app, env, &name).await?;
    let read = project_files(&app, Some(&who.session.user), env, &p).await;
    let (compose, env_text) = match &read {
        Ok((c, e)) => (Some(c.as_str()), e.as_str()),
        Err(_) => (None, ""),
    };
    Ok(Json(json!({ "project": p, "state": p.state(), "containers": containers, "compose": compose, "env": env_text, "unreadable": read.as_ref().err() })))
}

#[derive(Deserialize)]
pub struct Adopt {
    name: String,
    /// The compose file, when Freightdock cannot read the project's own; else it is read from where it was started.
    compose: Option<String>,
    env: Option<String>,
}

/// Takes over project `p`: a stack by its name with its files, given or read from where it was started. Its containers
/// are left as they are until it is next deployed.
pub async fn adopt_project(app: &App, who: &Who, env: i64, p: &projects::Project, compose_text: Option<String>, env_text: Option<String>) -> ApiResult<i64> {
    let (compose_text, env_text) = match compose_text.filter(|c| !c.trim().is_empty()) {
        Some(c) => (c, env_text.unwrap_or_default()),
        None => {
            let (c, e) = project_files(app, who.user.as_ref(), env, p).await.map_err(|m| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("{m} Paste its compose file to adopt it.")).field("compose"))?;
            (c, env_text.unwrap_or(e))
        }
    };
    make(app, who, env, &p.name, &compose_text, &env_text, "stack.adopt").await
}

/// `POST .../stacks/adopt`: a project started elsewhere becomes one of Freightdock's stacks.
pub async fn adopt(State(app): State<App>, admin: Allowed, UrlPath(env): UrlPath<i64>, Json(a): Json<Adopt>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = project_of(&app, env, a.name.trim()).await?;
    let id = adopt_project(&app, &Who::from(&admin), env, &p, a.compose, a.env).await?;
    own(&app, env, id, Wanted::default().owner(&admin.1)?).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// `GET .../stacks/{id}`: the stack, its files and its containers. When its Docker does not answer, the files still
/// come (to read, fix and save), with the state `unknown` and Docker's reason as `docker_error`.
pub async fn get(State(app): State<App>, _: Authed, UrlPath((env, id)): UrlPath<(i64, i64)>) -> ApiResult<Json<Value>> {
    let owners = Owners::load(&app.db, env).await?;
    let s = load(&app, env, id).await?;
    let d = endpoint(&app, env).await?;
    let f = app.stacks.files(&s);
    let filters = HashMap::from([("label".to_string(), vec![format!("{}={}", projects::PROJECT, s.name)])]);
    let (state, containers, docker_error) = match d.client().list_containers(Some(q::ListContainersOptions { all: true, filters: Some(filters), ..Default::default() })).await {
        Ok(c) => (projects::group(&c).remove(&s.name).unwrap_or_default().state(), c, None),
        Err(e) => ("unknown", Vec::new(), Some(api_error(e).message)),
    };
    Ok(Json(json!({ "stack": s, "compose": f.compose_text(), "env": f.env_text(), "state": state, "containers": containers, "docker_error": docker_error, "access": owners.show(owners.stack(id)) })))
}

/// `PUT .../stacks/{id}/access`: `{level, team_id, user_id}` ("Access" 4).
pub async fn set_access(State(app): State<App>, admin: Allowed, UrlPath((env, id)): UrlPath<(i64, i64)>, Json(w): Json<Wanted>) -> ApiResult<StatusCode> {
    let o = w.owner(&admin.1)?;
    let s = load(&app, env, id).await?;
    own(&app, env, id, o.clone()).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "stack.access", target: Some(s.name), detail: Some(o.map_or("admins", |o| o.level.as_str()).to_string()) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET .../stacks/{id}/environment`: every service's variables as Compose would start it, from the stack's files with
/// `${...}`, `.env` and `env_file` filled in ("Stacks" 11). Read from the files, not the running containers, so it is
/// what the next deploy gives.
pub async fn environment(State(app): State<App>, _: Authed, UrlPath((env, id)): UrlPath<(i64, i64)>) -> ApiResult<Json<Value>> {
    let s = load(&app, env, id).await?;
    app.stacks.compose.bin()?;
    let config = resolved(&app, &s).await.map_err(ApiError::bad)?;
    let services = compose::environments(&config.to_string()).map_err(ApiError::bad)?;
    Ok(Json(json!({ "services": services })))
}

#[derive(Deserialize)]
pub struct Edit {
    /// The compose file; a Git stack's comes from its repository and is not taken from here.
    #[serde(default)]
    compose: String,
    #[serde(default)]
    env: String,
}

/// `PUT .../stacks/{id}`: new files, kept only when Compose takes them. Not deployed until asked.
pub async fn update(State(app): State<App>, admin: Allowed, UrlPath((env, id)): UrlPath<(i64, i64)>, Json(e): Json<Edit>) -> ApiResult<StatusCode> {
    let s = load(&app, env, id).await?;
    let lock = app.stacks.lock(id);
    let _held = lock.lock().await;
    let f = app.stacks.files(&s);
    let (old_compose, old_env) = (f.compose_text(), f.env_text());
    let git = s.git.is_some();
    let compose_text = if git { old_compose.clone() } else { e.compose };
    if git {
        compose::check_env(&e.env).map_err(|m| ApiError::bad(m).field("env"))?;
    } else {
        check_files(&compose_text, &e.env)?;
    }
    let put = |c: &str, v: &str| {
        let r = if git { write_whole(&f.env, v) } else { write_whole(&f.compose, c).and_then(|_| write_whole(&f.env, v)) };
        r.map_err(|e| ApiError::from(anyhow::Error::from(e).context("writing a stack's files")))
    };
    put(&compose_text, &e.env)?;
    if (!git || f.compose.is_file())
        && let Err(err) = compose_takes(&app, &s).await
    {
        put(&old_compose, &old_env)?;
        return Err(err);
    }
    app.db.call(move |c| Ok(c.execute("UPDATE stacks SET updated_at = ?1 WHERE id = ?2", params![now(), id])?)).await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "stack.update", target: Some(s.name), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct GitForm {
    /// A new stack's name; an edit keeps the name it has.
    #[serde(default)]
    name: String,
    url: String,
    /// A branch or a tag; empty for the default branch.
    #[serde(default)]
    reference: String,
    /// The compose file inside the repository; `docker-compose.yml` when empty, as Portainer has it.
    #[serde(default)]
    path: String,
    user: Option<String>,
    /// A new token; empty or missing keeps the one there is ("Secrets" 1), while the address stays on its origin
    /// ("Stacks" 13).
    token: Option<String>,
    #[serde(default)]
    clear_token: bool,
    /// Minutes between checks for a new commit; 0 is off.
    #[serde(default)]
    poll_minutes: i64,
    #[serde(default)]
    env: String,
    /// A new stack's ownership ("Access" 4); an edit keeps what it has.
    #[serde(default)]
    access: Option<Wanted>,
    /// Not from a page: what a stack from elsewhere needs (`Extra::stack_env`), linked beside the compose file in the
    /// clone.
    #[serde(skip)]
    stack_env: bool,
}

impl GitForm {
    /// A Git stack from Portainer: its repository and settings, its variables, and `stack.env` beside its compose file.
    #[allow(clippy::too_many_arguments)]
    pub fn from_portainer(name: &str, url: &str, reference: &str, path: &str, user: Option<String>, token: Option<String>, poll_minutes: i64, env: &str) -> Self {
        GitForm { name: name.into(), url: url.into(), reference: reference.into(), path: path.into(), user, token, clear_token: false, poll_minutes, env: env.into(), access: None, stack_env: true }
    }

    /// A stack template's repository as a Git stack: its default branch, not polled.
    pub fn for_template(name: &str, url: &str, path: &str, env: &str) -> Self {
        GitForm { name: name.into(), url: url.into(), reference: String::new(), path: path.into(), user: None, token: None, clear_token: false, poll_minutes: 0, env: env.into(), access: None, stack_env: false }
    }
}

/// The longest wait between checks, a day; the shortest is a minute.
const POLL_MAX_MIN: i64 = 24 * 60;

/// A Git form checked: address, reference, path (defaulted), the minutes as seconds.
fn check_git(f: &GitForm) -> ApiResult<(String, String, String, i64)> {
    let url = f.url.trim().to_string();
    if !git::allowed_url(&url) {
        return Err(ApiError::bad("A repository's address starts https:// (or http://), such as https://github.com/owner/repo.git.").field("url"));
    }
    let reference = f.reference.trim().to_string();
    if !git::valid_ref(&reference) {
        return Err(ApiError::bad("That is not a branch or tag name.").field("reference"));
    }
    let path = if f.path.trim().is_empty() { "docker-compose.yml".to_string() } else { f.path.trim().to_string() };
    if !git::valid_path(&path) {
        return Err(ApiError::bad("The compose file's path is relative to the repository, such as deploy/compose.yaml.").field("path"));
    }
    if !(0..=POLL_MAX_MIN).contains(&f.poll_minutes) {
        return Err(ApiError::bad("Check every 1 to 1440 minutes, or 0 for never.").field("poll_minutes"));
    }
    compose::check_env(&f.env).map_err(|m| ApiError::bad(m).field("env"))?;
    Ok((url, reference, path, f.poll_minutes * 60))
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Makes a Git stack: the remote is asked first, then cloned, and the compose file at its path checked by Compose; a
/// stack whose repository, branch or file does not work is not kept. Not deployed until asked.
pub async fn make_git(app: &App, who: &Who, env: i64, f: &GitForm) -> ApiResult<i64> {
    let name = f.name.trim().to_string();
    if !compose::valid_project(&name) {
        return Err(ApiError::bad("A stack's name is lower-case letters, digits, - and _, starting with a letter or digit.").field("name"));
    }
    let (url, reference, path, poll_s) = check_git(f)?;
    endpoint(app, env).await?;
    let (user, token) = (non_empty(&f.user), non_empty(&f.token));
    let r = git::Remote { url: url.clone(), reference: reference.clone(), user: user.clone(), token: token.clone() };
    git::head(&r).await.map_err(|m| ApiError::bad(m).field("url"))?;
    let sealed = token.as_deref().map(|t| app.sealer.seal(t)).transpose()?;
    let (n, by, u, rf, p) = (name.clone(), who.user.as_ref().map(|u| u.name.clone()), url.clone(), reference, path.clone());
    let id = app
        .db
        .call(move |c| {
            let t = now();
            let r = c.execute(
                "INSERT INTO stacks (env_id, name, source, git_url, git_ref, git_path, git_user, git_token, git_poll_s, created_by, created_at, updated_at)
                 VALUES (?1, ?2, 'git', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![env, n, u, rf, p, user, sealed, poll_s, by, t],
            );
            match r {
                Ok(_) => Ok(Some(c.last_insert_rowid())),
                Err(crate::db::rusqlite::Error::SqliteFailure(e, _)) if e.code == crate::db::rusqlite::ErrorCode::ConstraintViolation => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await?
        .ok_or_else(|| ApiError::conflict("There is a stack by that name here already.").field("name"))?;
    app.stacks.clear(id);
    let s = load(app, env, id).await?;
    let checked = async {
        write_whole(&app.stacks.files(&s).env, &f.env).map_err(|e| ApiError::from(anyhow::Error::from(e).context("writing a stack's variables")))?;
        git::sync(&app.stacks.repo(id), &r).await.map_err(|m| ApiError::bad(m).field("url"))?;
        let files = app.stacks.files(&s);
        if !files.compose.is_file() {
            return Err(ApiError::bad(format!("The repository has no {path}.")).field("path"));
        }
        if !files.compose_inside() {
            return Err(ApiError::bad(OUT_OF_FOLDER).field("path"));
        }
        if f.stack_env {
            link_stack_env(&files.project_dir, &files.env).map_err(|e| ApiError::from(anyhow::Error::from(e).context("linking stack.env")))?;
        }
        compose_takes(app, &s).await
    };
    if let Err(e) = checked.await {
        forget(app, &s).await;
        return Err(e);
    }
    audit::record_by(&app.db, who, Entry { action: "stack.create", target: Some(name), detail: Some(format!("from {url}")) }).await;
    Ok(id)
}

/// `POST .../stacks/git`: a new stack from a Git repository.
pub async fn create_git(State(app): State<App>, admin: Allowed, UrlPath(env): UrlPath<i64>, Json(f): Json<GitForm>) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = f.access.clone().unwrap_or_default().owner(&admin.1)?;
    unclaimed(&app, &admin.1, env, f.name.trim()).await?;
    let id = make_git(&app, &Who::from(&admin), env, &f).await?;
    own(&app, env, id, o).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// `PUT .../stacks/{id}/git`: a Git stack's repository, branch, path, credentials, polling and variables. The remote is
/// asked first; the new commit is fetched on the next deploy.
pub async fn update_git(State(app): State<App>, admin: Allowed, UrlPath((env, id)): UrlPath<(i64, i64)>, Json(f): Json<GitForm>) -> ApiResult<StatusCode> {
    let s = load(&app, env, id).await?;
    let Some(old) = &s.git else { return Err(ApiError::bad("That stack is not from Git.")) };
    let (url, reference, path, poll_s) = check_git(&f)?;
    let token = match (non_empty(&f.token), f.clear_token) {
        (Some(t), _) => Some(t),
        (None, true) => None,
        // The saved token is for the origin it was saved with; another host never sees it.
        (None, false) if git::same_origin(&old.url, &url) => remote(&app, old).map_err(|m| ApiError::from(anyhow::anyhow!(m)))?.token,
        (None, false) if old.has_token => return Err(ApiError::bad("The repository is on another host now: give its token again, or clear the token.").field("token")),
        (None, false) => None,
    };
    let user = non_empty(&f.user);
    git::head(&git::Remote { url: url.clone(), reference: reference.clone(), user: user.clone(), token: token.clone() }).await.map_err(|m| ApiError::bad(m).field("url"))?;
    let sealed = token.as_deref().map(|t| app.sealer.seal(t)).transpose()?;
    let lock = app.stacks.lock(id);
    let _held = lock.lock().await;
    write_whole(&app.stacks.files(&s).env, &f.env).map_err(|e| ApiError::from(anyhow::Error::from(e).context("writing a stack's variables")))?;
    let u = url.clone();
    app.db
        .call(move |c| {
            c.execute(
                "UPDATE stacks SET git_url = ?1, git_ref = ?2, git_path = ?3, git_user = ?4, git_token = ?5, git_poll_s = ?6, git_error = NULL, updated_at = ?7 WHERE id = ?8",
                params![u, reference, path, user, sealed, poll_s, now(), id],
            )?;
            Ok(())
        })
        .await?;
    audit::record_by(&app.db, &Who::from(&admin), Entry { action: "stack.update", target: Some(s.name), detail: Some(format!("from {url}")) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// How often the poller looks for Git stacks that are due.
const POLL_TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// The Git poller: every half minute, each Git stack whose interval has passed asks its remote what its branch points
/// at, and is deployed again when that is not the commit it runs ("Stacks" 8).
pub fn spawn_poller(app: App) {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(POLL_TICK);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            poll_once(&app, now()).await;
        }
    });
}

/// One round of the poller at time `at`: the stacks it deployed again.
pub async fn poll_once(app: &App, at: i64) -> Vec<i64> {
    let stacks = match app
        .db
        .call(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM stacks WHERE source = 'git' AND git_poll_s > 0"))?;
            let rows = st.query_map([], row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("the Git poller could not read the stacks: {e:#}");
            return Vec::new();
        }
    };
    let mut deployed = Vec::new();
    for s in stacks {
        let Some(g) = &s.git else { continue };
        if !git::due(at, g.checked_at, g.poll_s) {
            continue;
        }
        let outcome = match remote(app, g) {
            Ok(r) => git::head(&r).await,
            Err(m) => Err(m),
        };
        let error = match outcome {
            Ok(c) if Some(&c) == g.commit.as_ref() => None,
            Ok(c) => {
                let r = run(app, &s, Act::Deploy { pull: false }, &Who::default(), &Judge::Named(s.created_by.clone()), Some(&format!("by the Git poller, for {}", &c[..c.len().min(12)])), None).await;
                deployed.push(s.id);
                r.err()
            }
            Err(m) => Some(m),
        };
        if let Some(m) = &error {
            tracing::warn!("Git stack {}: {m}", s.name);
        }
        let id = s.id;
        if let Err(e) = app.db.call(move |c| Ok(c.execute("UPDATE stacks SET git_checked_at = ?1, git_error = ?2 WHERE id = ?3", params![at, error, id])?)).await {
            tracing::error!("the Git poller could not note its check: {e:#}");
        }
    }
    deployed
}

#[derive(Deserialize, Default)]
pub struct JobBody {
    /// Deploy and redeploy: every image pulled again first.
    #[serde(default)]
    pull: bool,
    /// Remove: the stack's volumes too.
    #[serde(default)]
    volumes: bool,
}

/// What a page or a webhook asks a stack to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Deploy { pull: bool },
    Stop,
    Start,
    Pull,
    Remove { volumes: bool },
}

impl Act {
    fn parse(word: &str, b: &JobBody) -> Option<Act> {
        Some(match word {
            "deploy" => Act::Deploy { pull: b.pull },
            "stop" => Act::Stop,
            "start" => Act::Start,
            "pull" => Act::Pull,
            "remove" => Act::Remove { volumes: b.volumes },
            _ => return None,
        })
    }

    fn job(self) -> Job {
        match self {
            Act::Deploy { pull } => Job::Up { pull },
            Act::Stop => Job::Stop,
            Act::Start => Job::Start,
            Act::Pull => Job::Pull,
            Act::Remove { volumes } => Job::Down { volumes },
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Act::Deploy { .. } => "stack.deploy",
            Act::Stop => "stack.stop",
            Act::Start => "stack.start",
            Act::Pull => "stack.pull",
            Act::Remove { .. } => "stack.remove",
        }
    }
}

/// Runs `act` on stack `s`, holding its lock, saying what Compose says to `tx` as it goes, and writes the outcome to the
/// activity log, a failure with its reason. `via` says, in the log, what asked for it when nobody signed in did.
///
/// A deploy is held to the environment's security settings as `by` ("Access" 13): the person at the page, a webhook's
/// owner, the Git poller as the stack's maker.
pub async fn run(app: &App, s: &Stack, act: Act, who: &Who, by: &Judge, via: Option<&str>, tx: Option<&Sender<Line>>) -> Result<(), String> {
    let lock = app.stacks.lock(s.id);
    let _held = lock.lock().await;
    let r = run_held(app, s, act, by, tx).await;
    let mut detail: Vec<String> = via.map(str::to_string).into_iter().collect();
    match act {
        Act::Deploy { pull: true } => detail.push("images pulled again".into()),
        Act::Remove { volumes: true } => detail.push("with its volumes".into()),
        _ => {}
    }
    if let Err(m) = &r {
        detail.push(format!("failed: {m}"));
        // A deploy that failed is news, whoever asked for it: a person, a webhook, the Git poller ("Notifications" 6).
        if let Act::Deploy { .. } = act {
            let env = s.env_id;
            let name = app
                .db
                .call(move |c| Ok(c.query_row("SELECT name FROM environments WHERE id = ?1", params![env], |r| r.get::<_, String>(0)).optional()?))
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("environment {env}"));
            crate::notify::deploy_failed(app, (env, name), &s.name, m);
        }
    }
    let detail = (!detail.is_empty()).then(|| detail.join("; "));
    audit::record_by(&app.db, who, Entry { action: act.action(), target: Some(s.name.clone()), detail }).await;
    r
}

async fn run_held(app: &App, s: &Stack, act: Act, by: &Judge, tx: Option<&Sender<Line>>) -> Result<(), String> {
    let bin = app.stacks.compose.bin().map_err(|e| e.message)?;
    let env = compose::docker_env(app, s.env_id).await.map_err(|e| e.message)?;
    std::fs::create_dir_all(app.stacks.dir(s.id)).map_err(|e| format!("The stack's folder could not be made: {e}"))?;
    let mut commit = None;
    if let (Some(g), Act::Deploy { .. }) = (&s.git, act) {
        let r = remote(app, g)?;
        let c = git::sync(&app.stacks.repo(s.id), &r).await?;
        if let Some(tx) = tx {
            let _ = tx.send(Line { stream: Some(format!("{} at {}\n", g.url, &c[..c.len().min(12)])), ..Default::default() }).await;
        }
        commit = Some(c);
    }
    let f = app.stacks.files(s);
    if act.job().needs_file() && !f.compose.is_file() {
        return Err(format!("There is no compose file at {}.", s.git.as_ref().map_or("compose.yaml", |g| g.path.as_str())));
    }
    if act.job().needs_file() {
        f.confined()?;
    }
    if let Act::Deploy { .. } = act {
        hold(app, s, by).await.map_err(|e| e.message)?;
    }
    let p = Project { name: &s.name, file: &f.compose, dir: &f.project_dir, env_file: f.env.exists().then_some(f.env.as_path()) };
    // Kept until Compose is done, then the folder goes.
    let config = if act.job().pulls() { compose::registry_config(app, app.stacks.docker_config_dir(s.id), tx).await? } else { None };
    let mut env = env;
    env.extend(config.as_ref().map(compose::DockerConfig::env));
    compose::run(bin, &compose::args(&p, act.job()), &env, &f.dir, tx).await?;
    match act {
        Act::Deploy { .. } => {
            let id = s.id;
            app.db
                .call(move |c| Ok(c.execute("UPDATE stacks SET deployed_at = ?1, git_commit = COALESCE(?2, git_commit) WHERE id = ?3", params![now(), commit, id])?))
                .await
                .map_err(|e| format!("{e:#}"))?;
        }
        Act::Remove { .. } => forget(app, s).await,
        _ => {}
    }
    Ok(())
}

/// A job's lines as NDJSON, from a task that runs to the end whether or not anybody reads: the last line `{"done":true}`
/// when it went well, or an `error` line with the reason ("Streams" 1, "Stacks" 2).
fn ndjson<F, Fut>(job: F) -> Response
where
    F: FnOnce(Sender<Line>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Line>(256);
    let fut = job(tx.clone());
    tokio::spawn(async move {
        let last = match fut.await {
            Ok(()) => Line { done: Some(true), ..Default::default() },
            Err(m) => Line { error: Some(m), ..Default::default() },
        };
        let _ = tx.send(last).await;
    });
    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        let l = rx.recv().await?;
        Some((Ok::<_, std::io::Error>(Bytes::from(serde_json::to_string(&l).unwrap_or_default() + "\n")), rx))
    });
    ([(header::CONTENT_TYPE, "application/x-ndjson"), (header::CACHE_CONTROL, "no-cache")], Body::from_stream(body)).into_response()
}

/// `POST .../stacks/{id}/{deploy,stop,start,pull,remove}`: the job, as NDJSON.
pub async fn act(State(app): State<App>, admin: Allowed, UrlPath((env, id, word)): UrlPath<(i64, i64, String)>, body: Option<Json<JobBody>>) -> ApiResult<Response> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let act = Act::parse(&word, &b).ok_or_else(|| ApiError::not_found("stack action"))?;
    let s = load(&app, env, id).await?;
    app.stacks.compose.bin()?;
    compose::docker_env(&app, env).await?;
    let who = Who::from(&admin);
    let by = Judge::of(&admin.1);
    // A file already here is checked before the job starts, so the refusal comes as an answer on the file's field; a
    // Git stack's comes with its fetch, and is checked in the job.
    if let (Act::Deploy { .. }, None) = (act, &s.git) {
        hold(&app, &s, &by).await?;
    }
    Ok(ndjson(move |tx| async move { run(&app, &s, act, &who, &by, None, Some(&tx)).await }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::tests::{call, json_body, post, signed_in};
    use axum::http::Request;

    /// An app whose Compose is the stand-in of `compose::tests::fake`, in `dir`.
    pub fn app_with_compose(dir: &Path) -> App {
        let db = crate::db::Db::open_in_memory().unwrap();
        let stacks = Stacks::new(dir.join("stacks"), Compose::at(compose::tests::fake(dir)));
        crate::state::AppState::new(db, false, crate::docker::Docker::connect("tcp://127.0.0.1:1").unwrap(), crate::secret::Sealer::for_test(), crate::docker::sockets::Sockets::for_test(), crate::agent::Agents::for_test(), stacks)
    }

    pub fn calls(dir: &Path) -> Vec<String> {
        read_or_empty(&dir.join("calls")).lines().map(str::to_string).collect()
    }

    pub async fn send(app: &App, cookie: &str, csrf: &str, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let req = post(path).method(method).header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(app, req).await;
        (s, b)
    }

    /// A job's NDJSON lines, read to the end.
    pub async fn stream(app: &App, cookie: &str, csrf: &str, path: &str, body: Value) -> (StatusCode, Vec<Value>) {
        use tower::ServiceExt;
        let req = post(path).header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap();
        let r = crate::api::router(app.clone()).oneshot(req).await.unwrap();
        let s = r.status();
        let bytes = http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes();
        (s, String::from_utf8_lossy(&bytes).lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }

    #[tokio::test]
    async fn a_stack_is_made_with_its_files_in_the_data_folder_and_checked_by_compose() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services:\n  app:\n    image: nginx\n", "env": "TAG=1\n" })).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let id = b["id"].as_i64().unwrap();
        let folder = dir.path().join(format!("stacks/{id}"));
        assert_eq!(std::fs::read_to_string(folder.join("compose.yaml")).unwrap(), "services:\n  app:\n    image: nginx\n");
        assert_eq!(std::fs::read_to_string(folder.join(".env")).unwrap(), "TAG=1\n");
        let checked = calls(dir.path());
        assert!(checked[0].ends_with("config --quiet") && checked[0].contains("-p web -f ") && checked[0].contains("--env-file "), "{checked:?}");

        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")), "one stack by a name in an environment");
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "Not OK", "compose": "services: {}\n" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("name")));
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "vars", "compose": "services: {}\n", "env": "no equals" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("env")));
    }

    #[tokio::test]
    async fn a_stacks_environment_is_every_services_variables_as_compose_resolves_them() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services:\n  db:\n    image: postgres\n", "env": "" })).await;
        let id = b["id"].as_i64().unwrap();
        let req = Request::get(format!("/api/envs/1/stacks/{id}/environment")).header(header::COOKIE, &cookie).body(axum::body::Body::empty()).unwrap();
        let (s, _, v) = call(&app, req).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["services"][1]["service"], "db");
        assert_eq!(v["services"][1]["environment"], json!([["EMPTY", ""], ["POSTGRES_PASSWORD", "s3cret"], ["POSTGRES_USER", "postgres"]]));
        let asked = calls(dir.path()).into_iter().last().unwrap();
        assert!(asked.ends_with("config --format json") && asked.contains("--env-file "), "{asked}");
        let anon = Request::get(format!("/api/envs/1/stacks/{id}/environment")).body(axum::body::Body::empty()).unwrap();
        assert_eq!(call(&app, anon).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn compose_pulls_sign_in_to_the_saved_registries_and_the_config_goes_after() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services:\n  app:\n    image: registry.example.com/team/app:1\n" })).await;
        let id = b["id"].as_i64().unwrap();
        let configs = || read_or_empty(&dir.path().join("configs"));
        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/deploy"), json!({})).await;
        assert_eq!(lines.last().unwrap()["done"], true, "{lines:?}");
        assert!(!configs().contains("registry.example.com"), "no registry saved, no configuration written: {}", configs());

        let sealed = app.sealer.seal("hunter2").unwrap();
        app.db
            .call(move |c| {
                Ok(c.execute(
                    "INSERT INTO registries (name, kind, url, host, username, secret, created_at) VALUES ('team', 'custom', 'registry.example.com', 'registry.example.com', 'deployer', ?1, 0)",
                    params![sealed],
                )?)
            })
            .await
            .unwrap();
        let auth = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "deployer:hunter2");
        for (word, body) in [("deploy", json!({ "pull": true })), ("pull", json!({}))] {
            std::fs::remove_file(dir.path().join("configs")).ok();
            let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/{word}"), body).await;
            assert_eq!(lines.last().unwrap()["done"], true, "{word}: {lines:?}");
            let got = configs();
            let mut parts = got.splitn(2, '\n');
            let folder = PathBuf::from(parts.next().unwrap());
            let config: Value = serde_json::from_str(parts.next().unwrap_or("").trim()).unwrap_or_default();
            assert_eq!(config["auths"]["registry.example.com"]["auth"], json!(auth), "{word}: Compose signs in with the saved credentials: {got}");
            assert!(folder.is_absolute() && !folder.starts_with(dir.path().join(format!("stacks/{id}"))), "{word}: not in the stack's own folder: {}", folder.display());
            assert!(!folder.exists(), "{word}: the configuration is gone once Compose is done: {}", folder.display());
        }
        std::fs::remove_file(dir.path().join("configs")).ok();
        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/stop"), json!({})).await;
        assert_eq!(lines.last().unwrap()["done"], true);
        assert!(!configs().contains("registry.example.com"), "a job that pulls nothing is given no credentials");
    }

    #[tokio::test]
    async fn a_deploy_that_fails_is_told_to_the_channels_that_want_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (url, got) = crate::notify::tests::listener().await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/notifications", json!({ "name": "hook", "kind": "webhook", "events": ["deploy.failed"], "config": { "url": url } })).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        // The file breaks after it was taken (a Git push, a hand on the server): Compose refuses the deploy.
        std::fs::write(dir.path().join(format!("stacks/{id}/compose.yaml")), "services: broken\n").unwrap();
        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/deploy"), json!({})).await;
        assert!(lines.last().unwrap()["error"].is_string(), "{lines:?}");
        let got = tokio::time::timeout(std::time::Duration::from_secs(10), got).await.expect("nothing was told").unwrap();
        assert!(got.contains("\"event\":\"deploy.failed\"") && got.contains("web did not deploy on local"), "{got}");
        assert!(got.contains("brokn is not allowed"), "Compose's reason goes with it: {got}");
    }

    #[tokio::test]
    async fn a_file_compose_refuses_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: broken\n" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["error"], "services.web Additional property brokn is not allowed", "Compose's own words");
        let n: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM stacks", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 0);
        assert!(!dir.path().join("stacks/1").exists(), "its folder went with it");

        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        let (s, _) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}"), json!({ "compose": "services: broken\n", "env": "" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let kept = std::fs::read_to_string(dir.path().join(format!("stacks/{id}/compose.yaml"))).unwrap();
        assert_eq!(kept, "services: {}\n", "an edit Compose refuses leaves the file as it was");
        let (s, _) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}"), json!({ "compose": "services:\n  a: {image: x}\n", "env": "A=1" })).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn jobs_stream_compose_s_output_and_are_logged() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        let (s, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/deploy"), json!({ "pull": true })).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(lines.last().unwrap()["done"], true, "{lines:?}");
        assert!(lines.iter().any(|l| l["stream"] == "DOCKER_HOST=tcp://127.0.0.1:1 FREIGHTDOCK_SECRET=\n"), "compose reaches the environment's Docker: {lines:?}");
        let up = calls(dir.path()).pop().unwrap();
        assert!(up.ends_with("up --detach --remove-orphans --pull always"), "{up}");
        for (word, tail) in [("stop", "-p web stop"), ("start", "-p web start"), ("pull", "pull")] {
            let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/{word}"), json!({})).await;
            assert_eq!(lines.last().unwrap()["done"], true);
            assert!(calls(dir.path()).pop().unwrap().ends_with(tail));
        }
        let (s, _) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/explode"), json!({})).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/remove"), json!({ "volumes": true })).await;
        assert_eq!(lines.last().unwrap()["done"], true);
        assert!(calls(dir.path()).pop().unwrap().ends_with("-p web down --remove-orphans --volumes"));
        assert!(!dir.path().join(format!("stacks/{id}")).exists());
        let (s, _, _) = call(&app, Request::get(format!("/api/envs/1/stacks/{id}")).header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let actions: Vec<(String, Option<String>)> = app
            .db
            .call(|c| {
                let mut st = c.prepare("SELECT action, detail FROM audit WHERE action LIKE 'stack.%' ORDER BY id")?;
                let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(r)
            })
            .await
            .unwrap();
        let names: Vec<&str> = actions.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(names, ["stack.create", "stack.deploy", "stack.stop", "stack.start", "stack.pull", "stack.remove"]);
        assert_eq!(actions[1].1.as_deref(), Some("images pulled again"));
    }

    #[tokio::test]
    async fn a_job_that_fails_ends_with_compose_s_reason_and_is_logged_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        // Broken behind Freightdock's back, so only the deploy finds out.
        std::fs::write(dir.path().join(format!("stacks/{id}/compose.yaml")), "services: broken\n").unwrap();
        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/deploy"), json!({})).await;
        assert_eq!(lines.last().unwrap()["error"], "services.web Additional property brokn is not allowed");
        let detail: String = app.db.call(|c| Ok(c.query_row("SELECT detail FROM audit WHERE action = 'stack.deploy'", [], |r| r.get(0))?)).await.unwrap();
        assert!(detail.starts_with("failed: "), "{detail}");
    }

    #[tokio::test]
    async fn without_compose_a_job_is_a_503_and_reading_still_works() {
        let app = crate::api::tests::app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let (s, _) = stream(&app, &cookie, &csrf, "/api/envs/1/stacks/1/deploy", json!({})).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    }

    fn started_elsewhere(dir: &Path, portainer: bool) -> projects::Project {
        let wd = dir.join("elsewhere");
        std::fs::create_dir_all(&wd).unwrap();
        std::fs::write(wd.join("compose.yaml"), "services:\n  app: {image: nginx, volumes: [./data:/data]}\n").unwrap();
        std::fs::write(wd.join(".env"), "TAG=2\n").unwrap();
        let wd = wd.to_string_lossy().to_string();
        projects::Project {
            name: "blog".into(),
            containers: 1,
            running: 1,
            config_files: vec![if portainer { "/data/compose/12/docker-compose.yml".into() } else { format!("{wd}/compose.yaml") }],
            working_dir: Some(if portainer { "/data/compose/12".into() } else { wd }),
            portainer,
            ..Default::default()
        }
    }

    /// Who an administrator is in the activity log.
    fn an_admin() -> Who {
        Who { user: Some(crate::auth::User { id: 1, name: "admin".into(), role: "admin".into(), token: None }), ip: None }
    }

    #[tokio::test]
    async fn a_project_s_files_are_read_where_its_labels_say_only_for_an_administrator_on_the_local_docker() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        app.db.call(|c| Ok(c.execute("INSERT INTO environments (id, name, kind, url, created_at) VALUES (2, 'vm-1', 'agent', '', 0)", [])?)).await.unwrap();
        let p = started_elsewhere(dir.path(), false);
        assert!(project_files(&app, an_admin().user.as_ref(), 1, &p).await.is_ok());
        let helpdesk = crate::auth::User { id: 9, name: "deckhand".into(), role: "user".into(), token: None };
        let e = project_files(&app, Some(&helpdesk), 1, &p).await.unwrap_err();
        assert!(e.contains("administrator"), "{e}");
        let e = project_files(&app, an_admin().user.as_ref(), 2, &p).await.unwrap_err();
        assert!(e.contains("local"), "labels from another host name no file on this one: {e}");

        // Labels that name Freightdock's own files: another stack's, or the data folder's.
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "vault", "compose": "services: {}\n", "env": "DB_PASSWORD=admins-only\n" })).await;
        let folder = dir.path().join(format!("stacks/{}", b["id"]));
        let bait = projects::Project { config_files: vec![folder.join("compose.yaml").display().to_string()], working_dir: Some(folder.display().to_string()), ..p.clone() };
        let e = project_files(&app, an_admin().user.as_ref(), 1, &bait).await.unwrap_err();
        assert!(e.contains("Freightdock's own"), "{e}");
        let _ = app.backups.data_dir.set(dir.path().join("elsewhere"));
        assert!(project_files(&app, an_admin().user.as_ref(), 1, &p).await.unwrap_err().contains("Freightdock's own"), "the data folder");
        let e = adopt_project(&app, &an_admin(), 1, &bait, None, None).await.unwrap_err();
        assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY, "adopting is held to it too: {}", e.message);
    }

    #[tokio::test]
    async fn a_project_started_elsewhere_is_adopted_from_its_own_files_merged_by_compose() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        signed_in(&app).await;
        let p = started_elsewhere(dir.path(), false);
        let id = adopt_project(&app, &an_admin(), 1, &p, None, None).await.unwrap();
        let folder = dir.path().join(format!("stacks/{id}"));
        assert_eq!(std::fs::read_to_string(folder.join("compose.yaml")).unwrap(), "name: merged\nservices: {}\n", "Compose made the one file");
        assert_eq!(std::fs::read_to_string(folder.join(".env")).unwrap(), "TAG=2\n", "its variables came along");
        let merged = calls(dir.path()).into_iter().find(|c| c.contains("--no-interpolate")).unwrap();
        assert!(merged.contains(&format!("--project-directory {}", p.working_dir.as_deref().unwrap())), "{merged}");
        let action: String = app.db.call(|c| Ok(c.query_row("SELECT action FROM audit ORDER BY id DESC LIMIT 1", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(action, "stack.adopt");
        let again = adopt_project(&app, &an_admin(), 1, &p, None, None).await.unwrap_err();
        assert_eq!(again.status, StatusCode::CONFLICT, "adopted once");
    }

    #[tokio::test]
    async fn a_project_whose_files_cannot_be_read_needs_its_file_pasted() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        signed_in(&app).await;
        let p = started_elsewhere(dir.path(), true);
        let e = adopt_project(&app, &an_admin(), 1, &p, None, None).await.unwrap_err();
        assert_eq!((e.status, e.field), (StatusCode::UNPROCESSABLE_ENTITY, Some("compose")));
        assert!(e.message.contains("Portainer's own volume") && e.message.ends_with("Paste its compose file to adopt it."), "{}", e.message);
        let id = adopt_project(&app, &an_admin(), 1, &p, Some("services:\n  app: {image: nginx}\n".into()), Some(String::new())).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join(format!("stacks/{id}/compose.yaml"))).unwrap(), "services:\n  app: {image: nginx}\n");
    }

    #[tokio::test]
    async fn a_git_stack_is_cloned_checked_and_deployed_from_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (url, _) = git::tests::upstream(dir.path());
        let form = |path: &str| json!({ "name": "from-git", "url": url, "reference": "main", "path": path, "user": "me", "token": "s3cret-token", "poll_minutes": 5, "env": "TAG=1\n" });
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", form("nowhere.yml")).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("path")), "{b}");
        assert_eq!(b["error"], "The repository has no nowhere.yml.");
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", json!({ "name": "x", "url": "ftp://example.com/r", "path": "a.yml" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("url")));

        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", form("deploy/compose.yaml")).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let id = b["id"].as_i64().unwrap();
        let s = load(&app, 1, id).await.unwrap();
        let g = s.git.clone().unwrap();
        assert!(g.has_token && g.token_sealed.as_deref().is_some_and(|t| t.starts_with("sealed1:") && !t.contains("s3cret")), "the token is sealed");
        assert_eq!(g.poll_s, 300);
        assert!(!serde_json::to_string(&s).unwrap().contains("sealed1:"), "the token never leaves the server");

        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/deploy"), json!({})).await;
        assert_eq!(lines.last().unwrap()["done"], true, "{lines:?}");
        let up = calls(dir.path()).pop().unwrap();
        let clone = dir.path().join(format!("stacks/{id}/repo/deploy"));
        assert!(up.contains(&format!("-f {}/compose.yaml --project-directory {}", clone.display(), clone.display())), "{up}");
        let s = load(&app, 1, id).await.unwrap();
        assert_eq!(s.git.unwrap().commit.as_deref().map(str::len), Some(40), "the commit deployed is kept");
    }

    #[tokio::test]
    async fn the_poller_deploys_a_git_stack_again_only_when_its_branch_moved() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (url, work) = git::tests::upstream(dir.path());
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", json!({ "name": "polled", "url": url, "path": "deploy/compose.yaml", "poll_minutes": 1 })).await;
        let id = b["id"].as_i64().unwrap();
        let ups = || calls(dir.path()).iter().filter(|c| c.contains(" up ")).count();
        let t = now();
        assert_eq!(poll_once(&app, t).await, vec![id], "never deployed: the poller deploys it");
        assert_eq!(ups(), 1);
        assert!(poll_once(&app, t + 10).await.is_empty(), "not due yet");
        assert!(poll_once(&app, t + 61).await.is_empty(), "due, but nothing new");
        git::tests::commit(&work, "services:\n  web: {image: nginx:2}\n");
        assert_eq!(poll_once(&app, t + 122).await, vec![id], "a new commit: deployed again");
        assert_eq!(ups(), 2);
        assert!(std::fs::read_to_string(dir.path().join(format!("stacks/{id}/repo/deploy/compose.yaml"))).unwrap().contains("nginx:2"));
        let detail: String = app.db.call(|c| Ok(c.query_row("SELECT detail FROM audit WHERE action = 'stack.deploy' ORDER BY id DESC LIMIT 1", [], |r| r.get(0))?)).await.unwrap();
        assert!(detail.starts_with("by the Git poller, for "), "{detail}");

        let (s, b) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}/git"), json!({ "url": url, "reference": "gone", "path": "deploy/compose.yaml" })).await;
        assert_eq!((s, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("The repository has no branch or tag \"gone\".")));
        let (s, _) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}/git"), json!({ "url": url, "reference": "main", "path": "deploy/compose.yaml", "poll_minutes": 0, "env": "A=1" })).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(poll_once(&app, t + 10_000).await.is_empty(), "polling off");
    }

    #[tokio::test]
    async fn a_stack_s_files_can_be_read_when_its_docker_does_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n", "env": "A=1\n" })).await;
        let (s, _, d) = call(&app, Request::get(format!("/api/envs/1/stacks/{}", b["id"])).header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::OK, "{d}");
        assert_eq!((d["compose"].as_str(), d["env"].as_str()), (Some("services: {}\n"), Some("A=1\n")));
        assert_eq!(d["state"], "unknown");
        assert!(d["docker_error"].as_str().is_some_and(|e| e.starts_with("Docker did not answer")), "{d}");
    }

    #[tokio::test]
    async fn removing_an_environment_takes_its_stacks_folders_and_a_new_stack_starts_in_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        app.db
            .call(|c| Ok(c.execute_batch("INSERT INTO environments (id, name, kind, url, created_at) VALUES (2, 'nas', 'tls', 'tcp://nas:2376', 0); INSERT INTO stacks (id, env_id, name, source, created_at, updated_at) VALUES (1, 2, 'vault', 'editor', 0, 0);")?))
            .await
            .unwrap();
        let old = dir.path().join("stacks/1");
        std::fs::create_dir_all(old.join("repo")).unwrap();
        std::fs::write(old.join("repo/private.env"), "API_KEY=left-behind\n").unwrap();
        std::fs::create_dir_all(dir.path().join("stacks/.docker-1")).unwrap();
        let (s, b) = send(&app, &cookie, &csrf, "DELETE", "/api/environments/2", json!({})).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert!(!old.exists() && !dir.path().join("stacks/.docker-1").exists(), "the removed environment's stack folders went with it");

        // A folder left from before (a crash, or an environment removed by an older Freightdock) is not handed on.
        std::fs::create_dir_all(old.join("repo")).unwrap();
        std::fs::write(old.join("repo/private.env"), "API_KEY=left-behind\n").unwrap();
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        assert_eq!((s, b["id"].as_i64()), (StatusCode::CREATED, Some(1)), "{b}");
        let mut left: Vec<String> = std::fs::read_dir(&old).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, [".env", "compose.yaml"], "a new stack's folder holds only what it wrote");
    }

    #[tokio::test]
    async fn a_saved_git_token_is_never_sent_to_a_new_address_s_host() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (url, _) = git::tests::upstream(dir.path());
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", json!({ "name": "private", "url": url, "path": "deploy/compose.yaml", "token": "the-admins-token" })).await;
        let id = b["id"].as_i64().unwrap();

        // Somebody else's host, which notes every connection made to it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let theirs = format!("http://{}/x.git", listener.local_addr().unwrap());
        let reached = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = reached.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut c, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = c.read(&mut buf).await.unwrap_or(0);
                seen.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
            }
        });
        let (s, b) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}/git"), json!({ "url": theirs, "path": "deploy/compose.yaml" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("token")), "{b}");
        assert!(reached.lock().unwrap().is_empty(), "nothing was sent to the new host: {:?}", reached.lock().unwrap());
        let g = load(&app, 1, id).await.unwrap().git.unwrap();
        assert_eq!((g.url.as_str(), g.has_token), (url.as_str(), true), "the stack is as it was");

        // Within the same origin the token stays; cleared, the new address takes none.
        let (s, b) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}/git"), json!({ "url": url, "path": "deploy/compose.yaml", "poll_minutes": 0 })).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert!(load(&app, 1, id).await.unwrap().git.unwrap().has_token);
        let (_, _) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}/git"), json!({ "url": theirs, "path": "deploy/compose.yaml", "clear_token": true })).await;
        assert!(reached.lock().unwrap().iter().all(|r| !r.to_ascii_lowercase().contains("authorization")), "{:?}", reached.lock().unwrap());
    }

    #[tokio::test]
    async fn a_git_stack_s_compose_file_never_leads_out_of_its_clone() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let outside = dir.path().join("outside.env");
        std::fs::write(&outside, "V26_SECRET=admins-only\n").unwrap();
        let (url, work) = git::tests::upstream(dir.path());
        let target = outside.clone();
        git::tests::push(&work, move |w| {
            std::fs::remove_file(w.join("deploy/compose.yaml")).unwrap();
            std::os::unix::fs::symlink(&target, w.join("deploy/compose.yaml")).unwrap();
            std::os::unix::fs::symlink(target.parent().unwrap(), w.join("linked")).unwrap();
        });
        let get = |id: i64| {
            let (app, cookie) = (app.clone(), cookie.clone());
            async move { call(&app, Request::get(format!("/api/envs/1/stacks/{id}")).header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.2 }
        };

        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", json!({ "name": "dirlink", "url": url, "path": "linked/outside.env" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("path")), "a linked folder is not followed: {b}");

        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks/git", json!({ "name": "filelink", "url": url, "path": "deploy/compose.yaml" })).await;
        let id = match b["id"].as_i64() {
            Some(id) => id,
            None => panic!("{s} {b}"),
        };
        let clone = dir.path().join(format!("stacks/{id}/repo"));
        assert!(!clone.join("deploy/compose.yaml").is_symlink(), "a link in the repository is checked out as a plain file");
        let d = get(id).await;
        assert!(!d.to_string().contains("admins-only"), "{d}");

        // A link put there later (a container writing in the folder) is not followed either.
        std::fs::remove_file(clone.join("deploy/compose.yaml")).unwrap();
        std::os::unix::fs::symlink(&outside, clone.join("deploy/compose.yaml")).unwrap();
        let d = get(id).await;
        assert!(!d.to_string().contains("admins-only"), "{d}");
        let s = load(&app, 1, id).await.unwrap();
        let e = compose_takes(&app, &s).await.unwrap_err();
        assert_eq!(e.field, Some("compose"), "{}", e.message);
    }

    #[tokio::test]
    async fn a_stack_may_make_compose_read_only_files_in_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        let (cookie, csrf) = signed_in(&app).await;
        let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "vault", "compose": "services:\n  db: {image: postgres}\n", "env": "DB_PASSWORD=admins-only\n" })).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let before = calls(dir.path()).len();
        for compose in ["services:\n  probe:\n    image: alpine\n    env_file: ../1/.env\n", "include:\n  - ../1/compose.yaml\n", "services:\n  probe:\n    build: ../..\n"] {
            let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "probe", "compose": compose })).await;
            assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("compose")), "{compose}: {b}");
        }
        assert_eq!(calls(dir.path()).len(), before, "Compose was never run over them");

        // An edit is held to it too, and so is every job, should the file have changed on disk since.
        let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "probe", "compose": "services:\n  probe: {image: alpine}\n" })).await;
        let id = b["id"].as_i64().unwrap();
        let (s, b) = send(&app, &cookie, &csrf, "PUT", &format!("/api/envs/1/stacks/{id}"), json!({ "compose": "services:\n  probe:\n    image: alpine\n    env_file: ../1/.env\n" })).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("compose")), "{b}");
        std::fs::write(dir.path().join(format!("stacks/{id}/compose.yaml")), "services:\n  probe:\n    image: alpine\n    env_file: ../1/.env\n").unwrap();
        let (s, _, b) = call(&app, Request::get(format!("/api/envs/1/stacks/{id}/environment")).header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        let (_, lines) = stream(&app, &cookie, &csrf, &format!("/api/envs/1/stacks/{id}/pull"), json!({})).await;
        assert!(lines.last().unwrap()["error"].as_str().is_some_and(|e| e.contains("outside the stack's own folder")), "{lines:?}");
    }

    #[tokio::test]
    async fn only_an_administrator_changes_a_stack() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with_compose(dir.path());
        signed_in(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, format!("freightdock_session={token}")).body(Body::empty()).unwrap()).await;
        let (s, _) = send(&app, &format!("freightdock_session={token}"), me["csrf"].as_str().unwrap(), "POST", "/api/envs/1/stacks", json!({ "name": "web", "compose": "services: {}\n" })).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
