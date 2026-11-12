//! The host as a whole: what Docker is and holds, the disk it uses, and one prune for everything.

use axum::Json;
use axum::extract::{Path, State};
use bollard::query_parameters as q;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{api_error, endpoint};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry};
use crate::error::ApiResult;
use crate::state::App;

/// `GET .../info`: Docker's info and version, for the dashboard.
pub async fn info(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let (info, version) = tokio::try_join!(d.client().info(), d.client().version()).map_err(api_error)?;
    Ok(Json(json!({ "info": info, "version": version })))
}

/// `GET .../df`: the disk Docker uses, by kind.
pub async fn df(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let r = d.client().df(None::<q::DataUsageOptions>).await.map_err(api_error)?;
    let sum = |v: Option<i64>| v.unwrap_or(0).max(0);
    let images: i64 = r.images.iter().flatten().map(|i| sum(Some(i.size))).sum();
    let containers: i64 = r.containers.iter().flatten().map(|c| sum(c.size_rw)).sum();
    let volumes: i64 = r.volumes.iter().flatten().map(|v| sum(v.usage_data.as_ref().map(|u| u.size))).sum();
    let cache: i64 = r.build_cache.iter().flatten().map(|b| sum(b.size)).sum();
    Ok(Json(json!({ "images": images, "containers": containers, "volumes": volumes, "build_cache": cache })))
}

#[derive(Deserialize, Default)]
pub struct Prune {
    /// Unused images with tags too, and named volumes: everything no container uses.
    #[serde(default)]
    all: bool,
    #[serde(default)]
    volumes: bool,
}

/// `POST .../prune`: stopped containers, unused networks, dangling (or all unused) images, the build cache, and the
/// volumes when asked, as `docker system prune` does.
pub async fn prune(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(p): Json<Prune>) -> ApiResult<Json<Value>> {
    use std::collections::HashMap;
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let containers = c.prune_containers(None::<q::PruneContainersOptions>).await.map_err(api_error)?;
    let networks = c.prune_networks(None::<q::PruneNetworksOptions>).await.map_err(api_error)?;
    let filters = HashMap::from([("dangling".to_string(), vec![(!p.all).to_string()])]);
    let images = c.prune_images(Some(q::PruneImagesOptions { filters: Some(filters) })).await.map_err(api_error)?;
    let cache = c.prune_build(Some(q::PruneBuildOptions { all: Some(p.all), ..Default::default() })).await.map_err(api_error)?;
    let volumes = if p.volumes {
        let f = p.all.then(|| HashMap::from([("all".to_string(), vec!["true".to_string()])]));
        Some(c.prune_volumes(Some(q::PruneVolumesOptions { filters: f })).await.map_err(api_error)?)
    } else {
        None
    };
    let freed = containers.space_reclaimed.unwrap_or(0)
        + images.space_reclaimed.unwrap_or(0)
        + cache.space_reclaimed.unwrap_or(0)
        + volumes.as_ref().and_then(|v| v.space_reclaimed).unwrap_or(0);
    let a = &admin.0;
    let what = if p.all { "everything unused" } else { "dangling" };
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "system.prune", target: Some(what.into()), detail: Some(format!("{freed} bytes freed")) }).await;
    Ok(Json(json!({ "freed": freed, "containers": containers, "networks": networks, "images": images, "build_cache": cache, "volumes": volumes })))
}
