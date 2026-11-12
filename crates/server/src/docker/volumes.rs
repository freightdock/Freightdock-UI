//! Volumes: listed with the containers that mount them, inspected, made, removed, pruned.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bollard::models::VolumeCreateOptions;
use bollard::query_parameters as q;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{api_error, endpoint, valid_name, valid_ref};
use crate::access::Access;
use crate::access::ownership::{self, Kind, Wanted};
use crate::api::Allowed;
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

async fn log(app: &App, admin: &Allowed, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

/// Volume name to the names of the containers that mount it.
pub fn users_of(containers: &[bollard::models::ContainerSummary]) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for c in containers {
        let name = c.names.as_ref().and_then(|n| n.first()).map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default();
        for mount in c.mounts.iter().flatten() {
            if mount.typ == Some(bollard::models::MountPointTypeEnum::VOLUME)
                && let Some(v) = &mount.name {
                    m.entry(v.clone()).or_default().push(name.clone());
                }
        }
    }
    m
}

/// Every volume the person can see ("Access" 18), with the containers they can see that mount it, and whom it belongs
/// to (`freightdock_access`).
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let (vols, containers) = tokio::try_join!(
        c.list_volumes(None::<q::ListVolumesOptions>),
        c.list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })),
    )
    .map_err(api_error)?;
    let users = users_of(&containers);
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    let out: Vec<Value> = vols
        .volumes
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let mut j = json!(v);
            let project = ownership::project_of(&j);
            if !seen.volume(&access, &owners, &v.name, project.as_deref()) {
                return None;
            }
            j["containers"] = json!(seen.only(&access, users.get(&v.name).into_iter().flatten()));
            j["freightdock_access"] = owners.show(owners.volume(&v.name, project.as_deref()));
            Some(j)
        })
        .collect();
    Ok(Json(json!(out)))
}

/// One volume (the guard has seen that the person may), with the containers they can see that mount it.
pub async fn inspect(State(app): State<App>, access: Access, Path((env, name)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    if !valid_ref(&name) {
        return Err(ApiError::bad("That is not a volume's name."));
    }
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let (v, containers) = tokio::try_join!(c.inspect_volume(&name), c.list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() }))).map_err(api_error)?;
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    let used = users_of(&containers).remove(&name).unwrap_or_default();
    let project = ownership::project_of(&json!(v));
    let shown = owners.show(owners.volume(&name, project.as_deref()));
    Ok(Json(json!({ "volume": v, "containers": seen.only(&access, &used), "freightdock_access": shown })))
}

/// `PUT .../volumes/{name}/access`: `{level, team_id, user_id}`, as a container's ("Access" 4, 18).
pub async fn set_access(State(app): State<App>, admin: Allowed, Path((env, name)): Path<(i64, String)>, Json(w): Json<Wanted>) -> ApiResult<StatusCode> {
    if !valid_ref(&name) {
        return Err(ApiError::bad("That is not a volume's name."));
    }
    let o = w.owner(&admin.1)?;
    let d = endpoint(&app, env).await?;
    let v = d.client().inspect_volume(&name).await.map_err(api_error)?;
    ownership::save(&app.db, env, Kind::Volume, v.name.clone(), o.clone()).await?;
    log(&app, &admin, "volume.access", v.name, Some(o.map_or("admins", |o| o.level.as_str()).to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What the page sends to make a volume: Docker's own body, and whom it belongs to (private to its maker when left
/// out).
#[derive(Deserialize)]
pub struct NewVolume {
    #[serde(flatten)]
    spec: VolumeCreateOptions,
    #[serde(default)]
    access: Option<Wanted>,
}

pub async fn create(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(n): Json<NewVolume>) -> ApiResult<(StatusCode, Json<Value>)> {
    let owner = n.access.unwrap_or_default().owner(&admin.1)?;
    let body = n.spec;
    if let Some(n) = body.name.as_deref().filter(|n| !n.is_empty())
        && !valid_name(n) {
            return Err(ApiError::bad("A name starts with a letter or digit, then letters, digits, _ . or -.").field("name"));
        }
    // A local volume can be a host path underneath (`o=bind`): a bind mount by another name ("Access" 13, 15).
    let spec = crate::access::security::of_volume(&serde_json::to_value(&body).unwrap_or_default());
    crate::access::security::hold(&app.db, &crate::access::security::Judge::of(&admin.1), env, &[spec], Some("driver_opts")).await?;
    let d = endpoint(&app, env).await?;
    let v = d.client().create_volume(body).await.map_err(api_error)?;
    ownership::save(&app.db, env, Kind::Volume, v.name.clone(), owner).await?;
    log(&app, &admin, "volume.create", v.name.clone(), Some(format!("driver {}", v.driver))).await;
    Ok((StatusCode::CREATED, Json(json!(v))))
}

#[derive(Deserialize, Default)]
pub struct RemoveQuery {
    force: Option<bool>,
}

pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, name)): Path<(i64, String)>, Query(rq): Query<RemoveQuery>) -> ApiResult<StatusCode> {
    if !valid_ref(&name) {
        return Err(ApiError::bad("That is not a volume's name."));
    }
    let d = endpoint(&app, env).await?;
    d.client().remove_volume(&name, Some(q::RemoveVolumeOptions { force: rq.force.unwrap_or(false) })).await.map_err(api_error)?;
    ownership::save(&app.db, env, Kind::Volume, name.clone(), None).await?;
    log(&app, &admin, "volume.remove", name, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct Prune {
    /// Named volumes too, not only anonymous ones (Docker's own default since 23.0).
    #[serde(default)]
    all: bool,
}

pub async fn prune(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(p): Json<Prune>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let filters = p.all.then(|| HashMap::from([("all".to_string(), vec!["true".to_string()])]));
    let r = d.client().prune_volumes(Some(q::PruneVolumesOptions { filters })).await.map_err(api_error)?;
    let n = r.volumes_deleted.as_ref().map_or(0, Vec::len);
    // A volume made later under the same name is not given what the pruned one had.
    for v in r.volumes_deleted.iter().flatten() {
        ownership::save(&app.db, env, Kind::Volume, v.clone(), None).await?;
    }
    log(&app, &admin, "volume.prune", format!("{n} volumes"), r.space_reclaimed.map(|b| format!("{b} bytes freed"))).await;
    Ok(Json(json!(r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummary, MountPoint, MountPointTypeEnum};

    #[test]
    fn a_volume_knows_who_mounts_it_and_a_bind_mount_is_no_volume() {
        let c = |name: &str, mounts: Vec<MountPoint>| ContainerSummary { names: Some(vec![format!("/{name}")]), mounts: Some(mounts), ..Default::default() };
        let vol = |n: &str| MountPoint { typ: Some(MountPointTypeEnum::VOLUME), name: Some(n.into()), ..Default::default() };
        let bind = MountPoint { typ: Some(MountPointTypeEnum::BIND), source: Some("/srv".into()), ..Default::default() };
        let m = users_of(&[c("db", vec![vol("pgdata")]), c("backup", vec![vol("pgdata"), bind])]);
        assert_eq!(m["pgdata"], vec!["db", "backup"]);
        assert_eq!(m.len(), 1);
    }
}
