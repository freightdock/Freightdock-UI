//! Every role against representative routes of every area, in process, against a fake Docker: the guard lets each
//! role do exactly what the table says ("Access" 2), hides what a limited role cannot see ("Access" 4), and keeps
//! Freightdock's own pages to administrators.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use crate::api::tests::{call, json_body, signed_in};
use crate::state::App;

/// A Docker that answers what these tests ask: three containers (`web`, `mine`, `secret`), and success for the rest.
pub async fn fake_docker() -> String {
    async fn answer(method: Method, uri: axum::http::Uri) -> axum::response::Response {
        use axum::response::IntoResponse;
        let path = uri.path();
        // bollard puts the API version first: /v1.49/containers/json
        let path = path.trim_start_matches('/');
        let path = match path.split_once('/') {
            Some((v, rest)) if v.starts_with('v') && v[1..].starts_with(|c: char| c.is_ascii_digit()) => rest,
            _ => path,
        };
        let names = ["web", "mine", "secret"];
        // What each container mounts and is on: `secret` shares `webdata` with `web`, and has a volume and a network
        // nobody else uses.
        let uses = |n: &str| -> (Vec<&str>, Vec<&str>) {
            match n {
                "web" => (vec!["webdata"], vec!["front"]),
                "mine" => (vec!["minedata"], vec!["mynet"]),
                _ => (vec!["webdata", "secretdata"], vec!["front", "secretnet"]),
            }
        };
        let summary = |n: &str| {
            let (vols, nets) = uses(n);
            // `secret` was started by Compose on the command line, as project `extproj`.
            let labels = if n == "secret" { json!({ "com.docker.compose.project": "extproj" }) } else { json!({}) };
            json!({ "Id": format!("id-{n}"), "Names": [format!("/{n}")], "Image": "alpine", "ImageID": format!("sha256:{n}"), "State": "running", "Labels": labels,
                "Mounts": vols.iter().map(|v| json!({ "Type": "volume", "Name": v, "Destination": "/d" })).collect::<Vec<_>>(),
                "NetworkSettings": { "Networks": nets.iter().map(|x| (x.to_string(), json!({ "NetworkID": format!("net-{x}") }))).collect::<serde_json::Map<_, _>>() } })
        };
        let volume = |v: &str| json!({ "Name": v, "Driver": "local", "Mountpoint": format!("/var/lib/docker/volumes/{v}"), "Labels": {}, "Scope": "local", "Options": {} });
        let network = |x: &str| {
            let on: serde_json::Map<String, Value> = names.iter().filter(|n| uses(n).1.contains(&x)).map(|n| (format!("id-{n}"), json!({ "Name": n }))).collect();
            json!({ "Name": x, "Id": format!("net-{x}"), "Driver": "bridge", "Scope": "local", "Labels": {}, "Containers": on })
        };
        let image = |i: &str| json!({ "Id": i, "ParentId": "", "RepoTags": [], "RepoDigests": [], "Created": 0, "Size": 1, "SharedSize": 0, "Labels": {}, "Containers": 0 });
        let image_id = |r: &str| if r.starts_with("sha256:") { r.to_string() } else { format!("sha256:{r}") };
        let parts: Vec<&str> = path.split('/').collect();
        let known = |id: &str| names.iter().find(|n| **n == id || format!("id-{n}") == id).copied();
        match (method.as_str(), parts.as_slice()) {
            ("GET", ["containers", "json"]) => axum::Json(json!(names.iter().map(|n| summary(n)).collect::<Vec<_>>())).into_response(),
            ("GET", ["containers", id, "json"]) => match known(id) {
                Some(n) => axum::Json(json!({ "Id": format!("id-{n}"), "Name": format!("/{n}"), "Image": "alpine", "Config": { "Image": "alpine", "Labels": {} }, "State": { "Running": true } })).into_response(),
                None => (StatusCode::NOT_FOUND, axum::Json(json!({ "message": format!("No such container: {id}") }))).into_response(),
            },
            ("POST", ["containers", "create"]) => (StatusCode::CREATED, axum::Json(json!({ "Id": "id-new", "Warnings": [] }))).into_response(),
            ("POST", ["containers", _, "exec"]) => (StatusCode::CREATED, axum::Json(json!({ "Id": "e1" }))).into_response(),
            ("GET", ["exec", _, "json"]) => axum::Json(json!({ "ExitCode": 0, "Running": false })).into_response(),
            ("POST", ["containers", "prune"]) => axum::Json(json!({ "ContainersDeleted": ["id-web"], "SpaceReclaimed": 0 })).into_response(),
            ("POST", ["containers", ..]) | ("DELETE", ["containers", ..]) => StatusCode::NO_CONTENT.into_response(),
            ("GET", ["images", "json"]) => axum::Json(json!(["sha256:web", "sha256:mine", "sha256:secret", "sha256:loose"].map(image))).into_response(),
            ("GET", ["images", id, "json"]) => axum::Json(json!({ "Id": image_id(id), "Config": {} })).into_response(),
            ("GET", ["images", _, "history"]) => axum::Json(json!([])).into_response(),
            ("DELETE", ["images", ..]) => axum::Json(json!([])).into_response(),
            ("GET", ["volumes"]) => axum::Json(json!({ "Volumes": json!(["webdata", "minedata", "secretdata", "loose"].map(volume)) })).into_response(),
            ("GET", ["volumes", v]) => axum::Json(volume(v)).into_response(),
            ("POST", ["volumes", "create"]) => (StatusCode::CREATED, axum::Json(volume("made"))).into_response(),
            ("GET", ["networks"]) => axum::Json(json!(["bridge", "front", "mynet", "secretnet"].map(network))).into_response(),
            ("GET", ["networks", x]) => axum::Json(network(x.trim_start_matches("net-"))).into_response(),
            ("POST", ["networks", "create"]) => (StatusCode::CREATED, axum::Json(json!({ "Id": "net-made", "Warning": "" }))).into_response(),
            // One swarm service, in stack `ops`, with a variable in its spec, and its task.
            ("GET", ["services"]) => axum::Json(json!([{ "ID": "svc-vault", "Spec": { "Name": "vault", "Labels": { "com.docker.stack.namespace": "ops" }, "TaskTemplate": { "ContainerSpec": { "Env": ["TOKEN=s3cret"] } } } }])).into_response(),
            ("GET", ["services", _]) => axum::Json(json!({ "ID": "svc-vault", "Spec": { "Name": "vault", "TaskTemplate": { "ContainerSpec": { "Env": ["TOKEN=s3cret"] } } } })).into_response(),
            ("GET", ["tasks"]) => axum::Json(json!([{ "ID": "task-1", "ServiceID": "svc-vault" }])).into_response(),
            _ => axum::Json(json!({})).into_response(),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("tcp://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, axum::Router::new().fallback(answer)).await.unwrap() });
    url
}

/// An app on the fake Docker, its administrator, and one user per role on the local environment (and one with none).
pub struct World {
    pub app: App,
    pub people: Vec<(&'static str, String, String)>,
}

pub const ROLES: [&str; 7] = ["admin", "env_admin", "operator", "helpdesk", "standard", "read_only", "nobody"];
/// The Standard user's id, owner of `mine`.
pub const STANDARD: i64 = 14;

impl World {
    pub async fn new() -> World {
        World::with(crate::stacks::compose::Compose::none(), std::env::temp_dir().join(format!("freightdock-test-{}", crate::auth::random_token()))).await
    }

    /// The same, with this Compose and the stacks kept in `stacks`.
    pub async fn with(compose: crate::stacks::compose::Compose, stacks: std::path::PathBuf) -> World {
        let url = fake_docker().await;
        let db = crate::db::Db::open_in_memory().unwrap();
        let stacks = crate::stacks::Stacks::new(stacks, compose);
        let app = crate::state::AppState::new(db, false, crate::docker::Docker::connect(&url).unwrap(), crate::secret::Sealer::for_test(), crate::docker::sockets::Sockets::for_test(), crate::agent::Agents::for_test(), stacks);
        let (cookie, csrf) = signed_in(&app).await;
        let mut people = vec![("admin", cookie, csrf)];
        for (i, role) in ROLES.iter().enumerate().skip(1) {
            let (id, role) = (10 + i as i64, role.to_string());
            app.db
                .call(move |c| {
                    c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (?1, ?2, 'x', 'user', 0)", crate::db::rusqlite::params![id, role])?;
                    if role != "nobody" {
                        c.execute("INSERT INTO grants (user_id, env_id, role) VALUES (?1, 1, ?2)", crate::db::rusqlite::params![id, role])?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            let token = crate::auth::start(&app.db, id, None, None).await.unwrap();
            let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
            people.push((ROLES[i], format!("freightdock_session={token}"), csrf));
        }
        assert_eq!(10 + ROLES.iter().position(|r| *r == "standard").unwrap() as i64, STANDARD);
        let w = World { app, people };
        w.seed().await;
        w
    }

    /// `web` public, `mine` the Standard user's, `secret` nobody's (the administrators'): again, after a test changed it.
    pub async fn seed(&self) {
        self.app
            .db
            .call(|c| {
                use super::ownership::{Kind, Owner, Share, set};
                c.execute("DELETE FROM resource_access", [])?;
                set(c, 1, Kind::Container, "web", Some(&Owner { level: Share::Public, user: Some(1), team: None }))?;
                set(c, 1, Kind::Container, "mine", Some(&Owner { level: Share::Private, user: Some(STANDARD), team: None }))?;
                Ok(())
            })
            .await
            .unwrap();
    }

    pub async fn as_(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (_, cookie, csrf) = self.people.iter().find(|p| p.0 == who).unwrap();
        let r = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "freightdock.test")
            .header(header::ORIGIN, "https://freightdock.test")
            .header(header::COOKIE, cookie)
            .header("x-csrf-token", csrf)
            .header(header::CONTENT_TYPE, "application/json");
        let (s, _, b) = call(&self.app, r.body(body.map_or(Body::empty(), json_body)).unwrap()).await;
        (s, b)
    }
}

/// Whether the guard let it through: anything but its refusals (the handler may still say no for its own reasons).
fn let_through(s: StatusCode, b: &Value) -> bool {
    let refused = s == StatusCode::FORBIDDEN
        || s == StatusCode::UNAUTHORIZED
        || (s == StatusCode::NOT_FOUND && b["error"].as_str().is_some_and(|e| ["No such environment.", "No such container.", "No such stack.", "No such volume.", "No such network.", "No such image.", "No such service."].contains(&e)));
    !refused
}

#[tokio::test]
async fn every_role_may_do_what_the_table_says_on_every_area() {
    #[derive(Clone, Copy, PartialEq)]
    enum Who {
        A,
        EA,
        OP,
        HD,
        ST,
        RO,
    }
    use Who::*;
    let w = World::new().await;
    let name = |x: Who| match x {
        A => "admin",
        EA => "env_admin",
        OP => "operator",
        HD => "helpdesk",
        ST => "standard",
        RO => "read_only",
    };
    let all = [A, EA, OP, HD, ST, RO];
    let reads = &all[..];
    let admins = &[A, EA][..];
    let operate = &[A, EA, OP, ST][..];
    let create = &[A, EA, ST][..];
    let cases: Vec<(&str, &str, Option<Value>, &[Who])> = vec![
        // containers: a public one
        ("GET", "/api/envs/1/containers", None, reads),
        ("GET", "/api/envs/1/containers/web", None, reads),
        ("GET", "/api/envs/1/containers/web/logs", None, reads),
        ("GET", "/api/envs/1/containers/web/config", None, reads),
        ("POST", "/api/envs/1/containers/web/stop", None, operate),
        ("POST", "/api/envs/1/containers/web/restart", None, operate),
        ("POST", "/api/envs/1/containers/web/exec", Some(json!({ "cmd": ["true"] })), operate),
        ("POST", "/api/envs/1/containers/web/rename", Some(json!({ "name": "web2" })), create),
        ("DELETE", "/api/envs/1/containers/web", None, create),
        ("POST", "/api/envs/1/containers/web/webhook", None, create),
        ("PUT", "/api/envs/1/containers/web/access", Some(json!({ "level": "public" })), create),
        // the administrators' container: Standard and Read-only do not see it
        ("GET", "/api/envs/1/containers/secret", None, &[A, EA, OP, HD]),
        ("POST", "/api/envs/1/containers/secret/stop", None, &[A, EA, OP]),
        ("DELETE", "/api/envs/1/containers/secret", None, admins),
        ("POST", "/api/envs/1/containers", Some(json!({ "name": "made", "config": { "Image": "alpine" } })), create),
        ("POST", "/api/envs/1/containers/prune", None, admins),
        // images, volumes, networks, the host
        ("GET", "/api/envs/1/images", None, reads),
        ("POST", "/api/envs/1/images/pull", Some(json!({ "image": "alpine" })), create),
        ("DELETE", "/api/envs/1/images/alpine", None, admins),
        ("POST", "/api/envs/1/images/alpine/tag", Some(json!({ "repo": "x", "tag": "1" })), admins),
        ("GET", "/api/envs/1/volumes", None, reads),
        ("POST", "/api/envs/1/volumes", Some(json!({ "Name": "v" })), create),
        ("DELETE", "/api/envs/1/volumes/v", None, admins),
        ("POST", "/api/envs/1/volumes/prune", Some(json!({})), admins),
        ("GET", "/api/envs/1/networks", None, reads),
        ("POST", "/api/envs/1/networks", Some(json!({ "Name": "n" })), create),
        ("DELETE", "/api/envs/1/networks/n", None, admins),
        ("POST", "/api/envs/1/networks/front/connect", Some(json!({ "container": "web" })), create),
        // volumes, networks and images: seen through a container the person sees, or their own
        ("GET", "/api/envs/1/volumes/webdata", None, reads),
        ("GET", "/api/envs/1/volumes/secretdata", None, &[A, EA, OP, HD]),
        ("GET", "/api/envs/1/networks/secretnet", None, &[A, EA, OP, HD]),
        ("GET", "/api/envs/1/images/sha256:web", None, reads),
        ("GET", "/api/envs/1/images/sha256:secret", None, &[A, EA, OP, HD]),
        ("PUT", "/api/envs/1/volumes/webdata/access", Some(json!({ "level": "public" })), create),
        ("PUT", "/api/envs/1/networks/front/access", Some(json!({ "level": "public" })), create),
        ("PUT", "/api/envs/1/images/sha256:web/access", Some(json!({ "level": "public" })), create),
        // An image's export is every layer's files, as a volume's download is a volume's ("Access" 19).
        ("GET", "/api/envs/1/images/sha256:web/export", None, admins),
        ("GET", "/api/envs/1/images/sha256:mine/export", None, admins),
        ("GET", "/api/envs/1/info", None, reads),
        ("GET", "/api/envs/1/summary", None, reads),
        ("POST", "/api/envs/1/prune", Some(json!({})), admins),
        // stacks, templates, webhooks
        ("GET", "/api/envs/1/stacks", None, reads),
        ("POST", "/api/envs/1/stacks", Some(json!({ "name": "s", "compose": "services: {}\n" })), create),
        ("POST", "/api/envs/1/stacks/adopt", Some(json!({ "name": "s" })), admins),
        ("POST", "/api/envs/1/templates/deploy", Some(json!({ "custom": 99 })), create),
        ("GET", "/api/envs/1/webhooks", None, reads),
        // Swarm
        ("GET", "/api/envs/1/swarm", None, reads),
        ("GET", "/api/envs/1/swarm/services", None, reads),
        ("POST", "/api/envs/1/swarm/services", Some(json!({ "Name": "x" })), admins),
        ("DELETE", "/api/envs/1/swarm/nodes/n1", None, admins),
        ("POST", "/api/envs/1/swarm/stacks", Some(json!({ "name": "app", "compose": "services: {}" })), admins),
        // Freightdock's own
        ("GET", "/api/environments/1", None, reads),
        ("PUT", "/api/environments/1", Some(json!({ "name": "local" })), &[A]),
        ("GET", "/api/environments/1/access", None, &[A]),
        ("GET", "/api/users", None, &[A]),
        ("POST", "/api/teams", Some(json!({ "name": "t" })), &[A]),
        ("GET", "/api/audit", None, &[A]),
        ("GET", "/api/auth-log", None, &[A]),
        ("GET", "/api/settings/sso", None, &[A]),
        ("PUT", "/api/settings/appearance", Some(json!({ "preset": "" })), &[A]),
        ("GET", "/api/registries", None, reads),
        ("POST", "/api/registries", Some(json!({ "name": "r" })), &[A]),
        ("POST", "/api/templates/custom", Some(json!({ "title": "t", "compose": "services: {}" })), &[A]),
        ("POST", "/api/environment-groups", Some(json!({ "name": "g" })), &[A]),
        ("GET", "/api/tags", None, reads),
    ];
    for (m, p, body, allowed) in cases {
        for who in all {
            w.seed().await;
            let (s, b) = w.as_(name(who), m, p, body.clone()).await;
            let want = allowed.contains(&who);
            assert_eq!(let_through(s, &b), want, "{} {m} {p}: {s} {b}", name(who));
            if !want && p.starts_with("/api/envs/") && !matches!(who, ST | RO) {
                assert_eq!(s, StatusCode::FORBIDDEN, "{} {m} {p} is refused, not hidden: {b}", name(who));
            }
        }
        // Nobody with no role on the environment gets as far as an environment's route.
        let (s, b) = w.as_("nobody", m, p, body.clone()).await;
        if p.starts_with("/api/envs/") || p.starts_with("/api/environments/1") {
            assert!(!let_through(s, &b), "nobody {m} {p}: {s} {b}");
        }
    }
}

#[tokio::test]
async fn a_limited_role_sees_only_public_its_team_s_and_its_own() {
    let w = World::new().await;
    let names = |b: &Value| -> Vec<String> { b.as_array().unwrap().iter().map(|c| c["Names"][0].as_str().unwrap().trim_start_matches('/').to_string()).collect() };
    let (_, b) = w.as_("admin", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b), ["web", "mine", "secret"]);
    assert_eq!(b[2]["freightdock_access"]["level"], "admins");
    let (_, b) = w.as_("helpdesk", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b).len(), 3, "Helpdesk reads everything");
    let (_, b) = w.as_("standard", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b), ["web", "mine"], "public and their own");
    assert_eq!(b[1]["freightdock_access"]["user"]["name"], "standard");
    let (_, b) = w.as_("read_only", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b), ["web"], "public only: nothing is theirs");
    let (s, _) = w.as_("read_only", "GET", "/api/envs/1/containers/mine", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "somebody else's private container is not there for them");
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/containers/mine/stop", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "their own");
    let (s, _) = w.as_("read_only", "POST", "/api/envs/1/containers/web/stop", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "seen, but a read-only user only reads");

    // Shared with a team the read-only user is in, they see it.
    w.app
        .db
        .call(|c| {
            c.execute_batch("INSERT INTO teams (id, name, created_at) VALUES (3, 'ops', 0); INSERT INTO team_members (team_id, user_id) VALUES (3, 15);")?;
            super::ownership::set(c, 1, super::ownership::Kind::Container, "secret", Some(&super::ownership::Owner { level: super::ownership::Share::Team, user: Some(1), team: Some(3) }))?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, b) = w.as_("read_only", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b), ["web", "secret"]);
    let (_, b) = w.as_("standard", "GET", "/api/envs/1/containers", None).await;
    assert_eq!(names(&b), ["web", "mine"], "not in that team");
    // A limited person may only share with a team they are in.
    let (s, b) = w.as_("standard", "PUT", "/api/envs/1/containers/mine/access", Some(json!({ "level": "team", "team_id": 3 }))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("team_id")));
}

#[tokio::test]
async fn a_new_stack_never_claims_a_compose_project_that_is_already_running() {
    let w = World::new().await;
    let (s, _) = w.as_("standard", "GET", "/api/envs/1/containers/secret", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "the administrators' external project is not there for them");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/stacks", Some(json!({ "name": "extproj", "compose": "services: {}\n" }))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")), "{b}");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/stacks/git", Some(json!({ "name": "extproj", "url": "https://git.invalid/x.git" }))).await;
    assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")), "a Git stack neither, before the remote is asked: {b}");
    let (s, _) = w.as_("standard", "GET", "/api/envs/1/containers/secret", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "still not theirs");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/stacks", Some(json!({ "name": "fresh", "compose": "services: {}\n" }))).await;
    assert_eq!(s, StatusCode::CREATED, "a name nothing runs under: {b}");
}

#[tokio::test]
async fn what_a_standard_user_makes_is_theirs_and_labelled_with_their_name() {
    let w = World::new().await;
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(json!({ "name": "made", "config": { "Image": "alpine" } }))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let o = super::ownership::Owners::load(&w.app.db, 1).await.unwrap();
    assert_eq!(o.container("made", None), Some(&super::ownership::Owner { level: super::ownership::Share::Private, user: Some(STANDARD), team: None }));
    // The create body sent on to Docker carried the label; it is checked where it is added (containers::owned).
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers", Some(json!({ "name": "edit", "config": { "Image": "alpine" }, "replace": "secret" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "an edit of what they cannot see: {b}");
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/networks/n/connect", Some(json!({ "container": "secret" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nor connect it to a network");
}

#[tokio::test]
async fn an_operator_s_read_only_token_reads_and_a_role_follows_its_token() {
    let w = World::new().await;
    let (_, cookie, csrf) = w.people.iter().find(|p| p.0 == "operator").unwrap().clone();
    let ro = crate::tokens::tests::token(&w.app, &cookie, &csrf, json!({ "name": "ro", "read_only": true })).await;
    let rw = crate::tokens::tests::token(&w.app, &cookie, &csrf, json!({ "name": "rw" })).await;
    let with = |t: &str, m: &str, p: &str| Request::builder().method(m).uri(p).header(header::AUTHORIZATION, format!("Bearer {t}")).body(Body::empty()).unwrap();
    let (s, _, _) = call(&w.app, with(&ro, "GET", "/api/envs/1/containers")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, b) = call(&w.app, with(&ro, "POST", "/api/envs/1/containers/web/stop")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "read-only stays read-only: {b}");
    let (s, _, _) = call(&w.app, with(&rw, "POST", "/api/envs/1/containers/web/stop")).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "the operator's own token operates");
    let (s, _, _) = call(&w.app, with(&rw, "DELETE", "/api/envs/1/containers/web")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "and no more than the operator may");
    let (_, _, log) = call(&w.app, Request::get("/api/auth-log?event=token").header(header::COOKIE, &w.people[0].1).body(Body::empty()).unwrap()).await;
    assert_eq!(log["entries"].as_array().unwrap().len(), 2, "each token's use is in the auth log: {log}");
}

#[tokio::test]
async fn a_stack_s_environment_is_a_read_of_the_stack_and_hidden_where_the_stack_is() {
    // The Environment tab sends every variable's value, secrets too ("Stacks" 11), so it is held to the stack's own
    // rule: whoever may read the stack reads it, and a stack they cannot see is not there for them.
    let w = World::new().await;
    w.app
        .db
        .call(|c| {
            c.execute_batch(
                "INSERT INTO stacks (id, env_id, name, source, created_at, updated_at) VALUES (7, 1, 'shop', 'editor', 0, 0);
                 INSERT INTO stacks (id, env_id, name, source, created_at, updated_at) VALUES (8, 1, 'open', 'editor', 0, 0);",
            )?;
            super::ownership::set(c, 1, super::ownership::Kind::Stack, "8", Some(&super::ownership::Owner { level: super::ownership::Share::Public, user: Some(1), team: None }))?;
            Ok(())
        })
        .await
        .unwrap();
    for who in ["admin", "env_admin", "operator", "helpdesk"] {
        let (s, b) = w.as_(who, "GET", "/api/envs/1/stacks/7/environment", None).await;
        assert!(let_through(s, &b), "{who} reads the administrators' stack's environment: {s} {b}");
    }
    for who in ["standard", "read_only"] {
        let (s, b) = w.as_(who, "GET", "/api/envs/1/stacks/7/environment", None).await;
        assert_eq!((s, b["error"].as_str()), (StatusCode::NOT_FOUND, Some("No such stack.")), "{who} does not see the administrators' stack");
        let (s, b) = w.as_(who, "GET", "/api/envs/1/stacks/8/environment", None).await;
        assert!(let_through(s, &b), "{who} reads a public stack's environment: {s} {b}");
    }
    let (s, b) = w.as_("nobody", "GET", "/api/envs/1/stacks/8/environment", None).await;
    assert!(!let_through(s, &b), "no role, no environment: {s} {b}");
}

#[tokio::test]
async fn volumes_networks_and_images_are_seen_as_their_owners_and_containers_say() {
    let w = World::new().await;
    let names = |b: &Value, key: &str| -> Vec<String> { b.as_array().unwrap().iter().map(|x| x[key].as_str().unwrap().to_string()).collect() };
    let (_, b) = w.as_("admin", "GET", "/api/envs/1/volumes", None).await;
    assert_eq!(names(&b, "Name"), ["webdata", "minedata", "secretdata", "loose"]);
    assert_eq!(b[0]["containers"], json!(["web", "secret"]));
    assert_eq!(b[3]["freightdock_access"]["level"], "admins");
    let (_, b) = w.as_("helpdesk", "GET", "/api/envs/1/volumes", None).await;
    assert_eq!(names(&b, "Name").len(), 4, "Helpdesk reads everything");
    let (_, b) = w.as_("standard", "GET", "/api/envs/1/volumes", None).await;
    assert_eq!(names(&b, "Name"), ["webdata", "minedata"], "what the containers they see mount");
    assert_eq!(b[0]["containers"], json!(["web"]), "and not the name of the administrators' container that mounts it too");
    let (_, b) = w.as_("read_only", "GET", "/api/envs/1/volumes", None).await;
    assert_eq!(names(&b, "Name"), ["webdata"]);
    let (s, _) = w.as_("standard", "GET", "/api/envs/1/volumes/secretdata", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, b) = w.as_("standard", "GET", "/api/envs/1/volumes/webdata", None).await;
    assert_eq!((s, &b["containers"]), (StatusCode::OK, &json!(["web"])), "{b}");

    let (_, b) = w.as_("standard", "GET", "/api/envs/1/networks", None).await;
    assert_eq!(names(&b, "Name"), ["bridge", "front", "mynet"], "Docker's own are everybody's");
    let (_, b) = w.as_("read_only", "GET", "/api/envs/1/networks", None).await;
    assert_eq!(names(&b, "Name"), ["bridge", "front"]);
    let (s, b) = w.as_("standard", "GET", "/api/envs/1/networks/front", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["Containers"].as_object().unwrap().values().map(|c| c["Name"].as_str().unwrap()).collect::<Vec<_>>(), ["web"], "{b}");
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/networks/secretnet/connect", Some(json!({ "container": "mine" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nor put a container on a network they cannot see");

    let (_, b) = w.as_("standard", "GET", "/api/envs/1/images", None).await;
    assert_eq!(names(&b, "Id"), ["sha256:web", "sha256:mine"]);
    let (_, b) = w.as_("operator", "GET", "/api/envs/1/images", None).await;
    assert_eq!(names(&b, "Id").len(), 4);

    // Shared, and what they make or pull is theirs.
    let (s, _) = w.as_("admin", "PUT", "/api/envs/1/volumes/loose/access", Some(json!({ "level": "public" }))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, b) = w.as_("read_only", "GET", "/api/envs/1/volumes", None).await;
    assert_eq!(names(&b, "Name"), ["webdata", "loose"]);
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/volumes", Some(json!({ "Name": "made" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/networks", Some(json!({ "Name": "made-net", "access": { "level": "public" } }))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/images/pull", Some(json!({ "image": "loose" }))).await;
    assert_eq!(s, StatusCode::OK);
    use super::ownership::{Kind, Owner, Share, get};
    let mine = Some(Owner { level: Share::Private, user: Some(STANDARD), team: None });
    assert_eq!(get(&w.app.db, 1, Kind::Volume, "made".into()).await.unwrap(), mine);
    assert_eq!(get(&w.app.db, 1, Kind::Network, "made-net".into()).await.unwrap().unwrap().level, Share::Public);
    assert_eq!(get(&w.app.db, 1, Kind::Image, "sha256:loose".into()).await.unwrap(), mine, "the puller is its owner");
    let (_, b) = w.as_("standard", "GET", "/api/envs/1/images", None).await;
    assert_eq!(names(&b, "Id"), ["sha256:web", "sha256:mine", "sha256:loose"]);
    let (s, b) = w.as_("standard", "PUT", "/api/envs/1/volumes/secretdata/access", Some(json!({ "level": "public" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nor share what they cannot see: {b}");
}

#[tokio::test]
async fn swarm_workloads_are_the_administrators_for_a_limited_role() {
    // A swarm service, its tasks and logs, and a swarm stack have no owner record and only an environment
    // administrator makes them: for Standard and Read-only users they are the administrators', as an external project
    // is ("Swarm" 7). Lists leave them out; a route about one is not there.
    let w = World::new().await;
    w.app.db.call(|c| Ok(c.execute("INSERT INTO swarm_stacks (env, name, compose, updated_at) VALUES (1, 'ops', 'services: {}', 0)", [])?)).await.unwrap();
    for who in ["admin", "env_admin", "operator", "helpdesk"] {
        let (s, b) = w.as_(who, "GET", "/api/envs/1/swarm/services", None).await;
        assert_eq!((s, b.as_array().map(|a| a.len())), (StatusCode::OK, Some(1)), "{who} lists the services: {b}");
        let (s, b) = w.as_(who, "GET", "/api/envs/1/swarm/tasks", None).await;
        assert_eq!(b.as_array().map(|a| a.len()), Some(1), "{who} lists the tasks: {s} {b}");
        let (_, b) = w.as_(who, "GET", "/api/envs/1/swarm/stacks", None).await;
        assert_eq!(b.as_array().map(|a| a.len()), Some(1), "{who} lists the stacks: {b}");
        for p in ["/api/envs/1/swarm/services/vault", "/api/envs/1/swarm/services/vault/logs/ws", "/api/envs/1/swarm/stacks/ops"] {
            let (s, b) = w.as_(who, "GET", p, None).await;
            assert!(let_through(s, &b), "{who} GET {p}: {s} {b}");
        }
    }
    for who in ["standard", "read_only"] {
        for p in ["/api/envs/1/swarm/services", "/api/envs/1/swarm/tasks", "/api/envs/1/swarm/stacks"] {
            let (s, b) = w.as_(who, "GET", p, None).await;
            assert_eq!((s, b), (StatusCode::OK, json!([])), "{who} GET {p} lists nothing");
        }
        for p in ["/api/envs/1/swarm/services/vault", "/api/envs/1/swarm/services/vault/logs/ws", "/api/envs/1/swarm/stacks/ops"] {
            let (s, b) = w.as_(who, "GET", p, None).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{who} GET {p}: {b}");
            assert!(!b.to_string().contains("s3cret"), "{who} GET {p}: {b}");
        }
        // The swarm's own state and its nodes stay a read.
        let (s, b) = w.as_(who, "GET", "/api/envs/1/swarm", None).await;
        assert!(let_through(s, &b), "{who} GET swarm: {s} {b}");
    }
}

#[tokio::test]
async fn a_console_in_freightdock_s_own_container_is_an_administrator_s_alone() {
    // A console there is root beside Freightdock's data folder (its key, its database, its CA) and the Docker socket, so
    // it is every environment's: only a Freightdock Administrator opens one, whatever the role here ("Access" 20).
    let w = World::new().await;
    w.app.own_container.set("id-web".into()).unwrap();
    for p in ["/api/envs/1/containers/web/exec", "/api/envs/1/containers/id-web/exec"] {
        let (s, b) = w.as_("admin", "POST", p, Some(json!({ "cmd": ["true"] }))).await;
        assert!(let_through(s, &b), "admin POST {p}: {s} {b}");
        for who in ["env_admin", "operator", "standard"] {
            let (s, b) = w.as_(who, "POST", p, Some(json!({ "cmd": ["true"] }))).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{who} POST {p}: {b}");
        }
    }
    for p in ["/api/envs/1/containers/web/exec/ws", "/api/envs/1/containers/web/attach/ws"] {
        let (s, b) = w.as_("operator", "GET", p, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "operator GET {p}: {b}");
    }
    // Every other container's console is as the table says.
    let (s, b) = w.as_("operator", "POST", "/api/envs/1/containers/secret/exec", Some(json!({ "cmd": ["true"] }))).await;
    assert!(let_through(s, &b), "{s} {b}");
    // And a limited person who cannot see it is still told it is not there.
    w.app.db.call(|c| Ok(c.execute("DELETE FROM resource_access", [])?)).await.unwrap();
    let (s, _) = w.as_("standard", "POST", "/api/envs/1/containers/web/exec", Some(json!({ "cmd": ["true"] }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// The container names kept with a webhook and an automatic-update switch on environment 1.
async fn kept_by_name(w: &World) -> (Vec<String>, Vec<String>) {
    w.app
        .db
        .call(|c| {
            let hooks = c.prepare("SELECT target FROM webhooks WHERE env_id = 1 AND kind = 'container' ORDER BY target")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            let auto = c.prepare("SELECT container FROM auto_updates WHERE env_id = 1 ORDER BY container")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok((hooks, auto))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_container_s_webhook_and_automatic_updates_follow_it_and_go_with_it() {
    // Both are kept by the container's name, like its ownership record ("Access" 4): a later container that takes the
    // name must not be recreated by an old address or updated by an old switch nobody set for it ("Access" 21).
    let w = World::new().await;
    let (s, b) = w.as_("standard", "POST", "/api/envs/1/containers/mine/webhook", None).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let url = b["url"].as_str().unwrap().to_string();
    let (s, _) = w.as_("standard", "PUT", "/api/envs/1/containers/mine/auto-update", Some(json!({ "enabled": true }))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    w.as_("admin", "POST", "/api/envs/1/containers/web/webhook", None).await;
    w.as_("admin", "PUT", "/api/envs/1/containers/web/auto-update", Some(json!({ "enabled": true }))).await;
    assert_eq!(kept_by_name(&w).await, (vec!["mine".into(), "web".into()], vec!["mine".into(), "web".into()]));

    let (s, b) = w.as_("standard", "DELETE", "/api/envs/1/containers/mine", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(kept_by_name(&w).await, (vec!["web".into()], vec!["web".into()]), "removed: its webhook and its switch go");
    let (s, _, _) = call(&w.app, Request::post(&url).body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "the old address reaches nothing, though a container called mine is there again");

    let (s, b) = w.as_("admin", "POST", "/api/envs/1/containers/web/rename", Some(json!({ "name": "web2" }))).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(kept_by_name(&w).await, (vec!["web2".into()], vec!["web2".into()]), "renamed: both follow it");

    // A prune removes containers too (this Docker says it removed `web`).
    w.as_("admin", "POST", "/api/envs/1/containers/web/webhook", None).await;
    let (s, b) = w.as_("admin", "POST", "/api/envs/1/containers/prune", None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(kept_by_name(&w).await, (vec!["web2".into()], vec!["web2".into()]), "pruned: its webhook goes");
    assert_eq!(super::ownership::get(&w.app.db, 1, super::ownership::Kind::Container, "web".into()).await.unwrap(), None, "and its record");
}

#[tokio::test]
async fn the_update_badges_name_only_the_opted_in_containers_a_person_sees() {
    let w = World::new().await;
    for n in ["web", "secret"] {
        let (s, _) = w.as_("admin", "PUT", &format!("/api/envs/1/containers/{n}/auto-update"), Some(json!({ "enabled": true }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (_, b) = w.as_("helpdesk", "GET", "/api/envs/1/updates", None).await;
    assert_eq!(b["opted"], json!(["secret", "web"]), "{b}");
    for who in ["standard", "read_only"] {
        let (_, b) = w.as_(who, "GET", "/api/envs/1/updates", None).await;
        assert_eq!(b["opted"], json!(["web"]), "{who} is not told the administrators' container's name: {b}");
    }
}
