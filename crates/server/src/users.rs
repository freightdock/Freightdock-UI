//! People and teams: `/api/users`, `/api/teams`, and a person's own password ("Access" in DECISIONS.md). Only an Administrator manages people; anybody
//! changes their own password in Account.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::auth;
use crate::db::{now, rusqlite::{self, OptionalExtension, params}};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/users", get(list).post(create))
        .route("/api/users/{id}", put(update).delete(remove))
        .route("/api/users/{id}/password", post(reset_password))
        .route("/api/me/password", put(own_password))
        .route("/api/teams", get(list_teams).post(create_team))
        .route("/api/teams/{id}", put(update_team).delete(remove_team))
}

async fn log(app: &App, a: &Authed, action: &'static str, target: String, detail: Option<String>) {
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

fn unique(e: anyhow::Error, what: &str) -> ApiError {
    if format!("{e:#}").contains("UNIQUE constraint failed") {
        return ApiError::conflict(format!("There is a {what} of that name.")).field("name");
    }
    e.into()
}

// ---------------------------------------------------------------- users

fn user_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let teams: Vec<Value> = r
        .get::<_, Option<String>>(8)?
        .map(|t| t.split('\n').filter_map(|x| x.split_once('\t')).map(|(id, name)| json!({ "id": id.parse::<i64>().unwrap_or(0), "name": name })).collect())
        .unwrap_or_default();
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "role": r.get::<_, String>(2)?, "disabled": r.get::<_, bool>(3)?,
        "created_at": r.get::<_, i64>(4)?, "last_login_at": r.get::<_, Option<i64>>(5)?,
        "kind": r.get::<_, String>(6)?, "email": r.get::<_, Option<String>>(7)?, "teams": teams, "totp": r.get::<_, bool>(9)?,
    }))
}

/// `kind`: `local`, `sso` (an OAuth provider), or `ldap` (a directory); `totp`: whether it has a second factor.
const USER_COLUMNS: &str = "id, name, role, disabled, created_at, last_login_at, \
    CASE WHEN oidc_sub IS NULL THEN 'local' WHEN oidc_issuer LIKE 'ldap:%' THEN 'ldap' ELSE 'sso' END, email, \
    (SELECT group_concat(t.id || char(9) || t.name, char(10)) FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = users.id), \
    totp_secret IS NOT NULL";

/// `GET /api/users`: everybody, with their teams, last sign-in and whether they come through SSO.
async fn list(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let rows = app
        .db
        .call(|c| {
            let mut st = c.prepare(&format!("SELECT {USER_COLUMNS} FROM users ORDER BY name COLLATE NOCASE"))?;
            let rows = st.query_map([], user_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    Ok(Json(json!(rows)))
}

fn check_role(role: &str) -> ApiResult<()> {
    if role == "admin" || role == "user" { Ok(()) } else { Err(ApiError::bad("A user is an administrator (admin) or not (user).").field("role")) }
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    password: String,
    #[serde(default = "user_role")]
    role: String,
}

fn user_role() -> String {
    "user".into()
}

/// `POST /api/users`: a local user with a password.
async fn create(State(app): State<App>, admin: Admin, Json(n): Json<NewUser>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = n.name.trim().to_string();
    auth::check_name(&name).map_err(|e| ApiError::bad(e.to_string()).field("name"))?;
    auth::check_password(&n.password).map_err(|e| ApiError::bad(e.to_string()).field("password"))?;
    check_role(&n.role)?;
    let hash = auth::hash_password(&n.password)?;
    let (nm, role) = (name.clone(), n.role.clone());
    let id = app
        .db
        .call(move |c| {
            c.execute("INSERT INTO users (name, password_hash, role, created_at) VALUES (?1, ?2, ?3, ?4)", params![nm, hash, role, now()])?;
            Ok(c.last_insert_rowid())
        })
        .await
        .map_err(|e| unique(e, "user"))?;
    log(&app, &admin.0, "user.create", name.clone(), Some(n.role.clone())).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name, "role": n.role }))))
}

#[derive(Deserialize)]
struct Change {
    name: Option<String>,
    role: Option<String>,
    disabled: Option<bool>,
}

/// Whether taking `id` away as an administrator (demoted, disabled, removed) would leave none.
fn last_admin(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<bool> {
    let is: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM users WHERE id = ?1 AND role = 'admin' AND disabled = 0)", [id], |r| r.get(0))?;
    let others: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE id <> ?1 AND role = 'admin' AND disabled = 0", [id], |r| r.get(0))?;
    Ok(is && others == 0)
}

const LAST_ADMIN: &str = "That is the last administrator. Make somebody else one first.";

/// `PUT /api/users/{id}`: rename, set the role, disable or enable. Never the last administrator out, and never
/// yourself out of being one or signed in: that is done by another administrator, on purpose.
async fn update(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(ch): Json<Change>) -> ApiResult<Json<Value>> {
    let me = admin.0.session.user.id;
    if let Some(r) = &ch.role {
        check_role(r)?;
        if id == me && r != "admin" {
            return Err(ApiError::conflict("You cannot take away your own administrator's role.").field("role"));
        }
    }
    if id == me && ch.disabled == Some(true) {
        return Err(ApiError::conflict("You cannot disable yourself.").field("disabled"));
    }
    let name = match &ch.name {
        Some(n) => {
            let n = n.trim().to_string();
            auth::check_name(&n).map_err(|e| ApiError::bad(e.to_string()).field("name"))?;
            Some(n)
        }
        None => None,
    };
    let (n, role, disabled) = (name.clone(), ch.role.clone(), ch.disabled);
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let Some(old) = tx.query_row("SELECT name FROM users WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()? else { return Ok(Err(ApiError::not_found("user"))) };
            let demoted = role.as_deref().is_some_and(|r| r != "admin") || disabled == Some(true);
            if demoted && last_admin(&tx, id)? {
                return Ok(Err(ApiError::conflict(LAST_ADMIN)));
            }
            if let Some(n) = &n {
                tx.execute("UPDATE users SET name = ?1 WHERE id = ?2", params![n, id])?;
            }
            if let Some(r) = &role {
                tx.execute("UPDATE users SET role = ?1 WHERE id = ?2", params![r, id])?;
            }
            if let Some(d) = disabled {
                tx.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![d, id])?;
                if d {
                    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
                }
            }
            tx.commit()?;
            Ok(Ok(old))
        })
        .await
        .map_err(|e| unique(e, "user"))?;
    let old = r?;
    let detail = [name.as_ref().map(|n| format!("name {n}")), ch.role.as_ref().map(|r| format!("role {r}")), ch.disabled.map(|d| if d { "disabled".into() } else { "enabled".into() })]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    log(&app, &admin.0, "user.update", old, (!detail.is_empty()).then_some(detail)).await;
    Ok(Json(json!({ "id": id })))
}

/// `DELETE /api/users/{id}`: gone, with their sessions, tokens and team places. Never yourself, never the last
/// administrator.
async fn remove(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    if id == admin.0.session.user.id {
        return Err(ApiError::conflict("You cannot remove yourself."));
    }
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let Some(name) = tx.query_row("SELECT name FROM users WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()? else { return Ok(Err(ApiError::not_found("user"))) };
            if last_admin(&tx, id)? {
                return Ok(Err(ApiError::conflict(LAST_ADMIN)));
            }
            tx.execute("DELETE FROM users WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(Ok(name))
        })
        .await?;
    log(&app, &admin.0, "user.remove", r?, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewPassword {
    password: String,
    /// One's own: the password it replaces.
    #[serde(default)]
    current: String,
}

/// What the activity log says of the API tokens a new password revoked ("Access" 22).
fn revoked(tokens: usize) -> Option<String> {
    match tokens {
        0 => None,
        1 => Some("revoked 1 API token".into()),
        n => Some(format!("revoked {n} API tokens")),
    }
}

/// `POST /api/users/{id}/password`: a new password for a local user, whose sessions and API tokens end. An SSO user has none ("SSO" 4).
async fn reset_password(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(p): Json<NewPassword>) -> ApiResult<StatusCode> {
    auth::check_password(&p.password).map_err(|e| ApiError::bad(e.to_string()).field("password"))?;
    let hash = auth::hash_password(&p.password)?;
    let r = app
        .db
        .call(move |c| {
            let Some((name, sso)) = c.query_row("SELECT name, password_hash = '!' FROM users WHERE id = ?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))).optional()? else {
                return Ok(Err(ApiError::not_found("user")));
            };
            if sso {
                return Ok(Err(ApiError::conflict("This user signs in through SSO and has no password here.")));
            }
            c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![hash, id])?;
            c.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
            let tokens = c.execute("DELETE FROM api_tokens WHERE user_id = ?1", [id])?;
            Ok(Ok((name, tokens)))
        })
        .await?;
    let (name, tokens) = r?;
    log(&app, &admin.0, "user.password", name, revoked(tokens)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/me/password`: one's own, with the one it replaces. Every other session of theirs ends, and every API token.
async fn own_password(State(app): State<App>, a: Authed, peer: crate::api::Peer, Json(p): Json<NewPassword>) -> ApiResult<StatusCode> {
    if a.session.user.token.is_some() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "A password is changed in the Account page, not with a token."));
    }
    auth::check_password(&p.password).map_err(|e| ApiError::bad(e.to_string()).field("password"))?;
    let (id, keep) = (a.session.user.id, a.session.token_hash.clone());
    let hash: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT password_hash FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?)).await?;
    let hash = hash.ok_or_else(ApiError::unauthorized)?;
    if hash == "!" {
        return Err(ApiError::conflict("You sign in through SSO and have no password here."));
    }
    // The one it replaces is a password check like a sign-in's, held to the same limit and logged the same way, or a
    // stolen session could guess the password at Argon2's pace ("Sign-in" 13).
    let (ip, name, at) = (a.ip.clone().unwrap_or_default(), a.session.user.name.clone(), crate::db::now());
    app.limiter.attempt(&ip, &name, at).map_err(auth::too_many)?;
    if !auth::verify_password(&p.current, &hash) {
        let e = crate::authlog::Event { event: "password.failed", method: "password", ok: false, user: Some(&a.session.user), name: None, ip: a.ip.clone(), user_agent: peer.user_agent, detail: Some("wrong current password".into()) };
        crate::authlog::record(&app.db, e).await;
        return Err(ApiError::bad("That is not your password.").field("current"));
    }
    app.limiter.release(&ip, &name, at);
    let new = auth::hash_password(&p.password)?;
    let tokens = app.db
        .call(move |c| {
            c.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![new, id])?;
            c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash <> ?2", params![id, keep])?;
            Ok(c.execute("DELETE FROM api_tokens WHERE user_id = ?1", [id])?)
        })
        .await?;
    log(&app, &a, "account.password", a.session.user.name.clone(), revoked(tokens)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- teams

/// `GET /api/teams`: every team and its members for an Administrator; anybody else, the teams they are in (to share
/// a container or stack with).
async fn list_teams(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let (uid, admin) = (a.session.user.id, a.session.user.is_admin());
    let rows = app
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT t.id, t.name, (SELECT group_concat(u.id || char(9) || u.name, char(10)) FROM team_members m JOIN users u ON u.id = m.user_id WHERE m.team_id = t.id)
                 FROM teams t WHERE ?1 OR t.id IN (SELECT team_id FROM team_members WHERE user_id = ?2) ORDER BY t.name COLLATE NOCASE",
            )?;
            let rows = st.query_map(params![admin, uid], |r| {
                let members: Vec<Value> = r
                    .get::<_, Option<String>>(2)?
                    .map(|t| t.split('\n').filter_map(|x| x.split_once('\t')).map(|(id, name)| json!({ "id": id.parse::<i64>().unwrap_or(0), "name": name })).collect())
                    .unwrap_or_default();
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "members": if admin { json!(members) } else { Value::Null } }))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
struct TeamFields {
    name: Option<String>,
    /// Its whole membership, by user id; left out keeps it.
    members: Option<Vec<i64>>,
}

fn team_name(n: &str) -> ApiResult<String> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(ApiError::bad("A team's name is 1 to 64 characters.").field("name"));
    }
    Ok(n.to_string())
}

/// The team's members as given. A member who stays keeps how they came in, so somebody a provider's groups put in is
/// still taken out by that provider when their groups change ("SSO" 11).
fn set_members(tx: &rusqlite::Transaction, team: i64, members: &[i64]) -> rusqlite::Result<bool> {
    let keep = serde_json::to_string(members).unwrap_or_else(|_| "[]".into());
    tx.execute("DELETE FROM team_members WHERE team_id = ?1 AND user_id NOT IN (SELECT value FROM json_each(?2))", params![team, keep])?;
    for m in members {
        if !tx.query_row("SELECT EXISTS (SELECT 1 FROM users WHERE id = ?1)", [m], |r| r.get::<_, bool>(0))? {
            return Ok(false);
        }
        tx.execute("INSERT OR IGNORE INTO team_members (team_id, user_id) VALUES (?1, ?2)", params![team, m])?;
    }
    Ok(true)
}

async fn create_team(State(app): State<App>, admin: Admin, Json(f): Json<TeamFields>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = team_name(f.name.as_deref().unwrap_or(""))?;
    let (n, members) = (name.clone(), f.members.clone().unwrap_or_default());
    let id = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO teams (name, created_at) VALUES (?1, ?2)", params![n, now()])?;
            let id = tx.last_insert_rowid();
            if !set_members(&tx, id, &members)? {
                return Ok(None);
            }
            tx.commit()?;
            Ok(Some(id))
        })
        .await
        .map_err(|e| unique(e, "team"))?
        .ok_or_else(|| ApiError::bad("One of those members is not a user.").field("members"))?;
    log(&app, &admin.0, "team.create", name.clone(), f.members.map(|m| format!("{} members", m.len()))).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name }))))
}

async fn update_team(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<TeamFields>) -> ApiResult<StatusCode> {
    let name = f.name.as_deref().map(team_name).transpose()?;
    let (n, members) = (name.clone(), f.members.clone());
    let r = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let Some(old) = tx.query_row("SELECT name FROM teams WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()? else { return Ok(Err(ApiError::not_found("team"))) };
            if let Some(n) = &n {
                tx.execute("UPDATE teams SET name = ?1 WHERE id = ?2", params![n, id])?;
            }
            if let Some(m) = &members
                && !set_members(&tx, id, m)?
            {
                return Ok(Err(ApiError::bad("One of those members is not a user.").field("members")));
            }
            tx.commit()?;
            Ok(Ok(old))
        })
        .await
        .map_err(|e| unique(e, "team"))?;
    let old = r?;
    let detail = [name.map(|n| format!("name {n}")), f.members.map(|m| format!("{} members", m.len()))].into_iter().flatten().collect::<Vec<_>>().join(", ");
    log(&app, &admin.0, "team.update", old, (!detail.is_empty()).then_some(detail)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/teams/{id}`: its members stay; only the team goes.
async fn remove_team(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let name = app
        .db
        .call(move |c| {
            let name = c.query_row("SELECT name FROM teams WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?;
            c.execute("DELETE FROM teams WHERE id = ?1", [id])?;
            Ok(name)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("team"))?;
    log(&app, &admin.0, "team.remove", name, None).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use crate::access::api_tests::World;
    use axum::http::StatusCode;
    use serde_json::json;

    #[tokio::test]
    async fn an_administrator_makes_renames_disables_resets_and_removes_a_user() {
        let w = World::new().await;
        let (s, b) = w.as_("admin", "POST", "/api/users", Some(json!({ "name": "viewer", "password": "short" }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("password")));
        let (s, b) = w.as_("admin", "POST", "/api/users", Some(json!({ "name": "viewer", "password": "a long enough password" }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let id = b["id"].as_i64().unwrap();
        let (s, _) = w.as_("admin", "POST", "/api/users", Some(json!({ "name": "VIEWER", "password": "a long enough password" }))).await;
        assert_eq!(s, StatusCode::CONFLICT, "names are unique without regard to case");

        let login = |name: &str, pw: &str| crate::api::tests::post("/api/login").body(crate::api::tests::json_body(json!({ "name": name, "password": pw }))).unwrap();
        let (s, _, _) = crate::api::tests::call(&w.app, login("viewer", "a long enough password")).await;
        assert_eq!(s, StatusCode::OK, "a local user signs in with the password given");

        let (_, list) = w.as_("admin", "GET", "/api/users", None).await;
        let v = list.as_array().unwrap().iter().find(|u| u["name"] == "viewer").unwrap().clone();
        assert_eq!((v["kind"].as_str(), v["role"].as_str(), v["disabled"].as_bool()), (Some("local"), Some("user"), Some(false)));
        assert!(v["last_login_at"].is_i64(), "the last sign-in is shown");

        let (s, _) = w.as_("admin", "PUT", &format!("/api/users/{id}"), Some(json!({ "name": "watcher", "disabled": true }))).await;
        assert_eq!(s, StatusCode::OK);
        let sessions: i64 = w.app.db.call(move |c| Ok(c.query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert_eq!(sessions, 0, "disabled: signed out at once");
        let (s, _, _) = crate::api::tests::call(&w.app, login("watcher", "a long enough password")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        w.as_("admin", "PUT", &format!("/api/users/{id}"), Some(json!({ "disabled": false }))).await;
        let (s, _) = w.as_("admin", "POST", &format!("/api/users/{id}/password"), Some(json!({ "password": "another long password" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _, _) = crate::api::tests::call(&w.app, login("watcher", "another long password")).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = w.as_("admin", "DELETE", &format!("/api/users/{id}"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=user", None).await;
        let actions: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
        assert_eq!(actions, ["user.remove", "user.password", "user.update", "user.update", "user.create"]);
    }

    #[tokio::test]
    async fn the_last_administrator_and_oneself_cannot_be_taken_away_by_accident() {
        let w = World::new().await;
        let (s, b) = w.as_("admin", "PUT", "/api/users/1", Some(json!({ "role": "user" }))).await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
        let (s, _) = w.as_("admin", "PUT", "/api/users/1", Some(json!({ "disabled": true }))).await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _) = w.as_("admin", "DELETE", "/api/users/1", None).await;
        assert_eq!(s, StatusCode::CONFLICT);
        // Another administrator may not take the first one's place away either while it is the last.
        w.as_("admin", "PUT", "/api/users/11", Some(json!({ "role": "admin" }))).await;
        w.app.db.call(|c| Ok(c.execute("UPDATE users SET disabled = 1 WHERE id = 11", [])?)).await.unwrap();
        let (s, b) = w.as_("admin", "PUT", "/api/users/1", Some(json!({ "name": "captain" }))).await;
        assert_eq!(s, StatusCode::OK, "renaming oneself is fine: {b}");
        w.app.db.call(|c| Ok(c.execute("UPDATE users SET disabled = 0 WHERE id = 11", [])?)).await.unwrap();
        let (s, b) = w.as_("admin", "DELETE", "/api/users/11", None).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "another administrator, with one left: {b}");
        let (s, _) = w.as_("admin", "PUT", "/api/users/12", Some(json!({ "role": "root" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_person_changes_their_own_password_with_the_one_it_replaces() {
        let w = World::new().await;
        let (s, b) = w.as_("admin", "PUT", "/api/me/password", Some(json!({ "current": "wrong", "password": "a new long password" }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("current")));
        let other = crate::auth::start(&w.app.db, 1, None, None).await.unwrap();
        let (s, _) = w.as_("admin", "PUT", "/api/me/password", Some(json!({ "current": "a long enough password", "password": "a new long password" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(crate::auth::lookup(&w.app.db, &other).await.unwrap().is_none(), "every other session ends");
        let (s, _) = w.as_("admin", "GET", "/api/me", None).await;
        assert_eq!(s, StatusCode::OK, "this one stays");
        w.app.db.call(|c| Ok(c.execute("UPDATE users SET password_hash = '!' WHERE id = 13", [])?)).await.unwrap();
        let (s, _) = w.as_("helpdesk", "PUT", "/api/me/password", Some(json!({ "current": "x", "password": "a new long password" }))).await;
        assert_eq!(s, StatusCode::CONFLICT, "an SSO user has no password here");
        let (s, _) = w.as_("admin", "POST", "/api/users/13/password", Some(json!({ "password": "a new long password" }))).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn the_current_password_is_held_to_the_sign_in_limit_and_logged() {
        let w = World::new().await;
        let wrong = json!({ "current": "not my password", "password": "a new long password" });
        for _ in 0..crate::auth::Limiter::LIMIT {
            let (s, _) = w.as_("admin", "PUT", "/api/me/password", Some(wrong.clone())).await;
            assert_eq!(s, StatusCode::BAD_REQUEST);
        }
        let right = json!({ "current": "a long enough password", "password": "a new long password" });
        let (s, _) = w.as_("admin", "PUT", "/api/me/password", Some(right)).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "five wrong ones lock it, as at sign-in");
        let login = crate::api::tests::post("/api/login").body(crate::api::tests::json_body(json!({ "name": "admin", "password": "a long enough password" }))).unwrap();
        let (s, _, _) = crate::api::tests::call(&w.app, login).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "the name is locked for signing in too");
        let failed: i64 = w.app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM auth_log WHERE event = 'password.failed' AND ok = 0 AND user_id = 1", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(failed as usize, crate::auth::Limiter::LIMIT, "every wrong one is in the auth log");
    }

    #[tokio::test]
    async fn a_new_password_revokes_the_api_tokens_made_under_the_old_one() {
        let w = World::new().await;
        let db = w.app.db.clone();
        let tokens = |id: i64| {
            let db = db.clone();
            async move { db.call(move |c| Ok(c.query_row("SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1", [id], |r| r.get::<_, i64>(0))?)).await.unwrap() }
        };
        let (s, b) = w.as_("admin", "POST", "/api/tokens", Some(json!({ "name": "ci" }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let (s, _) = w.as_("admin", "PUT", "/api/me/password", Some(json!({ "current": "a long enough password", "password": "a new long password" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(tokens(1).await, 0, "one's own change revokes one's tokens");
        let operator = w.people.iter().position(|p| p.0 == "operator").unwrap() as i64 + 10;
        let (s, b) = w.as_("operator", "POST", "/api/tokens", Some(json!({ "name": "ci" }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let (s, _) = w.as_("admin", "POST", &format!("/api/users/{operator}/password"), Some(json!({ "password": "another long password" }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(tokens(operator).await, 0, "an administrator's reset revokes them too");
        let (_, log) = w.as_("admin", "GET", "/api/audit?action=user.password", None).await;
        assert!(log["entries"][0]["detail"].as_str().is_some_and(|d| d.contains("1 API token")), "{log}");
    }

    #[tokio::test]
    async fn teams_are_made_with_members_and_a_member_sees_their_own_teams_without_the_members() {
        let w = World::new().await;
        let nobody = w.people.iter().position(|p| p.0 == "nobody").unwrap() as i64 + 10;
        let (s, b) = w.as_("admin", "POST", "/api/teams", Some(json!({ "name": "Support", "members": [nobody] }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let team = b["id"].as_i64().unwrap();
        let (s, _) = w.as_("admin", "POST", "/api/teams", Some(json!({ "name": "support" }))).await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _) = w.as_("nobody", "POST", "/api/teams", Some(json!({ "name": "Mine" }))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "only an Administrator makes teams");
        let (_, teams) = w.as_("nobody", "GET", "/api/teams", None).await;
        assert_eq!((teams[0]["name"].as_str(), teams[0]["members"].is_null()), (Some("Support"), true), "their own teams, without the members");
        let (_, teams) = w.as_("helpdesk", "GET", "/api/teams", None).await;
        assert_eq!(teams, json!([]), "not in it");
        let (s, _) = w.as_("admin", "DELETE", &format!("/api/teams/{team}"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, teams) = w.as_("nobody", "GET", "/api/teams", None).await;
        assert_eq!(teams, json!([]), "the team is gone");
    }

    #[tokio::test]
    async fn the_auth_log_has_sign_ins_failures_and_sign_outs_filtered_and_as_csv() {
        let w = World::new().await;
        let login = |pw: &str| crate::api::tests::post("/api/login").header("user-agent", "qa-browser").body(crate::api::tests::json_body(json!({ "name": "admin", "password": pw }))).unwrap();
        crate::api::tests::call(&w.app, login("a long enough password")).await;
        crate::api::tests::call(&w.app, login("not it")).await;
        let (_, all) = w.as_("admin", "GET", "/api/auth-log", None).await;
        let events: Vec<&str> = all["entries"].as_array().unwrap().iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(events, ["login.failed", "login"]);
        assert_eq!(all["entries"][1]["user_agent"], "qa-browser");
        let (_, failed) = w.as_("admin", "GET", "/api/auth-log?ok=false", None).await;
        assert_eq!(failed["entries"].as_array().unwrap().len(), 1);
        let (_, none) = w.as_("admin", "GET", "/api/auth-log?user=nobody", None).await;
        assert_eq!(none["entries"].as_array().unwrap().len(), 0);
        let (_, cookie, csrf) = w.people[0].clone();
        let r = axum::http::Request::get("/api/auth-log?format=csv").header("cookie", &cookie).body(axum::body::Body::empty()).unwrap();
        let resp = tower::ServiceExt::oneshot(crate::api::router(w.app.clone()), r).await.unwrap();
        assert_eq!(resp.headers()["content-type"], "text/csv; charset=utf-8");
        let body = String::from_utf8(http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes().to_vec()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "at,event,method,ok,user_name,ip,user_agent,detail");
        assert!(lines[1].contains(",login.failed,password,false,admin,"), "{body}");
        let r = axum::http::Request::get("/api/audit?format=csv").header("cookie", &cookie).body(axum::body::Body::empty()).unwrap();
        let resp = tower::ServiceExt::oneshot(crate::api::router(w.app.clone()), r).await.unwrap();
        let body = String::from_utf8(http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(body.starts_with("at,user_name,action,target,detail,ip,via\r\n") && body.contains(",admin,setup,admin,"), "{body}");
        let _ = csrf;
        let (s, _) = w.as_("operator", "GET", "/api/auth-log", None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
