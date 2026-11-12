//! The import against a fake Portainer (an axum server on loopback answering the API with recorded, anonymised
//! answers) and a fake Docker for the local environment, through the API as the page calls it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, Uri, header};
use serde_json::{Value, json};

use super::*;
use crate::api::tests::{call, signed_in};
use crate::stacks::tests::{calls, send};

const TOKEN: &str = "ptr_test-token";
/// The engine ID the recorded Portainer reports for its environment 2, and the fake Docker for Freightdock's local one.
const ENGINE: &str = "0f3c5e2a-7b1d-4c8e-9a6f-2d4b8e1c7a90";

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<(Method, String)>>>,
}

/// Starts a server answering with `answer` on loopback; its address.
async fn serve<F>(answer: F) -> String
where
    F: Fn(Method, Uri, axum::http::HeaderMap) -> (StatusCode, Value) + Clone + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().fallback(move |m: Method, u: Uri, h: axum::http::HeaderMap| {
        let a = answer.clone();
        async move {
            let (s, v) = a(m, u, h);
            (s, axum::Json(v))
        }
    });
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A fake Portainer from the recorded answers, with `git_url` as the Git stack's repository; what it was asked.
async fn portainer(git_url: &str) -> (String, Seen) {
    let seen = Seen::default();
    let s = seen.clone();
    let mut endpoints: Value = serde_json::from_str(include_str!("fixtures/endpoints.json")).unwrap();
    endpoints[0]["GroupId"] = json!(2);
    let mut stacks: Value = serde_json::from_str(include_str!("fixtures/stacks.json")).unwrap();
    for st in stacks.as_array_mut().unwrap() {
        if st["Name"] == "webapp" {
            st["GitConfig"]["URL"] = json!(git_url);
            st["GitConfig"]["ReferenceName"] = json!("refs/heads/main");
            st["GitConfig"]["ConfigFilePath"] = json!("deploy/compose.yaml");
        }
    }
    let info: Value = serde_json::from_str(include_str!("fixtures/info.json")).unwrap();
    let url = serve(move |m: Method, u: Uri, h: axum::http::HeaderMap| {
        s.requests.lock().unwrap().push((m.clone(), u.path().to_string()));
        if m != Method::GET {
            return (StatusCode::METHOD_NOT_ALLOWED, json!({ "message": "read only" }));
        }
        let path = u.path();
        if path == "/api/system/status" {
            return (StatusCode::OK, json!({ "Version": "2.45.3", "InstanceID": "instance-1" }));
        }
        if h.get("x-api-key").and_then(|v| v.to_str().ok()) != Some(TOKEN) {
            return (StatusCode::UNAUTHORIZED, json!({ "message": "A valid authorization token is missing", "details": "Unauthorized" }));
        }
        let ok = |v: Value| (StatusCode::OK, v);
        match path {
            "/api/endpoints" => ok(endpoints.clone()),
            "/api/stacks" => ok(stacks.clone()),
            "/api/endpoints/2/docker/info" => ok(info.clone()),
            "/api/endpoints/8/docker/info" | "/api/endpoints/9/docker/info" => (StatusCode::BAD_GATEWAY, json!({ "message": "Proxy failure", "details": "no route to host" })),
            p if p.starts_with("/api/endpoints/") && p.ends_with("/docker/info") => ok(json!({ "ID": format!("other-{p}"), "Name": "elsewhere" })),
            p if p.starts_with("/api/stacks/") && p.ends_with("/file") => ok(json!({ "StackFileContent": "services:\n  app:\n    image: nginx\n    env_file: stack.env\n" })),
            "/api/custom_templates" => ok(json!([
                { "Id": 1, "Title": "Whoami", "Description": "who", "Type": 2, "Variables": [{ "name": "PORT", "label": "Port", "defaultValue": "80" }] },
                { "Id": 2, "Title": "Swarmy", "Type": 1, "Variables": null }
            ])),
            "/api/custom_templates/1/file" => ok(json!({ "FileContent": "services:\n  who: {image: traefik/whoami}\n" })),
            "/api/registries" => ok(json!([
                { "Id": 1, "Name": "GitHub", "Type": 8, "URL": "ghcr.io", "Authentication": true, "Username": "owner", "Password": "" },
                { "Id": 2, "Name": "Hub", "Type": 6, "URL": "docker.io", "Authentication": false, "Username": "" }
            ])),
            "/api/endpoint_groups" => ok(json!([{ "Id": 1, "Name": "Unassigned" }, { "Id": 2, "Name": "Home lab", "Description": "the house" }])),
            "/api/tags" => ok(json!([{ "ID": 1, "Name": "prod", "Endpoints": { "2": true }, "EndpointGroups": {} }])),
            "/api/users" => ok(json!([{ "Id": 1, "Username": "admin", "Role": 1 }, { "Id": 2, "Username": "viewer", "Role": 2 }, { "Id": 3, "Username": "ghost", "Role": 2 }])),
            "/api/teams" => ok(json!([{ "Id": 1, "Name": "readers" }, { "Id": 2, "Name": "Ops" }])),
            "/api/team_memberships" => ok(json!([
                { "Id": 1, "UserID": 2, "TeamID": 1, "Role": 2 }, { "Id": 2, "UserID": 3, "TeamID": 1, "Role": 2 }, { "Id": 3, "UserID": 1, "TeamID": 2, "Role": 1 }
            ])),
            _ => (StatusCode::NOT_FOUND, json!({ "message": "not found" })),
        }
    })
    .await;
    (url, seen)
}

/// A fake Docker for Freightdock's local environment: the same engine as Portainer's environment 2, running the project
/// `arr` that Portainer started.
async fn docker() -> String {
    serve(|_m, u: Uri, _h| {
        let p = u.path();
        if p.ends_with("/info") {
            (StatusCode::OK, json!({ "ID": ENGINE, "Name": "alpha" }))
        } else if p.ends_with("/containers/json") {
            let labels = json!({ "com.docker.compose.project": "arr", "com.docker.compose.service": "app",
                                 "com.docker.compose.project.working_dir": "/data/compose/3/v2", "com.docker.compose.project.config_files": "/data/compose/3/v2/docker-compose.yml" });
            (StatusCode::OK, json!([{ "Id": "abc", "Names": ["/arr-app-1"], "State": "running", "Labels": labels }]))
        } else {
            (StatusCode::NOT_FOUND, json!({ "message": "page not found" }))
        }
    })
    .await
}

fn app_at(dir: &Path, docker_url: &str) -> App {
    let db = crate::db::Db::open_in_memory().unwrap();
    let stacks = crate::stacks::Stacks::new(dir.join("stacks"), crate::stacks::compose::Compose::at(crate::stacks::compose::tests::fake(dir)));
    crate::state::AppState::new(db, false, crate::docker::Docker::connect(&docker_url.replacen("http://", "tcp://", 1)).unwrap(), crate::secret::Sealer::for_test(), crate::docker::sockets::Sockets::for_test(), crate::agent::Agents::for_test(), stacks)
}

async fn get(app: &App, cookie: &str, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = call(app, Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await;
    (s, b)
}

async fn count(app: &App, sql: &'static str) -> i64 {
    app.db.call(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test]
async fn connecting_tries_the_token_seals_it_and_never_sends_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (purl, seen) = portainer("file:///nowhere").await;
    let app = app_at(dir.path(), &docker().await);
    let (cookie, csrf) = signed_in(&app).await;
    let (s, b) = send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": "ptr_wrong" })).await;
    assert_eq!((s, b["field"].as_str(), b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("token"), Some("Portainer refused the access token.")));
    let (s, b) = send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": "portainer.example" })).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("url")));
    let (s, b) = send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": TOKEN, "ca": "not a certificate" })).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("ca")));

    let (s, b) = send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": format!("{purl}/"), "token": TOKEN })).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!((b["connected"].as_bool(), b["version"].as_str(), b["has_token"].as_bool()), (Some(true), Some("2.45.3"), Some(true)));
    assert!(!b.to_string().contains(TOKEN));
    let kept: String = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'portainer'", [], |r| r.get(0))?)).await.unwrap();
    assert!(kept.contains("sealed1:") && !kept.contains(TOKEN), "{kept}");
    let (_, b) = get(&app, &cookie, "/api/portainer").await;
    assert!(!b.to_string().contains("sealed1:") && b["url"] == purl.as_str());
    // A new certificate setting with no token keeps the one there is, for the same address.
    let (s, b) = send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "skip_verify": true })).await;
    assert_eq!((s, b["skip_verify"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    let detail: String = app.db.call(|c| Ok(c.query_row("SELECT detail FROM audit WHERE action = 'portainer.connect' ORDER BY id DESC LIMIT 1", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(detail, "Portainer 2.45.3; certificate not checked, as chosen", "the choice is in the activity log");
    assert!(seen.requests.lock().unwrap().iter().all(|(m, _)| m == Method::GET), "only ever GET");

    let (s, _) = send(&app, &cookie, &csrf, "DELETE", "/api/portainer", json!({})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, b) = get(&app, &cookie, "/api/portainer").await;
    assert_eq!(b["connected"], false);
    let (s, _) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to read until connected");
}

#[tokio::test]
async fn only_an_administrator_imports() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_at(dir.path(), "http://127.0.0.1:1");
    signed_in(&app).await;
    app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
    let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
    let cookie = format!("freightdock_session={token}");
    let (_, me) = get(&app, &cookie, "/api/me").await;
    for (m, p) in [("PUT", "/api/portainer"), ("POST", "/api/portainer/scan"), ("POST", "/api/portainer/import")] {
        let (s, _) = send(&app, &cookie, me["csrf"].as_str().unwrap(), m, p, json!({ "url": "http://x" })).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
    assert_eq!(get(&app, &cookie, "/api/portainer").await.0, StatusCode::FORBIDDEN);
    let (s, _) = send(&app, "nobody=1", "x", "POST", "/api/portainer/scan", json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_scan_matches_by_engine_id_and_plans_without_changing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (purl, seen) = portainer("file:///nowhere").await;
    let app = app_at(dir.path(), &docker().await);
    let (cookie, csrf) = signed_in(&app).await;
    send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": TOKEN })).await;
    let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let env2 = &b["environments"][0];
    assert_eq!((env2["name"].as_str(), env2["env"].as_i64(), env2["match"]["by"].as_str()), (Some("alpha"), Some(1), Some("engine ID")));
    assert_eq!((env2["group"].as_str(), env2["tags"][0].as_str()), (Some("Home lab"), Some("prod")));
    let spare = b["environments"].as_array().unwrap().iter().find(|e| e["name"] == "spare").unwrap();
    assert_eq!(spare["status"], "down");
    assert!(spare["env"].is_null());
    assert_eq!(b["freightdock"][0]["engine"], ENGINE);
    let arr = b["stacks"].as_array().unwrap().iter().find(|s| s["name"] == "arr").unwrap();
    assert_eq!((arr["importable"].as_bool(), arr["containers"].as_i64(), arr["working_dir"].as_str()), (Some(true), Some(1), Some("/data/compose/3/v2")));
    assert_eq!(b["registries"][0]["needs_password"], true);
    assert_eq!(b["templates"][1]["importable"], false);
    assert_eq!(b["people"], json!({ "users": 3, "teams": 2 }));

    // The page says another mapping: nginx's environment (3, proxy) to the local one.
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({ "mapping": { "2": null, "3": 1 } })).await;
    let nginx = b["stacks"].as_array().unwrap().iter().find(|s| s["name"] == "nginx").unwrap();
    assert_eq!((nginx["env"].as_i64(), nginx["importable"].as_bool()), (Some(1), Some(true)));
    assert!(b["stacks"].as_array().unwrap().iter().filter(|s| s["endpoint_id"] == 2).all(|s| s["importable"] == false));

    assert_eq!(count(&app, "SELECT COUNT(*) FROM stacks").await, 0, "a scan makes nothing");
    assert!(seen.requests.lock().unwrap().iter().all(|(m, _)| m == Method::GET));
}

#[tokio::test]
async fn an_import_adopts_in_place_and_a_second_makes_nothing_twice() {
    let dir = tempfile::tempdir().unwrap();
    let (git_url, _) = crate::stacks::git::tests::upstream(dir.path());
    let (purl, seen) = portainer(&git_url).await;
    let app = app_at(dir.path(), &docker().await);
    let (cookie, csrf) = signed_in(&app).await;
    send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": TOKEN })).await;
    let body = json!({ "stacks": [{ "id": 3 }, { "id": 259 }, { "id": 24 }, { "id": 147 }], "groups": [2], "tags": [1], "registries": [1, 2], "templates": [1, 2], "people": true });
    let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let r = b["results"].as_array().unwrap();
    let of = |kind: &str, id: i64| r.iter().find(|o| o["kind"] == kind && o["id"] == id).unwrap_or_else(|| panic!("{kind} {id}: {b}"));
    assert_eq!(of("stack", 3)["outcome"], "done", "{b}");
    assert_eq!(of("stack", 259)["outcome"], "done", "{b}");
    assert_eq!((of("stack", 24)["outcome"].as_str(), of("stack", 24)["reason"].as_str()), (Some("skipped"), Some("Its environment (4) is no longer in Portainer.")));
    assert_eq!(of("stack", 147)["reason"], "Its environment is not matched to one of Freightdock's.");
    for (k, id) in [("group", 2), ("tag", 1), ("registry", 1), ("registry", 2), ("template", 1), ("people", 0)] {
        assert_eq!(of(k, id)["outcome"], "done", "{k} {id}: {b}");
    }
    assert_eq!(of("template", 2)["outcome"], "skipped");

    // The compose stack: merged by Compose from where Portainer ran it, its variables and stack.env beside it.
    let arr = of("stack", 3)["freightdock_id"].as_i64().unwrap();
    let folder = dir.path().join(format!("stacks/{arr}"));
    assert_eq!(std::fs::read_to_string(folder.join("compose.yaml")).unwrap(), "name: merged\nservices: {}\n");
    assert_eq!(std::fs::read_link(folder.join("stack.env")).unwrap(), std::fs::canonicalize(folder.join(".env")).unwrap());
    let merge = calls(dir.path()).into_iter().find(|c| c.contains("--no-interpolate")).unwrap();
    assert!(merge.contains("-p arr ") && merge.contains("--project-directory /data/compose/3/v2"), "{merge}");
    assert!(!calls(dir.path()).iter().any(|c| c.contains(" up ")), "not deployed: the containers are kept as they are");
    // The Git stack: a Git stack, polled as Portainer polled it, its variables written.
    let sw = crate::stacks::load(&app, 1, of("stack", 259)["freightdock_id"].as_i64().unwrap()).await.unwrap();
    let g = sw.git.unwrap();
    assert_eq!((g.url.as_str(), g.reference.as_str(), g.path.as_str(), g.poll_s), (git_url.as_str(), "main", "deploy/compose.yaml", 300));
    let env = std::fs::read_to_string(dir.path().join(format!("stacks/{}/.env", sw.id))).unwrap();
    assert_eq!(env, "API_KEY=value-0\nAPI_SECRET=value-1\n");
    assert!(dir.path().join(format!("stacks/{}/repo/deploy/stack.env", sw.id)).symlink_metadata().is_ok());
    // Groups, tags, registries, templates, people.
    assert_eq!(count(&app, "SELECT COUNT(*) FROM environments e JOIN environment_groups g ON g.id = e.group_id WHERE e.id = 1 AND g.name = 'Home lab'").await, 1);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM environment_tags et JOIN tags t ON t.id = et.tag_id WHERE et.environment_id = 1 AND t.name = 'prod'").await, 1);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM registries WHERE host = 'ghcr.io' AND username = 'owner' AND secret IS NULL").await, 1, "no password: Portainer gives none");
    assert_eq!(count(&app, "SELECT COUNT(*) FROM templates WHERE title = 'Whoami' AND env LIKE '%PORT%'").await, 1);
    let people: String = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'portainer_people'", [], |r| r.get(0))?)).await.unwrap();
    assert!(people.contains("\"viewer\"") && people.contains("readers"));
    assert_eq!(count(&app, "SELECT COUNT(*) FROM users").await, 1, "nobody was made a Freightdock user");
    let adopt: String = app.db.call(|c| Ok(c.query_row("SELECT detail FROM audit WHERE action = 'stack.adopt'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(adopt, "from Portainer stack 3");
    assert_eq!(count(&app, "SELECT COUNT(*) FROM audit WHERE action = 'portainer.import'").await, 1);

    // Again: nothing is made twice.
    let (counts, made) = (count(&app, "SELECT (SELECT COUNT(*) FROM stacks) * 1000 + (SELECT COUNT(*) FROM registries) * 100 + (SELECT COUNT(*) FROM templates) * 10 + (SELECT COUNT(*) FROM tags)").await, r.len());
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", body).await;
    let r2 = b["results"].as_array().unwrap();
    assert_eq!(r2.len(), made);
    assert!(r2.iter().filter(|o| o["kind"] != "people").all(|o| o["outcome"] != "done"), "{b}");
    assert_eq!(r2.iter().find(|o| o["kind"] == "stack" && o["id"] == 3).unwrap()["reason"], "Imported already.");
    assert_eq!(count(&app, "SELECT (SELECT COUNT(*) FROM stacks) * 1000 + (SELECT COUNT(*) FROM registries) * 100 + (SELECT COUNT(*) FROM templates) * 10 + (SELECT COUNT(*) FROM tags)").await, counts);
    assert!(seen.requests.lock().unwrap().iter().all(|(m, _)| m == Method::GET), "Portainer was only read");
}

#[tokio::test]
async fn a_stack_by_a_name_freightdock_has_is_a_conflict_and_a_failure_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let (purl, _) = portainer("http://127.0.0.1:1/unreachable.git").await;
    let app = app_at(dir.path(), &docker().await);
    let (cookie, csrf) = signed_in(&app).await;
    send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": TOKEN })).await;
    send(&app, &cookie, &csrf, "POST", "/api/envs/1/stacks", json!({ "name": "watchtower", "compose": "services: {}\n" })).await;
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({})).await;
    let w = b["stacks"].as_array().unwrap().iter().find(|s| s["id"] == 4).unwrap();
    assert_eq!((w["importable"].as_bool(), w["reason"].as_str()), (Some(false), Some("Freightdock has a stack named watchtower there already.")));
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", json!({ "stacks": [{ "id": 4 }, { "id": 259 }] })).await;
    let r = b["results"].as_array().unwrap();
    assert_eq!(r[0]["outcome"], "skipped");
    assert_eq!(r[1]["outcome"], "failed", "{b}");
    assert!(r[1]["reason"].as_str().unwrap().ends_with("Portainer signs in to it: give its token."), "{b}");
    assert_eq!(count(&app, "SELECT COUNT(*) FROM stacks").await, 1);
}

/// The members of Freightdock's team `id`, by name.
async fn members_of(app: &App, id: i64) -> Vec<String> {
    app.db
        .call(move |c| {
            let mut st = c.prepare("SELECT u.name FROM team_members m JOIN users u ON u.id = m.user_id WHERE m.team_id = ?1 ORDER BY u.name")?;
            let r = st.query_map([id], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(r)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn teams_come_with_the_members_freightdock_has_and_a_second_import_adds_only_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (purl, seen) = portainer("file:///nowhere").await;
    let app = app_at(dir.path(), &docker().await);
    let (cookie, csrf) = signed_in(&app).await;
    send(&app, &cookie, &csrf, "PUT", "/api/portainer", json!({ "url": purl, "token": TOKEN })).await;
    // Freightdock has a Viewer (Portainer's viewer, case-blind) and a team OPS (Portainer's Ops), nobody in it; no ghost.
    app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (7, 'Viewer', 'x', 'user', 0)", [])?)).await.unwrap();
    let (_, t) = send(&app, &cookie, &csrf, "POST", "/api/teams", json!({ "name": "OPS" })).await;
    let ops = t["id"].as_i64().unwrap();

    // The preview: readers made with Viewer, ghost left out; Ops matched by name, admin added.
    let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["people"], json!({ "users": 3, "teams": 2 }), "the counts as before");
    let team = |b: &Value, name: &str| b["teams"].as_array().unwrap().iter().find(|t| t["name"] == name).cloned().unwrap_or_else(|| panic!("{name}: {b}"));
    let readers = team(&b, "readers");
    assert_eq!((readers["outcome"].as_str(), readers["freightdock_id"].as_i64(), readers["importable"].as_bool()), (Some("new"), None, Some(true)));
    assert_eq!(readers["members"], json!([{ "name": "ghost", "freightdock_user_id": null, "already": false }, { "name": "viewer", "freightdock_user_id": 7, "already": false }]));
    let p_ops = team(&b, "Ops");
    assert_eq!((p_ops["outcome"].as_str(), p_ops["freightdock_id"].as_i64()), (Some("exists"), Some(ops)));
    assert_eq!(p_ops["members"], json!([{ "name": "admin", "freightdock_user_id": 1, "already": false }]));
    assert_eq!(count(&app, "SELECT COUNT(*) FROM teams").await, 1, "a scan makes nothing");

    // The import: readers made, OPS used, the members Freightdock has added.
    let (s, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", json!({ "teams": [1, 2] })).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let r = b["results"].as_array().unwrap();
    let of = |id: i64| r.iter().find(|o| o["kind"] == "team" && o["id"] == id).cloned().unwrap_or_else(|| panic!("team {id}: {b}"));
    assert_eq!((of(1)["outcome"].as_str(), of(1)["name"].as_str()), (Some("done"), Some("readers")), "{b}");
    let made = of(1)["freightdock_id"].as_i64().unwrap();
    assert_eq!(of(1)["reason"], "Made. viewer added. ghost left out: Freightdock has no user of that name.");
    assert_eq!((of(2)["outcome"].as_str(), of(2)["freightdock_id"].as_i64()), (Some("done"), Some(ops)));
    assert_eq!(of(2)["reason"], "Freightdock had a team by that name: used. admin added.");
    assert_eq!(count(&app, "SELECT COUNT(*) FROM teams").await, 2, "OPS matched, not made again");
    assert_eq!(members_of(&app, made).await, ["Viewer"]);
    assert_eq!(members_of(&app, ops).await, ["admin"]);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM portainer_imports WHERE kind = 'team'").await, 2);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM audit WHERE action = 'team.create' AND detail = 'from Portainer'").await, 1);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM audit WHERE action = 'team.update' AND detail LIKE '%added; from Portainer'").await, 2);
    assert_eq!(count(&app, "SELECT COUNT(*) FROM users").await, 2, "nobody was made a Freightdock user");

    // Renamed and given a member by hand in Freightdock; then ghost signs up. Again: the record holds, ghost is added,
    // and nobody is taken out.
    app.db.call(move |c| Ok(c.execute("UPDATE teams SET name = 'Readers here' WHERE id = ?1", [made])?)).await.unwrap();
    app.db.call(move |c| Ok(c.execute("INSERT INTO team_members (team_id, user_id) VALUES (?1, 1)", [made])?)).await.unwrap();
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/scan", json!({})).await;
    let readers = team(&b, "readers");
    assert_eq!((readers["outcome"].as_str(), readers["freightdock_id"].as_i64()), (Some("imported"), Some(made)));
    assert_eq!(readers["members"][1]["already"], true);
    app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (8, 'ghost', 'x', 'user', 0)", [])?)).await.unwrap();
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", json!({ "teams": [1, 2] })).await;
    let r = b["results"].as_array().unwrap();
    let of = |id: i64| r.iter().find(|o| o["kind"] == "team" && o["id"] == id).cloned().unwrap();
    assert_eq!((of(1)["outcome"].as_str(), of(1)["reason"].as_str()), (Some("done"), Some("Imported already. ghost added.")), "{b}");
    assert_eq!((of(2)["outcome"].as_str(), of(2)["reason"].as_str()), (Some("skipped"), Some("Freightdock has it, with those members, already.")), "{b}");
    assert_eq!(count(&app, "SELECT COUNT(*) FROM teams").await, 2, "nothing made twice");
    assert_eq!(members_of(&app, made).await, ["admin", "ghost", "Viewer"], "the member added in Freightdock stays");

    // A team Portainer does not have fails, and says so.
    let (_, b) = send(&app, &cookie, &csrf, "POST", "/api/portainer/import", json!({ "teams": [42] })).await;
    assert_eq!((b["results"][0]["outcome"].as_str(), b["results"][0]["reason"].as_str()), (Some("failed"), Some("Portainer has no such team.")));
    assert!(seen.requests.lock().unwrap().iter().all(|(m, _)| m == Method::GET), "Portainer was only read");
}
