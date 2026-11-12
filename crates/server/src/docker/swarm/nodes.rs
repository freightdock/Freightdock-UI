//! The swarm's nodes: listed, inspected, set to active, pause or drain, promoted or demoted, labelled, removed.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use hyper::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{call, check, list_of, log, versioned_change};
use crate::api::{Allowed, Authed};
use crate::docker::{endpoint, raw};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub async fn list(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    Ok(Json(json!(list_of(call(&d, Method::GET, "/nodes", None).await?))))
}

/// `GET .../swarm/nodes/{id}`: the node, and the tasks it runs.
pub async fn inspect(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check(&id, "node")?;
    let d = endpoint(&app, env).await?;
    let node = call(&d, Method::GET, &format!("/nodes/{id}"), None).await?;
    let node_id = node["ID"].as_str().unwrap_or(&id).to_string();
    let filters = json!({ "node": [node_id] }).to_string();
    let tasks = call(&d, Method::GET, &raw::path("/tasks", &[("filters", &filters)]), None).await?;
    Ok(Json(json!({ "node": node, "tasks": list_of(tasks) })))
}

/// What may change on a node; what is left out stays as it is.
#[derive(Deserialize, Default, Debug)]
pub struct Change {
    /// `active`, `pause` (no new tasks) or `drain` (its tasks move elsewhere).
    pub availability: Option<String>,
    /// `manager` (promote) or `worker` (demote).
    pub role: Option<String>,
    /// The node's labels, whole: what is not here goes.
    pub labels: Option<BTreeMap<String, String>>,
}

impl Change {
    /// The activity log's words for it.
    pub fn words(&self) -> String {
        let mut w = vec![];
        if let Some(a) = &self.availability {
            w.push(format!("availability {a}"));
        }
        if let Some(r) = &self.role {
            w.push(format!("role {r}"));
        }
        if let Some(l) = &self.labels {
            w.push(format!("{} labels", l.len()));
        }
        w.join(", ")
    }
}

/// The node's spec with the change made, every other field as Docker gave it.
pub fn changed(mut spec: Value, c: &Change) -> ApiResult<Value> {
    if !spec.is_object() {
        spec = json!({});
    }
    if c.availability.is_none() && c.role.is_none() && c.labels.is_none() {
        return Err(ApiError::bad("Say what changes: availability, role or labels."));
    }
    if let Some(a) = &c.availability {
        if !["active", "pause", "drain"].contains(&a.as_str()) {
            return Err(ApiError::bad("Availability is active, pause or drain.").field("availability"));
        }
        spec["Availability"] = json!(a);
    }
    if let Some(r) = &c.role {
        if !["manager", "worker"].contains(&r.as_str()) {
            return Err(ApiError::bad("A node is a manager or a worker.").field("role"));
        }
        spec["Role"] = json!(r);
    }
    if let Some(l) = &c.labels {
        if l.keys().any(|k| k.trim().is_empty()) {
            return Err(ApiError::bad("A label needs a name.").field("labels"));
        }
        spec["Labels"] = json!(l);
    }
    Ok(spec)
}

/// `PUT .../swarm/nodes/{id}`: `{availability, role, labels}`, made on the spec Docker has now, at its version, so a
/// change made meanwhile elsewhere is not overwritten (Docker refuses an old version); made again on a fresh read when
/// Swarm moved the version by itself ("Swarm" 9).
pub async fn update(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(c): Json<Change>) -> ApiResult<StatusCode> {
    check(&id, "node")?;
    changed(json!({}), &c)?;
    let d = endpoint(&app, env).await?;
    let (_, node) = versioned_change(&d, "nodes", &id, |n| Ok((changed(n["Spec"].clone(), &c)?, false))).await?;
    let name = node["Description"]["Hostname"].as_str().unwrap_or(&id).to_string();
    log(&app, &admin, "swarm.node.update", name, Some(c.words())).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct RemoveQuery {
    #[serde(default)]
    force: bool,
}

/// `DELETE .../swarm/nodes/{id}`: takes a node out of the swarm's list. Docker refuses a node that is still up unless
/// forced, and a manager until it is demoted.
pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Query(q): Query<RemoveQuery>) -> ApiResult<StatusCode> {
    check(&id, "node")?;
    let d = endpoint(&app, env).await?;
    let path = if q.force { raw::path(&format!("/nodes/{id}"), &[("force", "true")]) } else { format!("/nodes/{id}") };
    call(&d, Method::DELETE, &path, None).await?;
    log(&app, &admin, "swarm.node.remove", id, q.force.then(|| "forced".to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call as http};
    use crate::docker::swarm::tests::{change, people};

    /// A node's version moves by itself too (its status, a task leaving it), so a change that meets "update out of
    /// sequence" is read and made again ("Swarm" 9).
    #[tokio::test]
    async fn a_node_change_meeting_a_version_swarm_moved_meanwhile_is_sent_again() {
        let (sw, a) = crate::docker::swarm::tests::versioned("nodes", None).await;
        let (admin, csrf, _, env) = people(&a).await;
        let (s, _, b) = http(&a, change("PUT", &format!("/api/envs/{env}/swarm/nodes/web"), &admin, Some(&csrf), json!({ "availability": "drain" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let sw = sw.lock().unwrap();
        assert_eq!(sw.updates, vec![(10, false), (11, false)], "refused at 10, sent again at 11");
        assert_eq!(sw.spec["Availability"], "drain");
    }

    #[test]
    fn a_change_keeps_every_field_it_does_not_name() {
        let spec = json!({ "Availability": "active", "Role": "worker", "Labels": { "zone": "a" }, "Name": "n1", "SomethingNew": 7 });
        let s = changed(spec.clone(), &Change { availability: Some("drain".into()), ..Default::default() }).unwrap();
        assert_eq!(s["Availability"], "drain");
        assert_eq!(s["Role"], "worker");
        assert_eq!(s["Labels"]["zone"], "a");
        assert_eq!(s["SomethingNew"], 7, "a field Freightdock does not know goes back to Docker as it came");
        let s = changed(spec, &Change { role: Some("manager".into()), labels: Some(BTreeMap::from([("disk".into(), "ssd".into())])), ..Default::default() }).unwrap();
        assert_eq!(s["Role"], "manager");
        assert_eq!(s["Labels"], json!({ "disk": "ssd" }), "labels are given whole");
    }

    #[test]
    fn a_change_is_one_docker_takes() {
        let bad = |c: Change| changed(json!({}), &c).unwrap_err();
        assert_eq!(bad(Change { availability: Some("off".into()), ..Default::default() }).field, Some("availability"));
        assert_eq!(bad(Change { role: Some("boss".into()), ..Default::default() }).field, Some("role"));
        assert_eq!(bad(Change { labels: Some(BTreeMap::from([(" ".into(), "x".into())])), ..Default::default() }).field, Some("labels"));
        assert_eq!(bad(Change::default()).status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_log_says_what_changed() {
        let c = Change { availability: Some("drain".into()), role: Some("worker".into()), labels: None };
        assert_eq!(c.words(), "availability drain, role worker");
    }

    #[tokio::test]
    async fn a_wrong_availability_is_refused_before_docker_is_asked() {
        let a = app();
        let (admin, csrf, _, env) = people(&a).await;
        let (s, _, b) = http(&a, change("PUT", &format!("/api/envs/{env}/swarm/nodes/n1"), &admin, Some(&csrf), json!({ "availability": "sleep" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(b["field"], "availability");
    }
}
