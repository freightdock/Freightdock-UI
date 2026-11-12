//! Secrets and configs: made, listed with the services that use them, inspected, removed. A secret's value goes into
//! Docker once and never comes back out of Freightdock: Docker does not give it, and anything that looks like it is taken
//! out before an answer leaves ("Swarm" 4). A config is not secret, so its text is shown.

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine;
use hyper::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{call, check, list_of, log};
use crate::api::{Allowed, Authed};
use crate::docker::{Docker, endpoint, valid_name};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Secret,
    Config,
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Kind::Secret => "/secrets",
            Kind::Config => "/configs",
        }
    }
    fn word(self) -> &'static str {
        match self {
            Kind::Secret => "secret",
            Kind::Config => "config",
        }
    }
    /// The field of a service's container spec that names them, and the reference's id field.
    fn refs(self) -> (&'static str, &'static str) {
        match self {
            Kind::Secret => ("Secrets", "SecretID"),
            Kind::Config => ("Configs", "ConfigID"),
        }
    }
}

/// A secret as it may leave Freightdock: whatever Docker answered, without `Spec.Data`.
pub fn without_value(mut v: Value) -> Value {
    if let Some(spec) = v.get_mut("Spec").and_then(Value::as_object_mut) {
        spec.remove("Data");
    }
    v
}

/// A config with its data as text beside it (`text`, null when it is not UTF-8).
pub fn with_text(mut v: Value) -> Value {
    let text = v["Spec"]["Data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()).and_then(|b| String::from_utf8(b).ok());
    v["text"] = json!(text);
    v
}

/// Secret or config id to the names of the services that use it.
pub fn users_of(services: &[Value], kind: Kind) -> HashMap<String, Vec<String>> {
    let (field, id) = kind.refs();
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for s in services {
        let name = s["Spec"]["Name"].as_str().unwrap_or("").to_string();
        for r in s["Spec"]["TaskTemplate"]["ContainerSpec"][field].as_array().into_iter().flatten() {
            if let Some(i) = r[id].as_str() {
                let users = m.entry(i.to_string()).or_default();
                if !users.contains(&name) {
                    users.push(name.clone());
                }
            }
        }
    }
    m
}

async fn list(app: &App, env: i64, kind: Kind) -> ApiResult<Json<Value>> {
    let d = endpoint(app, env).await?;
    let (items, services) = tokio::try_join!(call(&d, Method::GET, kind.path(), None), call(&d, Method::GET, "/services", None))?;
    let users = users_of(&list_of(services), kind);
    let out: Vec<Value> = list_of(items)
        .into_iter()
        .map(|v| {
            let used = users.get(v["ID"].as_str().unwrap_or("")).cloned().unwrap_or_default();
            // A list carries no values: a secret's never, a config's on its own page.
            let mut v = without_value(v);
            v["services"] = json!(used);
            v
        })
        .collect();
    Ok(Json(json!(out)))
}

async fn inspect(app: &App, env: i64, id: &str, kind: Kind) -> ApiResult<Json<Value>> {
    check(id, kind.word())?;
    let d = endpoint(app, env).await?;
    let one = format!("{}/{id}", kind.path());
    let (v, services) = tokio::try_join!(call(&d, Method::GET, &one, None), call(&d, Method::GET, "/services", None))?;
    let used = users_of(&list_of(services), kind).remove(v["ID"].as_str().unwrap_or("")).unwrap_or_default();
    let mut v = match kind {
        Kind::Secret => without_value(v),
        Kind::Config => with_text(v),
    };
    v["services"] = json!(used);
    Ok(Json(v))
}

#[derive(Deserialize, Debug)]
pub struct New {
    pub name: String,
    /// The value as text.
    pub data: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Docker's limit for one secret or config.
pub const MAX_BYTES: usize = 500 * 1024;

/// The spec Docker is sent for a new one.
pub fn spec(n: &New) -> ApiResult<Value> {
    if !valid_name(&n.name) || n.name.len() > 64 {
        return Err(ApiError::bad("A name starts with a letter or digit, then letters, digits, _ . or -, at most 64.").field("name"));
    }
    if n.data.is_empty() {
        return Err(ApiError::bad("It needs a value.").field("data"));
    }
    if n.data.len() > MAX_BYTES {
        return Err(ApiError::bad("Docker keeps at most 500 KB in one.").field("data"));
    }
    Ok(json!({ "Name": n.name, "Labels": n.labels, "Data": base64::engine::general_purpose::STANDARD.encode(n.data.as_bytes()) }))
}

async fn create(app: &App, admin: &Allowed, env: i64, kind: Kind, n: New) -> ApiResult<(StatusCode, Json<Value>)> {
    let body = spec(&n)?;
    let d: Docker = endpoint(app, env).await?;
    let r = call(&d, Method::POST, &format!("{}/create", kind.path()), Some(&body)).await?;
    let action = match kind {
        Kind::Secret => "swarm.secret.create",
        Kind::Config => "swarm.config.create",
    };
    log(app, admin, action, n.name, Some(format!("{} bytes", n.data.len()))).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": r["ID"] }))))
}

async fn remove(app: &App, admin: &Allowed, env: i64, id: String, kind: Kind) -> ApiResult<StatusCode> {
    check(&id, kind.word())?;
    let d = endpoint(app, env).await?;
    call(&d, Method::DELETE, &format!("{}/{id}", kind.path()), None).await?;
    let action = match kind {
        Kind::Secret => "swarm.secret.remove",
        Kind::Config => "swarm.config.remove",
    };
    log(app, admin, action, id, None).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_secrets(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    list(&app, env, Kind::Secret).await
}
pub async fn list_configs(State(app): State<App>, _: Authed, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    list(&app, env, Kind::Config).await
}
pub async fn inspect_secret(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    inspect(&app, env, &id, Kind::Secret).await
}
pub async fn inspect_config(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    inspect(&app, env, &id, Kind::Config).await
}
pub async fn create_secret(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(n): Json<New>) -> ApiResult<(StatusCode, Json<Value>)> {
    create(&app, &admin, env, Kind::Secret, n).await
}
pub async fn create_config(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(n): Json<New>) -> ApiResult<(StatusCode, Json<Value>)> {
    create(&app, &admin, env, Kind::Config, n).await
}
pub async fn remove_secret(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    remove(&app, &admin, env, id, Kind::Secret).await
}
pub async fn remove_config(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    remove(&app, &admin, env, id, Kind::Config).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_leaves_with_its_value() {
        let v = without_value(json!({ "ID": "s1", "Spec": { "Name": "db", "Data": "aHVudGVyMg==", "Labels": {} } }));
        assert!(v["Spec"].get("Data").is_none());
        assert_eq!(v["Spec"]["Name"], "db");
    }

    #[test]
    fn a_config_s_text_is_shown_beside_it() {
        let v = with_text(json!({ "Spec": { "Data": "bGlzdGVuIDgw" } }));
        assert_eq!(v["text"], "listen 80");
        assert_eq!(with_text(json!({ "Spec": { "Data": "//79" } }))["text"], Value::Null, "not UTF-8");
    }

    #[test]
    fn a_new_one_is_named_as_docker_takes_and_its_value_goes_as_base64() {
        let s = spec(&New { name: "db_password".into(), data: "hunter2".into(), labels: BTreeMap::new() }).unwrap();
        assert_eq!(s["Data"], "aHVudGVyMg==");
        assert_eq!(spec(&New { name: "a b".into(), data: "x".into(), labels: BTreeMap::new() }).unwrap_err().field, Some("name"));
        assert_eq!(spec(&New { name: "a".into(), data: String::new(), labels: BTreeMap::new() }).unwrap_err().field, Some("data"));
        assert_eq!(spec(&New { name: "a".into(), data: "x".repeat(MAX_BYTES + 1), labels: BTreeMap::new() }).unwrap_err().field, Some("data"));
    }

    #[test]
    fn a_secret_knows_the_services_that_use_it() {
        let svc = |name: &str, secrets: Value| json!({ "Spec": { "Name": name, "TaskTemplate": { "ContainerSpec": { "Secrets": secrets } } } });
        let services = [svc("api", json!([{ "SecretID": "s1" }, { "SecretID": "s2" }])), svc("worker", json!([{ "SecretID": "s1" }])), json!({ "Spec": { "Name": "plain" } })];
        let m = users_of(&services, Kind::Secret);
        assert_eq!(m["s1"], vec!["api", "worker"]);
        assert_eq!(m["s2"], vec!["api"]);
        assert!(users_of(&services, Kind::Config).is_empty());
    }
}
