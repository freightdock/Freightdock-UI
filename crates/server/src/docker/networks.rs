//! Networks: listed with how many containers are on each, inspected, made, removed, pruned; containers connected and
//! disconnected.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use bollard::models::{EndpointSettings, NetworkConnectRequest, NetworkCreateRequest, NetworkDisconnectRequest};
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

/// The networks Docker makes itself and will not let go of.
pub const BUILT_IN: [&str; 3] = ["bridge", "host", "none"];

async fn log(app: &App, admin: &Allowed, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

fn check(id: &str) -> ApiResult<()> {
    if valid_ref(id) { Ok(()) } else { Err(ApiError::bad("That is not a network's id or name.")) }
}

/// Every network the person can see ("Access" 18): Docker's own, theirs, and those a container they see is on; with
/// how many containers are on each, and whom it belongs to (`freightdock_access`).
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let (nets, containers) = tokio::try_join!(
        c.list_networks(None::<q::ListNetworksOptions>),
        c.list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })),
    )
    .map_err(api_error)?;
    let mut on: HashMap<String, usize> = HashMap::new();
    for ct in &containers {
        for (_, e) in ct.network_settings.as_ref().and_then(|n| n.networks.as_ref()).into_iter().flatten() {
            if let Some(id) = &e.network_id {
                *on.entry(id.clone()).or_default() += 1;
            }
        }
    }
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    let out: Vec<Value> = nets
        .into_iter()
        .filter_map(|n| {
            let name = n.name.clone().unwrap_or_default();
            let id = n.id.clone().unwrap_or_default();
            let count = on.get(&id).copied().unwrap_or(0);
            let built_in = BUILT_IN.contains(&name.as_str());
            let mut j = json!(n);
            let project = ownership::project_of(&j);
            if !seen.network(&access, &owners, &name, &id, project.as_deref()) {
                return None;
            }
            j["container_count"] = json!(count);
            j["built_in"] = json!(built_in);
            j["freightdock_access"] = owners.show(owners.network(&name, project.as_deref()));
            Some(j)
        })
        .collect();
    Ok(Json(json!(out)))
}

/// One network (the guard has seen that the person may), with only the containers on it they can see.
pub async fn inspect(State(app): State<App>, access: Access, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check(&id)?;
    let d = endpoint(&app, env).await?;
    let n = d.client().inspect_network(&id, Some(q::InspectNetworkOptions { verbose: false, scope: None })).await.map_err(api_error)?;
    let mut j = json!(n);
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    if access.limited()
        && let Some(m) = j["Containers"].as_object_mut() {
            m.retain(|_, c| c["Name"].as_str().is_some_and(|name| seen.containers.contains(name)));
        }
    let project = ownership::project_of(&j);
    j["freightdock_access"] = owners.show(owners.network(j["Name"].as_str().unwrap_or_default(), project.as_deref()));
    Ok(Json(j))
}

/// `PUT .../networks/{id}/access`: `{level, team_id, user_id}`, as a container's ("Access" 4, 18). Docker's own networks
/// are everybody's.
pub async fn set_access(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(w): Json<Wanted>) -> ApiResult<StatusCode> {
    check(&id)?;
    let o = w.owner(&admin.1)?;
    let d = endpoint(&app, env).await?;
    let n = d.client().inspect_network(&id, None::<q::InspectNetworkOptions>).await.map_err(api_error)?;
    let name = n.name.unwrap_or_default();
    if ownership::PREDEFINED_NETWORKS.contains(&name.as_str()) {
        return Err(ApiError::bad("Docker's own networks are everybody's.").field("level"));
    }
    ownership::save(&app.db, env, Kind::Network, name.clone(), o.clone()).await?;
    log(&app, &admin, "network.access", name, Some(o.map_or("admins", |o| o.level.as_str()).to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What the page sends to make a network: Docker's own body, and whom it belongs to (private to its maker when left
/// out).
#[derive(Deserialize)]
pub struct NewNetwork {
    #[serde(flatten)]
    spec: NetworkCreateRequest,
    #[serde(default)]
    access: Option<Wanted>,
}

pub async fn create(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(n): Json<NewNetwork>) -> ApiResult<(StatusCode, Json<Value>)> {
    let owner = n.access.unwrap_or_default().owner(&admin.1)?;
    let body = n.spec;
    if !valid_name(&body.name) {
        return Err(ApiError::bad("A name starts with a letter or digit, then letters, digits, _ . or -.").field("name"));
    }
    let d = endpoint(&app, env).await?;
    let name = body.name.clone();
    let driver = body.driver.clone().unwrap_or_else(|| "bridge".into());
    let r = d.client().create_network(body).await.map_err(api_error)?;
    ownership::save(&app.db, env, Kind::Network, name.clone(), owner).await?;
    log(&app, &admin, "network.create", name, Some(format!("driver {driver}"))).await;
    Ok((StatusCode::CREATED, Json(json!(r))))
}

pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    check(&id)?;
    if BUILT_IN.contains(&id.as_str()) {
        return Err(ApiError::bad("Docker's own networks cannot be removed."));
    }
    let d = endpoint(&app, env).await?;
    let name = d.client().inspect_network(&id, None::<q::InspectNetworkOptions>).await.ok().and_then(|n| n.name);
    d.client().remove_network(&id).await.map_err(api_error)?;
    if let Some(n) = name {
        ownership::save(&app.db, env, Kind::Network, n, None).await?;
    }
    log(&app, &admin, "network.remove", id, None).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn prune(State(app): State<App>, admin: Allowed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let r = d.client().prune_networks(None::<q::PruneNetworksOptions>).await.map_err(api_error)?;
    let n = r.networks_deleted.as_ref().map_or(0, Vec::len);
    for name in r.networks_deleted.iter().flatten() {
        ownership::save(&app.db, env, Kind::Network, name.clone(), None).await?;
    }
    log(&app, &admin, "network.prune", format!("{n} networks"), None).await;
    Ok(Json(json!(r)))
}

#[derive(Deserialize)]
pub struct Connect {
    container: String,
    #[serde(default)]
    aliases: Vec<String>,
    ipv4: Option<String>,
}

pub async fn connect(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(c): Json<Connect>) -> ApiResult<StatusCode> {
    check(&id)?;
    if !valid_ref(&c.container) {
        return Err(ApiError::bad("Say which container.").field("container"));
    }
    crate::access::routes::may_container(&app, &admin.1, crate::access::Action::Manage, &c.container).await?;
    let d = endpoint(&app, env).await?;
    let ipam = c.ipv4.clone().filter(|i| !i.is_empty()).map(|ip| bollard::models::EndpointIpamConfig { ipv4_address: Some(ip), ..Default::default() });
    let aliases: Vec<String> = c.aliases.into_iter().filter(|a| !a.trim().is_empty()).collect();
    let req = NetworkConnectRequest {
        container: Some(c.container.clone()),
        endpoint_config: Some(EndpointSettings { aliases: (!aliases.is_empty()).then_some(aliases), ipam_config: ipam, ..Default::default() }),
    };
    d.client().connect_network(&id, req).await.map_err(api_error)?;
    log(&app, &admin, "network.connect", id, Some(c.container)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Disconnect {
    container: String,
    #[serde(default)]
    force: bool,
}

pub async fn disconnect(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(c): Json<Disconnect>) -> ApiResult<StatusCode> {
    check(&id)?;
    if !valid_ref(&c.container) {
        return Err(ApiError::bad("Say which container.").field("container"));
    }
    crate::access::routes::may_container(&app, &admin.1, crate::access::Action::Manage, &c.container).await?;
    let d = endpoint(&app, env).await?;
    d.client().disconnect_network(&id, NetworkDisconnectRequest { container: Some(c.container.clone()), force: Some(c.force) }).await.map_err(api_error)?;
    log(&app, &admin, "network.disconnect", id, Some(c.container)).await;
    Ok(StatusCode::NO_CONTENT)
}
