//! Environment groups and tags. An environment is in at most one group (`environments.group_id`) and has any number of
//! tags (`environment_tags`). Groups are what roles will be given on in M4, beside single environments; tags are for
//! finding and, in M5, for deploying to many hosts at once ("Groups" in DECISIONS.md). Everybody signed in sees them;
//! only an administrator changes them, and every change is in the activity log.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::Connection, rusqlite::OptionalExtension, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/environment-groups", get(list_groups).post(create_group))
        .route("/api/environment-groups/{id}", axum::routing::put(update_group).delete(remove_group))
        .route("/api/tags", get(list_tags).post(create_tag))
        .route("/api/tags/{id}", axum::routing::put(rename_tag).delete(remove_tag))
}

/// A field that may be left out, given as null, or given: `None`, `Some(None)`, `Some(Some(v))`.
pub fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

fn check_group_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(ApiError::bad("A group's name is 1 to 64 characters.").field("name"));
    }
    Ok(n.to_string())
}

/// A tag as given, trimmed: 1 to 40 characters, no comma (a list of tags is typed with commas).
fn check_tag(name: &str, field: &'static str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 40 || n.chars().any(|c| c.is_control() || c == ',') {
        return Err(ApiError::bad("A tag is 1 to 40 characters, without a comma.").field(field));
    }
    Ok(n.to_string())
}

/// Tags as given: each checked, and each once, the first spelling kept (they are case-blind).
pub fn tag_list(names: &[String]) -> ApiResult<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for n in names {
        let t = check_tag(n, "tags")?;
        if seen.insert(t.to_lowercase()) {
            out.push(t);
        }
    }
    Ok(out)
}

/// Where an environment goes when it is made or edited: its group (`Some(None)` is none) and its tags (the whole
/// list), each `None` when left as it is.
#[derive(Default)]
pub struct Placement {
    pub group: Option<Option<i64>>,
    pub tags: Option<Vec<String>>,
}

impl Placement {
    /// Checks the group is there and the tags are tags; answers what the activity log says of it.
    pub async fn check(&mut self, app: &App) -> ApiResult<Option<String>> {
        let mut said = Vec::new();
        if let Some(g) = self.group {
            match g {
                None => said.push("no group".to_string()),
                Some(id) => {
                    let name = app.db.call(move |c| Ok(c.query_row("SELECT name FROM environment_groups WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?)).await?;
                    let name = name.ok_or_else(|| ApiError::bad("There is no such group.").field("group_id"))?;
                    said.push(format!("group {name}"));
                }
            }
        }
        if let Some(t) = &self.tags {
            let t = tag_list(t)?;
            said.push(if t.is_empty() { "no tags".to_string() } else { format!("tags {}", t.join(", ")) });
            self.tags = Some(t);
        }
        Ok((!said.is_empty()).then(|| said.join("; ")))
    }

    /// Writes it for environment `env` in the caller's transaction; answers the tags it had to make.
    pub fn apply(&self, c: &Connection, env: i64) -> crate::db::rusqlite::Result<Vec<String>> {
        if let Some(g) = self.group {
            c.execute("UPDATE environments SET group_id = ?1 WHERE id = ?2", params![g, env])?;
        }
        let mut made = Vec::new();
        if let Some(tags) = &self.tags {
            c.execute("DELETE FROM environment_tags WHERE environment_id = ?1", [env])?;
            for t in tags {
                if c.execute("INSERT OR IGNORE INTO tags (name, created_at) VALUES (?1, ?2)", params![t, now()])? > 0 {
                    made.push(t.clone());
                }
                c.execute("INSERT OR IGNORE INTO environment_tags (environment_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2", params![env, t])?;
            }
        }
        Ok(made)
    }
}

/// Logs each tag `made` on the way, as a change of its own.
pub async fn log_made(app: &App, admin: &Admin, made: Vec<String>) {
    for t in made {
        audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "tag.create", target: Some(t), detail: None }).await;
    }
}

fn unique(e: anyhow::Error, table: &str, what: &str) -> ApiError {
    if format!("{e:#}").contains(&format!("UNIQUE constraint failed: {table}.name")) {
        return ApiError::conflict(format!("There is a {what} of that name.")).field("name");
    }
    e.into()
}

/// The ids in a column of `sql`'s rows.
fn ids(c: &Connection, sql: &str, id: i64) -> crate::db::rusqlite::Result<Vec<i64>> {
    c.prepare(sql)?.query_map([id], |r| r.get(0))?.collect()
}

fn group_json(c: &Connection, id: i64) -> crate::db::rusqlite::Result<Option<Value>> {
    let g = c.query_row("SELECT name, description FROM environment_groups WHERE id = ?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional()?;
    let Some((name, description)) = g else { return Ok(None) };
    let envs = ids(c, "SELECT id FROM environments WHERE group_id = ?1 ORDER BY id", id)?;
    Ok(Some(json!({ "id": id, "name": name, "description": description, "environments": envs })))
}

fn tag_json(c: &Connection, id: i64) -> crate::db::rusqlite::Result<Option<Value>> {
    let Some(name) = c.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()? else { return Ok(None) };
    let envs = ids(c, "SELECT environment_id FROM environment_tags WHERE tag_id = ?1 ORDER BY environment_id", id)?;
    Ok(Some(json!({ "id": id, "name": name, "environments": envs })))
}

fn all(c: &Connection, sql: &str, one: fn(&Connection, i64) -> crate::db::rusqlite::Result<Option<Value>>) -> anyhow::Result<Vec<Value>> {
    let list: Vec<i64> = c.prepare(sql)?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(list.into_iter().map(|id| one(c, id)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect())
}

/// Leaves out of each group's or tag's `environments` those the person has no role on ("Access" 6).
async fn only_seen(app: &App, a: &Authed, mut list: Vec<Value>) -> ApiResult<Vec<Value>> {
    if a.session.user.is_admin() {
        return Ok(list);
    }
    let levels = crate::access::levels(&app.db, &a.session.user).await?;
    for x in &mut list {
        if let Some(envs) = x["environments"].as_array() {
            x["environments"] = json!(envs.iter().filter(|e| e.as_i64().is_some_and(|e| levels.contains_key(&e))).collect::<Vec<_>>());
        }
    }
    Ok(list)
}

/// `GET /api/environment-groups`: every group by name, with the ids of the environments in it.
async fn list_groups(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let list = app.db.call(|c| all(c, "SELECT id FROM environment_groups ORDER BY name", group_json)).await?;
    Ok(Json(json!(only_seen(&app, &a, list).await?)))
}

#[derive(Deserialize, Default)]
struct GroupFields {
    name: Option<String>,
    description: Option<String>,
    /// Every environment in the group: those left out leave it. Left out, the members stay.
    environments: Option<Vec<i64>>,
}

/// The names of `envs`, all of which must be environments.
async fn env_names(app: &App, envs: &[i64]) -> ApiResult<String> {
    let list = envs.to_vec();
    let names = app
        .db
        .call(move |c| {
            let mut st = c.prepare("SELECT name FROM environments WHERE id = ?1")?;
            let mut out = Vec::new();
            for id in list {
                out.push(st.query_row([id], |r| r.get::<_, String>(0)).optional()?);
            }
            Ok(out)
        })
        .await?;
    if names.iter().any(Option::is_none) {
        return Err(ApiError::bad("One of those environments is not there.").field("environments"));
    }
    let names: Vec<String> = names.into_iter().flatten().collect();
    Ok(if names.is_empty() { "no environments".into() } else { names.join(", ") })
}

fn description(d: Option<&str>) -> ApiResult<Option<String>> {
    match d.map(str::trim) {
        Some(d) if d.chars().count() > 200 => Err(ApiError::bad("A description is at most 200 characters.").field("description")),
        d => Ok(d.map(str::to_string)),
    }
}

fn set_members(c: &Connection, group: i64, envs: &[i64]) -> crate::db::rusqlite::Result<()> {
    c.execute("UPDATE environments SET group_id = NULL WHERE group_id = ?1", [group])?;
    for e in envs {
        c.execute("UPDATE environments SET group_id = ?1 WHERE id = ?2", params![group, e])?;
    }
    Ok(())
}

/// `POST /api/environment-groups`: `{name, description, environments}`.
async fn create_group(State(app): State<App>, admin: Admin, Json(f): Json<GroupFields>) -> ApiResult<Response> {
    let name = check_group_name(f.name.as_deref().unwrap_or(""))?;
    let desc = description(f.description.as_deref())?.unwrap_or_default();
    let envs = f.environments.unwrap_or_default();
    let members = if envs.is_empty() { None } else { Some(env_names(&app, &envs).await?) };
    let n = name.clone();
    let g = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO environment_groups (name, description, created_at) VALUES (?1, ?2, ?3)", params![n, desc, now()])?;
            let id = tx.last_insert_rowid();
            set_members(&tx, id, &envs)?;
            let g = group_json(&tx, id)?;
            tx.commit()?;
            Ok(g)
        })
        .await
        .map_err(|e| unique(e, "environment_groups", "group"))?;
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "environment-group.create", target: Some(name), detail: members }).await;
    Ok((StatusCode::CREATED, Json(g)).into_response())
}

/// `PUT /api/environment-groups/{id}`: a new name or description; `environments`, when given, is the whole membership.
async fn update_group(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<GroupFields>) -> ApiResult<Json<Value>> {
    let old = app.db.call(move |c| Ok(c.query_row("SELECT name FROM environment_groups WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?)).await?.ok_or_else(|| ApiError::not_found("group"))?;
    let name = match &f.name {
        Some(n) => check_group_name(n)?,
        None => old.clone(),
    };
    let desc = description(f.description.as_deref())?;
    let members = match &f.environments {
        Some(e) => Some(env_names(&app, e).await?),
        None => None,
    };
    let (n, envs) = (name.clone(), f.environments);
    let g = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE environment_groups SET name = ?1, description = COALESCE(?2, description) WHERE id = ?3", params![n, desc, id])?;
            if let Some(envs) = envs {
                set_members(&tx, id, &envs)?;
            }
            let g = group_json(&tx, id)?;
            tx.commit()?;
            Ok(g)
        })
        .await
        .map_err(|e| unique(e, "environment_groups", "group"))?;
    let renamed = (old != name).then(|| format!("was {old}"));
    let detail = [renamed, members].into_iter().flatten().collect::<Vec<_>>().join("; ");
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "environment-group.update", target: Some(name), detail: (!detail.is_empty()).then_some(detail) }).await;
    g.map(Json).ok_or_else(|| ApiError::not_found("group"))
}

/// `DELETE /api/environment-groups/{id}`: the group goes; its environments stay, in no group.
async fn remove_group(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let name = app
        .db
        .call(move |c| {
            let name = c.query_row("SELECT name FROM environment_groups WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?;
            c.execute("DELETE FROM environment_groups WHERE id = ?1", [id])?;
            Ok(name)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("group"))?;
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "environment-group.remove", target: Some(name), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/tags`: every tag by name, with the ids of the environments that have it.
async fn list_tags(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let list = app.db.call(|c| all(c, "SELECT id FROM tags ORDER BY name", tag_json)).await?;
    Ok(Json(json!(only_seen(&app, &a, list).await?)))
}

#[derive(Deserialize)]
struct TagFields {
    #[serde(default)]
    name: String,
}

/// `POST /api/tags`: `{name}`.
async fn create_tag(State(app): State<App>, admin: Admin, Json(f): Json<TagFields>) -> ApiResult<Response> {
    let name = check_tag(&f.name, "name")?;
    let n = name.clone();
    let t = app
        .db
        .call(move |c| {
            c.execute("INSERT INTO tags (name, created_at) VALUES (?1, ?2)", params![n, now()])?;
            Ok(tag_json(c, c.last_insert_rowid())?)
        })
        .await
        .map_err(|e| unique(e, "tags", "tag"))?;
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "tag.create", target: Some(name), detail: None }).await;
    Ok((StatusCode::CREATED, Json(t)).into_response())
}

/// `PUT /api/tags/{id}`: `{name}`, a new name for it on every environment that has it.
async fn rename_tag(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<TagFields>) -> ApiResult<Json<Value>> {
    let name = check_tag(&f.name, "name")?;
    let n = name.clone();
    let (old, t) = app
        .db
        .call(move |c| {
            let old = c.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?;
            c.execute("UPDATE tags SET name = ?1 WHERE id = ?2", params![n, id])?;
            Ok((old, tag_json(c, id)?))
        })
        .await
        .map_err(|e| unique(e, "tags", "tag"))?;
    let (Some(old), Some(t)) = (old, t) else { return Err(ApiError::not_found("tag")) };
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "tag.update", target: Some(name), detail: Some(format!("was {old}")) }).await;
    Ok(Json(t))
}

/// `DELETE /api/tags/{id}`: the tag goes, off every environment that had it.
async fn remove_tag(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let (name, on) = app
        .db
        .call(move |c| {
            let name = c.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional()?;
            let on: i64 = c.query_row("SELECT COUNT(*) FROM environment_tags WHERE tag_id = ?1", [id], |r| r.get(0))?;
            c.execute("DELETE FROM tags WHERE id = ?1", [id])?;
            Ok((name, on))
        })
        .await?;
    let name = name.ok_or_else(|| ApiError::not_found("tag"))?;
    let detail = format!("on {on} environment{}", if on == 1 { "" } else { "s" });
    audit::record(&app.db, Some(&admin.0.session.user), admin.0.ip.clone(), Entry { action: "tag.remove", target: Some(name), detail: Some(detail) }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    async fn audited(app: &crate::state::App, action: &str) -> Vec<(String, Option<String>)> {
        let a = action.to_string();
        app.db
            .call(move |c| Ok(c.prepare("SELECT target, detail FROM audit WHERE action = ?1 ORDER BY id")?.query_map([a], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap()
    }

    /// An agent environment named `name`; answers its id.
    async fn agent(app: &crate::state::App, cookie: &str, csrf: &str, name: &str) -> i64 {
        let (s, _, b) = call(app, req(Method::POST, "/api/environments", cookie, csrf, Some(json!({ "name": name, "kind": "agent" })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        b["environment"]["id"].as_i64().unwrap()
    }

    #[tokio::test]
    async fn a_group_holds_environments_each_in_one_group_at_most() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let vm = agent(&app, &cookie, &csrf, "vm").await;
        let (s, _, b) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": " Production ", "description": "the paying ones", "environments": [1, vm] })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!(b["name"], "Production");
        assert_eq!(b["description"], "the paying ones");
        assert_eq!(b["environments"], json!([1, vm]));
        let prod = b["id"].as_i64().unwrap();

        let (_, _, envs) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!(envs[0]["group"], json!({ "id": prod, "name": "Production" }));
        assert_eq!(envs[1]["group"]["id"], prod);

        // Into another group, it leaves the first.
        let (s, _, b) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": "Lab", "environments": [vm] })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let lab = b["id"].as_i64().unwrap();
        let (s, _, list) = call(&app, req(Method::GET, "/api/environment-groups", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(list, json!([
            { "id": lab, "name": "Lab", "description": "", "environments": [vm] },
            { "id": prod, "name": "Production", "description": "the paying ones", "environments": [1] },
        ]), "groups by name");

        let (s, _, b) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": "lab" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")), "names are unique, case-blind");
        let (s, _, b) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": "" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("name")));
        let (s, _, b) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": "x", "environments": [99] })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("environments")), "{b}");

        // An edit with `environments` says the whole membership; without it, the members stay.
        let (s, _, b) = call(&app, req(Method::PUT, &format!("/api/environment-groups/{prod}"), &cookie, &csrf, Some(json!({ "name": "Prod" })))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!((b["name"].as_str(), b["environments"].clone()), (Some("Prod"), json!([1])));
        let (_, _, b) = call(&app, req(Method::PUT, &format!("/api/environment-groups/{prod}"), &cookie, &csrf, Some(json!({ "environments": [vm] })))).await;
        assert_eq!(b["environments"], json!([vm]));
        let (_, _, envs) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!(envs[0]["group"], json!(null), "left out, the local one is in no group now");

        // Removing a group leaves its environments in none.
        let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/environment-groups/{prod}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, envs) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!(envs[1]["group"], json!(null));
        assert_eq!(envs[1]["name"], "vm", "the environment stays");
        let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/environment-groups/{prod}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        assert_eq!(audited(&app, "environment-group.create").await, vec![("Production".into(), Some("local, vm".into())), ("Lab".into(), Some("vm".into()))]);
        assert_eq!(audited(&app, "environment-group.update").await, vec![("Prod".into(), Some("was Production".into())), ("Prod".into(), Some("vm".into()))]);
        assert_eq!(audited(&app, "environment-group.remove").await, vec![("Prod".into(), None)]);
    }

    #[tokio::test]
    async fn an_environment_is_given_its_group_and_tags_when_edited() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (_, _, g) = call(&app, req(Method::POST, "/api/environment-groups", &cookie, &csrf, Some(json!({ "name": "Edge" })))).await;
        let edge = g["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "group_id": edge, "tags": ["eu", " prod ", "EU"] })))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["group"], json!({ "id": edge, "name": "Edge" }));
        assert_eq!(b["tags"], json!(["eu", "prod"]), "tags are made as needed, once each, case-blind, by name");
        let (_, _, tags) = call(&app, req(Method::GET, "/api/tags", &cookie, &csrf, None)).await;
        assert_eq!(tags.as_array().unwrap().iter().map(|t| (t["name"].as_str().unwrap(), t["environments"].clone())).collect::<Vec<_>>(), vec![("eu", json!([1])), ("prod", json!([1]))]);

        // Left out, they stay; a rename alone keeps them.
        let (_, _, b) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "name": "home" })))).await;
        assert_eq!((b["group"]["id"].as_i64(), b["tags"].clone()), (Some(edge), json!(["eu", "prod"])));
        // null is no group; [] no tags.
        let (_, _, b) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "group_id": null, "tags": [] })))).await;
        assert_eq!((b["group"].clone(), b["tags"].clone()), (json!(null), json!([])));
        let (s, _, b) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "group_id": 99 })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("group_id")));
        let (s, _, b) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "tags": ["a,b"] })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("tags")));

        // A new environment can come with them too.
        let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "far", "kind": "agent", "group_id": edge, "tags": ["prod"] })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!((b["environment"]["group"]["id"].as_i64(), b["environment"]["tags"].clone()), (Some(edge), json!(["prod"])));
        let far = b["environment"]["id"].as_i64().unwrap();
        // Removed, it leaves its tags behind for the others.
        call(&app, req(Method::DELETE, &format!("/api/environments/{far}"), &cookie, &csrf, None)).await;
        let n: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM environment_tags", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 0);

        let updates = audited(&app, "environment.update").await;
        assert_eq!(updates[0], ("local".into(), Some("group Edge; tags eu, prod".into())));
        assert_eq!(updates[2], ("home".into(), Some("no group; no tags".into())));
        assert_eq!(audited(&app, "tag.create").await.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), vec!["eu", "prod"]);
    }

    #[tokio::test]
    async fn tags_are_made_renamed_and_removed_on_their_own() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::POST, "/api/tags", &cookie, &csrf, Some(json!({ "name": "gpu" })))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        assert_eq!(b["environments"], json!([]));
        let gpu = b["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::POST, "/api/tags", &cookie, &csrf, Some(json!({ "name": "GPU" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("name")));
        call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "tags": ["gpu"] })))).await;
        let (s, _, b) = call(&app, req(Method::PUT, &format!("/api/tags/{gpu}"), &cookie, &csrf, Some(json!({ "name": "cuda" })))).await;
        assert_eq!((s, b["name"].as_str(), b["environments"].clone()), (StatusCode::OK, Some("cuda"), json!([1])));
        let (_, _, envs) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!(envs[0]["tags"], json!(["cuda"]));
        let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/tags/{gpu}"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, envs) = call(&app, req(Method::GET, "/api/environments", &cookie, &csrf, None)).await;
        assert_eq!(envs[0]["tags"], json!([]), "a tag removed is off every environment");
        assert_eq!(audited(&app, "tag.update").await, vec![("cuda".into(), Some("was gpu".into()))]);
        assert_eq!(audited(&app, "tag.remove").await, vec![("cuda".into(), Some("on 1 environment".into()))]);
    }

    #[tokio::test]
    async fn only_an_administrator_changes_groups_and_tags_and_only_with_the_csrf_token() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        for path in ["/api/environment-groups", "/api/tags"] {
            let (s, _, _) = call(&app, req(Method::POST, path, &cookie, "wrong", Some(json!({ "name": "a" })))).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
        }
        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let cookie = format!("freightdock_session={token}");
        let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
        for path in ["/api/environment-groups", "/api/tags"] {
            let (s, _, _) = call(&app, req(Method::GET, path, &cookie, &csrf, None)).await;
            assert_eq!(s, StatusCode::OK, "everybody sees {path}, to filter by");
            let (s, _, _) = call(&app, req(Method::POST, path, &cookie, &csrf, Some(json!({ "name": "a" })))).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
        }
        let (s, _, _) = call(&app, req(Method::PUT, "/api/environments/1", &cookie, &csrf, Some(json!({ "tags": ["x"] })))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
