//! Swarm stacks: a Compose file deployed with Docker's own `docker stack deploy`, so it becomes the services, networks,
//! secrets and configs Docker would make of it on the command line ("Endpoints" 2). Docker keeps no Compose file, so
//! Freightdock keeps the one each stack was last deployed with (`swarm_stacks`), to edit and deploy again. A stack is
//! found by its services' `com.docker.stack.namespace` label, so one deployed elsewhere is listed too.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hyper::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Role, call, list_of, log};
use crate::access::Access;
use crate::api::{Allowed, Authed};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::docker::{Docker, cli, endpoint, raw, valid_name};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub const LABEL: &str = "com.docker.stack.namespace";
/// The longest Compose file taken.
pub const MAX_COMPOSE: usize = 1024 * 1024;
/// How long a deploy or a removal may take: `--detach` does not wait for tasks, but images are resolved at the registry.
const WITHIN: Duration = Duration::from_secs(300);

pub fn valid_stack(name: &str) -> bool {
    valid_name(name) && name.len() <= 63
}

/// A stack as the list shows it.
#[derive(Default, Debug, PartialEq)]
pub struct Summary {
    pub services: usize,
    pub running: u64,
    pub desired: u64,
}

/// The stacks the services belong to, by their label, with Docker's task counts (`ServiceStatus`, from `?status=true`).
pub fn summaries(services: &[Value]) -> BTreeMap<String, Summary> {
    let mut m: BTreeMap<String, Summary> = BTreeMap::new();
    for s in services {
        let Some(stack) = s["Spec"]["Labels"][LABEL].as_str() else { continue };
        let e = m.entry(stack.to_string()).or_default();
        e.services += 1;
        e.running += s["ServiceStatus"]["RunningTasks"].as_u64().unwrap_or(0);
        e.desired += s["ServiceStatus"]["DesiredTasks"].as_u64().unwrap_or(0);
    }
    m
}

async fn services_of(d: &Docker, stack: &str) -> ApiResult<Vec<Value>> {
    let filters = json!({ "label": [format!("{LABEL}={stack}")] }).to_string();
    Ok(list_of(call(d, Method::GET, &raw::path("/services", &[("status", "true"), ("filters", &filters)]), None).await?))
}

type Stored = (String, i64, Option<String>);

async fn stored(app: &App, env: i64, name: &str) -> ApiResult<Option<Stored>> {
    let name = name.to_string();
    Ok(app
        .db
        .call(move |c| Ok(c.query_row("SELECT compose, updated_at, updated_by FROM swarm_stacks WHERE env = ?1 AND name = ?2", params![env, name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?))
        .await?)
}

/// `GET .../swarm/stacks`: every stack, from the services' labels and from the files Freightdock keeps; `file` says
/// whether Freightdock has its Compose file.
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    // A swarm stack is the administrators' for a limited role ("Swarm" 7).
    if access.limited() {
        return Ok(Json(json!([])));
    }
    let d = endpoint(&app, env).await?;
    let services = list_of(call(&d, Method::GET, "/services?status=true", None).await?);
    let files: Vec<(String, i64)> = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT name, updated_at FROM swarm_stacks WHERE env = ?1")?;
            let rows = st.query_map([env], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    let mut by = summaries(&services);
    for (name, _) in &files {
        by.entry(name.clone()).or_default();
    }
    let out: Vec<Value> = by
        .into_iter()
        .map(|(name, s)| {
            let file = files.iter().find(|(n, _)| *n == name);
            json!({ "name": name, "services": s.services, "running": s.running, "desired": s.desired, "file": file.is_some(), "updated_at": file.map(|f| f.1) })
        })
        .collect();
    Ok(Json(json!(out)))
}

/// `GET .../swarm/stacks/{name}`: its services and the Compose file it was deployed with, when Freightdock has it.
pub async fn inspect(State(app): State<App>, _: Authed, Path((env, name)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    if !valid_stack(&name) {
        return Err(ApiError::bad("That is not a stack's name."));
    }
    let d = endpoint(&app, env).await?;
    let services = services_of(&d, &name).await?;
    let file = stored(&app, env, &name).await?;
    if services.is_empty() && file.is_none() {
        return Err(ApiError::not_found("stack"));
    }
    let (compose, updated_at, updated_by) = match file {
        Some((c, at, by)) => (Some(c), Some(at), by),
        None => (None, None, None),
    };
    Ok(Json(json!({ "name": name, "services": services, "compose": compose, "updated_at": updated_at, "updated_by": updated_by })))
}

#[derive(Deserialize)]
pub struct Deploy {
    pub name: String,
    pub compose: String,
}

pub fn checked(d: &Deploy) -> ApiResult<()> {
    if !valid_stack(&d.name) {
        return Err(ApiError::bad("A stack's name starts with a letter or digit, then letters, digits, _ . or -, at most 63.").field("name"));
    }
    if d.compose.trim().is_empty() {
        return Err(ApiError::bad("A stack needs its Compose file.").field("compose"));
    }
    if d.compose.len() > MAX_COMPOSE {
        return Err(ApiError::bad("A Compose file of at most 1 MB.").field("compose"));
    }
    reads_no_files(&d.compose).map_err(|m| ApiError::bad(m).field("compose"))
}

/// Keys whose value is a path the CLI would read: a secret's or a config's `file`, an `extends` file, a service's
/// `env_file` and `label_file`.
const READS: [&str; 3] = ["file", "env_file", "label_file"];

/// Refuses a Compose file that would have `docker stack deploy`, which runs on Freightdock's side, read a file there and
/// send it into the swarm ("Swarm" 8): any key of [`READS`] anywhere in it, after `<<:` merge keys are applied (as the
/// CLI does) and compared without case (as the CLI's decoder does), and any tagged node, whose meaning to the CLI's
/// parser might differ from this one's. No path is safe to allow: an absolute one is read as it stands, and a relative
/// one climbs out of the scratch folder with `..`.
fn reads_no_files(text: &str) -> Result<(), String> {
    let mut doc: yaml_serde::Value = yaml_serde::from_str(text).map_err(|e| format!("Not a Compose file: {e}"))?;
    doc.apply_merge().map_err(|e| format!("Not a Compose file: {e}"))?;
    let mut todo = vec![&doc];
    while let Some(v) = todo.pop() {
        match v {
            yaml_serde::Value::Mapping(m) => {
                for (k, v) in m {
                    if let Some(k) = k.as_str().filter(|k| READS.iter().any(|r| r.eq_ignore_ascii_case(k))) {
                        return Err(format!(
                            "\"{k}\" would be read from the Freightdock server, not from the swarm: a stack's secrets and configs are made on the Swarm pages (and named here as external: true), and its variables are written in the file."
                        ));
                    }
                    todo.push(k);
                    todo.push(v);
                }
            }
            yaml_serde::Value::Sequence(s) => todo.extend(s),
            yaml_serde::Value::Tagged(_) => return Err("A Compose file for a swarm stack has no YAML tags (\"!name\").".into()),
            _ => {}
        }
    }
    Ok(())
}

/// The last lines of what the CLI said, for a person to read.
pub fn said(out: &str, err: &str) -> String {
    let all = format!("{}\n{}", out.trim(), err.trim());
    let lines: Vec<&str> = all.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(12)..].join("\n")
}

/// Refuses a host that is not a swarm manager before the CLI is run, in the words the routes use.
async fn manager(d: &Docker) -> ApiResult<()> {
    let info = raw::call(d, Method::GET, "/info", None).await?;
    if Role::of(&info) != Role::Manager {
        return Err(ApiError::conflict("This host is not a swarm manager: a stack is deployed to a swarm."));
    }
    Ok(())
}

/// A folder of its own for the CLI to run in, empty, so a relative path in the Compose file finds nothing of
/// Freightdock's; removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> ApiResult<Self> {
        let p = std::env::temp_dir().join(format!("freightdock-stack-{}", hex::encode(rand::random::<[u8; 8]>())));
        std::fs::create_dir(&p).map_err(|e| ApiError::from(anyhow::anyhow!("making {}: {e}", p.display())))?;
        Ok(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `POST .../swarm/stacks`: `{name, compose}`, deployed (or deployed again: services no longer in the file are
/// removed, `--prune`), and the file kept for the next time.
pub async fn deploy(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(body): Json<Deploy>) -> ApiResult<Json<Value>> {
    checked(&body)?;
    // Held to the host's security settings, service by service, from the file Compose resolves ("Access" 13).
    use crate::access::security;
    if let Some(r) = security::applies(&app.db, &security::Judge::of(&admin.1), env).await? {
        let config = crate::stacks::resolve_text(&app, &body.compose).await.map_err(|m| ApiError::bad(format!("The file could not be checked against this host's security settings: {m}")).field("compose"))?;
        security::check(&r, &security::of_compose(&config), &[], Some("compose"))?;
    }
    let d = endpoint(&app, env).await?;
    manager(&d).await?;
    let again = !services_of(&d, &body.name).await?.is_empty();
    let host = cli::docker_host(&app, env).await?;
    let dir = Scratch::new()?;
    let ran = cli::run(&cli::binary(), &host, &["stack", "deploy", "--detach=true", "--prune", "--compose-file", "-", &body.name], Some(&body.compose), Some(&dir.0), WITHIN).await?;
    if !ran.ok {
        return Err(ApiError::bad(format!("Docker did not deploy {}:\n{}", body.name, said(&ran.stdout, &ran.stderr))).field("compose"));
    }
    let by = admin.0.session.user.name.clone();
    let (name, compose) = (body.name.clone(), body.compose.clone());
    app.db
        .call(move |c| {
            c.execute(
                "INSERT INTO swarm_stacks (env, name, compose, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (env, name) DO UPDATE SET compose = excluded.compose, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
                params![env, name, compose, now(), by],
            )?;
            Ok(())
        })
        .await?;
    log(&app, &admin, "swarm.stack.deploy", body.name, Some(if again { "deployed again".into() } else { "deployed".into() })).await;
    Ok(Json(json!({ "output": said(&ran.stdout, &ran.stderr) })))
}

/// `DELETE .../swarm/stacks/{name}`: `docker stack rm`: its services, networks, secrets and configs; and its file.
pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, name)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    if !valid_stack(&name) {
        return Err(ApiError::bad("That is not a stack's name."));
    }
    let d = endpoint(&app, env).await?;
    let running = !services_of(&d, &name).await?.is_empty();
    if !running && stored(&app, env, &name).await?.is_none() {
        return Err(ApiError::not_found("stack"));
    }
    if running {
        let host = cli::docker_host(&app, env).await?;
        let ran = cli::run(&cli::binary(), &host, &["stack", "rm", &name], None, None, WITHIN).await?;
        if !ran.ok {
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("Docker did not remove {name}:\n{}", said(&ran.stdout, &ran.stderr))));
        }
    }
    let n = name.clone();
    app.db.call(move |c| Ok(c.execute("DELETE FROM swarm_stacks WHERE env = ?1 AND name = ?2", params![env, n])?)).await?;
    log(&app, &admin, "swarm.stack.remove", name, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call as http};
    use crate::docker::swarm::tests::{change, people};

    #[test]
    fn services_are_gathered_into_their_stacks_with_their_tasks() {
        let svc = |stack: Option<&str>, run: u64, want: u64| {
            let labels = stack.map_or(json!({}), |s| json!({ LABEL: s }));
            json!({ "Spec": { "Labels": labels }, "ServiceStatus": { "RunningTasks": run, "DesiredTasks": want } })
        };
        let m = summaries(&[svc(Some("shop"), 2, 2), svc(Some("shop"), 0, 1), svc(None, 1, 1), svc(Some("blog"), 1, 1)]);
        assert_eq!(m.len(), 2);
        assert_eq!(m["shop"], Summary { services: 2, running: 2, desired: 3 });
        assert_eq!(m["blog"], Summary { services: 1, running: 1, desired: 1 });
    }

    #[test]
    fn a_deploy_needs_a_name_and_a_file() {
        assert!(checked(&Deploy { name: "shop".into(), compose: "services: {}".into() }).is_ok());
        assert_eq!(checked(&Deploy { name: "a/b".into(), compose: "x".into() }).unwrap_err().field, Some("name"));
        assert_eq!(checked(&Deploy { name: "shop".into(), compose: " \n".into() }).unwrap_err().field, Some("compose"));
        assert_eq!(checked(&Deploy { name: "shop".into(), compose: "x".repeat(MAX_COMPOSE + 1) }).unwrap_err().field, Some("compose"));
    }

    #[test]
    fn a_file_that_would_have_the_cli_read_freightdock_s_own_files_is_refused() {
        // `docker stack deploy` runs on Freightdock's side: a secret's or a config's `file:`, an `env_file` or an
        // `extends` file would be read there and sent into the swarm ("Swarm" 8).
        let deploy = |compose: &str| checked(&Deploy { name: "shop".into(), compose: compose.into() });
        let refused = |compose: &str| {
            let e = deploy(compose).expect_err(compose);
            assert_eq!((e.status, e.field), (StatusCode::BAD_REQUEST, Some("compose")), "{compose}: {}", e.message);
        };
        refused("services: {web: {image: nginx}}\nsecrets:\n  key:\n    file: /data/secret.key\n");
        refused("services: {web: {image: nginx}}\nconfigs:\n  ca:\n    file: ../../data/ca/ca.key\n");
        refused("services:\n  web:\n    image: nginx\n    env_file: /proc/self/environ\n");
        refused("services:\n  web:\n    image: nginx\n    env_file:\n      - path: .env\n");
        refused("services:\n  web:\n    image: nginx\n    extends: {file: /etc/passwd, service: x}\n");
        // The CLI merges `<<:` keys, so a file hidden in an anchor is found all the same, and a quoted or escaped key
        // is the same key.
        refused("x-src: &src {file: /data/secret.key}\nservices: {web: {image: nginx}}\nsecrets:\n  key:\n    <<: *src\n");
        refused("services: {web: {image: nginx}}\nsecrets:\n  key: {\"\\x66ile\": /data/secret.key}\n");
        refused("services: {web: {image: nginx}}\nsecrets:\n  key: !custom {file: /data/secret.key}\n");
        refused("services: [");
        // Secrets and configs made in the swarm, or named external, are what a stack uses instead.
        assert!(deploy("services:\n  web:\n    image: nginx\n    secrets: [key]\n    environment: {A: \"${A:-1}\"}\nsecrets:\n  key: {external: true}\n").is_ok());
    }

    #[test]
    fn what_the_cli_said_is_its_last_lines() {
        assert_eq!(said("Creating service shop_web\n", "\n"), "Creating service shop_web");
        let long: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let s = said(&long, "failed");
        assert!(s.starts_with("line 10") && s.ends_with("failed"), "{s}");
    }

    #[tokio::test]
    async fn a_stack_without_a_file_is_refused_before_docker_is_asked() {
        let a = app();
        let (admin, csrf, _, env) = people(&a).await;
        let (s, _, b) = http(&a, change("POST", &format!("/api/envs/{env}/swarm/stacks"), &admin, Some(&csrf), json!({ "name": "shop", "compose": "" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(b["field"], "compose");
    }
}
