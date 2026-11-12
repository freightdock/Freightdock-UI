//! Services: listed with their running and desired tasks, inspected with their tasks, made and updated from Docker's
//! own service spec, scaled, rolled back, removed; their logs followed as a container's are; tasks across the swarm.

use axum::Json;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use hyper::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{call, check, list_of, log, versioned_change};
use crate::access::Access;
use crate::api::{Allowed, Authed};
use crate::docker::{endpoint, raw, valid_name};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// `GET .../swarm/services`: every service, with Docker's count of running and desired tasks (`ServiceStatus`).
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    if access.limited() {
        return Ok(Json(json!([])));
    }
    let d = endpoint(&app, env).await?;
    Ok(Json(json!(list_of(call(&d, Method::GET, "/services?status=true", None).await?))))
}

/// The service's tasks, newest first.
async fn tasks_of(d: &crate::docker::Docker, service: &str) -> ApiResult<Vec<Value>> {
    let filters = json!({ "service": [service] }).to_string();
    let mut t = list_of(call(d, Method::GET, &raw::path("/tasks", &[("filters", &filters)]), None).await?);
    t.sort_by(|a, b| b["CreatedAt"].as_str().unwrap_or("").cmp(a["CreatedAt"].as_str().unwrap_or("")));
    Ok(t)
}

/// The service with `ServiceStatus` as the list has it: Docker counts running and desired tasks only for a list
/// (`?status=true`), so for one service they are counted from its tasks. Desired: the replicas asked for, or for a
/// global service one for each task meant to run.
pub fn counted(mut service: Value, tasks: &[Value]) -> Value {
    let meant = |t: &&Value| t["DesiredState"] == json!("running");
    let running = tasks.iter().filter(meant).filter(|t| t["Status"]["State"] == json!("running")).count();
    let mode = &service["Spec"]["Mode"];
    let desired = mode["Replicated"]["Replicas"].as_u64().unwrap_or_else(|| tasks.iter().filter(meant).count() as u64);
    service["ServiceStatus"] = json!({ "RunningTasks": running, "DesiredTasks": desired });
    service
}

/// `GET .../swarm/services/{id}`: the service and its tasks.
pub async fn inspect(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check(&id, "service")?;
    let d = endpoint(&app, env).await?;
    let service = call(&d, Method::GET, &format!("/services/{id}"), None).await?;
    let sid = service["ID"].as_str().unwrap_or(&id).to_string();
    let tasks = tasks_of(&d, &sid).await?;
    Ok(Json(json!({ "service": counted(service, &tasks), "tasks": tasks })))
}

#[derive(Deserialize, Default)]
pub struct TaskQuery {
    service: Option<String>,
    node: Option<String>,
    /// `running`, `shutdown` or `accepted`: Docker's desired state.
    state: Option<String>,
}

/// `GET .../swarm/tasks?service=&node=&state=`: tasks across the swarm.
pub async fn tasks(State(app): State<App>, access: Access, Path(env): Path<i64>, Query(q): Query<TaskQuery>) -> ApiResult<Json<Value>> {
    if access.limited() {
        return Ok(Json(json!([])));
    }
    let d = endpoint(&app, env).await?;
    let mut f = serde_json::Map::new();
    for (k, v) in [("service", &q.service), ("node", &q.node), ("desired-state", &q.state)] {
        if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
            f.insert(k.into(), json!([v]));
        }
    }
    let path = if f.is_empty() { "/tasks".to_string() } else { raw::path("/tasks", &[("filters", &Value::Object(f).to_string())]) };
    Ok(Json(json!(list_of(call(&d, Method::GET, &path, None).await?))))
}

/// A service spec Docker can take: an object with a name it takes and an image to run.
pub fn checked(spec: &Value) -> ApiResult<(String, String)> {
    if !spec.is_object() {
        return Err(ApiError::bad("A service is Docker's service spec, as JSON: an object."));
    }
    let name = spec["Name"].as_str().unwrap_or("").to_string();
    if !valid_name(&name) || name.len() > 63 {
        return Err(ApiError::bad("A service's name starts with a letter or digit, then letters, digits, _ . or -, at most 63.").field("name"));
    }
    let image = spec["TaskTemplate"]["ContainerSpec"]["Image"].as_str().unwrap_or("").trim().to_string();
    if image.is_empty() {
        return Err(ApiError::bad("A service needs an image to run.").field("image"));
    }
    Ok((name, image))
}

/// `POST .../swarm/services`: Docker's service spec, as Docker documents it, so every option it has is there.
pub async fn create(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(spec): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (name, image) = checked(&spec)?;
    use crate::access::security;
    security::hold(&app.db, &security::Judge::of(&admin.1), env, &[security::of_service(&spec)], None).await?;
    let d = endpoint(&app, env).await?;
    let r = call(&d, Method::POST, "/services/create", Some(&spec)).await?;
    log(&app, &admin, "swarm.service.create", name, Some(image)).await;
    Ok((StatusCode::CREATED, Json(r)))
}

fn name_of(s: &Value, id: &str) -> String {
    s["Spec"]["Name"].as_str().unwrap_or(id).to_string()
}

/// `PUT .../swarm/services/{id}`: the whole new spec, sent at the version Docker has now. Docker rolls the tasks over as
/// the spec's `UpdateConfig` says, and keeps the old spec for a roll back.
pub async fn update(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(spec): Json<Value>) -> ApiResult<Json<Value>> {
    check(&id, "service")?;
    let (_, image) = checked(&spec)?;
    let d = endpoint(&app, env).await?;
    // Held to the security settings for what it adds to the spec it replaces ("Access" 14).
    use crate::access::security;
    let rules = security::applies(&app.db, &security::Judge::of(&admin.1), env).await?;
    // Sent again only over the spec it was first written over: another person's change meanwhile is not overwritten.
    let mut was: Option<Value> = None;
    let (r, s) = versioned_change(&d, "services", &id, |s| {
        match &was {
            None => {
                if let Some(r) = &rules {
                    security::check(r, &[security::of_service(&spec)], &[security::of_service(&s["Spec"])], None)?;
                }
                was = Some(s["Spec"].clone());
            }
            Some(w) if *w != s["Spec"] => {
                return Err(ApiError::conflict("The service was changed by somebody else meanwhile. Open it again to see what it is now."));
            }
            Some(_) => {}
        }
        Ok((spec.clone(), false))
    })
    .await?;
    let name = name_of(&s, &id);
    log(&app, &admin, "swarm.service.update", name, Some(image)).await;
    Ok(Json(r))
}

/// The spec with `replicas` tasks. A global service has one on every node and no count to set.
pub fn scaled(mut spec: Value, replicas: u64) -> ApiResult<Value> {
    if spec["Mode"].get("Global").is_some() || spec["Mode"].get("GlobalJob").is_some() {
        return Err(ApiError::bad("A global service runs one task on every node; it has no count to scale."));
    }
    if spec["Mode"].get("ReplicatedJob").is_some() {
        spec["Mode"]["ReplicatedJob"]["TotalCompletions"] = json!(replicas);
        return Ok(spec);
    }
    if !spec["Mode"].is_object() {
        spec["Mode"] = json!({});
    }
    spec["Mode"]["Replicated"] = json!({ "Replicas": replicas });
    Ok(spec)
}

#[derive(Deserialize)]
pub struct Scale {
    replicas: u64,
}

/// `POST .../swarm/services/{id}/scale`: `{replicas}`.
pub async fn scale(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(s): Json<Scale>) -> ApiResult<Json<Value>> {
    check(&id, "service")?;
    if s.replicas > 10_000 {
        return Err(ApiError::bad("At most 10000 tasks.").field("replicas"));
    }
    let d = endpoint(&app, env).await?;
    let (r, svc) = versioned_change(&d, "services", &id, |svc| Ok((scaled(svc["Spec"].clone(), s.replicas)?, false))).await?;
    log(&app, &admin, "swarm.service.scale", name_of(&svc, &id), Some(format!("{} replicas", s.replicas))).await;
    Ok(Json(r))
}

/// `POST .../swarm/services/{id}/rollback`: back to the spec before the last update, as `docker service rollback`.
pub async fn rollback(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check(&id, "service")?;
    let d = endpoint(&app, env).await?;
    let (r, s) = versioned_change(&d, "services", &id, |s| {
        if s["PreviousSpec"].is_null() {
            return Err(ApiError::conflict("This service has not been updated, so there is nothing to roll back to."));
        }
        Ok((s["Spec"].clone(), true))
    })
    .await?;
    log(&app, &admin, "swarm.service.rollback", name_of(&s, &id), None).await;
    Ok(Json(r))
}

/// `DELETE .../swarm/services/{id}`: the service and its tasks.
pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    check(&id, "service")?;
    let d = endpoint(&app, env).await?;
    call(&d, Method::DELETE, &format!("/services/{id}"), None).await?;
    log(&app, &admin, "swarm.service.remove", id, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct LogQuery {
    tail: Option<String>,
    #[serde(default)]
    timestamps: bool,
}

/// The query Docker is asked for a service's logs: followed, both streams, a tail that is a number or `all`.
pub fn logs_path(id: &str, q: &LogQuery) -> String {
    let tail = match q.tail.as_deref() {
        Some("all") => "all".to_string(),
        Some(n) if n.parse::<u32>().is_ok() => n.to_string(),
        _ => "500".to_string(),
    };
    raw::path(&format!("/services/{id}/logs"), &[("follow", "true"), ("stdout", "true"), ("stderr", "true"), ("tail", &tail), ("timestamps", if q.timestamps { "true" } else { "false" })])
}

/// `GET .../swarm/services/{id}/logs/ws`: every task's logs together, the last lines then each new one, in the
/// container logs' messages: `{"s":"out"|"err","t":"text"}`, then `{"end":true}` or `{"error":...}`.
pub async fn logs_ws(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>, Query(q): Query<LogQuery>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    check(&id, "service")?;
    let d = endpoint(&app, env).await?;
    let service = call(&d, Method::GET, &format!("/services/{id}"), None).await?;
    let tty = service["Spec"]["TaskTemplate"]["ContainerSpec"]["TTY"] == json!(true);
    let body = raw::stream(&d, &logs_path(&id, &q)).await.map_err(super::not_manager)?;
    Ok(ws.on_upgrade(move |mut socket| async move {
        let mut frames = body.into_data_stream();
        let mut demux = raw::Demux::new(tty);
        loop {
            tokio::select! {
                next = frames.next() => match next {
                    Some(Ok(bytes)) => {
                        let pieces = match demux.push(&bytes) {
                            Ok(p) => p,
                            Err(e) => { let _ = socket.send(Message::Text(json!({ "error": e }).to_string().into())).await; break; }
                        };
                        for (s, piece) in pieces {
                            let msg = json!({ "s": s, "t": String::from_utf8_lossy(&piece) }).to_string();
                            if socket.send(Message::Text(msg.into())).await.is_err() { return; }
                        }
                    }
                    Some(Err(e)) => { let _ = socket.send(Message::Text(json!({ "error": format!("Docker's logs broke off: {e}") }).to_string().into())).await; break; }
                    None => { let _ = socket.send(Message::Text(json!({ "end": true }).to_string().into())).await; break; }
                },
                incoming = socket.recv() => if !matches!(incoming, Some(Ok(_))) { break },
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call as http};
    use crate::docker::swarm::tests::{change, people, versioned};

    #[test]
    fn a_spec_needs_a_name_docker_takes_and_an_image() {
        let ok = json!({ "Name": "web", "TaskTemplate": { "ContainerSpec": { "Image": "nginx:1" } } });
        assert_eq!(checked(&ok).unwrap(), ("web".into(), "nginx:1".into()));
        assert_eq!(checked(&json!({ "Name": "-web", "TaskTemplate": { "ContainerSpec": { "Image": "x" } } })).unwrap_err().field, Some("name"));
        assert_eq!(checked(&json!({ "Name": "web" })).unwrap_err().field, Some("image"));
        assert_eq!(checked(&json!([1])).unwrap_err().status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn one_service_s_counts_are_its_tasks_as_the_list_counts_them() {
        let t = |state: &str, want: &str| json!({ "Status": { "State": state }, "DesiredState": want });
        let tasks = [t("running", "running"), t("starting", "running"), t("shutdown", "shutdown"), t("running", "shutdown")];
        let s = counted(json!({ "Spec": { "Mode": { "Replicated": { "Replicas": 3 } } } }), &tasks);
        assert_eq!(s["ServiceStatus"], json!({ "RunningTasks": 1, "DesiredTasks": 3 }));
        let g = counted(json!({ "Spec": { "Mode": { "Global": {} } } }), &tasks);
        assert_eq!(g["ServiceStatus"], json!({ "RunningTasks": 1, "DesiredTasks": 2 }), "global: one wanted for each task meant to run");
    }

    #[test]
    fn scaling_sets_the_count_and_keeps_the_rest() {
        let spec = json!({ "Name": "web", "Mode": { "Replicated": { "Replicas": 1 } }, "Labels": { "a": "b" } });
        let s = scaled(spec, 4).unwrap();
        assert_eq!(s["Mode"]["Replicated"]["Replicas"], 4);
        assert_eq!(s["Labels"]["a"], "b");
        assert_eq!(scaled(json!({ "Name": "w" }), 2).unwrap()["Mode"]["Replicated"]["Replicas"], 2, "no mode is replicated");
        assert!(scaled(json!({ "Mode": { "Global": {} } }), 2).is_err(), "a global service has no count");
        assert_eq!(scaled(json!({ "Mode": { "ReplicatedJob": { "MaxConcurrent": 1 } } }), 3).unwrap()["Mode"]["ReplicatedJob"]["TotalCompletions"], 3);
    }

    #[test]
    fn logs_are_followed_with_a_tail_that_is_a_number_or_all() {
        let p = |t: Option<&str>| logs_path("web", &LogQuery { tail: t.map(str::to_string), timestamps: false });
        assert_eq!(p(Some("50")), "/services/web/logs?follow=true&stdout=true&stderr=true&tail=50&timestamps=false");
        assert!(p(Some("all")).contains("tail=all"));
        assert!(p(Some("1; rm")).contains("tail=500"));
    }

    #[tokio::test]
    async fn a_service_without_an_image_is_refused_before_docker_is_asked() {
        let a = app();
        let (admin, csrf, _, env) = people(&a).await;
        let (s, _, b) = http(&a, change("POST", &format!("/api/envs/{env}/swarm/services"), &admin, Some(&csrf), json!({ "Name": "web" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(b["field"], "image");
        let (s, _, b) = http(&a, change("POST", &format!("/api/envs/{env}/swarm/services/web/scale"), &admin, Some(&csrf), json!({ "replicas": 100000 }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    }

    /// Rolling back, scaling and updating read the service and send the change at the version read. When Swarm has
    /// moved the version in between ("update out of sequence"), the change is read and sent again, not refused.
    #[tokio::test]
    async fn a_change_meeting_a_version_swarm_moved_meanwhile_is_sent_again() {
        let new_spec = json!({ "Name": "web", "Mode": { "Replicated": { "Replicas": 1 } }, "TaskTemplate": { "ContainerSpec": { "Image": "nginx:2" } } });
        for (method, route, body, rollback) in [
            ("POST", "services/web/rollback", json!({}), true),
            ("POST", "services/web/scale", json!({ "replicas": 3 }), false),
            ("PUT", "services/web", new_spec, false),
        ] {
            let (sw, a) = versioned("services", None).await;
            let (admin, csrf, _, env) = people(&a).await;
            let (s, _, b) = http(&a, change(method, &format!("/api/envs/{env}/swarm/{route}"), &admin, Some(&csrf), body)).await;
            assert_eq!(s, StatusCode::OK, "{route}: {b}");
            assert_eq!(sw.lock().unwrap().updates, vec![(10, rollback), (11, rollback)], "{route}: refused at 10, sent again at 11");
        }
    }

    /// A whole new spec is sent again only while the service is still the one it was written over: when somebody else
    /// changed it meanwhile, theirs is not overwritten, and the update is refused as a conflict.
    #[tokio::test]
    async fn an_update_over_somebody_else_s_change_is_refused() {
        let theirs = json!({ "Name": "web", "Mode": { "Replicated": { "Replicas": 5 } }, "TaskTemplate": { "ContainerSpec": { "Image": "nginx:1" } } });
        let (sw, a) = versioned("services", Some(theirs.clone())).await;
        let (admin, csrf, _, env) = people(&a).await;
        let mine = json!({ "Name": "web", "Mode": { "Replicated": { "Replicas": 1 } }, "TaskTemplate": { "ContainerSpec": { "Image": "nginx:2" } } });
        let (s, _, b) = http(&a, change("PUT", &format!("/api/envs/{env}/swarm/services/web"), &admin, Some(&csrf), mine)).await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
        assert!(b["error"].as_str().unwrap().contains("changed"), "{b}");
        let sw = sw.lock().unwrap();
        assert_eq!((sw.updates.len(), &sw.spec), (1, &theirs), "theirs kept");
    }
}
