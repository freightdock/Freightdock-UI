//! Roles and ownership, end to end, against the real binary and the dind ("Access" in DECISIONS.md): users signed in
//! as each role through the API, as the UI does, are let do what their role allows and refused the rest, and a
//! private container is not there for somebody it is not shared with.

use anyhow::{Context, Result, ensure};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::{IMAGE, Qa, step};

const PASSWORD: &str = "a long enough qa password";

impl Qa {
    /// The administrator makes a local user, and the suite signs in as them: a Qa that speaks as that user.
    async fn person(&self, name: &str) -> Result<Qa> {
        let (s, b) = self.post(&self.url("/api/users"), json!({ "name": name, "password": PASSWORD })).await?;
        ensure!(s == StatusCode::CREATED, "making {name}: {s}: {b}");
        let r = self.http.post(self.url("/api/login")).header("origin", &self.base).json(&json!({ "name": name, "password": PASSWORD })).send().await?;
        let cookie = r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|v| v.starts_with("freightdock_session=")).map(|v| v.split(';').next().unwrap_or_default().to_string());
        ensure!(r.status() == StatusCode::OK, "signing in as {name}: {}", r.status());
        let mut qa = Qa { http: self.http.clone(), base: self.base.clone(), cookie: cookie.context("no session cookie")?, csrf: String::new(), env: self.env };
        qa.csrf = qa.get_json(&qa.url("/api/me")).await?["csrf"].as_str().context("no csrf")?.to_string();
        Ok(qa)
    }

    fn user_id(list: &Value, name: &str) -> Result<i64> {
        list.as_array().and_then(|l| l.iter().find(|u| u["name"] == json!(name))).and_then(|u| u["id"].as_i64()).with_context(|| format!("no {name} in {list}"))
    }

    /// Gives `who` (user ids) and `teams` their roles on this environment.
    async fn grant(&self, users: &[(i64, &str)], teams: &[(i64, &str)]) -> Result<()> {
        let body = json!({ "users": users.iter().map(|(id, r)| json!({ "id": id, "role": r })).collect::<Vec<_>>(), "teams": teams.iter().map(|(id, r)| json!({ "id": id, "role": r })).collect::<Vec<_>>() });
        let (s, b) = self.send(Method::PUT, &self.url(&format!("/api/environments/{}/access", self.env)), Some(body)).await?;
        ensure!(s == StatusCode::NO_CONTENT, "grants: {s}: {b}");
        Ok(())
    }

    async fn status_of(&self, method: Method, path: &str, body: Option<Value>) -> Result<StatusCode> {
        Ok(self.send(method, &self.env(path), body).await?.0)
    }

    /// The names of the containers this person sees.
    async fn seen(&self) -> Result<Vec<String>> {
        Ok(super::names(&self.containers().await?))
    }

    /// The status a console opening answers with: 101 when it opens.
    async fn console_status(&self, id: &str) -> Result<u16> {
        let url = self.env(&format!("/containers/{id}/exec/ws?cmd=sh")).replacen("http", "ws", 1);
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("cookie", self.cookie.parse()?);
        req.headers_mut().insert("origin", self.base.parse()?);
        Ok(match tokio_tungstenite::connect_async(req).await {
            Ok(_) => 101,
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => r.status().as_u16(),
            Err(e) => return Err(e.into()),
        })
    }
}

pub async fn steps(qa: &Qa) {
    step("an administrator makes a container everybody with a role may see", async {
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-shared", "access": { "level": "public" }, "config": { "Image": IMAGE, "Cmd": ["sh", "-c", "echo shared-hello; exec sleep 3600"] } })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        let c = qa.inspect("qa-shared").await?.1;
        ensure!(c["freightdock_access"]["level"] == json!("public"), "its access: {}", c["freightdock_access"]);
        ensure!(c["Config"]["Labels"]["freightdock.owner"] == json!("qa-admin"), "the owner label: {}", c["Config"]["Labels"]);
        let (s, made) = qa.post(&qa.env("/containers"), json!({ "name": "qa-private", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"] } })).await?;
        ensure!(s == StatusCode::CREATED, "create: {s}: {made}");
        Ok(())
    })
    .await;

    let viewer = step("a read-only user signs in and sees no host until given a role", async {
        let v = qa.person("qa-viewer").await?;
        let envs = v.get_json(&v.url("/api/environments")).await?;
        ensure!(envs == json!([]), "before a role: {envs}");
        let users = qa.get_json(&qa.url("/api/users")).await?;
        qa.grant(&[(Qa::user_id(&users, "qa-viewer")?, "read_only")], &[]).await?;
        let envs = v.get_json(&v.url("/api/environments")).await?;
        ensure!(envs[0]["role"] == json!("read_only") && envs[0]["actions"] == json!(["read"]), "after: {envs}");
        Ok(v)
    })
    .await;

    step("the read-only user sees the public container and its logs, and not the private one", async {
        let seen = viewer.seen().await?;
        ensure!(seen == ["qa-shared"], "they see {seen:?}");
        qa.until(std::time::Duration::from_secs(10), || async {
            let logs = viewer.logs("qa-shared").await?;
            ensure!(logs.contains("shared-hello"), "logs: {logs}");
            Ok(())
        })
        .await?;
        let (s, _) = viewer.inspect("qa-private").await?;
        ensure!(s == StatusCode::NOT_FOUND, "the private container: {s}");
        Ok(())
    })
    .await;

    step("the read-only user is refused a stop, a console and a create", async {
        let s = viewer.status_of(Method::POST, "/containers/qa-shared/stop", None).await?;
        ensure!(s == StatusCode::FORBIDDEN, "stop: {s}");
        let c = viewer.console_status("qa-shared").await?;
        ensure!(c == 403, "console: {c}");
        let s = viewer.status_of(Method::POST, "/containers", Some(json!({ "name": "qa-nope", "config": { "Image": IMAGE } }))).await?;
        ensure!(s == StatusCode::FORBIDDEN, "create: {s}");
        ensure!(qa.running("qa-shared").await?, "it still runs");
        Ok(())
    })
    .await;

    let op = step("an operator stops and starts a container, opens its console, and may not remove it", async {
        let op = qa.person("qa-operator").await?;
        let users = qa.get_json(&qa.url("/api/users")).await?;
        qa.grant(&[(Qa::user_id(&users, "qa-viewer")?, "read_only"), (Qa::user_id(&users, "qa-operator")?, "operator")], &[]).await?;
        let s = op.status_of(Method::POST, "/containers/qa-private/stop", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "an operator sees and stops everything: {s}");
        ensure!(!qa.running("qa-private").await?, "it stopped");
        let s = op.status_of(Method::POST, "/containers/qa-private/start", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "start: {s}");
        let c = op.console_status("qa-shared").await?;
        ensure!(c == 101, "console: {c}");
        let s = op.status_of(Method::DELETE, "/containers/qa-shared?force=true", None).await?;
        ensure!(s == StatusCode::FORBIDDEN, "remove: {s}");
        let s = op.status_of(Method::POST, "/containers", Some(json!({ "name": "qa-nope", "config": { "Image": IMAGE } }))).await?;
        ensure!(s == StatusCode::FORBIDDEN, "create: {s}");
        Ok(op)
    })
    .await;

    let group = step("a team given Helpdesk on a group reads every container of its hosts, and changes none", async {
        let helper = qa.person("qa-helper").await?;
        let users = qa.get_json(&qa.url("/api/users")).await?;
        let (s, team) = qa.post(&qa.url("/api/teams"), json!({ "name": "qa-helpdesk", "members": [Qa::user_id(&users, "qa-helper")?] })).await?;
        ensure!(s == StatusCode::CREATED, "team: {s}: {team}");
        let (s, g) = qa.post(&qa.url("/api/environment-groups"), json!({ "name": "qa-access", "environments": [qa.env] })).await?;
        ensure!(s == StatusCode::CREATED, "group: {s}: {g}");
        let gid = g["id"].as_i64().context("group id")?;
        let (s, b) = qa.send(Method::PUT, &qa.url(&format!("/api/environment-groups/{gid}/access")), Some(json!({ "teams": [{ "id": team["id"], "role": "helpdesk" }] }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "group grants: {s}: {b}");
        let envs = helper.get_json(&helper.url("/api/environments")).await?;
        ensure!(envs[0]["role"] == json!("helpdesk"), "through the team on the group: {envs}");
        let seen = helper.seen().await?;
        ensure!(seen.contains(&"qa-private".to_string()) && seen.contains(&"qa-shared".to_string()), "Helpdesk reads everything: {seen:?}");
        let s = helper.status_of(Method::POST, "/containers/qa-shared/restart", None).await?;
        ensure!(s == StatusCode::FORBIDDEN, "restart: {s}");
        let c = helper.console_status("qa-shared").await?;
        ensure!(c == 403, "console: {c}");
        Ok(gid)
    })
    .await;

    let std = step("a standard user sees only what is shared and their own, and what they make is theirs", async {
        let std = qa.person("qa-standard").await?;
        let users = qa.get_json(&qa.url("/api/users")).await?;
        qa.grant(&[(Qa::user_id(&users, "qa-viewer")?, "read_only"), (Qa::user_id(&users, "qa-standard")?, "standard")], &[]).await?;
        ensure!(std.seen().await? == ["qa-shared"], "before: {:?}", std.seen().await?);
        let (s, _) = std.inspect("qa-private").await?;
        ensure!(s == StatusCode::NOT_FOUND, "the administrator's private container is not there for them: {s}");
        let s = std.status_of(Method::POST, "/containers/qa-private/stop", None).await?;
        ensure!(s == StatusCode::NOT_FOUND, "nor can they stop it: {s}");
        let (s, made) = std.post(&std.env("/containers"), json!({ "name": "qa-mine", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"] } })).await?;
        ensure!(s == StatusCode::CREATED, "their own: {s}: {made}");
        let mut seen = std.seen().await?;
        seen.sort();
        ensure!(seen == ["qa-mine", "qa-shared"], "after: {seen:?}");
        ensure!(viewer.seen().await? == ["qa-shared"], "the read-only user does not see qa-mine: {:?}", viewer.seen().await?);
        let s = std.status_of(Method::DELETE, "/containers/qa-mine?force=true", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "they remove their own: {s}");
        Ok(std)
    })
    .await;

    owned(qa, &std, &viewer).await;
    security(qa, &std, &op, group).await;

    step("the auth log has the sign-ins, and both logs export as CSV", async {
        let log = qa.get_json(&qa.url("/api/auth-log?event=login&user=qa-viewer")).await?;
        ensure!(log["entries"].as_array().is_some_and(|e| !e.is_empty() && e[0]["ok"] == json!(true)), "qa-viewer's sign-in: {log}");
        for path in ["/api/auth-log?format=csv", "/api/audit?format=csv&action=access"] {
            let (s, body) = qa.get(&qa.url(path)).await?;
            ensure!(s == StatusCode::OK && body.lines().count() > 1, "{path}: {s}: {body}");
        }
        let (s, _) = viewer.get(&viewer.url("/api/auth-log")).await?;
        ensure!(s == StatusCode::FORBIDDEN, "the auth log is an administrator's: {s}");
        Ok(())
    })
    .await;

    step("the people, the team, the group and the containers go again", async {
        for c in ["qa-shared", "qa-private"] {
            let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{c}?force=true")), None).await?;
            ensure!(s == StatusCode::NO_CONTENT, "remove {c}: {s}: {b}");
        }
        let users = qa.get_json(&qa.url("/api/users")).await?;
        for u in ["qa-viewer", "qa-operator", "qa-helper", "qa-standard"] {
            let (s, b) = qa.send(Method::DELETE, &qa.url(&format!("/api/users/{}", Qa::user_id(&users, u)?)), None).await?;
            ensure!(s == StatusCode::NO_CONTENT, "remove {u}: {s}: {b}");
        }
        let teams = qa.get_json(&qa.url("/api/teams")).await?;
        let (s, _) = qa.send(Method::DELETE, &qa.url(&format!("/api/teams/{}", Qa::user_id(&teams, "qa-helpdesk")?)), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "team: {s}");
        let (s, _) = qa.send(Method::DELETE, &qa.url(&format!("/api/environment-groups/{group}")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "group: {s}");
        Ok(())
    })
    .await;
}

/// The security settings ("Access" 12 to 17), on the group and then the environment, against the real Docker and
/// Compose: the standard user refused what they turn off, and allowed what they do not.
async fn security(qa: &Qa, std: &Qa, op: &Qa, group: i64) {
    let privileged = || json!({ "name": "qa-priv", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"], "HostConfig": { "Privileged": true } } });
    let bind = |path: &str| json!({ "name": "qa-bind", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"], "HostConfig": { "Binds": [format!("{path}:/data")] } } });

    step("the group locked down: the standard user is refused a privileged container and a host path, on the field", async {
        let mut rules = qa.get_json(&qa.url(&format!("/api/environment-groups/{group}/security"))).await?["presets"]["locked_down"].clone();
        rules["bind_allow"] = json!(["/tmp/qa-allowed"]);
        let (s, b) = qa.send(Method::PUT, &qa.url(&format!("/api/environment-groups/{group}/security")), Some(json!({ "rules": rules }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the group's settings: {s}: {b}");
        let e = qa.get_json(&qa.url(&format!("/api/environments/{}/security", qa.env))).await?;
        ensure!(e["source"] == json!("group"), "the environment follows its group: {e}");
        let (s, b) = std.post(&std.env("/containers"), privileged()).await?;
        ensure!(s == StatusCode::FORBIDDEN && b["field"] == json!("privileged"), "privileged: {s}: {b}");
        let (s, b) = std.post(&std.env("/containers"), bind("/etc")).await?;
        ensure!(s == StatusCode::FORBIDDEN && b["field"] == json!("binds") && b["error"].as_str().unwrap_or_default().contains("/etc"), "a host path: {s}: {b}");
        let (s, b) = std.post(&std.env("/containers"), bind("/tmp/qa-allowed/data")).await?;
        ensure!(s == StatusCode::CREATED, "under the allow-list: {s}: {b}");
        ensure!(std.running("qa-bind").await?, "it runs");
        let s = std.status_of(Method::DELETE, "/containers/qa-bind?force=true", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "removed: {s}");
        let (s, b) = qa.post(&qa.env("/containers"), privileged()).await?;
        ensure!(s == StatusCode::CREATED, "an administrator is above the settings: {s}: {b}");
        let (s, _) = qa.send(Method::DELETE, &qa.env("/containers/qa-priv?force=true"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "removed: {s}");
        Ok(())
    })
    .await;

    step("a stack on the host's network is refused to the standard user, naming the service, by Compose's own reading", async {
        let compose = format!("services:\n  web:\n    image: {IMAGE}\n    command: [\"sleep\", \"3600\"]\n    network_mode: ${{NET:-host}}\n");
        let (s, b) = std.post(&std.env("/stacks"), json!({ "name": "qa-hostnet", "compose": compose, "env": "" })).await?;
        ensure!(s == StatusCode::CREATED, "made (not yet deployed): {s}: {b}");
        let id = b["id"].as_i64().context("stack id")?;
        let (s, b) = std.send(Method::POST, &std.env(&format!("/stacks/{id}/deploy")), Some(json!({}))).await?;
        ensure!(s == StatusCode::FORBIDDEN && b["field"] == json!("compose"), "deploy: {s}: {b}");
        let e = b["error"].as_str().unwrap_or_default();
        ensure!(e.contains("Service web") && e.contains("network"), "the variable filled in, the service named: {e}");
        let (s, b) = std.send(Method::PUT, &std.env(&format!("/stacks/{id}")), Some(json!({ "compose": compose, "env": "NET=bridge\n" }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "its variables: {s}: {b}");
        std.stack_job(id, "deploy", json!({})).await?;
        ensure!(std.running("qa-hostnet-web-1").await?, "on its own network it deploys");
        std.stack_job(id, "remove", json!({})).await?;
        Ok(())
    })
    .await;

    step("the environment's own settings win: privileged allowed again, consoles closed to the operator", async {
        let users = qa.get_json(&qa.url("/api/users")).await?;
        qa.grant(&[(Qa::user_id(&users, "qa-viewer")?, "read_only"), (Qa::user_id(&users, "qa-standard")?, "standard"), (Qa::user_id(&users, "qa-operator")?, "operator")], &[]).await?;
        ensure!(op.console_status("qa-shared").await? == 101, "a console while the group's settings leave them open");
        let (s, b) = qa.send(Method::PUT, &qa.url(&format!("/api/environments/{}/security", qa.env)), Some(json!({ "rules": { "block_consoles": true } }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the environment's own: {s}: {b}");
        let c = op.console_status("qa-shared").await?;
        ensure!(c == 403, "a console: {c}");
        let envs = op.get_json(&op.url("/api/environments")).await?;
        ensure!(!envs[0]["actions"].as_array().is_some_and(|a| a.contains(&json!("console"))), "the UI is told: {envs}");
        let s = op.status_of(Method::POST, "/containers/qa-shared/restart", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the rest of an operator's work: {s}");
        let (s, b) = std.post(&std.env("/containers"), privileged()).await?;
        ensure!(s == StatusCode::CREATED, "the group's rules no longer hold here: {s}: {b}");
        let s = std.status_of(Method::DELETE, "/containers/qa-priv?force=true", None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "removed: {s}");
        for path in [format!("/api/environments/{}/security", qa.env), format!("/api/environment-groups/{group}/security")] {
            let (s, b) = qa.send(Method::PUT, &qa.url(&path), Some(json!({ "rules": null }))).await?;
            ensure!(s == StatusCode::NO_CONTENT, "{path}: {s}: {b}");
        }
        ensure!(op.console_status("qa-shared").await? == 101, "open again");
        Ok(())
    })
    .await;
}

/// Volumes, networks and images owned as containers are ("Access" 18), against the real Docker.
async fn owned(qa: &Qa, std: &Qa, viewer: &Qa) {
    let names = |b: &Value, key: &str| -> Vec<String> { b.as_array().into_iter().flatten().filter_map(|x| x[key].as_str().map(str::to_string)).collect() };
    step("the standard user sees the volumes and networks their containers use and their own, and no hidden one's name", async {
        for (path, body) in [
            ("/volumes", json!({ "Name": "qa-sharedvol", "access": { "level": "admins" } })),
            ("/volumes", json!({ "Name": "qa-adminvol" })),
            ("/networks", json!({ "Name": "qa-adminnet" })),
        ] {
            let (s, b) = qa.post(&qa.env(path), body).await?;
            ensure!(s == StatusCode::CREATED, "{path}: {s}: {b}");
        }
        let (s, b) = qa.post(&qa.env("/containers"), json!({ "name": "qa-vols", "access": { "level": "public" }, "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"], "HostConfig": { "Binds": ["qa-sharedvol:/shared"] } } })).await?;
        ensure!(s == StatusCode::CREATED, "qa-vols: {s}: {b}");
        let (s, b) = qa.post(&qa.env("/containers"), json!({ "name": "qa-hidden", "config": { "Image": IMAGE, "Cmd": ["sleep", "3600"], "HostConfig": { "Binds": ["qa-sharedvol:/shared", "qa-adminvol:/mine"], "NetworkMode": "qa-adminnet" } } })).await?;
        ensure!(s == StatusCode::CREATED, "qa-hidden: {s}: {b}");

        let all = qa.get_json(&qa.env("/volumes")).await?;
        let shared = all.as_array().into_iter().flatten().find(|v| v["Name"] == json!("qa-sharedvol")).context("qa-sharedvol")?;
        ensure!(shared["containers"].as_array().map_or(0, Vec::len) == 2, "the administrator sees both users: {shared}");
        let seen = std.get_json(&std.env("/volumes")).await?;
        ensure!(names(&seen, "Name").contains(&"qa-sharedvol".to_string()) && !names(&seen, "Name").contains(&"qa-adminvol".to_string()), "volumes: {seen}");
        let shared = seen.as_array().into_iter().flatten().find(|v| v["Name"] == json!("qa-sharedvol")).context("qa-sharedvol")?;
        ensure!(shared["containers"] == json!(["qa-vols"]), "the private container's name is not there: {shared}");
        let (s, _) = std.send(Method::GET, &std.env("/volumes/qa-adminvol"), None).await?;
        ensure!(s == StatusCode::NOT_FOUND, "a hidden volume: {s}");
        let nets = std.get_json(&std.env("/networks")).await?;
        ensure!(names(&nets, "Name").contains(&"bridge".to_string()) && !names(&nets, "Name").contains(&"qa-adminnet".to_string()), "networks: {nets}");
        let (s, b) = std.post(&std.env("/volumes"), json!({ "Name": "qa-stdvol" })).await?;
        ensure!(s == StatusCode::CREATED, "their own volume: {s}: {b}");
        ensure!(names(&std.get_json(&std.env("/volumes")).await?, "Name").contains(&"qa-stdvol".to_string()), "they see what they made");
        ensure!(!names(&viewer.get_json(&viewer.env("/volumes")).await?, "Name").contains(&"qa-stdvol".to_string()), "the read-only user does not");
        Ok(())
    })
    .await;

    step("an image the standard user pulls is theirs", async {
        let (_, done) = std.stream(&std.env("/images/pull"), json!({ "image": IMAGE })).await?;
        ensure!(done, "the pull ended done");
        let id = qa.get_json(&qa.env(&format!("/images/{IMAGE}"))).await?["image"]["Id"].as_str().context("its id")?.to_string();
        let mine = std.get_json(&std.env(&format!("/images/{id}"))).await?;
        ensure!(mine["freightdock_access"]["level"] == json!("private") && mine["freightdock_access"]["user"]["name"] == json!("qa-standard"), "{}", mine["freightdock_access"]);
        ensure!(names(&std.get_json(&std.env("/images")).await?, "Id").contains(&id), "in their list");
        Ok(())
    })
    .await;

    step("the volumes, the network and their containers go", async {
        for c in ["qa-vols", "qa-hidden"] {
            let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/containers/{c}?force=true")), None).await?;
            ensure!(s == StatusCode::NO_CONTENT, "remove {c}: {s}: {b}");
        }
        for v in ["qa-sharedvol", "qa-adminvol", "qa-stdvol"] {
            let (s, b) = qa.send(Method::DELETE, &qa.env(&format!("/volumes/{v}")), None).await?;
            ensure!(s == StatusCode::NO_CONTENT, "remove {v}: {s}: {b}");
        }
        let (s, b) = qa.send(Method::DELETE, &qa.env("/networks/qa-adminnet"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the network: {s}: {b}");
        Ok(())
    })
    .await;
}
