//! What one host holds, in a few numbers: containers (running, stopped, unhealthy), images, volumes, stacks, nodes,
//! CPUs and memory. The home page asks every host for it at once and draws each answer as it comes; the health check
//! takes it too and keeps the last one (`environments.snapshot`), so a host that is down still shows what it had and
//! when ("Home" in DECISIONS.md).

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use bollard::models::{ContainerSummary, ContainerSummaryStateEnum, LocalNodeState, SystemInfo, VolumeListResponse};
use serde_json::{Value, json};

use super::{Docker, endpoint};
use crate::api::Authed;
use crate::db::{now, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// How long a host gets to answer before it is left out.
pub const WITHIN: Duration = Duration::from_secs(10);

/// The numbers, from Docker's info, its containers and how many volumes it has.
pub fn summarise(info: &SystemInfo, containers: &[ContainerSummary], volumes: usize) -> Value {
    let (mut running, mut paused, mut stopped, mut unhealthy) = (0, 0, 0, 0);
    let mut stacks = BTreeSet::new();
    for c in containers {
        match c.state {
            Some(ContainerSummaryStateEnum::RUNNING | ContainerSummaryStateEnum::RESTARTING) => running += 1,
            Some(ContainerSummaryStateEnum::PAUSED) => paused += 1,
            _ => stopped += 1,
        }
        if c.status.as_deref().is_some_and(|s| s.contains("(unhealthy)")) {
            unhealthy += 1;
        }
        if let Some(p) = c.labels.as_ref().and_then(|l| l.get("com.docker.compose.project")) {
            stacks.insert(p.clone());
        }
    }
    let swarm = info.swarm.as_ref().filter(|s| s.local_node_state == Some(LocalNodeState::ACTIVE));
    let nodes = swarm.filter(|s| s.control_available == Some(true)).and_then(|s| s.nodes).unwrap_or(1);
    json!({
        "containers": { "total": containers.len(), "running": running, "paused": paused, "stopped": stopped, "unhealthy": unhealthy },
        "images": info.images.unwrap_or(0),
        "volumes": volumes,
        "stacks": stacks.len(),
        "swarm": swarm.is_some(),
        "nodes": nodes,
        "cpus": info.ncpu.unwrap_or(0),
        "memory": info.mem_total.unwrap_or(0),
        "docker_version": info.server_version,
    })
}

/// Asks `d` for its numbers, all three questions at once. Each answer is held to `raw::MAX_ANSWER`: the health pass
/// asks every environment every 30 seconds, and a remote one's Docker is not trusted to end its answer ("Endpoints" 6).
pub async fn take(d: &Docker) -> ApiResult<Value> {
    let (info, containers, volumes) = tokio::try_join!(
        super::raw::get::<SystemInfo>(d, "/info"),
        super::raw::get::<Vec<ContainerSummary>>(d, "/containers/json?all=true"),
        super::raw::get::<VolumeListResponse>(d, "/volumes"),
    )?;
    Ok(summarise(&info, &containers, volumes.volumes.map_or(0, |v| v.len())))
}

/// Takes environment `env`'s numbers within `WITHIN` and keeps them as its last known; it answered, so it is up.
pub async fn snapshot(app: &App, env: i64) -> ApiResult<Value> {
    let d = endpoint(app, env).await?;
    let s = tokio::time::timeout(WITHIN, take(&d)).await.map_err(|_| ApiError::new(StatusCode::GATEWAY_TIMEOUT, "Docker took too long to answer."))??;
    let (kept, version, t) = (s.to_string(), s["docker_version"].as_str().map(str::to_string), now());
    app.db
        .call(move |c| {
            Ok(c.execute(
                "UPDATE environments SET snapshot = ?1, snapshot_at = ?2, status = 'up', last_seen_at = ?2, last_error = NULL, docker_version = COALESCE(?3, docker_version) WHERE id = ?4",
                params![kept, t, version, env],
            )?)
        })
        .await?;
    let mut s = s;
    s["at"] = json!(t);
    Ok(s)
}

/// `GET .../summary`: the host's numbers now, for its row on the home page.
pub async fn summary(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(snapshot(&app, env).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_host_that_answers_without_end_is_refused_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let d = crate::docker::raw::tests::endless_docker(dir.path());
        let e = tokio::time::timeout(WITHIN, take(&d)).await.expect("refused at the limit, not when the time ran out").unwrap_err();
        assert!(e.message.contains("too big"), "{}", e.message);
    }

    fn containers(v: Value) -> Vec<ContainerSummary> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_host_is_counted_as_docker_ps_would_and_its_stacks_by_their_compose_project() {
        let info: SystemInfo = serde_json::from_value(json!({ "Images": 7, "NCPU": 8, "MemTotal": 16_000_000_000_i64, "ServerVersion": "28.1.1" })).unwrap();
        let list = containers(json!([
            { "Id": "a", "State": "running", "Status": "Up 2 hours (healthy)", "Labels": { "com.docker.compose.project": "web" } },
            { "Id": "b", "State": "running", "Status": "Up 5 minutes (unhealthy)", "Labels": { "com.docker.compose.project": "web" } },
            { "Id": "c", "State": "restarting", "Status": "Restarting (1) 3 seconds ago", "Labels": { "com.docker.compose.project": "db" } },
            { "Id": "d", "State": "exited", "Status": "Exited (0) 2 days ago" },
            { "Id": "e", "State": "created", "Status": "Created" },
            { "Id": "f", "State": "paused", "Status": "Up 1 hour (Paused)" },
        ]));
        let s = summarise(&info, &list, 3);
        assert_eq!(s["containers"], json!({ "total": 6, "running": 3, "paused": 1, "stopped": 2, "unhealthy": 1 }));
        assert_eq!((s["images"].clone(), s["volumes"].clone(), s["stacks"].clone()), (json!(7), json!(3), json!(2)));
        assert_eq!((s["nodes"].clone(), s["swarm"].clone()), (json!(1), json!(false)), "a host on its own is one node");
        assert_eq!((s["cpus"].clone(), s["memory"].clone(), s["docker_version"].clone()), (json!(8), json!(16_000_000_000_i64), json!("28.1.1")));
    }

    #[test]
    fn a_swarm_manager_counts_the_swarm_s_nodes() {
        let info: SystemInfo = serde_json::from_value(json!({ "Swarm": { "LocalNodeState": "active", "ControlAvailable": true, "Nodes": 3 } })).unwrap();
        let s = summarise(&info, &[], 0);
        assert_eq!((s["nodes"].clone(), s["swarm"].clone()), (json!(3), json!(true)));
        let worker: SystemInfo = serde_json::from_value(json!({ "Swarm": { "LocalNodeState": "active", "ControlAvailable": false } })).unwrap();
        assert_eq!(summarise(&worker, &[], 0)["nodes"], 1, "a worker sees only itself");
        assert_eq!(summarise(&SystemInfo::default(), &[], 0)["containers"]["total"], 0);
    }
}
