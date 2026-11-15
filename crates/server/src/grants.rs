//! Finer roles over the cluster ("Access" 24): an administrator lets a person, or a team, operate one stack or one
//! node. Operating a stack is starting, stopping and restarting it; operating a node is reading its Docker pages (its
//! containers, their logs, images, volumes, networks) and starting, stopping and restarting its containers. Nothing
//! that reaches the host is ever granted: deploying or editing a stack (its file can bind any path), removing,
//! pulling, pruning and shells stay an administrator's.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit;
use crate::auth::User;
use crate::db::{now, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The stack the managers run this UI in (the manager's `ui::STACK`).
const SYSTEM_STACK: &str = "freightdock";

/// What a node operator may change on its Docker; the rest of `CHANGES` stays an administrator's.
pub const NODE_CHANGES: [&str; 3] = ["container.start", "container.stop", "container.restart"];

fn kind_ok(kind: &str) -> bool {
    matches!(kind, "stack" | "node")
}

/// Whether `user` may operate the stack or node `target`: an administrator always, anybody else by a grant to them or
/// to a team of theirs.
pub async fn may(app: &App, user: &User, kind: &'static str, target: &str) -> anyhow::Result<bool> {
    if user.is_admin() {
        return Ok(true);
    }
    let (uid, target) = (user.id, target.to_string());
    app.db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM cluster_grants WHERE kind = ?1 AND target = ?2
                 AND (user_id = ?3 OR team_id IN (SELECT team_id FROM team_members WHERE user_id = ?3)))",
                params![kind, target, uid],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await
}

/// What `uid` may operate, for the pages to show the buttons: `[{kind, target}]`.
pub async fn mine(app: &App, uid: i64) -> anyhow::Result<Value> {
    app.db
        .call(move |c| {
            let rows: Vec<Value> = c
                .prepare(
                    "SELECT DISTINCT kind, target FROM cluster_grants
                     WHERE user_id = ?1 OR team_id IN (SELECT team_id FROM team_members WHERE user_id = ?1) ORDER BY kind, target",
                )?
                .query_map([uid], |r| Ok(json!({ "kind": r.get::<_, String>(0)?, "target": r.get::<_, String>(1)? })))?
                .collect::<Result<_, _>>()?;
            Ok(Value::Array(rows))
        })
        .await
}

/// The grants on a stack or node go with it, so one made later under the same name starts with none.
pub async fn forget(app: &App, kind: &'static str, target: &str) {
    let t = target.to_string();
    if let Err(e) = app.db.call(move |c| Ok(c.execute("DELETE FROM cluster_grants WHERE kind = ?1 AND target = ?2", params![kind, t])?)).await {
        tracing::warn!("forgetting the grants on {kind} {target}: {e:#}");
    }
}

#[derive(Deserialize)]
pub struct ListAsk {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    target: String,
}

/// `GET /api/cluster/grants?kind=&target=`: the grants, with whom each is to.
pub async fn list(State(app): State<App>, _a: Admin, Query(q): Query<ListAsk>) -> ApiResult<Json<Value>> {
    let rows = app
        .db
        .call(move |c| {
            let rows: Vec<Value> = c
                .prepare(
                    "SELECT g.id, g.kind, g.target, g.user_id, u.name, g.team_id, t.name, g.created_at FROM cluster_grants g
                     LEFT JOIN users u ON u.id = g.user_id LEFT JOIN teams t ON t.id = g.team_id
                     WHERE (?1 = '' OR g.kind = ?1) AND (?2 = '' OR g.target = ?2) ORDER BY g.kind, g.target, g.id",
                )?
                .query_map(params![q.kind, q.target], |r| {
                    Ok(json!({ "id": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "target": r.get::<_, String>(2)?,
                        "user": r.get::<_, Option<i64>>(3)?.map(|id| json!({ "id": id, "name": r.get::<_, Option<String>>(4).ok().flatten() })),
                        "team": r.get::<_, Option<i64>>(5)?.map(|id| json!({ "id": id, "name": r.get::<_, Option<String>>(6).ok().flatten() })),
                        "created_at": r.get::<_, i64>(7)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await?;
    Ok(Json(json!({ "grants": rows })))
}

#[derive(Deserialize)]
pub struct Grant {
    kind: String,
    target: String,
    #[serde(default)]
    user_id: Option<i64>,
    #[serde(default)]
    team_id: Option<i64>,
}

/// `POST /api/cluster/grants`: a person or a team may operate a stack or a node.
pub async fn add(State(app): State<App>, a: Admin, Json(g): Json<Grant>) -> ApiResult<(StatusCode, Json<Value>)> {
    let target = g.target.trim().to_string();
    if !kind_ok(&g.kind) {
        return Err(ApiError::bad("A grant is on a stack or a node.").field("kind"));
    }
    let named = match g.kind.as_str() {
        "stack" => !target.is_empty() && target.len() <= 40 && target.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        _ => !target.is_empty() && target.len() <= 64 && target.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
    };
    if !named {
        return Err(ApiError::bad("No such stack or node.").field("target"));
    }
    // The cluster's own stack is this UI: stopping it is an administrator's alone.
    if g.kind == "stack" && target == SYSTEM_STACK {
        return Err(ApiError::bad("Freightdock's own stack is an administrator's alone.").field("target"));
    }
    if g.user_id.is_some() == g.team_id.is_some() {
        return Err(ApiError::bad("A grant is to one person or one team.").field("user_id"));
    }
    let (kind, t2, uid, tid) = (g.kind.clone(), target.clone(), g.user_id, g.team_id);
    let made = app
        .db
        .call(move |c| {
            let who: Option<String> = match (uid, tid) {
                (Some(u), _) => c.query_row("SELECT name FROM users WHERE id = ?1 AND role <> 'admin'", [u], |r| r.get(0)).ok(),
                (_, Some(t)) => c.query_row("SELECT name FROM teams WHERE id = ?1", [t], |r| r.get(0)).ok(),
                _ => None,
            };
            let Some(who) = who else { return Ok(Err("who")) };
            let n = c.execute("INSERT OR IGNORE INTO cluster_grants (kind, target, user_id, team_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![kind, t2, uid, tid, now()])?;
            Ok(Ok((who, n == 1, c.last_insert_rowid())))
        })
        .await?;
    let (who, new, id) = made.map_err(|_| ApiError::bad("No such person (an administrator needs no grant) or team.").field("user_id"))?;
    if !new {
        return Err(ApiError::conflict("They may operate it already."));
    }
    let to = if g.team_id.is_some() { format!("team {who}") } else { who };
    audit::record(&app.db, Some(&a.0.session.user), a.0.ip.clone(), audit::Entry { detail: Some(format!("{} {target} to {to}", g.kind)), ..audit::entry("cluster.grant.add", target.clone()) }).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// `DELETE /api/cluster/grants/{id}`.
pub async fn remove(State(app): State<App>, a: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let gone = app
        .db
        .call(move |c| {
            let row: Option<(String, String)> = c.query_row("SELECT kind, target FROM cluster_grants WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).ok();
            c.execute("DELETE FROM cluster_grants WHERE id = ?1", [id])?;
            Ok(row)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("grant"))?;
    audit::record(&app.db, Some(&a.0.session.user), a.0.ip.clone(), audit::Entry { detail: Some(format!("{} {}", gone.0, gone.1)), ..audit::entry("cluster.grant.remove", gone.1.clone()) }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use serde_json::{Value, json};

    use crate::api::tests::{call, json_body, post, signed_in};
    use crate::state::App;

    const NODE: &str = "6f1c7c2e-0000-4000-8000-000000000001";

    /// A plain person, `deck` (id 20), signed in: their cookie and CSRF token.
    async fn deckhand(app: &App) -> (String, String) {
        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (20, 'deck', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 20, None, None).await.unwrap();
        let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
        (format!("freightdock_session={token}"), csrf)
    }

    fn send(method: &str, path: &str, (cookie, csrf): &(String, String), body: Value) -> Request<Body> {
        post(path).method(method).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, cookie).header("x-csrf-token", csrf).body(json_body(body)).unwrap()
    }

    fn read(path: &str, (cookie, _): &(String, String)) -> Request<Body> {
        Request::get(path).header(header::COOKIE, cookie).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_stack_granted_is_started_stopped_and_restarted_and_no_other() {
        let (app, _asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let admin = signed_in(&app).await;
        let deck = deckhand(&app).await;
        assert_eq!(call(&app, send("POST", "/api/cluster/stacks/web/restart", &deck, json!({}))).await.0, StatusCode::FORBIDDEN);
        let (s, _, b) = call(&app, send("POST", "/api/cluster/grants", &admin, json!({ "kind": "stack", "target": "web", "user_id": 20 }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let id = b["id"].as_i64().unwrap();
        assert_eq!(call(&app, send("POST", "/api/cluster/grants", &admin, json!({ "kind": "stack", "target": "web", "user_id": 20 }))).await.0, StatusCode::CONFLICT);
        let (s, _, b) = call(&app, send("POST", "/api/cluster/stacks/web/restart", &deck, json!({}))).await;
        assert_eq!((s, b["restarting"].clone()), (StatusCode::OK, json!(1)), "{b}");
        assert_eq!(call(&app, send("POST", "/api/cluster/stacks/other/restart", &deck, json!({}))).await.0, StatusCode::FORBIDDEN, "only the stack granted");
        assert_eq!(call(&app, send("DELETE", "/api/cluster/stacks/web", &deck, json!({}))).await.0, StatusCode::FORBIDDEN, "removing stays an administrator's");
        assert_eq!(call(&app, send("POST", "/api/cluster/stacks", &deck, json!({ "name": "web", "compose": "services: {}" }))).await.0, StatusCode::FORBIDDEN, "and deploying");
        let (_, _, me) = call(&app, read("/api/me", &deck)).await;
        assert_eq!(me["user"]["grants"], json!([{ "kind": "stack", "target": "web" }]));
        assert_eq!(call(&app, send("DELETE", &format!("/api/cluster/grants/{id}"), &deck, json!({}))).await.0, StatusCode::FORBIDDEN, "a grant is an administrator's to give and take");
        assert_eq!(call(&app, send("DELETE", &format!("/api/cluster/grants/{id}"), &admin, json!({}))).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&app, send("POST", "/api/cluster/stacks/web/restart", &deck, json!({}))).await.0, StatusCode::FORBIDDEN, "taken away");
        let log: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action || ' ' || coalesce(detail, '') FROM audit WHERE action LIKE 'cluster.grant.%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log, ["cluster.grant.add stack web to deck", "cluster.grant.remove stack web"]);
    }

    #[tokio::test]
    async fn a_team_s_grant_is_each_member_s_and_goes_with_the_stack() {
        let (app, _asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let admin = signed_in(&app).await;
        let deck = deckhand(&app).await;
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO teams (id, name, created_at) VALUES (3, 'ops', 0); INSERT INTO team_members (team_id, user_id) VALUES (3, 20);")?)).await.unwrap();
        assert_eq!(call(&app, send("POST", "/api/cluster/grants", &admin, json!({ "kind": "stack", "target": "web", "team_id": 3 }))).await.0, StatusCode::CREATED);
        assert_eq!(call(&app, send("POST", "/api/cluster/stacks/web/restart", &deck, json!({}))).await.0, StatusCode::OK);
        assert_eq!(call(&app, send("DELETE", "/api/cluster/stacks/web", &admin, json!({}))).await.0, StatusCode::NO_CONTENT);
        let (_, _, b) = call(&app, read("/api/cluster/grants", &admin)).await;
        assert_eq!(b["grants"], json!([]), "a stack made later under the name starts with no grants");
    }

    #[tokio::test]
    async fn a_node_granted_shows_its_docker_and_starts_stops_and_restarts_its_containers_and_nothing_more() {
        let (app, asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let admin = signed_in(&app).await;
        let deck = deckhand(&app).await;
        let logs = format!("/api/cluster/nodes/{NODE}/docker?action=logs&target=web&tail=50");
        assert_eq!(call(&app, read(&logs, &deck)).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, send("POST", "/api/cluster/grants", &admin, json!({ "kind": "node", "target": NODE, "user_id": 20 }))).await.0, StatusCode::CREATED);
        let (s, _, b) = call(&app, read(&logs, &deck)).await;
        assert_eq!((s, b["lines"].clone()), (StatusCode::OK, json!(["hi"])), "{b}");
        let docker = format!("/api/cluster/nodes/{NODE}/docker");
        assert_eq!(call(&app, send("POST", &docker, &deck, json!({ "action": "container.restart", "target": "web" }))).await.0, StatusCode::OK);
        let before = asked.lock().unwrap().len();
        for action in ["container.remove", "image.pull", "images.prune", "volume.remove"] {
            assert_eq!(call(&app, send("POST", &docker, &deck, json!({ "action": action, "target": "web" }))).await.0, StatusCode::FORBIDDEN, "{action}");
        }
        assert_eq!(asked.lock().unwrap().len(), before, "the manager is not asked");
        assert_eq!(call(&app, read(&format!("/api/cluster/nodes/{NODE}/shell?container=web"), &deck)).await.0, StatusCode::FORBIDDEN, "a shell is never granted");
    }

    #[tokio::test]
    async fn a_grant_is_to_one_plain_person_or_one_team_on_a_stack_or_a_node() {
        let (app, _asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let admin = signed_in(&app).await;
        deckhand(&app).await;
        for bad in [
            json!({ "kind": "host", "target": "web", "user_id": 20 }),
            json!({ "kind": "stack", "target": "../x", "user_id": 20 }),
            json!({ "kind": "stack", "target": "web" }),
            json!({ "kind": "stack", "target": "web", "user_id": 20, "team_id": 3 }),
            json!({ "kind": "stack", "target": "freightdock", "user_id": 20 }),
            json!({ "kind": "stack", "target": "web", "user_id": 1 }),
            json!({ "kind": "stack", "target": "web", "user_id": 99 }),
            json!({ "kind": "stack", "target": "web", "team_id": 99 }),
        ] {
            assert_eq!(call(&app, send("POST", "/api/cluster/grants", &admin, bad.clone())).await.0, StatusCode::BAD_REQUEST, "{bad}");
        }
    }
}
