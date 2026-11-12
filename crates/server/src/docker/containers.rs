//! Containers: listed, inspected, started and stopped, created, edited (which is recreated), renamed, removed.
//! Reads are bollard's own models as Docker answers them; a change is an administrator's (M4 brings roles) and is
//! written to the activity log ("Containers" in DECISIONS.md).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bollard::models::{ContainerConfig, ContainerCreateBody, ContainerInspectResponse, EndpointSettings, NetworkingConfig};
use bollard::query_parameters as q;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{api_error, endpoint, valid_name, valid_ref};
use crate::access::Access;
use crate::access::ownership::{self, Kind, Owners};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

fn check_ref(id: &str) -> ApiResult<()> {
    if valid_ref(id) { Ok(()) } else { Err(ApiError::bad("That is not a container's id or name.")) }
}

async fn log(app: &App, admin: &Allowed, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

/// What Freightdock keeps by a container's name, beside Docker: its ownership record, its webhook and its automatic-update
/// switch. They go with it, so a later container that takes the name is given none of them ("Access" 4, 21).
pub async fn forget(app: &App, env: i64, name: String) -> ApiResult<()> {
    ownership::save(&app.db, env, Kind::Container, name.clone(), None).await?;
    app.db
        .call(move |c| {
            c.execute("DELETE FROM webhooks WHERE env_id = ?1 AND kind = 'container' AND target = ?2", crate::db::rusqlite::params![env, name])?;
            c.execute("DELETE FROM auto_updates WHERE env_id = ?1 AND container = ?2", crate::db::rusqlite::params![env, name])?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// The same, moved with the container from name `was` to `now` (a rename, an edit under a new name); whatever was kept
/// by `now` before goes, as it would have with the container that had it.
pub async fn renamed(app: &App, env: i64, was: String, now: String) -> ApiResult<()> {
    if was == now {
        return Ok(());
    }
    ownership::rename(&app.db, env, was.clone(), now.clone()).await?;
    app.db
        .call(move |c| {
            use crate::db::rusqlite::params;
            c.execute("DELETE FROM webhooks WHERE env_id = ?1 AND kind = 'container' AND target = ?2", params![env, now])?;
            c.execute("UPDATE webhooks SET target = ?3 WHERE env_id = ?1 AND kind = 'container' AND target = ?2", params![env, was, now])?;
            c.execute("DELETE FROM auto_updates WHERE env_id = ?1 AND container = ?2", params![env, now])?;
            c.execute("UPDATE auto_updates SET container = ?3 WHERE env_id = ?1 AND container = ?2", params![env, was, now])?;
            Ok(())
        })
        .await?;
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// Stopped ones too (the default); `all=false` for running only.
    all: Option<bool>,
}

/// Every container the person can see ("Access" 4), each with whom it belongs to (`freightdock_access`).
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>, Query(lq): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let list = d.client().list_containers(Some(q::ListContainersOptions { all: lq.all.unwrap_or(true), ..Default::default() })).await.map_err(api_error)?;
    let owners = Owners::load(&app.db, env).await?;
    let list: Vec<Value> = list
        .into_iter()
        .filter_map(|c| {
            let mut v = json!(c);
            let (name, project) = ownership::container_keys(&v);
            let o = owners.container(&name, project.as_deref());
            access.sees(o).then(|| {
                v["freightdock_access"] = owners.show(o);
                v
            })
        })
        .collect();
    Ok(Json(json!(list)))
}

pub async fn inspect(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let c = d.client().inspect_container(&id, Some(q::InspectContainerOptions { size: true })).await.map_err(api_error)?;
    let mut v = json!(c);
    let owners = Owners::load(&app.db, env).await?;
    let (name, project) = ownership::container_keys(&v);
    v["freightdock_access"] = owners.show(owners.container(&name, project.as_deref()));
    Ok(Json(v))
}

/// `PUT .../containers/{id}/access`: `{level, team_id, user_id}` ("Access" 4).
pub async fn set_access(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(w): Json<ownership::Wanted>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    let o = w.owner(&admin.1)?;
    let d = endpoint(&app, env).await?;
    let name = shown_name(&d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.map_err(api_error)?);
    ownership::save(&app.db, env, Kind::Container, name.clone(), o.clone()).await?;
    log(&app, &admin, "container.access", name, Some(o.map_or("admins", |o| o.level.as_str()).to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn top(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let t = d.client().top_processes(&id, None::<q::TopOptions>).await.map_err(api_error)?;
    Ok(Json(json!(t)))
}

/// The name a container is shown by: Docker's, without its leading slash.
pub fn shown_name(c: &ContainerInspectResponse) -> String {
    c.name.as_deref().unwrap_or_default().trim_start_matches('/').to_string()
}

pub async fn action(State(app): State<App>, admin: Allowed, Path((env, id, action)): Path<(i64, String, String)>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let (r, what) = match action.as_str() {
        "start" => (c.start_container(&id, None::<q::StartContainerOptions>).await, "container.start"),
        "stop" => (c.stop_container(&id, None::<q::StopContainerOptions>).await, "container.stop"),
        "restart" => (c.restart_container(&id, None::<q::RestartContainerOptions>).await, "container.restart"),
        "kill" => (c.kill_container(&id, Some(q::KillContainerOptions { signal: "SIGKILL".into() })).await, "container.kill"),
        "pause" => (c.pause_container(&id).await, "container.pause"),
        "unpause" => (c.unpause_container(&id).await, "container.unpause"),
        _ => return Err(ApiError::not_found("action")),
    };
    // Starting what runs, or stopping what is stopped, is Docker's 304: done, as far as anybody asked.
    match r {
        Ok(()) => {}
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 304, .. }) => {}
        Err(e) => return Err(api_error(e)),
    }
    log(&app, &admin, what, id, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct RemoveQuery {
    force: Option<bool>,
    /// Its anonymous volumes too.
    volumes: Option<bool>,
}

pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Query(rq): Query<RemoveQuery>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let opts = q::RemoveContainerOptions { force: rq.force.unwrap_or(false), v: rq.volumes.unwrap_or(false), link: false };
    let name = d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.ok().map(|c| shown_name(&c));
    d.client().remove_container(&id, Some(opts)).await.map_err(api_error)?;
    if let Some(n) = name {
        forget(&app, env, n).await?;
    }
    log(&app, &admin, "container.remove", id, rq.force.unwrap_or(false).then(|| "forced".into())).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

pub async fn rename(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(r): Json<Rename>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    if !valid_name(&r.name) {
        return Err(ApiError::bad("A name starts with a letter or digit, then letters, digits, _ . or -.").field("name"));
    }
    let d = endpoint(&app, env).await?;
    let was = shown_name(&d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.map_err(api_error)?);
    d.client().rename_container(&id, q::RenameContainerOptions { name: r.name.clone() }).await.map_err(api_error)?;
    renamed(&app, env, was, r.name.clone()).await?;
    log(&app, &admin, "container.rename", id, Some(format!("to {}", r.name))).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What the browser sends to make a container: Docker's own create body, as Docker documents it, so every option there
/// is reachable; and what to do around it.
#[derive(Deserialize)]
pub struct Create {
    pub name: Option<String>,
    pub config: ContainerCreateBody,
    /// Pull the image first (the default when it is not here).
    #[serde(default)]
    pub pull: bool,
    /// Start it once made (the default).
    pub start: Option<bool>,
    /// Edit: the container this one takes the place of. It is stopped and set aside, and removed once the new one runs;
    /// if the new one cannot be made, it is put back as it was.
    pub replace: Option<String>,
    /// Who a new one belongs to ("Access" 4): private to its maker when left out. An edit keeps what it had.
    #[serde(default)]
    pub access: Option<ownership::Wanted>,
    /// The ownership worked out from `access` by the handler; `None` leaves the record as it is.
    #[serde(skip)]
    pub owner: Option<Option<ownership::Owner>>,
}

impl Create {
    /// A new container by `name` (or one Docker makes up), its image pulled first and started once made.
    pub fn new(name: Option<String>, config: ContainerCreateBody) -> Self {
        Create { name, config, pull: true, start: None, replace: None, access: None, owner: None }
    }
}

/// What the image gives every container on its own: what an edit or a recreate must not copy as if it were the
/// container's, or a new image's defaults would never reach it.
fn without_image_defaults(mut c: ContainerConfig, image: Option<&ContainerConfig>, id: &str) -> ContainerConfig {
    // A host name Docker made up is the id's first 12 characters: the new container gets its own.
    if c.hostname.as_deref().is_some_and(|h| !h.is_empty() && id.starts_with(h)) {
        c.hostname = None;
    }
    let Some(img) = image else { return c };
    if let (Some(env), Some(base)) = (c.env.as_mut(), img.env.as_ref()) {
        env.retain(|e| !base.contains(e));
    }
    if c.cmd.is_some() && c.cmd == img.cmd {
        c.cmd = None;
    }
    if c.entrypoint.is_some() && c.entrypoint == img.entrypoint {
        c.entrypoint = None;
    }
    if c.working_dir.is_some() && c.working_dir == img.working_dir {
        c.working_dir = None;
    }
    if c.user.is_some() && c.user == img.user {
        c.user = None;
    }
    if c.healthcheck.is_some() && c.healthcheck == img.healthcheck {
        c.healthcheck = None;
    }
    if c.stop_signal.is_some() && c.stop_signal == img.stop_signal {
        c.stop_signal = None;
    }
    if let (Some(labels), Some(base)) = (c.labels.as_mut(), img.labels.as_ref()) {
        labels.retain(|k, v| base.get(k) != Some(v));
    }
    if let (Some(ports), Some(base)) = (c.exposed_ports.as_mut(), img.exposed_ports.as_ref()) {
        ports.retain(|k, _| !base.contains_key(k));
    }
    if let (Some(vols), Some(base)) = (c.volumes.as_mut(), img.volumes.as_ref()) {
        vols.retain(|k, _| !base.contains_key(k));
    }
    c
}

/// The create body that makes this container again: its own settings (not its image's defaults), its host settings, and
/// its networks with their aliases and fixed addresses, without what Docker filled in as it ran.
pub fn create_body(c: &ContainerInspectResponse, image: Option<&ContainerConfig>) -> ContainerCreateBody {
    let id = c.id.as_deref().unwrap_or_default();
    let cfg = without_image_defaults(c.config.clone().unwrap_or_default(), image, id);
    let short = &id[..id.len().min(12)];
    let networks = c.network_settings.as_ref().and_then(|n| n.networks.clone()).map(|nets| {
        nets.into_iter()
            .map(|(name, e)| {
                let aliases = e.aliases.map(|a| a.into_iter().filter(|x| x != short && !id.starts_with(x.as_str())).collect::<Vec<_>>());
                (name, EndpointSettings { ipam_config: e.ipam_config, links: e.links, aliases: aliases.filter(|a| !a.is_empty()), driver_opts: e.driver_opts, gw_priority: e.gw_priority, ..Default::default() })
            })
            .collect::<HashMap<_, _>>()
    });
    ContainerCreateBody {
        hostname: cfg.hostname,
        domainname: cfg.domainname,
        user: cfg.user,
        attach_stdin: cfg.attach_stdin,
        attach_stdout: cfg.attach_stdout,
        attach_stderr: cfg.attach_stderr,
        exposed_ports: cfg.exposed_ports.filter(|p| !p.is_empty()),
        tty: cfg.tty,
        open_stdin: cfg.open_stdin,
        stdin_once: cfg.stdin_once,
        env: cfg.env,
        cmd: cfg.cmd,
        healthcheck: cfg.healthcheck,
        args_escaped: cfg.args_escaped,
        image: cfg.image,
        volumes: cfg.volumes.filter(|v| !v.is_empty()),
        working_dir: cfg.working_dir,
        entrypoint: cfg.entrypoint,
        network_disabled: cfg.network_disabled,
        mac_address: None,
        on_build: None,
        labels: cfg.labels,
        stop_signal: cfg.stop_signal,
        stop_timeout: cfg.stop_timeout,
        shell: cfg.shell,
        host_config: c.host_config.clone(),
        networking_config: networks.map(|n| NetworkingConfig { endpoints_config: Some(n) }),
    }
}

/// Pulls `image` and waits for it to be here, signed in to its registry when one is saved for its host.
pub async fn pull(app: &App, d: &super::Docker, image: &str) -> ApiResult<()> {
    let creds = crate::registries::auth_for_image(app, image).await?.map(|a| a.credentials);
    pull_with(d, image, creds).await
}

/// Pulls `image` with these credentials (none for a public image Freightdock itself needs, as the volume browser's helper).
pub async fn pull_with(d: &super::Docker, image: &str, creds: Option<bollard::auth::DockerCredentials>) -> ApiResult<()> {
    use futures_util::StreamExt;
    let (from_image, tag) = split_tag(image);
    let opts = q::CreateImageOptions { from_image: Some(from_image.to_string()), tag: Some(tag.to_string()), ..Default::default() };
    let mut s = d.client().create_image(Some(opts), None, creds);
    while let Some(step) = s.next().await {
        let step = step.map_err(api_error)?;
        if let Some(e) = step.error_detail.and_then(|e| e.message) {
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, e));
        }
    }
    Ok(())
}

/// `nginx` → (`nginx`, `latest`); `ghcr.io:443/a/b:1.2` → (`ghcr.io:443/a/b`, `1.2`); a digest stays whole.
pub fn split_tag(image: &str) -> (&str, &str) {
    if image.contains('@') {
        return (image, "");
    }
    let last = image.rfind('/').map_or(0, |i| i + 1);
    match image[last..].rfind(':') {
        Some(i) => (&image[..last + i], &image[last + i + 1..]),
        None => (image, "latest"),
    }
}

pub async fn image_config(d: &super::Docker, image: &str) -> Option<ContainerConfig> {
    let i = d.client().inspect_image(image).await.ok()?;
    i.config.map(|c| ContainerConfig {
        env: c.env, cmd: c.cmd, entrypoint: c.entrypoint, working_dir: c.working_dir, user: c.user, healthcheck: c.healthcheck,
        stop_signal: c.stop_signal, labels: c.labels, exposed_ports: c.exposed_ports, volumes: c.volumes, ..Default::default()
    })
}

/// `GET .../containers/{id}/config`: the create body that would make this container again, for the editor.
pub async fn config(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let c = d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
    let img = image_config(&d, c.image.as_deref().unwrap_or_default()).await;
    Ok(Json(json!({ "name": shown_name(&c), "config": create_body(&c, img.as_ref()) })))
}

pub async fn create(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(req): Json<Create>) -> ApiResult<(StatusCode, Json<Value>)> {
    let req = owned(&app, &admin.1, env, req).await?;
    create_as(&app, &audit::Who::from(&admin), None, env, req).await
}

/// A create held to the person's access ("Access" 4): an edit (`replace`) is managing the container it replaces, which
/// they must be allowed; a new one gets the ownership they asked for (private to them when they did not say), and
/// its maker's name as the `freightdock.owner` label.
///
/// Either way it is held to the environment's security settings ("Access" 12): an edit only to what it adds to the
/// container it replaces, so what an administrator set stays.
pub async fn owned(app: &App, access: &Access, env: i64, mut req: Create) -> ApiResult<Create> {
    use crate::access::security;
    let rules = security::applies(&app.db, &security::Judge::of(access), env).await?;
    let spec = || security::of_container(&serde_json::to_value(&req.config).unwrap_or_default());
    if let Some(prev) = &req.replace {
        check_ref(prev)?;
        debug_assert_eq!(access.env, env);
        crate::access::routes::may_container(app, access, crate::access::Action::Manage, prev).await?;
        if let Some(r) = &rules {
            let d = endpoint(app, env).await?;
            let was = d.client().inspect_container(prev, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
            let old = security::of_container(&serde_json::to_value(create_body(&was, None)).unwrap_or_default());
            security::check(r, &[spec()], &[old], None)?;
        }
        return Ok(req);
    }
    if let Some(r) = &rules {
        security::check(r, &[spec()], &[], None)?;
    }
    req.owner = Some(req.access.clone().unwrap_or_default().owner(access)?);
    req.config.labels.get_or_insert_with(HashMap::new).insert(ownership::LABEL.to_string(), access.user.name.clone());
    Ok(req)
}

/// Makes (or, with `replace`, edits) a container for `who` and writes it to the activity log; `via` says there what
/// asked when nobody signed in did (a webhook).
pub async fn create_as(app: &App, who: &audit::Who, via: Option<&str>, env: i64, mut req: Create) -> ApiResult<(StatusCode, Json<Value>)> {
    let d = endpoint(app, env).await?;
    let owner = req.owner.take();
    let made = make(app, &d, req, None).await?;
    // Its ownership is kept by its name: an edit under a new name takes it along; a new one gets what was worked out.
    if let Some(was) = &made.was {
        renamed(app, env, was.clone(), made.name.clone()).await?;
    } else if let Some(o) = owner {
        ownership::save(&app.db, env, Kind::Container, made.name.clone(), o).await?;
    }
    let action = if made.replaced.is_some() { "container.edit" } else { "container.create" };
    let detail = Some(match via {
        Some(v) => format!("image {}; {v}", made.image),
        None => format!("image {}", made.image),
    });
    audit::record_by(&app.db, who, Entry { action, target: Some(made.target.clone()), detail }).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": made.id, "warnings": made.warnings }))))
}

/// What `make` made.
pub struct Made {
    pub id: String,
    pub warnings: Vec<String>,
    /// The id of the container it took the place of.
    pub replaced: Option<String>,
    pub image: String,
    /// Its name, or its id when it has none.
    pub target: String,
    /// Its name as Docker shows it (one Docker made up, when it was given none).
    pub name: String,
    /// The name of the container it took the place of.
    pub was: Option<String>,
}

/// Whether a container that was started is still up after a while: running, not restarting, and not unhealthy.
pub fn stayed_up(c: &ContainerInspectResponse) -> Result<(), String> {
    let st = c.state.as_ref();
    let running = st.and_then(|s| s.running).unwrap_or(false);
    let restarting = st.and_then(|s| s.restarting).unwrap_or(false);
    let unhealthy = st.and_then(|s| s.health.as_ref()).and_then(|h| h.status) == Some(bollard::models::HealthStatusEnum::UNHEALTHY);
    match (running, restarting, unhealthy) {
        (true, false, false) => Ok(()),
        (_, _, true) => Err("its health check failed".into()),
        (_, true, _) => Err("it keeps restarting".into()),
        _ => Err(format!("it stopped (exit code {})", st.and_then(|s| s.exit_code).unwrap_or(0))),
    }
}

/// Makes a container from Docker's create body, pulling its image first when asked or when it is not here (signed in
/// to a saved registry, as `pull`); with `replace`, in another's place ("Containers" 3). With `settle`, the new one must
/// still be up that long after it started, or it goes and the old one comes back, as when it could not start at all.
///
/// It runs as a task of its own, so it ends even when whoever asked stops waiting (a page closed, a webhook's caller
/// that hung up, whose handler hyper then drops): stopped halfway, the old container would stay stopped and set aside.
pub async fn make(app: &App, d: &super::Docker, req: Create, settle: Option<std::time::Duration>) -> ApiResult<Made> {
    let (app, d) = (app.clone(), d.clone());
    tokio::spawn(async move { make_now(&app, &d, req, settle).await })
        .await
        .map_err(|e| ApiError::from(anyhow::anyhow!("making the container failed: {e}")))?
}

async fn make_now(app: &App, d: &super::Docker, req: Create, settle: Option<std::time::Duration>) -> ApiResult<Made> {
    if let Some(n) = &req.name
        && !n.is_empty() && !valid_name(n) {
            return Err(ApiError::bad("A name starts with a letter or digit, then letters, digits, _ . or -.").field("name"));
        }
    let image = req.config.image.clone().filter(|i| !i.trim().is_empty()).ok_or_else(|| ApiError::bad("Say which image to run.").field("image"))?;
    if req.pull || d.client().inspect_image(&image).await.is_err() {
        pull(app, d, &image).await?;
    }
    let name = req.name.clone().filter(|n| !n.is_empty());
    let c = d.client();

    // Editing: the old one steps aside under another name, stopped, until the new one runs.
    let mut old: Option<(String, String, bool)> = None;
    if let Some(prev) = &req.replace {
        check_ref(prev)?;
        let o = c.inspect_container(prev, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
        let running = o.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        let was = shown_name(&o);
        let aside = format!("{was}-replaced-{}", crate::db::now());
        if running {
            c.stop_container(prev, None::<q::StopContainerOptions>).await.map_err(api_error)?;
        }
        c.rename_container(prev, q::RenameContainerOptions { name: aside.clone() }).await.map_err(api_error)?;
        old = Some((o.id.unwrap_or_default(), was, running));
    }
    let put_back = |old: Option<(String, String, bool)>| async move {
        if let Some((id, was, running)) = old {
            let _ = c.rename_container(&id, q::RenameContainerOptions { name: was }).await;
            if running {
                let _ = c.start_container(&id, None::<q::StartContainerOptions>).await;
            }
        }
    };
    let remove_new = |id: String| async move {
        let _ = c.remove_container(&id, Some(q::RemoveContainerOptions { force: true, v: false, link: false })).await;
    };

    let made = match c.create_container(Some(q::CreateContainerOptions { name: name.clone(), ..Default::default() }), req.config).await {
        Ok(m) => m,
        Err(e) => {
            put_back(old).await;
            return Err(api_error(e));
        }
    };
    if req.start.unwrap_or(true) {
        if let Err(e) = c.start_container(&made.id, None::<q::StartContainerOptions>).await {
            // It could not run: the new one goes, the old one comes back, and the reason is Docker's.
            remove_new(made.id.clone()).await;
            put_back(old).await;
            return Err(api_error(e));
        }
        if let Some(wait) = settle {
            tokio::time::sleep(wait).await;
            let up = match c.inspect_container(&made.id, None::<q::InspectContainerOptions>).await {
                Ok(n) => stayed_up(&n),
                Err(e) => Err(api_error(e).message),
            };
            if let Err(why) = up {
                remove_new(made.id.clone()).await;
                put_back(old).await;
                return Err(ApiError::new(StatusCode::CONFLICT, format!("The new container did not stay up ({why}), so the old one is back.")));
            }
        }
    }
    let replaced = old.as_ref().map(|(id, ..)| id.clone());
    if let Some(prev) = &replaced {
        let _ = c.remove_container(prev, Some(q::RemoveContainerOptions { force: true, v: false, link: false })).await;
    }
    // Its name as Docker shows it, which ownership is kept by ("Access" 4).
    let shown = match &name {
        Some(n) => n.clone(),
        None => c.inspect_container(&made.id, None::<q::InspectContainerOptions>).await.map(|i| shown_name(&i)).unwrap_or_else(|_| made.id.clone()),
    };
    let was = old.map(|(_, was, _)| was);
    let target = name.unwrap_or_else(|| made.id.clone());
    Ok(Made { id: made.id, warnings: made.warnings, replaced, image, target, name: shown, was })
}

#[derive(Deserialize, Default)]
pub struct Recreate {
    /// Pull the image again first, for a newer one under the same tag.
    #[serde(default)]
    pull: bool,
}

/// Recreate: the same container again, from its image as it is now (pulled anew, when asked).
pub async fn recreate(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(r): Json<Recreate>) -> ApiResult<(StatusCode, Json<Value>)> {
    recreate_as(&app, &audit::Who::from(&admin), None, env, &id, r.pull).await
}

/// Recreates container `id` for `who` (see `create_as`).
pub async fn recreate_as(app: &App, who: &audit::Who, via: Option<&str>, env: i64, id: &str, pull: bool) -> ApiResult<(StatusCode, Json<Value>)> {
    check_ref(id)?;
    let d = endpoint(app, env).await?;
    let c = d.client().inspect_container(id, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
    let img = image_config(&d, c.image.as_deref().unwrap_or_default()).await;
    let body = create_body(&c, img.as_ref());
    let req = Create { name: Some(shown_name(&c)), config: body, pull, start: Some(c.state.as_ref().and_then(|s| s.running).unwrap_or(true)), replace: Some(id.to_string()), access: None, owner: None };
    create_as(app, who, via, env, req).await
}

pub async fn prune(State(app): State<App>, admin: Allowed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    // Docker answers the ids it removed; what is kept by name goes with them.
    let before = d.client().list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })).await.map_err(api_error)?;
    let r = d.client().prune_containers(None::<q::PruneContainersOptions>).await.map_err(api_error)?;
    for id in r.containers_deleted.iter().flatten() {
        let name = before.iter().find(|c| c.id.as_deref() == Some(id.as_str())).and_then(|c| c.names.as_ref()?.first().map(|n| n.trim_start_matches('/').to_string()));
        if let Some(n) = name {
            forget(&app, env, n).await?;
        }
    }
    let n = r.containers_deleted.as_ref().map_or(0, Vec::len);
    log(&app, &admin, "container.prune", format!("{n} containers"), r.space_reclaimed.map(|b| format!("{b} bytes freed"))).await;
    Ok(Json(json!(r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerState, HostConfig, NetworkSettings};

    fn image() -> ContainerConfig {
        ContainerConfig {
            env: Some(vec!["PATH=/usr/bin".into(), "NGINX_VERSION=1.25".into()]),
            cmd: Some(vec!["nginx".into(), "-g".into(), "daemon off;".into()]),
            labels: Some(HashMap::from([("maintainer".into(), "nginx".into())])),
            exposed_ports: Some(HashMap::from([("80/tcp".into(), HashMap::new())])),
            ..Default::default()
        }
    }

    fn running() -> ContainerInspectResponse {
        let id = "3f2a9c1b7d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4";
        ContainerInspectResponse {
            id: Some(id.into()),
            name: Some("/web".into()),
            config: Some(ContainerConfig {
                hostname: Some(id[..12].into()),
                image: Some("nginx:latest".into()),
                env: Some(vec!["PATH=/usr/bin".into(), "NGINX_VERSION=1.25".into(), "MODE=prod".into()]),
                cmd: Some(vec!["nginx".into(), "-g".into(), "daemon off;".into()]),
                labels: Some(HashMap::from([("maintainer".into(), "nginx".into()), ("app".into(), "web".into())])),
                exposed_ports: Some(HashMap::from([("80/tcp".into(), HashMap::new()), ("8080/tcp".into(), HashMap::new())])),
                ..Default::default()
            }),
            host_config: Some(HostConfig { memory: Some(256 << 20), ..Default::default() }),
            state: Some(ContainerState { running: Some(true), ..Default::default() }),
            network_settings: Some(NetworkSettings {
                networks: Some(HashMap::from([(
                    "front".into(),
                    EndpointSettings { aliases: Some(vec!["web".into(), id[..12].into()]), ip_address: Some("172.20.0.5".into()), endpoint_id: Some("e".into()), network_id: Some("n".into()), ..Default::default() },
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_recreated_container_keeps_its_own_settings_and_not_its_image_s() {
        let b = create_body(&running(), Some(&image()));
        assert_eq!(b.image.as_deref(), Some("nginx:latest"), "by its reference, so a pull reaches it");
        assert_eq!(b.env, Some(vec!["MODE=prod".to_string()]));
        assert_eq!(b.cmd, None, "the image's command comes from the image");
        assert_eq!(b.labels, Some(HashMap::from([("app".to_string(), "web".to_string())])));
        assert_eq!(b.exposed_ports.unwrap().keys().collect::<Vec<_>>(), vec!["8080/tcp"]);
        assert_eq!(b.hostname, None, "a host name Docker made up is not kept");
        assert_eq!(b.host_config.unwrap().memory, Some(256 << 20));
    }

    #[test]
    fn a_recreated_container_keeps_its_networks_and_aliases_but_not_what_docker_filled_in() {
        let b = create_body(&running(), Some(&image()));
        let e = &b.networking_config.unwrap().endpoints_config.unwrap()["front"];
        assert_eq!(e.aliases, Some(vec!["web".to_string()]), "the old id is not an alias of the new one");
        assert_eq!(e.ip_address, None);
        assert_eq!(e.endpoint_id, None);
        assert_eq!(e.network_id, None);
    }

    #[test]
    fn without_the_image_nothing_is_taken_away_but_the_made_up_host_name() {
        let b = create_body(&running(), None);
        assert_eq!(b.env.unwrap().len(), 3);
        assert!(b.cmd.is_some());
        assert_eq!(b.hostname, None);
    }

    #[test]
    fn a_container_stayed_up_when_it_runs_and_is_not_unhealthy() {
        use bollard::models::{ContainerState, Health, HealthStatusEnum};
        let with = |s: ContainerState| ContainerInspectResponse { state: Some(s), ..Default::default() };
        assert_eq!(stayed_up(&with(ContainerState { running: Some(true), ..Default::default() })), Ok(()));
        let starting = ContainerState { running: Some(true), health: Some(Health { status: Some(HealthStatusEnum::STARTING), ..Default::default() }), ..Default::default() };
        assert_eq!(stayed_up(&with(starting)), Ok(()), "a health check still starting is no failure yet");
        let sick = ContainerState { running: Some(true), health: Some(Health { status: Some(HealthStatusEnum::UNHEALTHY), ..Default::default() }), ..Default::default() };
        assert_eq!(stayed_up(&with(sick)), Err("its health check failed".into()));
        assert_eq!(stayed_up(&with(ContainerState { running: Some(true), restarting: Some(true), ..Default::default() })), Err("it keeps restarting".into()));
        assert_eq!(stayed_up(&with(ContainerState { running: Some(false), exit_code: Some(2), ..Default::default() })), Err("it stopped (exit code 2)".into()));
    }

    #[test]
    fn an_image_reference_splits_into_name_and_tag() {
        assert_eq!(split_tag("nginx"), ("nginx", "latest"));
        assert_eq!(split_tag("nginx:1.25"), ("nginx", "1.25"));
        assert_eq!(split_tag("registry:5000/team/app"), ("registry:5000/team/app", "latest"));
        assert_eq!(split_tag("ghcr.io/a/b:v2"), ("ghcr.io/a/b", "v2"));
        assert_eq!(split_tag("alpine@sha256:abc"), ("alpine@sha256:abc", ""));
    }
}
