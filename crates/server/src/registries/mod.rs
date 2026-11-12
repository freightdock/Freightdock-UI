//! Registries: where images come from and go to, and how Freightdock signs in there (Docker Hub, GHCR, GitLab, ECR, Quay,
//! the package registry of one's own Forgejo or Gitea, or any other registry speaking the v2 API). An administrator adds one; the connection is tested before it is saved;
//! the password, token or secret key is sealed and never sent back. A pull, a push, and a container made or recreated
//! from an image on a saved registry's host sign in with its credentials (`auth_for_image`), and so does a compose
//! deploy (`auth_for`, `docker_config`). The browser lists a registry's repositories and their tags
//! ("Registries" in DECISIONS.md).

pub mod ecr;
pub mod hub;
pub mod reference;
pub mod v2;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, Router, routing::get};
use base64::Engine;
use bollard::auth::DockerCredentials;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use v2::Fail;

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/registries", get(list).post(create))
        .route("/api/registries/{id}", get(one).put(update).delete(remove))
        .route("/api/registries/{id}/repositories", get(repositories))
        .route("/api/registries/{id}/tags", get(tags))
}

/// The kinds, each with the address it has unless one is given. A Forgejo or Gitea has none: its registry is the
/// instance's own host.
const KINDS: &[(&str, &str)] = &[
    ("dockerhub", reference::DOCKER_HUB),
    ("ghcr", "ghcr.io"),
    ("gitlab", "registry.gitlab.com"),
    ("quay", "quay.io"),
    ("ecr", ""),
    ("forgejo", ""),
    ("gitea", ""),
    ("custom", ""),
];

/// Docker's own name for Docker Hub's server, as `docker login` writes it.
const HUB_SERVER: &str = "https://index.docker.io/v1/";

/// A registry as saved, its secret opened.
#[derive(Clone, Debug)]
pub struct Registry {
    pub id: i64,
    pub name: String,
    pub kind: String,
    /// `host[:port]`, or `http://host[:port]` for a registry on plain HTTP.
    pub url: String,
    pub host: String,
    pub username: String,
    pub secret: Option<String>,
    pub region: Option<String>,
}

impl Registry {
    /// Where its v2 API is.
    fn api(&self) -> String {
        if self.kind == "dockerhub" {
            hub::REGISTRY.to_string()
        } else if self.url.starts_with("http://") {
            self.url.clone()
        } else {
            format!("https://{}", self.host)
        }
    }

    /// The user name and password a registry takes: as saved, or for ECR the token its keys are exchanged for.
    async fn sign_in(&self) -> Result<Option<(String, String)>, Fail> {
        // A registry brought over from Portainer has its user but not yet its password: until it is given, anonymous.
        if self.username.is_empty() || self.secret.as_deref().is_none_or(str::is_empty) {
            return Ok(None);
        }
        let secret = self.secret.clone().unwrap_or_default();
        if self.kind == "ecr" {
            let region = self.region.clone().unwrap_or_default();
            let t = ecr::token(&ecr::endpoint(&region), &self.username, &secret, &region).await?;
            return Ok(Some((t.user, t.password)));
        }
        Ok(Some((self.username.clone(), secret)))
    }

    async fn client(&self) -> Result<v2::Registry, Fail> {
        Ok(v2::Registry::new(&self.api(), self.sign_in().await?))
    }

    /// What `docker pull` would be told, for an image on this registry.
    fn image_prefix(&self) -> String {
        if self.kind == "dockerhub" { String::new() } else { format!("{}/", self.host) }
    }
}

/// A row as it is kept: the secret still sealed.
fn row(r: &crate::db::rusqlite::Row) -> crate::db::rusqlite::Result<Registry> {
    Ok(Registry { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, url: r.get(3)?, host: r.get(4)?, username: r.get(5)?, secret: r.get(6)?, region: r.get(7)? })
}

const COLUMNS: &str = "id, name, kind, url, host, username, secret, region";

/// What the browser sees of a registry: everything but the secret, and whether there is one.
fn shown(r: &Registry) -> Value {
    json!({
        "id": r.id, "name": r.name, "kind": r.kind, "url": r.url, "host": r.host, "username": r.username,
        "has_secret": r.secret.as_deref().is_some_and(|s| !s.is_empty()), "region": r.region, "image_prefix": r.image_prefix(),
    })
}

/// Registries as saved, `id` alone or every one, their secrets opened.
async fn fetch(app: &App, id: Option<i64>, host: Option<String>) -> ApiResult<Vec<Registry>> {
    let rows = app
        .db
        .call(move |c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM registries WHERE (?1 IS NULL OR id = ?1) AND (?2 IS NULL OR host = ?2) ORDER BY name"))?;
            let rows = st.query_map(params![id, host], row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    rows.into_iter()
        .map(|mut r| {
            r.secret = r.secret.filter(|s| !s.is_empty()).map(|s| app.sealer.open(&s)).transpose()?;
            Ok(r)
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(Into::into)
}

async fn get_one(app: &App, id: i64) -> ApiResult<Registry> {
    fetch(app, Some(id), None).await?.pop().ok_or_else(|| ApiError::not_found("registry"))
}

/// What Docker is told when it pulls or pushes on a saved registry, and which registry that was.
pub struct Auth {
    pub registry: String,
    pub host: String,
    pub credentials: DockerCredentials,
}

/// The credentials for the registry at `host` (any of Docker Hub's names for Docker Hub), when one is saved there with
/// a user name. For the stacks too: `docker compose` gets them through `docker_config`.
pub async fn auth_for(app: &App, host: &str) -> ApiResult<Option<Auth>> {
    let host = reference::host(host);
    let Some(r) = fetch(app, None, Some(host.clone())).await?.pop() else { return Ok(None) };
    let Some((username, password)) = r.sign_in().await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("Signing in to {}: {}", r.name, e.message())))? else {
        return Ok(None);
    };
    let serveraddress = if r.kind == "dockerhub" { HUB_SERVER.to_string() } else { host.clone() };
    Ok(Some(Auth { registry: r.name, host, credentials: DockerCredentials { username: Some(username), password: Some(password), serveraddress: Some(serveraddress), ..Default::default() } }))
}

/// The credentials for the registry `image` is on, when one is saved there.
pub async fn auth_for_image(app: &App, image: &str) -> ApiResult<Option<Auth>> {
    match reference::parse(image) {
        Some(r) => auth_for(app, &r.host).await,
        None => Ok(None),
    }
}

/// Every saved registry's host, for a compose job, which may pull from any of them.
pub async fn hosts(app: &App) -> ApiResult<Vec<String>> {
    Ok(app.db.call(|c| Ok(c.prepare("SELECT host FROM registries ORDER BY name")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?)).await?)
}

/// A Docker client configuration (`config.json`) holding these credentials, for `docker compose` (with `DOCKER_CONFIG`
/// at the folder it is written to, `stacks::compose::registry_config`).
pub fn docker_config(auths: &[Auth]) -> Value {
    let mut out = serde_json::Map::new();
    for a in auths {
        let c = &a.credentials;
        let pair = format!("{}:{}", c.username.as_deref().unwrap_or_default(), c.password.as_deref().unwrap_or_default());
        let server = c.serveraddress.clone().unwrap_or_else(|| a.host.clone());
        out.insert(server, json!({ "auth": base64::engine::general_purpose::STANDARD.encode(pair) }));
    }
    json!({ "auths": out })
}

/// `GET /api/registries`: every registry, without its secret.
async fn list(State(app): State<App>, _: Authed) -> ApiResult<Json<Value>> {
    Ok(Json(json!(fetch(&app, None, None).await?.iter().map(shown).collect::<Vec<_>>())))
}

async fn one(State(app): State<App>, _: Authed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(shown(&get_one(&app, id).await?)))
}

#[derive(Deserialize, Default)]
struct Fields {
    name: Option<String>,
    kind: Option<String>,
    url: Option<String>,
    username: Option<String>,
    /// A password, a token, or ECR's secret access key; kept sealed. Empty or left out keeps the one there is.
    password: Option<String>,
    region: Option<String>,
}

fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(ApiError::bad("A name is 1 to 64 characters.").field("name"));
    }
    Ok(n.to_string())
}

/// A registry's address as saved: `host[:port]`, `http://` kept for plain HTTP, and its host for matching.
pub fn check_url(kind: &str, url: &str) -> ApiResult<(String, String)> {
    let bad = || ApiError::bad("An address is the registry's host, and its port if not 443: registry.example.com, or http://192.0.2.5:5000 for one on plain HTTP.").field("url");
    let default = KINDS.iter().find(|k| k.0 == kind).map(|k| k.1).unwrap_or_default();
    if kind == "dockerhub" {
        return Ok((default.to_string(), default.to_string()));
    }
    let u = url.trim().trim_end_matches('/');
    let u = if u.is_empty() { default } else { u };
    let plain = u.starts_with("http://");
    let host = reference::host(u);
    let bare = u.strip_prefix("http://").or_else(|| u.strip_prefix("https://")).unwrap_or(u);
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) if !n.ends_with(']') || p.chars().all(|c| c.is_ascii_digit()) => (n, Some(p)),
        _ => (host.as_str(), None),
    };
    let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'));
    let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p > 0));
    if host.is_empty() || bare.contains('/') || !name_ok || !port_ok || bare.contains('@') {
        return Err(bad());
    }
    if host == reference::DOCKER_HUB {
        return Err(ApiError::bad("That is Docker Hub: add it as Docker Hub.").field("url"));
    }
    Ok((if plain { format!("http://{host}") } else { host.clone() }, host))
}

fn unique(e: anyhow::Error) -> ApiError {
    let m = format!("{e:#}");
    if m.contains("UNIQUE constraint failed: registries.name") {
        return ApiError::conflict("There is a registry of that name.").field("name");
    }
    if m.contains("UNIQUE constraint failed: registries.host") {
        return ApiError::conflict("There is a registry at that address already: one sign-in a host, as Docker has.").field("url");
    }
    e.into()
}

/// The registry these fields describe, checked: its name, kind, address, and for ECR its keys and region.
fn settle(f: &Fields, was: Option<&Registry>) -> ApiResult<Registry> {
    let name = check_name(f.name.as_deref().or(was.map(|w| w.name.as_str())).unwrap_or(""))?;
    let kind = f.kind.clone().or(was.map(|w| w.kind.clone())).unwrap_or_default();
    if !KINDS.iter().any(|k| k.0 == kind) {
        return Err(ApiError::bad("A registry is Docker Hub, GitHub (ghcr.io), GitLab, Amazon ECR, Quay, a Forgejo, a Gitea or another registry.").field("kind"));
    }
    let url = f.url.as_deref().or(was.filter(|w| w.kind == kind).map(|w| w.url.as_str())).unwrap_or("");
    let (url, host) = check_url(&kind, url)?;
    let username = f.username.as_deref().map(str::trim).map(str::to_string).or(was.map(|w| w.username.clone())).unwrap_or_default();
    if username.chars().count() > 256 || username.chars().any(char::is_control) {
        return Err(ApiError::bad("That is not a user name.").field("username"));
    }
    let secret = match f.password.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(p.to_string()),
        None => was.and_then(|w| w.secret.clone()),
    };
    if !username.is_empty() && secret.is_none() {
        return Err(ApiError::bad(if kind == "ecr" { "Give the secret access key." } else { "Give the password or token for that user." }).field("password"));
    }
    let mut region = f.region.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string).or(was.and_then(|w| w.region.clone()));
    if kind == "ecr" {
        if username.is_empty() {
            return Err(ApiError::bad("Give the access key's id.").field("username"));
        }
        region = region.or_else(|| ecr::region_of(&host));
        if !region.as_deref().is_some_and(ecr::valid_region) {
            return Err(ApiError::bad("Give the region, such as eu-north-1.").field("region"));
        }
        if url.starts_with("http://") {
            return Err(ApiError::bad("ECR is reached over HTTPS.").field("url"));
        }
    } else {
        region = None;
    }
    Ok(Registry { id: was.map_or(0, |w| w.id), name, kind, url, host, username, secret, region })
}

/// The connection test on saving: the registry answers, and takes the credentials.
async fn test(r: &Registry) -> ApiResult<()> {
    let result = match r.client().await {
        Ok(c) => c.check().await,
        Err(e) => Err(e),
    };
    result.map_err(|e| {
        let field = match &e {
            Fail::Denied(_) if r.kind == "ecr" => "password",
            Fail::Denied(_) => if r.username.is_empty() { "username" } else { "password" },
            _ => "url",
        };
        ApiError::bad(e.message().to_string()).field(field)
    })
}

async fn log(app: &App, admin: &Admin, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

/// `POST /api/registries`: `{name, kind, url, username, password, region}`, tested, then saved.
async fn create(State(app): State<App>, admin: Admin, Json(f): Json<Fields>) -> ApiResult<(StatusCode, Json<Value>)> {
    let r = settle(&f, None)?;
    test(&r).await?;
    let sealed = r.secret.as_deref().map(|s| app.sealer.seal(s)).transpose()?;
    let (name, kind, url, host, username, region) = (r.name.clone(), r.kind.clone(), r.url.clone(), r.host.clone(), r.username.clone(), r.region.clone());
    let id = app
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO registries (name, kind, url, host, username, secret, region, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![name, kind, url, host, username, sealed, region, now()],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
        .map_err(unique)?;
    log(&app, &admin, "registry.create", r.name.clone(), Some(format!("{} at {}", r.kind, r.host))).await;
    Ok((StatusCode::CREATED, Json(shown(&get_one(&app, id).await?))))
}

/// A registry brought over from elsewhere (Portainer), which gives out its user but never its password: saved without
/// the test, and without a secret until an administrator gives one ("Portainer" 7). Until then it is used anonymously.
#[allow(clippy::too_many_arguments)]
pub async fn import(app: &App, who: &audit::Who, name: &str, kind: &str, url: &str, username: &str, region: Option<String>, detail: String) -> ApiResult<i64> {
    let name = check_name(name)?;
    if !KINDS.iter().any(|k| k.0 == kind) {
        return Err(ApiError::bad("That is not a kind of registry Freightdock knows.").field("kind"));
    }
    let (url, host) = check_url(kind, url)?;
    let region = if kind == "ecr" { region.filter(|r| ecr::valid_region(r)).or_else(|| ecr::region_of(&host)) } else { None };
    let (n, k, u, h, user) = (name.clone(), kind.to_string(), url, host.clone(), username.trim().to_string());
    let id = app
        .db
        .call(move |c| {
            c.execute("INSERT INTO registries (name, kind, url, host, username, secret, region, created_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)", params![n, k, u, h, user, region, now()])?;
            Ok(c.last_insert_rowid())
        })
        .await
        .map_err(unique)?;
    audit::record_by(&app.db, who, Entry { action: "registry.create", target: Some(name), detail: Some(format!("{kind} at {host}; {detail}")) }).await;
    Ok(id)
}

/// `PUT /api/registries/{id}`: what changed; an empty password keeps the one there is. Tested again when the address
/// or the sign-in changed.
async fn update(State(app): State<App>, admin: Admin, Path(id): Path<i64>, Json(f): Json<Fields>) -> ApiResult<Json<Value>> {
    let was = get_one(&app, id).await?;
    let r = settle(&f, Some(&was))?;
    let resigned = (&r.kind, &r.url, &r.username, &r.secret, &r.region) != (&was.kind, &was.url, &was.username, &was.secret, &was.region);
    if resigned {
        test(&r).await?;
    }
    let sealed = r.secret.as_deref().map(|s| app.sealer.seal(s)).transpose()?;
    let (name, kind, url, host, username, region) = (r.name.clone(), r.kind.clone(), r.url.clone(), r.host.clone(), r.username.clone(), r.region.clone());
    app.db
        .call(move |c| {
            c.execute(
                "UPDATE registries SET name = ?1, kind = ?2, url = ?3, host = ?4, username = ?5, secret = ?6, region = ?7 WHERE id = ?8",
                params![name, kind, url, host, username, sealed, region, id],
            )?;
            Ok(())
        })
        .await
        .map_err(unique)?;
    let detail = match (was.name != r.name, resigned) {
        (true, true) => Some(format!("was {}; address or sign-in changed", was.name)),
        (true, false) => Some(format!("was {}", was.name)),
        (false, true) => Some("address or sign-in changed".into()),
        (false, false) => None,
    };
    log(&app, &admin, "registry.update", r.name.clone(), detail).await;
    Ok(Json(shown(&get_one(&app, id).await?)))
}

/// `DELETE /api/registries/{id}`: forgets it and its credentials. Images from it stay.
async fn remove(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let was = get_one(&app, id).await?;
    app.db.call(move |c| Ok(c.execute("DELETE FROM registries WHERE id = ?1", [id])?)).await?;
    log(&app, &admin, "registry.remove", was.name, Some(was.host)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A failure while browsing, as the page shows it.
fn browse_error(r: &Registry, e: Fail) -> ApiError {
    match e {
        Fail::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, m),
        e => ApiError::new(StatusCode::BAD_GATEWAY, format!("{}: {}", r.name, e.message())),
    }
}

#[derive(Deserialize, Default)]
struct RepoQuery {
    /// Docker Hub: the account or organisation whose repositories to list (the user's own when not given).
    namespace: Option<String>,
}

/// `GET /api/registries/{id}/repositories`: what the registry lists. `listed: false` with the reason when it lists
/// nothing to anyone (GHCR, GitLab), so the page asks for a name instead.
async fn repositories(State(app): State<App>, _: Authed, Path(id): Path<i64>, Query(q): Query<RepoQuery>) -> ApiResult<Json<Value>> {
    let r = get_one(&app, id).await?;
    let found = if r.kind == "dockerhub" {
        let ns = q.namespace.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| r.username.clone());
        if ns.is_empty() {
            return Ok(Json(json!({ "repositories": [], "listed": false, "reason": "Docker Hub lists an account's repositories: say whose.", "image_prefix": r.image_prefix() })));
        }
        let user = (!r.username.is_empty()).then(|| (r.username.as_str(), r.secret.as_deref().unwrap_or_default()));
        hub::repositories(hub::API, &ns, user).await
    } else {
        match r.client().await {
            Ok(c) => c.catalog().await,
            Err(e) => Err(e),
        }
    };
    match found {
        Ok(list) => Ok(Json(json!({ "repositories": list, "listed": true, "image_prefix": r.image_prefix() }))),
        Err(Fail::Unsupported(reason)) => Ok(Json(json!({ "repositories": [], "listed": false, "reason": reason, "image_prefix": r.image_prefix() }))),
        Err(e) => Err(browse_error(&r, e)),
    }
}

#[derive(Deserialize, Default)]
struct TagQuery {
    repository: String,
}

/// `GET /api/registries/{id}/tags?repository=`: the repository's tags, newest names last as the registry has them, and
/// the image to pull them by (`<image>:<tag>`).
async fn tags(State(app): State<App>, _: Authed, Path(id): Path<i64>, Query(q): Query<TagQuery>) -> ApiResult<Json<Value>> {
    let r = get_one(&app, id).await?;
    let repo = q.repository.trim().trim_matches('/').to_string();
    let parsed = reference::parse(&format!("{}{repo}", r.image_prefix())).filter(|p| p.tag.is_none() && p.digest.is_none() && p.host == r.host);
    let Some(parsed) = parsed else { return Err(ApiError::bad("A repository is a path such as team/app.").field("repository")) };
    let c = r.client().await.map_err(|e| browse_error(&r, e))?;
    let list = c.tags(&parsed.repository).await.map_err(|e| browse_error(&r, e))?;
    Ok(Json(json!({ "repository": parsed.repository, "image": format!("{}{repo}", r.image_prefix()), "tags": list })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;
    use v2::tests::{Auth as FakeAuth, fake};

    #[tokio::test]
    async fn a_registry_with_its_user_but_no_password_yet_is_used_anonymously() {
        let r = Registry { id: 1, name: "ghcr".into(), kind: "ghcr".into(), url: "ghcr.io".into(), host: "ghcr.io".into(), username: "owner".into(), secret: None, region: None };
        assert_eq!(r.sign_in().await.ok(), Some(None), "brought over from Portainer, its password not given yet");
        let with = Registry { secret: Some("pw".into()), ..r };
        assert_eq!(with.sign_in().await.ok(), Some(Some(("owner".into(), "pw".into()))));
    }

    async fn audited(app: &App, action: &str) -> Vec<(String, Option<String>)> {
        let a = action.to_string();
        app.db
            .call(move |c| Ok(c.prepare("SELECT target, detail FROM audit WHERE action = ?1 ORDER BY id")?.query_map([a], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_registry_is_tested_before_it_is_saved_and_its_password_never_shown() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let f = fake(FakeAuth::Bearer, vec!["team/app", "team/web"], true).await;
        let body = |password: &str| json!({ "name": "Home", "kind": "custom", "url": f.base, "username": "deck", "password": password });

        let (s, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(body("wrong")))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("password")), "{b}");
        assert!(b["error"].as_str().unwrap().contains("refused"), "{b}");
        let (s, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(json!({ "name": "Gone", "kind": "custom", "url": "http://127.0.0.1:1" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("url")), "{b}");

        let (s, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(body("s3cret-pass")))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let host = f.base.trim_start_matches("http://").to_string();
        assert_eq!((b["host"].as_str(), b["url"].as_str()), (Some(host.as_str()), Some(f.base.as_str())));
        assert_eq!((b["username"].as_str(), b["has_secret"].as_bool()), (Some("deck"), Some(true)));
        let id = b["id"].as_i64().unwrap();

        let kept: String = app.db.call(move |c| Ok(c.query_row("SELECT secret FROM registries WHERE id = ?1", [id], |r| r.get(0))?)).await.unwrap();
        assert!(kept.starts_with("sealed1:") && !kept.contains("s3cret"), "sealed at rest");
        let (_, _, list) = call(&app, req(Method::GET, "/api/registries", &cookie, &csrf, None)).await;
        assert!(!list.to_string().contains("s3cret"), "never sent back: {list}");
        assert_eq!(list[0]["name"], "Home");
        assert_eq!(audited(&app, "registry.create").await, vec![("Home".into(), Some(format!("custom at {host}")))]);

        let (s, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(json!({ "name": "Again", "kind": "custom", "url": f.base, "username": "deck", "password": "s3cret-pass" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::CONFLICT, Some("url")), "one registry a host: {b}");
    }

    #[tokio::test]
    async fn editing_keeps_the_password_when_none_is_given_and_tests_a_new_one() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let f = fake(FakeAuth::Basic, vec!["qa/alpine"], true).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(json!({ "name": "qa", "kind": "custom", "url": f.base, "username": "deck", "password": "s3cret-pass" })))).await;
        let id = b["id"].as_i64().unwrap();
        let path = format!("/api/registries/{id}");
        let (s, _, b) = call(&app, req(Method::PUT, &path, &cookie, &csrf, Some(json!({ "name": "qa renamed", "password": "" })))).await;
        assert_eq!((s, b["name"].as_str(), b["has_secret"].as_bool()), (StatusCode::OK, Some("qa renamed"), Some(true)), "{b}");
        let (s, _, b) = call(&app, req(Method::PUT, &path, &cookie, &csrf, Some(json!({ "password": "a wrong one" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("password")), "a new password is tested: {b}");
        let auth = auth_for(&app, &f.base).await.unwrap().unwrap();
        assert_eq!(auth.credentials.password.as_deref(), Some("s3cret-pass"), "the refused one was not saved");
        assert_eq!(audited(&app, "registry.update").await, vec![("qa renamed".into(), Some("was qa".into()))]);

        let (s, _, _) = call(&app, req(Method::DELETE, &path, &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(auth_for(&app, &f.base).await.unwrap().is_none(), "forgotten");
        assert_eq!(audited(&app, "registry.remove").await.len(), 1);
    }

    #[tokio::test]
    async fn only_an_administrator_changes_registries_and_only_with_the_csrf_token() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let body = json!({ "name": "x", "kind": "custom", "url": "registry.example.com" });
        let (s, _, _) = call(&app, req(Method::POST, "/api/registries", &cookie, "wrong", Some(body.clone()))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let cookie = format!("freightdock_session={token}");
        let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
        let (s, _, _) = call(&app, req(Method::GET, "/api/registries", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK, "everybody may see them, to pull from them");
        let (s, _, _) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(body))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _, _) = call(&app, req(Method::GET, "/api/registries", "", "", None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_browser_lists_repositories_and_tags_through_the_token_flow() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let f = fake(FakeAuth::Bearer, vec!["team/app", "team/web", "solo"], true).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(json!({ "name": "home", "kind": "custom", "url": f.base, "username": "deck", "password": "s3cret-pass" })))).await;
        let id = b["id"].as_i64().unwrap();
        let host = f.base.trim_start_matches("http://");

        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/repositories"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["repositories"], json!(["solo", "team/app", "team/web"]));
        assert_eq!(b["image_prefix"], format!("{host}/"));
        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/tags?repository=team/app"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!((b["tags"].clone(), b["image"].as_str()), (json!(["1.0", "latest"]), Some(format!("{host}/team/app").as_str())));
        let (s, _, _) = call(&app, req(Method::GET, &format!("/api/registries/{id}/tags?repository=team/none"), &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/tags?repository=team/app:1"), &cookie, &csrf, None)).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("repository")));
    }

    #[tokio::test]
    async fn a_registry_without_a_catalog_is_browsed_by_name() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let f = fake(FakeAuth::Bearer, vec!["owner/app"], false).await;
        let (_, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(json!({ "name": "like ghcr", "kind": "custom", "url": f.base, "username": "deck", "password": "s3cret-pass" })))).await;
        let id = b["id"].as_i64().unwrap();
        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/repositories"), &cookie, &csrf, None)).await;
        assert_eq!((s, b["listed"].as_bool()), (StatusCode::OK, Some(false)), "{b}");
        assert!(b["reason"].as_str().unwrap().contains("Type a repository"));
        let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/tags?repository=owner/app"), &cookie, &csrf, None)).await;
        assert_eq!((s, b["tags"].clone()), (StatusCode::OK, json!(["1.0", "latest"])));
    }

    #[tokio::test]
    async fn an_image_s_registry_is_found_by_its_host_docker_hub_by_any_of_its_names() {
        let app = app();
        let sealed = app.sealer.seal("hub-token").unwrap();
        let sealed2 = app.sealer.seal("gh-token").unwrap();
        app.db
            .call(move |c| {
                c.execute("INSERT INTO registries (name, kind, url, host, username, secret, created_at) VALUES ('hub', 'dockerhub', 'docker.io', 'docker.io', 'deck', ?1, 0)", [&sealed])?;
                c.execute("INSERT INTO registries (name, kind, url, host, username, secret, created_at) VALUES ('gh', 'ghcr', 'ghcr.io', 'ghcr.io', 'owner', ?1, 0)", [&sealed2])?;
                c.execute("INSERT INTO registries (name, kind, url, host, username, created_at) VALUES ('open', 'custom', 'reg.example:5000', 'reg.example:5000', '', 0)", [])?;
                Ok(())
            })
            .await
            .unwrap();
        for image in ["nginx", "nginx:1.25", "grafana/grafana", "docker.io/library/nginx", "index.docker.io/deck/app:1", "registry-1.docker.io/deck/app"] {
            let a = auth_for_image(&app, image).await.unwrap().unwrap_or_else(|| panic!("{image}"));
            assert_eq!((a.registry.as_str(), a.credentials.username.as_deref(), a.credentials.password.as_deref()), ("hub", Some("deck"), Some("hub-token")), "{image}");
            assert_eq!(a.credentials.serveraddress.as_deref(), Some("https://index.docker.io/v1/"));
        }
        let a = auth_for_image(&app, "GHCR.io/owner/app:v2").await.unwrap().unwrap();
        assert_eq!((a.registry.as_str(), a.credentials.serveraddress.as_deref(), a.credentials.password.as_deref()), ("gh", Some("ghcr.io"), Some("gh-token")));
        assert!(auth_for_image(&app, "quay.io/a/b").await.unwrap().is_none(), "no registry saved there");
        assert!(auth_for_image(&app, "reg.example:5000/a").await.unwrap().is_none(), "saved without a user: nothing to send");
        assert!(auth_for_image(&app, "ghcr.io.evil.example/owner/app").await.unwrap().is_none(), "a host is matched whole");

        let cfg = docker_config(&[a]);
        assert_eq!(cfg, json!({ "auths": { "ghcr.io": { "auth": base64::engine::general_purpose::STANDARD.encode("owner:gh-token") } } }));
    }

    #[test]
    fn addresses_are_held_to_a_host_and_its_port() {
        assert_eq!(check_url("custom", "Registry.Example.com").unwrap(), ("registry.example.com".into(), "registry.example.com".into()));
        assert_eq!(check_url("custom", "https://registry.example.com:8443/").unwrap(), ("registry.example.com:8443".into(), "registry.example.com:8443".into()));
        assert_eq!(check_url("custom", "http://192.0.2.5:5000").unwrap(), ("http://192.0.2.5:5000".into(), "192.0.2.5:5000".into()));
        assert_eq!(check_url("ghcr", "").unwrap().1, "ghcr.io");
        assert_eq!(check_url("gitlab", "").unwrap().1, "registry.gitlab.com");
        assert_eq!(check_url("dockerhub", "whatever").unwrap().1, "docker.io");
        for bad in ["", "reg.example/path", "user@reg.example", "reg.example:99999", "reg example", "index.docker.io"] {
            assert!(check_url("custom", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_forgejo_or_gitea_registry_is_at_its_own_instance() {
        for kind in ["forgejo", "gitea"] {
            assert!(check_url(kind, "").is_err(), "{kind}: no address of its own to fall back on");
            assert_eq!(check_url(kind, "https://git.example.com/").unwrap(), ("git.example.com".into(), "git.example.com".into()));
            assert_eq!(check_url(kind, "http://192.0.2.4:3000").unwrap(), ("http://192.0.2.4:3000".into(), "192.0.2.4:3000".into()));
        }
    }

    #[tokio::test]
    async fn a_forgejo_or_gitea_registry_is_saved_and_browsed_like_any_other() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        for (kind, name) in [("forgejo", "Forgejo"), ("gitea", "Gitea")] {
            let f = fake(FakeAuth::Bearer, vec!["alice/freightdock"], true).await;
            let body = json!({ "name": name, "kind": kind, "url": f.base, "username": "deck", "password": "s3cret-pass" });
            let (s, _, b) = call(&app, req(Method::POST, "/api/registries", &cookie, &csrf, Some(body))).await;
            assert_eq!((s, b["kind"].as_str()), (StatusCode::CREATED, Some(kind)), "{b}");
            let id = b["id"].as_i64().unwrap();
            let (s, _, b) = call(&app, req(Method::GET, &format!("/api/registries/{id}/repositories"), &cookie, &csrf, None)).await;
            assert_eq!((s, b["repositories"].clone()), (StatusCode::OK, json!(["alice/freightdock"])), "{b}");
        }
    }

    #[test]
    fn ecr_wants_its_keys_and_a_region() {
        let f = |v: Value| serde_json::from_value::<Fields>(v).unwrap();
        let e = settle(&f(json!({ "name": "aws", "kind": "ecr", "url": "1.dkr.ecr.eu-north-1.amazonaws.com" })), None).unwrap_err();
        assert_eq!(e.field, Some("username"));
        let e = settle(&f(json!({ "name": "aws", "kind": "ecr", "url": "1.dkr.ecr.eu-north-1.amazonaws.com", "username": "AKIA" })), None).unwrap_err();
        assert_eq!(e.field, Some("password"));
        let r = settle(&f(json!({ "name": "aws", "kind": "ecr", "url": "1.dkr.ecr.eu-north-1.amazonaws.com", "username": "AKIA", "password": "s" })), None).unwrap();
        assert_eq!(r.region.as_deref(), Some("eu-north-1"), "from the host");
        let e = settle(&f(json!({ "name": "aws", "kind": "ecr", "url": "ecr.example.com", "username": "AKIA", "password": "s" })), None).unwrap_err();
        assert_eq!(e.field, Some("region"));
    }
}
