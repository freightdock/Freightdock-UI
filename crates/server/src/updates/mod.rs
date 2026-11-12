//! Image updates: is there a newer image under a container's tag? Every quarter of an hour (and when an administrator
//! asks) a pass goes over every environment that answers: each container's and each tagged image's reference is
//! compared with what its registry says the tag is now, the registry asked at most every `check_hours` per reference
//! (`image_checks` keeps its answer), and the verdicts are written to `update_status` for the badges. A container that
//! opted in (the `freightdock.auto-update=true` label, or the switch on its page) and is behind is updated: pulled and made
//! again with the same settings, and put back if the new one does not stay up ("Updates" in DECISIONS.md).

pub mod registry;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, Router, routing::get, routing::post, routing::put};
use bollard::query_parameters as q;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::docker::{api_error, containers};
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use registry::{Platform, Reference, Remote, Verdict};

const SETTINGS_KEY: &str = "updates";
/// The label a container opts in to automatic updates with.
pub const LABEL: &str = "freightdock.auto-update";
/// How long an updated container must stay up before the old one goes.
pub const SETTLE: Duration = Duration::from_secs(10);
/// How often the verdicts are worked out again from what is known (the registries are asked far less often).
pub const PASS_EVERY_S: i64 = 15 * 60;
/// Even when asked to check now, a reference asked about within this long is not asked again.
const FORCED_AFTER_S: i64 = 60;

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/settings/updates", get(get_settings).put(put_settings))
        .route("/api/updates/check", post(check_now))
        .route("/api/envs/{env}/updates", get(statuses))
        .route("/api/envs/{env}/containers/{id}/update", post(update_one))
        .route("/api/envs/{env}/containers/{id}/auto-update", put(set_auto))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Hours between two questions to a registry about the same reference, 1 to 168.
    pub check_hours: u64,
    /// Update the containers that opted in, when they are behind.
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, check_hours: 6, auto_update: true }
    }
}

pub async fn settings(app: &App) -> Settings {
    app.db.setting(SETTINGS_KEY).await.ok().flatten().unwrap_or_default()
}

async fn get_settings(State(app): State<App>, _: Admin) -> Json<Settings> {
    Json(settings(&app).await)
}

async fn put_settings(State(app): State<App>, admin: Admin, Json(s): Json<Settings>) -> ApiResult<StatusCode> {
    if !(1..=168).contains(&s.check_hours) {
        return Err(ApiError::bad("Ask a registry every 1 to 168 hours.").field("check_hours"));
    }
    app.db.put_setting(SETTINGS_KEY, &s).await?;
    let detail = if s.enabled { format!("every {} h, automatic updates {}", s.check_hours, if s.auto_update { "on" } else { "off" }) } else { "off".into() };
    log(&app, Some(&admin.0), "settings.updates", None, Some(detail)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn log(app: &App, admin: Option<&crate::api::Authed>, action: &'static str, target: Option<String>, detail: Option<String>) {
    let (user, ip) = admin.map_or((None, None), |a| (Some(&a.session.user), a.ip.clone()));
    audit::record(&app.db, user, ip, Entry { action, target, detail }).await;
}

/// Whether a label value opts in: `true`, `1`, `yes`, `on`.
pub fn opts_in(labels: Option<&HashMap<String, String>>) -> bool {
    labels.and_then(|l| l.get(LABEL)).is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
}

/// What is known about a reference's tag.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Known {
    pub digest: Option<String>,
    pub index: bool,
    /// The index's platforms, once fetched for this digest.
    pub platforms: Option<Vec<(Platform, String)>>,
    pub checked_at: i64,
    pub error: Option<String>,
}

/// Whether what was known at `checked_at` may still be used at `now`.
pub fn fresh(checked_at: i64, now: i64, max_age_s: i64) -> bool {
    now - checked_at < max_age_s
}

async fn known(app: &App, key: String) -> anyhow::Result<Option<Known>> {
    app.db
        .call(move |c| {
            Ok(c.query_row("SELECT digest, is_index, platforms, checked_at, error FROM image_checks WHERE reference = ?1", [key], |r| {
                let platforms: Option<String> = r.get(2)?;
                let platforms = platforms.and_then(|p| serde_json::from_str::<Vec<(String, String, Option<String>, String)>>(&p).ok()).map(|v| {
                    v.into_iter().map(|(os, architecture, variant, d)| (Platform { os, architecture, variant }, d)).collect()
                });
                Ok(Known { digest: r.get(0)?, index: r.get(1)?, platforms, checked_at: r.get(3)?, error: r.get(4)? })
            })
            .optional()?)
        })
        .await
}

async fn keep(app: &App, key: String, k: Known) -> anyhow::Result<()> {
    let platforms = k.platforms.map(|p| serde_json::to_string(&p.into_iter().map(|(p, d)| (p.os, p.architecture, p.variant, d)).collect::<Vec<_>>())).transpose()?;
    app.db
        .call(move |c| {
            c.execute(
                "INSERT INTO image_checks (reference, digest, is_index, platforms, checked_at, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(reference) DO UPDATE SET digest = excluded.digest, is_index = excluded.is_index, platforms = excluded.platforms,
                   checked_at = excluded.checked_at, error = excluded.error",
                params![key, k.digest, k.index, platforms, k.checked_at, k.error],
            )?;
            Ok(())
        })
        .await
}

/// What the registry says `r`'s tag is: what was kept, while it is younger than `max_age_s`; else asked again (a
/// failed question keeps the digest known before, with the error beside it). `asked` counts the questions.
pub async fn remote(app: &App, client: &registry::Client, r: &Reference, max_age_s: i64, asked: &mut usize) -> Known {
    let t = now();
    let before = known(app, r.key()).await.ok().flatten();
    if let Some(k) = &before
        && fresh(k.checked_at, t, max_age_s) {
            return k.clone();
        }
    *asked += 1;
    let creds = registry::credentials(app, &r.registry).await;
    let k = match client.head(r, creds.as_ref()).await {
        Ok(Remote { digest, index }) => {
            // The platforms are kept while the index is the same one.
            let platforms = before.as_ref().filter(|b| b.digest.as_deref() == Some(&digest)).and_then(|b| b.platforms.clone());
            Known { digest: Some(digest), index, platforms, checked_at: t, error: None }
        }
        Err(e) => Known { error: Some(e), checked_at: t, ..before.clone().unwrap_or_default() },
    };
    if let Err(e) = keep(app, r.key(), k.clone()).await {
        tracing::warn!("could not keep what {} said: {e:#}", r.key());
    }
    k
}

/// One verdict, as the badges read it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// `current`, `outdated`, `unknown` (no registry digest here to compare: built or loaded here), `pinned` (by
    /// digest), `error` (the registry did not say).
    pub status: &'static str,
    pub detail: Option<String>,
    /// The digest the registry says the tag is now.
    pub remote: Option<String>,
}

/// How an image here stands: `local` its digests from the reference's repository, `platform` its os, architecture and
/// variant (asked for only when an index must be looked into).
pub fn assess(reference: &str, local: &[String], k: &Known, platform: Option<(&str, &str, Option<&str>)>) -> Result<Status, ()> {
    if local.is_empty() {
        return Ok(Status { status: "unknown", detail: Some("No registry digest here to compare: it was built or loaded here, not pulled.".into()), remote: k.digest.clone() });
    }
    let Some(digest) = &k.digest else {
        return Ok(Status { status: "error", detail: k.error.clone().or_else(|| Some(format!("{reference} has not been checked yet."))), remote: None });
    };
    let remote = Remote { digest: digest.clone(), index: k.index };
    let pd = k.platforms.as_ref().map(|list| platform.and_then(|(os, arch, v)| registry::platform_digest(list, os, arch, v)));
    let detail = k.error.as_ref().map(|e| format!("As of the last answer; asking again failed: {e}"));
    match registry::verdict(local, &remote, pd) {
        Verdict::Current => Ok(Status { status: "current", detail, remote: Some(digest.clone()) }),
        Verdict::Outdated => Ok(Status { status: "outdated", detail: detail.or_else(|| Some(format!("{reference} is newer in its registry."))), remote: Some(digest.clone()) }),
        Verdict::NeedPlatforms => Err(()),
    }
}

/// A container as a pass sees it.
struct Seen {
    id: String,
    name: String,
    reference: String,
    image: String,
    labelled: bool,
}

#[derive(Clone)]
struct Row {
    kind: &'static str,
    id: String,
    name: String,
    reference: String,
    status: Status,
    labelled: bool,
}

/// What a pass did.
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub environments: usize,
    /// Questions asked of registries (the rest came from what was kept).
    pub asked: usize,
    pub outdated: usize,
    pub updated: Vec<String>,
    pub failed: Vec<String>,
}

/// One pass at a time: the schedule's and one asked for must not update the same container twice.
static PASS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Works out every verdict on every environment; with `apply`, updates the containers that opted in. `max_age_s`: how
/// old an answer from a registry may be.
pub async fn pass(app: &App, max_age_s: i64, apply: bool) -> Summary {
    let _one = PASS.lock().await;
    let client = registry::Client::default();
    let mut sum = Summary::default();
    let envs: Vec<(i64, String)> = app.db.call(|c| Ok(c.prepare("SELECT id, name FROM environments ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap_or_default();
    // A reference asked about once is the same answer on every environment.
    let mut answers: HashMap<String, Known> = HashMap::new();
    for (env, env_name) in envs {
        let Ok(d) = crate::docker::endpoint(app, env).await else { continue };
        let rows = match tokio::time::timeout(Duration::from_secs(300), survey(app, &d, &client, max_age_s, &mut answers, &mut sum.asked)).await {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                tracing::debug!("no update check on {env_name}: {e}");
                continue;
            }
            Err(_) => continue,
        };
        sum.environments += 1;
        let before = write(app, env, &rows).await.unwrap_or_default();
        let opted = opted(app, env).await;
        for r in rows.iter().filter(|r| r.kind == "container" && r.status.status == "outdated") {
            sum.outdated += 1;
            let auto = r.labelled || opted.contains(&r.name);
            if before.get(&r.name).is_none_or(|(s, remote)| s != "outdated" || remote.as_ref() != r.status.remote.as_ref()) {
                let message = if auto && apply { "It opted in to automatic updates, so Freightdock updates it now." } else { "Update it from its page in Freightdock." };
                crate::notify::notify(app, crate::notify::Event {
                    kind: "update.available", title: format!("A newer {} is out for {} on {env_name}", r.reference, r.name), message: message.into(),
                    env: Some((env, env_name.clone())), target: Some(r.name.clone()),
                });
            }
            if auto && apply {
                match update(app, env, &d, &r.id).await {
                    Ok(Some(new)) => {
                        log(app, None, "container.autoupdate", Some(r.name.clone()), Some(format!("{} to {}", r.reference, short(r.status.remote.as_deref())))).await;
                        mark_current(app, env, &r.id, &new).await;
                        sum.updated.push(r.name.clone());
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log(app, None, "container.autoupdate", Some(r.name.clone()), Some(format!("failed: {}", e.message))).await;
                        sum.failed.push(format!("{}: {}", r.name, e.message));
                    }
                }
            }
        }
    }
    sum
}

fn short(d: Option<&str>) -> String {
    d.map(|d| d.trim_start_matches("sha256:").chars().take(12).collect()).unwrap_or_else(|| "?".into())
}

/// The verdicts of one environment's containers and tagged images.
async fn survey(app: &App, d: &crate::docker::Docker, client: &registry::Client, max_age_s: i64, answers: &mut HashMap<String, Known>, asked: &mut usize) -> Result<Vec<Row>, bollard::errors::Error> {
    let c = d.client();
    let (list, images) = tokio::try_join!(
        c.list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })),
        c.list_images(Some(q::ListImagesOptions { all: false, digests: true, ..Default::default() })),
    )?;
    // The reference a container was made from is in its inspect: the list shows the image's id once the tag moved on.
    let seen: Vec<Seen> = futures_util::stream::iter(list.into_iter().filter_map(|s| s.id))
        .map(|id| async move {
            let i = c.inspect_container(&id, None::<q::InspectContainerOptions>).await.ok()?;
            let cfg = i.config.as_ref();
            Some(Seen {
                name: containers::shown_name(&i),
                reference: cfg.and_then(|c| c.image.clone()).unwrap_or_default(),
                image: i.image.clone().unwrap_or_default(),
                labelled: opts_in(cfg.and_then(|c| c.labels.as_ref())),
                id,
            })
        })
        .buffer_unordered(8)
        .filter_map(|x| async { x })
        .collect()
        .await;
    let mut digests: HashMap<String, Vec<String>> = images.iter().map(|i| (i.id.clone(), i.repo_digests.clone())).collect();
    // Which image each tag points at here.
    let tagged: HashMap<String, String> = images.iter().flat_map(|i| i.repo_tags.iter().filter_map(|t| Reference::parse(t)).map(|r| (r.key(), i.id.clone()))).collect();
    let mut wanted: Vec<(&'static str, String, String, String, String, bool)> = seen.into_iter().map(|s| ("container", s.id, s.name, s.reference, s.image, s.labelled)).collect();
    for i in &images {
        for t in i.repo_tags.iter().filter(|t| *t != "<none>:<none>") {
            wanted.push(("image", i.id.clone(), t.clone(), t.clone(), i.id.clone(), false));
        }
    }
    let mut rows = Vec::new();
    // An image tagged twice gets one row, by its first tag.
    let mut images_done: HashSet<String> = HashSet::new();
    for (kind, id, name, reference, image, labelled) in wanted {
        if kind == "image" && !images_done.insert(id.clone()) {
            continue;
        }
        let Some(r) = Reference::parse(&reference) else {
            if reference.contains('@') {
                rows.push(Row { kind, id, name, reference, status: Status { status: "pinned", detail: Some("Pinned to a digest: it never changes.".into()), remote: None }, labelled });
            }
            continue;
        };
        let k = match answers.get(&r.key()) {
            Some(k) => k.clone(),
            None => {
                let k = remote(app, client, &r, max_age_s, asked).await;
                answers.insert(r.key(), k.clone());
                k
            }
        };
        if kind == "container" && !digests.contains_key(&image) {
            // An image that lost its tag is not always in the list (Docker's containerd store): its inspect says.
            let rd = c.inspect_image(&image).await.ok().and_then(|i| i.repo_digests).unwrap_or_default();
            digests.insert(image.clone(), rd);
        }
        let local = r.digests_in(digests.get(&image).map(Vec::as_slice).unwrap_or_default());
        if kind == "container" && local.is_empty() && tagged.get(&r.key()).is_some_and(|t| *t != image) {
            // No registry digest to compare, but its tag points at another image here, made or pulled after it.
            let status = Status { status: "outdated", detail: Some(format!("{reference} is a newer image here than the one it runs.")), remote: k.digest.clone() };
            rows.push(Row { kind, id, name, reference, status, labelled });
            continue;
        }
        let status = match assess(&reference, &local, &k, None) {
            Ok(s) => s,
            Err(()) => {
                // The local digest is not the index's: look for this platform's own manifest in the index.
                let mut k = k;
                if k.platforms.is_none()
                    && let Some(digest) = k.digest.clone() {
                        let creds = registry::credentials(app, &r.registry).await;
                        *asked += 1;
                        if let Ok(p) = client.platforms(&r, &digest, creds.as_ref()).await {
                            k.platforms = Some(p);
                            let _ = keep(app, r.key(), k.clone()).await;
                            answers.insert(r.key(), k.clone());
                        }
                    }
                let platform = c.inspect_image(&image).await.ok().map(|i| (i.os.unwrap_or_default(), i.architecture.unwrap_or_default(), i.variant));
                let platform = platform.as_ref().map(|(o, a, v)| (o.as_str(), a.as_str(), v.as_deref()));
                assess(&reference, &local, &Known { platforms: Some(k.platforms.clone().unwrap_or_default()), ..k }, platform).unwrap_or(Status { status: "error", detail: Some("Its registry's index could not be read.".into()), remote: None })
            }
        };
        rows.push(Row { kind, id, name, reference, status, labelled });
    }
    Ok(rows)
}

/// Writes an environment's verdicts in place of the last; answers the containers' last verdicts by name.
async fn write(app: &App, env: i64, rows: &[Row]) -> anyhow::Result<HashMap<String, (String, Option<String>)>> {
    let rows = rows.to_vec();
    app.db
        .call(move |c| {
            let tx = c.transaction()?;
            let before = {
                let mut st = tx.prepare("SELECT name, status, remote FROM update_status WHERE env_id = ?1 AND kind = 'container'")?;
                let rows = st.query_map([env], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))))?;
                rows.collect::<Result<HashMap<_, _>, _>>()?
            };
            tx.execute("DELETE FROM update_status WHERE env_id = ?1", [env])?;
            {
                let mut st = tx.prepare("INSERT OR REPLACE INTO update_status (env_id, kind, id, name, reference, status, detail, remote, labelled, checked_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)")?;
                for r in rows {
                    st.execute(params![env, r.kind, r.id, r.name, r.reference, r.status.status, r.status.detail, r.status.remote, r.labelled, now()])?;
                }
            }
            tx.commit()?;
            Ok(before)
        })
        .await
}

/// The containers of `env` switched to automatic updates on their page, by name.
async fn opted(app: &App, env: i64) -> HashSet<String> {
    app.db.call(move |c| Ok(c.prepare("SELECT container FROM auto_updates WHERE env_id = ?1")?.query_map([env], |r| r.get(0))?.collect::<Result<HashSet<String>, _>>()?)).await.unwrap_or_default()
}

/// After an update: the new container is current, in the old one's row.
async fn mark_current(app: &App, env: i64, old: &str, new: &str) {
    let (old, new) = (old.to_string(), new.to_string());
    let _ = app
        .db
        .call(move |c| Ok(c.execute("UPDATE update_status SET id = ?1, status = 'current', detail = NULL WHERE env_id = ?2 AND kind = 'container' AND id = ?3", params![new, env, old])?))
        .await;
}

/// Pulls `image` with the registry's saved credentials, when there are any, and waits for it.
async fn pull(app: &App, d: &crate::docker::Docker, r: &Reference, image: &str) -> ApiResult<()> {
    let creds = registry::credentials(app, &r.registry).await.map(|c| bollard::auth::DockerCredentials { username: Some(c.username), password: Some(c.password), serveraddress: Some(c.server), ..Default::default() });
    let (from, tag) = containers::split_tag(image);
    let opts = q::CreateImageOptions { from_image: Some(from.to_string()), tag: Some(tag.to_string()), ..Default::default() };
    let mut s = d.client().create_image(Some(opts), None, creds);
    while let Some(step) = s.next().await {
        let step = step.map_err(api_error)?;
        if let Some(e) = step.error_detail.and_then(|e| e.message) {
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, e));
        }
    }
    Ok(())
}

/// Updates container `id`: its image pulled again under the same reference, then the container made again from it with
/// the same settings ("Containers" 3, 4); one that ran must stay up `SETTLE`, or the old one is put back. Answers the
/// new container's id, or None when the pull brought nothing newer.
pub async fn update(app: &App, _env: i64, d: &crate::docker::Docker, id: &str) -> ApiResult<Option<String>> {
    let c = d.client().inspect_container(id, None::<q::InspectContainerOptions>).await.map_err(api_error)?;
    let reference = c.config.as_ref().and_then(|c| c.image.clone()).unwrap_or_default();
    let r = Reference::parse(&reference).ok_or_else(|| ApiError::bad(format!("{reference} names no tag to update: it is pinned to a digest or an image id.")))?;
    let old_image = c.image.clone().unwrap_or_default();
    // The settings are its own against the image it has now, so the new image's defaults reach it.
    let img = containers::image_config(d, &old_image).await;
    let body = containers::create_body(&c, img.as_ref());
    pull(app, d, &r, &reference).await?;
    let new_image = d.client().inspect_image(&reference).await.map_err(api_error)?.id.unwrap_or_default();
    if new_image == old_image {
        return Ok(None);
    }
    let running = c.state.as_ref().and_then(|s| s.running).unwrap_or(false);
    let req = containers::Create { name: Some(containers::shown_name(&c)), config: body, pull: false, start: Some(running), replace: Some(id.to_string()), access: None, owner: None };
    let made = containers::make(app, d, req, running.then_some(SETTLE)).await?;
    Ok(Some(made.id))
}

#[derive(Deserialize, Default)]
struct CheckNow {
    /// Update the containers that opted in, too.
    #[serde(default)]
    apply: bool,
}

/// `POST /api/updates/check`: asks the registries now (an answer younger than a minute is used again), and with
/// `apply`, updates what opted in.
async fn check_now(State(app): State<App>, admin: Admin, body: Option<Json<CheckNow>>) -> ApiResult<Json<Summary>> {
    let apply = body.map(|b| b.0.apply).unwrap_or(false) && settings(&app).await.auto_update;
    let sum = pass(&app, FORCED_AFTER_S, apply).await;
    log(&app, Some(&admin.0), "updates.check", None, Some(format!("{} asked, {} behind, {} updated", sum.asked, sum.outdated, sum.updated.len()))).await;
    Ok(Json(sum))
}

/// `GET /api/envs/{env}/updates`: the verdicts for the badges, by container and image id, and who opted in.
/// A Standard or Read-only user gets only the containers they can see ("Access" 4).
async fn statuses(State(app): State<App>, access: crate::access::Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let owners = crate::access::ownership::Owners::load(&app.db, env).await?;
    let s = settings(&app).await;
    let (rows, opted) = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT kind, id, name, reference, status, detail, remote, labelled, checked_at FROM update_status WHERE env_id = ?1")?;
            let rows = st
                .query_map([env], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, json!({
                        "name": r.get::<_, String>(2)?, "reference": r.get::<_, String>(3)?, "status": r.get::<_, String>(4)?, "detail": r.get::<_, Option<String>>(5)?,
                        "remote": r.get::<_, Option<String>>(6)?, "labelled": r.get::<_, bool>(7)?, "checked_at": r.get::<_, i64>(8)?,
                    })))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let opted = c.prepare("SELECT container FROM auto_updates WHERE env_id = ?1")?.query_map([env], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok((rows, opted))
        })
        .await?;
    let (mut containers, mut images) = (serde_json::Map::new(), serde_json::Map::new());
    for (kind, id, mut v) in rows {
        let auto = v["labelled"].as_bool().unwrap_or(false) || opted.iter().any(|n| v["name"] == json!(n));
        v["auto"] = json!(auto);
        if kind == "container" {
            if access.sees(owners.container(v["name"].as_str().unwrap_or_default(), None)) {
                containers.insert(id, v);
            }
        } else {
            images.insert(id, v);
        }
    }
    // Names of containers they cannot see are not theirs to learn ("Access" 10).
    let opted: Vec<String> = opted.into_iter().filter(|n| access.sees(owners.container(n, None))).collect();
    Ok(Json(json!({ "enabled": s.enabled, "auto_update": s.auto_update, "containers": containers, "images": images, "opted": opted })))
}

/// `POST .../containers/{id}/update`: updates it now, whether it opted in or not.
async fn update_one(State(app): State<App>, admin: crate::api::Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    if !crate::docker::valid_ref(&id) {
        return Err(ApiError::bad("That is not a container's id or name."));
    }
    let d = crate::docker::endpoint(&app, env).await?;
    match update(&app, env, &d, &id).await {
        Ok(Some(new)) => {
            log(&app, Some(&admin.0), "container.update", Some(id.clone()), Some(format!("now {}", short(Some(&new))))).await;
            mark_current(&app, env, &id, &new).await;
            Ok(Json(json!({ "id": new, "updated": true })))
        }
        Ok(None) => Ok(Json(json!({ "id": id, "updated": false }))),
        Err(e) => {
            log(&app, Some(&admin.0), "container.update", Some(id), Some(format!("failed: {}", e.message))).await;
            Err(e)
        }
    }
}

#[derive(Deserialize)]
struct Auto {
    enabled: bool,
}

/// `PUT .../containers/{id}/auto-update {enabled}`: the switch on a container's page, kept by its name so it holds
/// through updates. (The label is the other way in, and is not changed here.)
async fn set_auto(State(app): State<App>, admin: crate::api::Allowed, Path((env, id)): Path<(i64, String)>, Json(a): Json<Auto>) -> ApiResult<StatusCode> {
    if !crate::docker::valid_ref(&id) {
        return Err(ApiError::bad("That is not a container's id or name."));
    }
    let inspected = match crate::docker::endpoint(&app, env).await {
        Ok(d) => d.client().inspect_container(&id, None::<q::InspectContainerOptions>).await.map_err(api_error),
        Err(e) => Err(e),
    };
    let name = match inspected {
        Ok(c) => containers::shown_name(&c),
        Err(e) if e.status == StatusCode::NOT_FOUND => return Err(e),
        // A host that does not answer now: the name is taken as given.
        Err(_) if crate::docker::valid_name(&id) => id.clone(),
        Err(e) => return Err(e),
    };
    let n = name.clone();
    app.db
        .call(move |c| {
            if a.enabled {
                c.execute("INSERT OR IGNORE INTO auto_updates (env_id, container) VALUES (?1, ?2)", params![env, n])?;
            } else {
                c.execute("DELETE FROM auto_updates WHERE env_id = ?1 AND container = ?2", params![env, n])?;
            }
            Ok(())
        })
        .await?;
    log(&app, Some(&admin.0), "container.auto_update", Some(name), Some(if a.enabled { "on" } else { "off" }.into())).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether a pass is due: none yet, or the last one `every_s` ago.
pub fn due(last: Option<i64>, now: i64, every_s: i64) -> bool {
    last.is_none_or(|l| now - l >= every_s)
}

/// A pass every quarter of an hour, the registries asked at most every `check_hours`, until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    let mut last: Option<i64> = None;
    loop {
        let s = settings(&app).await;
        let t = now();
        if s.enabled && due(last, t, PASS_EVERY_S) {
            last = Some(t);
            let sum = pass(&app, s.check_hours as i64 * 3600, s.auto_update).await;
            if sum.asked > 0 || !sum.updated.is_empty() || !sum.failed.is_empty() {
                tracing::info!("update check: {} asked, {} behind, {} updated, {} failed", sum.asked, sum.outdated, sum.updated.len(), sum.failed.len());
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = stop.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;

    #[test]
    fn a_label_opts_in_and_nothing_else_does() {
        let l = |v: &str| Some(HashMap::from([(LABEL.to_string(), v.to_string())]));
        assert!(opts_in(l("true").as_ref()) && opts_in(l(" Yes ").as_ref()) && opts_in(l("1").as_ref()));
        assert!(!opts_in(l("false").as_ref()) && !opts_in(l("").as_ref()) && !opts_in(None));
    }

    #[test]
    fn a_kept_answer_is_used_until_it_is_old() {
        assert!(fresh(1000, 1000 + 6 * 3600 - 1, 6 * 3600));
        assert!(!fresh(1000, 1000 + 6 * 3600, 6 * 3600));
        assert!(due(None, 5, PASS_EVERY_S) && !due(Some(0), PASS_EVERY_S - 1, PASS_EVERY_S) && due(Some(0), PASS_EVERY_S, PASS_EVERY_S));
    }

    fn k(digest: &str, index: bool) -> Known {
        Known { digest: Some(digest.into()), index, ..Default::default() }
    }

    #[test]
    fn an_image_is_judged_by_its_digests_against_the_registry_s() {
        assert_eq!(assess("nginx", &["sha256:a".into()], &k("sha256:a", false), None).unwrap().status, "current");
        let s = assess("nginx", &["sha256:a".into()], &k("sha256:b", false), None).unwrap();
        assert_eq!((s.status, s.remote.as_deref()), ("outdated", Some("sha256:b")));
        assert_eq!(assess("app", &[], &k("sha256:b", false), None).unwrap().status, "unknown", "built here: nothing to compare");
        let failed = Known { error: Some("registry.example answered 500".into()), checked_at: 1, ..Default::default() };
        assert_eq!(assess("x", &["sha256:a".into()], &failed, None).unwrap(), Status { status: "error", detail: Some("registry.example answered 500".into()), remote: None });
        // An index whose digest is not ours: its platforms must be looked at, and then ours decides.
        assert!(assess("nginx", &["sha256:amd64".into()], &k("sha256:index", true), None).is_err());
        let with = Known { platforms: Some(registry::platforms(&registry::tests::index_fixture())), ..k("sha256:index", true) };
        assert_eq!(assess("nginx", &["sha256:amd64".into()], &with, Some(("linux", "amd64", None))).unwrap().status, "current");
        assert_eq!(assess("nginx", &["sha256:old".into()], &with, Some(("linux", "amd64", None))).unwrap().status, "outdated");
    }

    #[tokio::test]
    async fn a_registry_is_asked_at_most_every_so_often() {
        let app = app();
        let (host, hits) = registry::tests::fake_registry_counted("sha256:feed", false).await;
        let r = Reference::parse(&format!("{host}/qa/app:1")).unwrap();
        let client = registry::Client::default();
        let mut asked = 0;
        let first = remote(&app, &client, &r, 3600, &mut asked).await;
        assert_eq!(first.digest.as_deref(), Some("sha256:feed"));
        let again = remote(&app, &client, &r, 3600, &mut asked).await;
        assert_eq!(again.digest.as_deref(), Some("sha256:feed"));
        assert_eq!((asked, hits.load(std::sync::atomic::Ordering::SeqCst)), (1, 1), "the second came from what was kept");
        // Old enough: asked again.
        let key = r.key();
        app.db.call(move |c| Ok(c.execute("UPDATE image_checks SET checked_at = checked_at - 7200 WHERE reference = ?1", [key])?)).await.unwrap();
        remote(&app, &client, &r, 3600, &mut asked).await;
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A registry that went away: what was known stays, with the error beside it.
        let gone = Reference { registry: "127.0.0.1:1".into(), ..r.clone() };
        let key = gone.key();
        app.db.call(move |c| Ok(c.execute("INSERT INTO image_checks (reference, digest, is_index, checked_at) VALUES (?1, 'sha256:kept', 0, 0)", [key])?)).await.unwrap();
        let k = remote(&app, &client, &gone, 3600, &mut asked).await;
        assert_eq!(k.digest.as_deref(), Some("sha256:kept"));
        assert!(k.error.is_some());
    }

    #[tokio::test]
    async fn the_badges_and_the_switch_are_read_and_kept_by_name() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let rows = vec![
            Row { kind: "container", id: "c1".into(), name: "web".into(), reference: "nginx:1".into(), status: Status { status: "outdated", detail: None, remote: Some("sha256:b".into()) }, labelled: false },
            Row { kind: "image", id: "sha256:i1".into(), name: "nginx:1".into(), reference: "nginx:1".into(), status: Status { status: "outdated", detail: None, remote: Some("sha256:b".into()) }, labelled: false },
        ];
        assert!(write(&app, 1, &rows).await.unwrap().is_empty());
        let (s, _, _) = call(&app, req(Method::PUT, "/api/envs/1/containers/web/auto-update", &cookie, &csrf, Some(json!({ "enabled": true })))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "a host that does not answer: the name is taken as given");
        let (s, _, b) = call(&app, req(Method::GET, "/api/envs/1/updates", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["containers"]["c1"]["status"], "outdated");
        assert_eq!(b["containers"]["c1"]["auto"], true);
        assert_eq!(b["images"]["sha256:i1"]["name"], "nginx:1");
        assert_eq!(b["opted"], json!(["web"]));
        let before = write(&app, 1, &rows[..1]).await.unwrap();
        assert_eq!(before["web"], ("outdated".to_string(), Some("sha256:b".to_string())), "the last verdicts, to tell only what is new");
        let (s, _, _) = call(&app, req(Method::PUT, "/api/envs/1/containers/web/auto-update", &cookie, &csrf, Some(json!({ "enabled": false })))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, b) = call(&app, req(Method::GET, "/api/envs/1/updates", &cookie, &csrf, None)).await;
        assert_eq!(b["containers"]["c1"]["auto"], false);
        let logged: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit WHERE action = 'container.auto_update'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(logged, 2);
    }

    #[tokio::test]
    async fn the_settings_are_an_administrator_s() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::PUT, "/api/settings/updates", &cookie, &csrf, Some(json!({ "enabled": true, "check_hours": 0, "auto_update": true })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("check_hours")));
        let (s, _, _) = call(&app, req(Method::PUT, "/api/settings/updates", &cookie, &csrf, Some(json!({ "enabled": true, "check_hours": 12, "auto_update": false })))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(settings(&app).await, Settings { enabled: true, check_hours: 12, auto_update: false });
        let (s, _, b) = call(&app, req(Method::POST, "/api/updates/check", &cookie, &csrf, Some(json!({})))).await;
        assert_eq!((s, b["environments"].as_u64()), (StatusCode::OK, Some(0)), "no Docker answers in the tests: {b}");
    }
}
