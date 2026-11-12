//! The security settings through the API, per role: who sets them, how an environment follows its group, and each
//! way a container comes to be held to them ("Access" 12 to 17).

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::Rules;
use crate::access::api_tests::World;

async fn set(w: &World, path: &str, rules: Option<Rules>) {
    let (s, b) = w.as_("admin", "PUT", path, Some(json!({ "rules": rules }))).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
}

fn privileged() -> Value {
    json!({ "name": "p", "config": { "Image": "alpine", "HostConfig": { "Privileged": true } } })
}

fn binding(path: &str) -> Value {
    json!({ "name": "b", "config": { "Image": "alpine", "HostConfig": { "Binds": [format!("{path}:/data")] } } })
}

#[tokio::test]
async fn the_settings_are_an_administrator_s_and_an_environment_follows_its_group_until_it_has_its_own() {
    let w = World::new().await;
    let (s, b) = w.as_("admin", "GET", "/api/environments/1/security", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((b["source"].as_str(), &b["rules"]), (Some("default"), &json!(Rules::default())), "everything allowed until said otherwise");
    assert_eq!(b["presets"]["locked_down"], json!(Rules::locked_down()));
    for who in ["env_admin", "operator", "standard"] {
        let (s, _) = w.as_(who, "GET", "/api/environments/1/security", None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{who}");
        let (s, _) = w.as_(who, "PUT", "/api/environments/1/security", Some(json!({ "rules": null }))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{who}");
    }
    let (s, g) = w.as_("admin", "POST", "/api/environment-groups", Some(json!({ "name": "prod", "environments": [1] }))).await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    let gid = g["id"].as_i64().unwrap();
    set(&w, &format!("/api/environment-groups/{gid}/security"), Some(Rules::locked_down())).await;
    let (_, b) = w.as_("admin", "GET", "/api/environments/1/security", None).await;
    assert_eq!((b["source"].as_str(), &b["rules"], &b["own"]), (Some("group"), &json!(Rules::locked_down()), &Value::Null));
    assert_eq!(b["group"]["name"], "prod");
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/containers", Some(privileged())).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the group's rules hold on its environments");

    set(&w, "/api/environments/1/security", Some(Rules::default())).await;
    let (_, b) = w.as_("admin", "GET", "/api/environments/1/security", None).await;
    assert_eq!(b["source"], "environment");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(privileged())).await;
    assert_eq!(s, StatusCode::CREATED, "the environment's own wins, whole: {b}");

    set(&w, "/api/environments/1/security", None).await;
    let (_, b) = w.as_("admin", "GET", "/api/environments/1/security", None).await;
    assert_eq!(b["source"], "group", "none of its own: the group's again");
    let (_, b) = w.as_("admin", "GET", &format!("/api/environment-groups/{gid}/security"), None).await;
    assert_eq!(b["rules"], json!(Rules::locked_down()));

    let (s, b) = w.as_("admin", "PUT", "/api/environments/1/security", Some(json!({ "rules": { "block_bind_mounts": true, "bind_allow": ["srv"] } }))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("bind_allow")));
    let (s, _) = w.as_("admin", "PUT", "/api/environments/99/security", Some(json!({ "rules": null }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, log) = w.as_("admin", "GET", "/api/audit?action=group.security", None).await;
    assert_eq!(log["entries"][0]["detail"], "locked down", "{log}");
    let (_, log) = w.as_("admin", "GET", "/api/audit?action=environment.security", None).await;
    assert_eq!(log["entries"].as_array().unwrap().len(), 2, "{log}");
}

#[tokio::test]
async fn below_environment_administrator_a_container_is_held_to_the_rules_on_the_field_that_breaks_one() {
    let w = World::new().await;
    set(&w, "/api/environments/1/security", Some(Rules { bind_allow: vec!["/srv".into()], ..Rules::locked_down() })).await;
    let refused = |b: &Value, field: &str| b["field"].as_str() == Some(field) && b["error"].as_str().is_some_and(|e| e.contains("turned off for non-administrators"));

    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(privileged())).await;
    assert!(s == StatusCode::FORBIDDEN && refused(&b, "privileged"), "{s} {b}");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(binding("/etc"))).await;
    assert!(s == StatusCode::FORBIDDEN && refused(&b, "binds"), "a host path outside the allow-list: {s} {b}");
    assert!(b["error"].as_str().unwrap().contains("/etc"), "the path is named: {b}");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(binding("/srv/app"))).await;
    assert_eq!(s, StatusCode::CREATED, "inside it: {b}");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(binding("data"))).await;
    assert_eq!(s, StatusCode::CREATED, "a named volume: {b}");
    let host = json!({ "name": "h", "config": { "Image": "alpine", "HostConfig": { "NetworkMode": "host" } } });
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(host)).await;
    assert!(s == StatusCode::FORBIDDEN && refused(&b, "network_mode"), "{s} {b}");
    let dev = json!({ "name": "d", "config": { "Image": "alpine", "HostConfig": { "Devices": [{ "PathOnHost": "/dev/kvm", "PathInContainer": "/dev/kvm", "CgroupPermissions": "rwm" }] } } });
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(dev)).await;
    assert!(s == StatusCode::FORBIDDEN && refused(&b, "devices"), "{s} {b}");

    let (s, b) = w.as_("operator", "POST", "/api/envs/1/containers", Some(privileged())).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "an operator makes nothing at all: {b}");
    for who in ["env_admin", "admin"] {
        let (s, b) = w.as_(who, "POST", "/api/envs/1/containers", Some(privileged())).await;
        assert_eq!(s, StatusCode::CREATED, "{who} is above the rules: {b}");
        let (s, b) = w.as_(who, "POST", "/api/envs/1/containers", Some(binding("/"))).await;
        assert_eq!(s, StatusCode::CREATED, "{who}: {b}");
    }

    // An edit of their own container that adds what the rules refuse.
    let edit = json!({ "name": "mine", "config": { "Image": "alpine", "HostConfig": { "CapAdd": ["SYS_ADMIN"] } }, "replace": "mine" });
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(edit)).await;
    assert!(s == StatusCode::FORBIDDEN && refused(&b, "cap_add"), "{s} {b}");

    // A volume that is a host path underneath.
    let vol = json!({ "Name": "root", "Driver": "local", "DriverOpts": { "type": "none", "o": "bind", "device": "/" } });
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/volumes", Some(vol.clone())).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::FORBIDDEN, Some("driver_opts")), "{b}");
    let (s, b) = w.as_("env_admin", "POST", "/api/envs/1/volumes", Some(vol)).await;
    assert_ne!(s, StatusCode::FORBIDDEN, "made (the fake Docker's answer aside): {b}");

    // A Swarm service with a device: an environment administrator's work in any case.
    let svc = json!({ "Name": "gpu", "TaskTemplate": { "ContainerSpec": { "Image": "alpine", "Mounts": [{ "Type": "bind", "Source": "/dev/dri", "Target": "/dev/dri" }] } } });
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/swarm/services", Some(svc.clone())).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = w.as_("env_admin", "POST", "/api/envs/1/swarm/services", Some(svc)).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");

    // What is made with an API token and through MCP goes through the same routes.
    let (_, cookie, csrf) = w.people.iter().find(|p| p.0 == "standard").unwrap().clone();
    let t = crate::tokens::tests::token(&w.app, &cookie, &csrf, json!({ "name": "agent" })).await;
    let rpc = |id: i64, method: &str, params: Value| json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let mcp = |body: Value, session: Option<String>| {
        let mut r = axum::http::Request::post("/mcp")
            .header(axum::http::header::HOST, "freightdock.test")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::ACCEPT, "application/json, text/event-stream")
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
        if let Some(s) = session {
            r = r.header("mcp-session-id", s);
        }
        r.body(axum::body::Body::from(body.to_string())).unwrap()
    };
    let (_, h, _) = crate::api::tests::call(&w.app, mcp(rpc(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } })), None)).await;
    let session = h.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    let (_, _, b) = crate::api::tests::call(&w.app, mcp(rpc(2, "tools/call", json!({ "name": "create_container", "arguments": { "config": { "Image": "alpine", "HostConfig": { "Privileged": true } } } })), session)).await;
    assert_eq!(b["result"]["isError"], true, "{b}");
    assert!(b["result"]["content"][0]["text"].as_str().unwrap().contains("Privileged mode is turned off"), "{b}");
}

#[tokio::test]
async fn consoles_can_be_closed_to_everybody_below_environment_administrator() {
    let w = World::new().await;
    let exec = || Some(json!({ "cmd": ["true"] }));
    // The fake Docker runs no command: anything but a refusal is the console let through.
    let (s, _) = w.as_("operator", "POST", "/api/envs/1/containers/web/exec", exec()).await;
    assert_ne!(s, StatusCode::FORBIDDEN, "open by default, as before");
    set(&w, "/api/environments/1/security", Some(Rules { block_consoles: true, ..Default::default() })).await;
    for who in ["operator", "standard"] {
        let (s, b) = w.as_(who, "POST", "/api/envs/1/containers/web/exec", exec()).await;
        assert_eq!((s, b["error"].as_str()), (StatusCode::FORBIDDEN, Some("Consoles are turned off for non-administrators on this host.")), "{who}");
        let (s, _) = w.as_(who, "GET", "/api/envs/1/containers/web/exec/ws", None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{who}: the terminal too");
        let (_, e) = w.as_(who, "GET", "/api/environments/1", None).await;
        assert!(!e["actions"].as_array().unwrap().contains(&json!("console")), "{who}: the UI is told: {e}");
    }
    for who in ["env_admin", "admin"] {
        let (s, b) = w.as_(who, "POST", "/api/envs/1/containers/web/exec", exec()).await;
        assert_ne!(s, StatusCode::FORBIDDEN, "{who}: {b}");
    }
    let (s, _) = w.as_("operator", "POST", "/api/envs/1/containers/web/stop", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "only consoles are closed");
}

#[tokio::test]
async fn a_stack_is_held_service_by_service_as_whoever_deploys_it_a_webhook_as_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let w = World::with(crate::stacks::compose::Compose::at(crate::stacks::compose::tests::fake(dir.path())), dir.path().join("stacks")).await;
    // What the stand-in Compose answers `config --format json` with: the file as Compose would resolve it.
    let config = json!({ "name": "shop", "services": { "web": { "image": "nginx", "network_mode": "host" }, "db": { "image": "postgres" } } });
    std::fs::write(dir.path().join("config.json"), config.to_string()).unwrap();
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/stacks", Some(json!({ "name": "shop", "compose": "services:\n  web:\n    image: nginx\n    network_mode: host\n" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let id = b["id"].as_i64().unwrap();
    let deploy = format!("/api/envs/1/stacks/{id}/deploy");
    let (s, _) = w.as_("standard", "POST", &deploy, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "everything allowed: it deploys");

    set(&w, "/api/environments/1/security", Some(Rules { block_host_network: true, ..Default::default() })).await;
    let (s, b) = w.as_("standard", "POST", &deploy, Some(json!({}))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::FORBIDDEN, Some("compose")), "{b}");
    assert_eq!(b["error"], "Service web: The host's network (network: host) is turned off for non-administrators on this host.");
    let (s, _) = w.as_("env_admin", "POST", &deploy, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "an environment administrator deploys it");

    // A webhook made by the standard user deploys as them; one made by the administrator as the administrator.
    let (s, b) = w.as_("standard", "POST", &format!("/api/envs/1/stacks/{id}/webhook"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let hook = |url: &str| axum::http::Request::post(url).header(axum::http::header::HOST, "freightdock.test").body(axum::body::Body::empty()).unwrap();
    let (s, _, b) = crate::api::tests::call(&w.app, hook(b["url"].as_str().unwrap())).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{b}");
    assert!(b["error"].as_str().unwrap().contains("Service web"), "{b}");
    let (_, b) = w.as_("admin", "POST", &format!("/api/envs/1/stacks/{id}/webhook"), Some(json!({}))).await;
    // A new address is a new webhook: its turn is its own, not the old address's gap ("Sign-in" 15).
    let (s, _, b) = crate::api::tests::call(&w.app, hook(b["url"].as_str().unwrap())).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");

    // The Git poller deploys as the stack's maker: held like a person at the page (`Judge::Named`).
    let by = super::Judge::Named(Some("standard".into()));
    assert!(super::applies(&w.app.db, &by, 1).await.unwrap().is_some());
    assert!(super::applies(&w.app.db, &super::Judge::Named(Some("env_admin".into())), 1).await.unwrap().is_none());
    assert!(super::applies(&w.app.db, &super::Judge::Named(None), 1).await.unwrap().is_some(), "nobody known: held to every rule");
}

#[tokio::test]
async fn a_container_template_is_held_to_the_rules_as_a_container_is() {
    let w = World::new().await;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/list.json", l.local_addr().unwrap());
    // A fallback, not a route: test servers stay out of the rules' check of every route.
    let r = axum::Router::new().fallback(|| async { &include_bytes!("../../templates/fixtures/v2.json")[..] });
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    let (s, _) = w.as_("admin", "PUT", "/api/settings/templates", Some(json!({ "urls": [url] }))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    set(&w, "/api/environments/1/security", Some(Rules::locked_down())).await;
    // "Monitor": privileged, on the host's network, with the Docker socket and a host folder.
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/templates/deploy", Some(json!({ "list": url, "index": 2, "name": "mon" }))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::FORBIDDEN, Some("privileged")), "{b}");
    for what in ["Privileged mode", "/var/run/docker.sock", "/srv/monitor", "network: host"] {
        assert!(b["error"].as_str().unwrap().contains(what), "{what} is named: {b}");
    }
    let (s, b) = w.as_("env_admin", "POST", "/api/envs/1/templates/deploy", Some(json!({ "list": url, "index": 2, "name": "mon" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}
