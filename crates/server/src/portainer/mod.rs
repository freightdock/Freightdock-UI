//! Import from Portainer, into the cluster ("Portainer" in DECISIONS.md, 11 on): an Administrator connects Freightdock
//! to a Portainer (its address and an access token, sealed), Freightdock reads its Compose stacks through Portainer's
//! API without changing anything (GET only), shows which can come and why not, and deploys what is ticked as cluster
//! stacks: the same file, its variables as the stack's `.env`, made stopped unless asked to start, since Portainer's copy
//! still runs where it was.

pub mod client;

use std::collections::BTreeSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use client::{Client, Conn};

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/portainer", get(get_conn).put(put_conn).delete(forget))
        .route("/api/portainer/scan", post(scan))
        .route("/api/portainer/import", post(import))
}

const KEY: &str = "portainer";

/// The connection as it is kept in the settings (`portainer`): the token sealed ("Secrets" 1).
#[derive(Serialize, Deserialize, Default, Clone)]
struct Saved {
    url: String,
    token: String,
    #[serde(default)]
    ca: String,
    #[serde(default)]
    skip_verify: bool,
    #[serde(default)]
    version: String,
    #[serde(default)]
    at: i64,
}

impl Saved {
    fn trust(&self) -> &'static str {
        if self.skip_verify {
            "certificate not checked, as chosen"
        } else if !self.ca.trim().is_empty() {
            "certificate checked against the one pasted"
        } else {
            "certificate checked"
        }
    }
}

fn shown(s: Option<&Saved>) -> Value {
    match s {
        None => json!({ "connected": false }),
        Some(s) => json!({ "connected": true, "url": s.url, "has_token": !s.token.is_empty(), "ca": s.ca, "skip_verify": s.skip_verify, "version": s.version, "at": s.at, "trust": s.trust() }),
    }
}

/// The client for the saved connection, or a 409 when there is none.
async fn connected(app: &App) -> ApiResult<(Client, Saved)> {
    let s: Saved = app.db.setting(KEY).await?.ok_or_else(|| ApiError::conflict("Connect to Portainer first."))?;
    let token = app.sealer.open(&s.token)?;
    let c = Client::new(&Conn { url: s.url.clone(), token, ca: s.ca.clone(), skip_verify: s.skip_verify }).map_err(|m| ApiError::new(StatusCode::BAD_GATEWAY, m))?;
    Ok((c, s))
}

/// `GET /api/portainer`: the connection, never its token.
async fn get_conn(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(shown(app.db.setting::<Saved>(KEY).await?.as_ref())))
}

#[derive(Deserialize)]
struct ConnForm {
    url: String,
    /// Empty or left out keeps the one saved for the same address.
    token: Option<String>,
    #[serde(default)]
    ca: String,
    #[serde(default)]
    skip_verify: bool,
}

/// `PUT /api/portainer`: `{url, token, ca, skip_verify}`, tried first (Portainer's status, then its environments with
/// the token), then saved.
async fn put_conn(State(app): State<App>, admin: Admin, Json(f): Json<ConnForm>) -> ApiResult<Json<Value>> {
    let url = client::check_url(&f.url).map_err(|m| ApiError::bad(m).field("url"))?;
    let was: Option<Saved> = app.db.setting(KEY).await?;
    let token = match f.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => match was.as_ref().filter(|w| w.url == url) {
            Some(w) => app.sealer.open(&w.token)?,
            None => return Err(ApiError::bad("Give an access token: in Portainer, My account, Access tokens, Add access token.").field("token")),
        },
    };
    let ca = f.ca.trim().to_string();
    if !ca.is_empty() {
        client::pem_certs(&ca).map_err(|m| ApiError::bad(m).field("ca"))?;
    }
    let c = Client::new(&Conn { url: url.clone(), token: token.clone(), ca: ca.clone(), skip_verify: f.skip_verify }).map_err(|m| ApiError::bad(m).field("url"))?;
    let status = c.status().await.map_err(|m| ApiError::bad(m).field("url"))?;
    c.endpoints().await.map_err(|m| ApiError::bad(m.clone()).field(if m.contains("token") { "token" } else { "url" }))?;
    let s = Saved { url: url.clone(), token: app.sealer.seal(&token)?, ca, skip_verify: f.skip_verify, version: status.version.clone(), at: now() };
    app.db.put_setting(KEY, &s).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "portainer.connect", target: Some(url), detail: Some(format!("Portainer {}; {}", s.version, s.trust())) }).await;
    Ok(Json(shown(Some(&s))))
}

/// `DELETE /api/portainer`: forgets the connection and its token; what was imported stays.
async fn forget(State(app): State<App>, admin: Admin) -> ApiResult<StatusCode> {
    let s: Option<Saved> = app.db.setting(KEY).await?;
    app.db.call(|c| Ok(c.execute("DELETE FROM settings WHERE key = 'portainer'", [])?)).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "portainer.forget", target: s.map(|s| s.url), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A Portainer stack's name as a cluster stack's: lower case, anything else a stack name cannot hold as `-`, at most
/// 40; `None` when nothing is left.
pub fn cluster_name(n: &str) -> Option<String> {
    let mut out = String::new();
    for c in n.trim().chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if !(c == '-' && (out.is_empty() || out.ends_with('-'))) {
            out.push(c);
        }
    }
    out.truncate(40);
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

/// A variable name Compose takes.
fn var_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// A stack's variables as a `.env`, written as Portainer writes its `stack.env` (`name=value`, as it is). A value over
/// several lines is put in double quotes with its line breaks as `\n` (and `\`, `"` and `$` escaped). Names Compose
/// would not take are left out, and answered.
pub fn env_text(vars: &[client::EnvVar]) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut left = Vec::new();
    for v in vars {
        if !var_name(&v.name) {
            left.push(v.name.clone());
            continue;
        }
        let value = if v.value.contains(['\n', '\r']) {
            let e = v.value.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('\r', "").replace('\n', "\\n");
            format!("\"{e}\"")
        } else {
            v.value.clone()
        };
        out.push_str(&format!("{}={value}\n", v.name));
    }
    (out, left)
}

/// One Portainer stack as the import sees it: what it would be in the cluster, or why it cannot come.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Row {
    pub id: i64,
    pub name: String,
    /// The Portainer environment it runs on, by name.
    pub env: String,
    pub kind: &'static str,
    pub active: bool,
    /// Its Git repository, when it was deployed from one: it comes as its file, and is not followed.
    pub git: Option<String>,
    pub vars: usize,
    /// Its name in the cluster.
    pub target: Option<String>,
    pub why: Option<String>,
}

fn kind_of(k: i64) -> &'static str {
    match k {
        1 => "swarm",
        2 => "compose",
        3 => "kubernetes",
        _ => "other",
    }
}

/// What each of Portainer's stacks would be, against the cluster's stack names (`taken`). Two that would have one
/// name: the active one comes, else the newer.
pub fn plan(stacks: &[client::Stack], endpoints: &[client::Endpoint], taken: &BTreeSet<String>) -> Vec<Row> {
    let env_name = |id: i64| endpoints.iter().find(|e| e.id == id).map_or_else(|| format!("environment {id}"), |e| e.name.clone());
    let mut rows: Vec<Row> = stacks
        .iter()
        .map(|s| {
            let target = cluster_name(&s.name);
            let why = match (s.kind, &target) {
                (1, _) => Some("A Swarm stack: the cluster takes Compose stacks. Bring its file over by hand.".to_string()),
                (3, _) => Some("A Kubernetes stack.".to_string()),
                (k, _) if k != 2 => Some("Not a Compose stack.".to_string()),
                _ if !s.additional_files.is_empty() => Some("It has more Compose files than Portainer's API gives out.".to_string()),
                (_, None) => Some("Its name cannot be made a stack name.".to_string()),
                (_, Some(t)) if taken.contains(t) => Some(format!("The cluster has a stack called {t} already.")),
                _ => None,
            };
            Row { id: s.id, name: s.name.clone(), env: env_name(s.endpoint_id), kind: kind_of(s.kind), active: s.status == 1, git: s.git_config.as_ref().map(|g| g.url.clone()), vars: s.env.len(), target, why }
        })
        .collect();
    let rank = |id: i64| stacks.iter().find(|s| s.id == id).map_or((false, 0), |s| (s.status == 1, s.update_date.max(s.creation_date)));
    let mut order: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].why.is_none()).collect();
    order.sort_by_key(|i| std::cmp::Reverse(rank(rows[*i].id)));
    let mut chosen: std::collections::BTreeMap<String, usize> = Default::default();
    for i in order {
        let t = rows[i].target.clone().unwrap_or_default();
        match chosen.get(&t) {
            Some(&w) => rows[i].why = Some(format!("{} on {} comes as {t}; there can be one.", rows[w].name, rows[w].env)),
            None => {
                chosen.insert(t, i);
            }
        }
    }
    rows
}

/// What the managers are sent for one stack: its file, its variables as its `.env`, where it came from, and stopped
/// unless it is to start; and the variables left out.
fn deploy_body(row: &Row, file: String, vars: &[client::EnvVar], start: bool) -> (Value, Vec<String>) {
    let (env, left) = env_text(vars);
    let body = json!({ "name": row.target, "compose": file, "env": env, "description": format!("From Portainer: {} on {}", row.name, row.env), "stopped": !start });
    (body, left)
}

/// The cluster's stack names.
async fn taken(app: &App) -> ApiResult<BTreeSet<String>> {
    let (_, v) = crate::cluster::cluster(app)?.call(reqwest::Method::GET, "/v1/stacks", None).await?;
    Ok(v.as_array().into_iter().flatten().filter_map(|s| s["name"].as_str().map(str::to_string)).collect())
}

/// `POST /api/portainer/scan`: Portainer's stacks and what each would be in the cluster. Changes nothing.
async fn scan(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let (c, s) = connected(&app).await?;
    let gw = |m: String| ApiError::new(StatusCode::BAD_GATEWAY, m);
    let endpoints = c.endpoints().await.map_err(gw)?;
    let stacks = c.stacks().await.map_err(gw)?;
    let rows = plan(&stacks, &endpoints, &taken(&app).await?);
    Ok(Json(json!({ "url": s.url, "version": s.version, "stacks": rows })))
}

#[derive(Deserialize)]
struct ImportBody {
    stacks: Vec<i64>,
    /// Start them in the cluster at once; else they are made stopped, to start once Portainer's copies are stopped.
    #[serde(default)]
    start: bool,
}

/// `POST /api/portainer/import`: deploys the stacks ticked as cluster stacks, each read from Portainer again, made
/// stopped unless `start`. Answers each one's outcome; one that fails leaves the others.
async fn import(State(app): State<App>, admin: Admin, Json(b): Json<ImportBody>) -> ApiResult<Json<Value>> {
    let (c, _) = connected(&app).await?;
    let gw = |m: String| ApiError::new(StatusCode::BAD_GATEWAY, m);
    let endpoints = c.endpoints().await.map_err(gw)?;
    let stacks = c.stacks().await.map_err(gw)?;
    let rows = plan(&stacks, &endpoints, &taken(&app).await?);
    let a = &admin.0;
    let mut out = vec![];
    for id in b.stacks {
        let Some(row) = rows.iter().find(|r| r.id == id) else {
            out.push(json!({ "id": id, "ok": false, "error": "Portainer has no such stack now." }));
            continue;
        };
        if let Some(why) = &row.why {
            out.push(json!({ "id": id, "name": row.name, "ok": false, "error": why }));
            continue;
        }
        let target = row.target.clone().unwrap_or_default();
        let file = match c.stack_file(id).await {
            Ok(f) => f,
            Err(e) => {
                out.push(json!({ "id": id, "name": row.name, "ok": false, "error": e }));
                continue;
            }
        };
        let vars = stacks.iter().find(|s| s.id == id).map(|s| s.env.clone()).unwrap_or_default();
        let (body, left) = deploy_body(row, file, &vars, b.start);
        match crate::cluster::cluster(&app)?.call(reqwest::Method::POST, "/v1/stacks", Some(body)).await {
            Ok((_, v)) => {
                let detail = format!("Portainer stack {id} on {}; {}", row.env, if b.start { "started" } else { "made stopped" });
                audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "portainer.import", target: Some(target.clone()), detail: Some(detail) }).await;
                out.push(json!({ "id": id, "name": row.name, "ok": true, "stack": target, "started": b.start, "warnings": v["warnings"], "left_out": left }));
            }
            Err(e) => out.push(json!({ "id": id, "name": row.name, "ok": false, "error": e.message })),
        }
    }
    Ok(Json(json!({ "results": out })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{call, json_body, signed_in};
    use axum::body::Body;
    use axum::http::{Request, header};

    fn stack(id: i64, name: &str, kind: i64, status: i64, updated: i64) -> client::Stack {
        client::Stack { id, name: name.into(), kind, endpoint_id: 3, status, update_date: updated, ..Default::default() }
    }

    #[test]
    fn a_name_becomes_a_stack_name() {
        assert_eq!(cluster_name("wg-easy").as_deref(), Some("wg-easy"));
        assert_eq!(cluster_name("My App_2").as_deref(), Some("my-app-2"));
        assert_eq!(cluster_name("--Ünïcode__").as_deref(), Some("n-code"));
        assert_eq!(cluster_name(&"a".repeat(50)).unwrap().len(), 40);
        assert_eq!(cluster_name("___"), None);
    }

    #[test]
    fn variables_become_an_env_file_as_portainer_writes_one() {
        let v = |n: &str, val: &str| client::EnvVar { name: n.into(), value: val.into() };
        let (text, left) = env_text(&[v("A", "1"), v("B", "two\nlines $x"), v("bad name", "x")]);
        assert_eq!(text, "A=1\nB=\"two\\nlines \\$x\"\n");
        assert_eq!(left, vec!["bad name".to_string()]);
    }

    #[test]
    fn the_plan_says_what_comes_and_why_the_rest_does_not() {
        let endpoints: Vec<client::Endpoint> = serde_json::from_str(include_str!("fixtures/endpoints.json")).unwrap();
        let stacks: Vec<client::Stack> = serde_json::from_str(include_str!("fixtures/stacks.json")).unwrap();
        let rows = plan(&stacks, &endpoints, &["arr".to_string()].into());
        let by = |id: i64| rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by(3).why.as_deref(), Some("The cluster has a stack called arr already."));
        assert_eq!((by(147).target.as_deref(), by(147).vars, by(147).why.as_ref()), (Some("nginx"), 5, None));
        assert!(by(259).git.is_some() && by(259).why.is_none(), "a Git stack comes as its file");
        let watch: Vec<&Row> = rows.iter().filter(|r| r.name == "watchtower").collect();
        assert_eq!(watch.iter().filter(|r| r.why.is_none()).count(), 1, "two of one name: one comes");
        assert!(by(24).why.is_none(), "the active one");
        assert!(by(4).why.as_deref().unwrap().contains("comes as watchtower"), "{:?}", by(4).why);
        let stats: Vec<&Row> = rows.iter().filter(|r| r.name == "statsapp").collect();
        assert!(stats.iter().any(|r| r.id == 237 && r.why.is_none()), "both active: the one changed last, {stats:?}");

        let odd = plan(&[stack(1, "s", 1, 1, 0), stack(2, "k", 3, 1, 0)], &[], &BTreeSet::new());
        assert!(odd[0].why.as_deref().unwrap().contains("Swarm") && odd[1].why.as_deref().unwrap().contains("Kubernetes"));
        assert_eq!(odd[0].env, "environment 3");
    }

    #[test]
    fn a_stack_is_sent_stopped_with_its_variables_unless_it_is_to_start() {
        let row = Row { id: 7, name: "Web".into(), env: "alpha".into(), kind: "compose", active: true, git: None, vars: 1, target: Some("web".into()), why: None };
        let vars = [client::EnvVar { name: "PORT".into(), value: "8080".into() }];
        let (b, left) = deploy_body(&row, "services: {}".into(), &vars, false);
        assert_eq!(b, json!({ "name": "web", "compose": "services: {}", "env": "PORT=8080\n", "description": "From Portainer: Web on alpha", "stopped": true }));
        assert!(left.is_empty());
        assert_eq!(deploy_body(&row, String::new(), &vars, true).0["stopped"], false);
    }

    /// A stand-in for Portainer over plain HTTP: its status, endpoints, stacks (two, one Swarm) and the Compose one's file.
    async fn portainer() -> String {
        let app = axum::Router::new().fallback(|req: axum::extract::Request| async move {
            if req.headers().get("x-api-key").and_then(|v| v.to_str().ok()) != Some("ptr_good") {
                return (StatusCode::UNAUTHORIZED, r#"{"message":"Unauthorized"}"#.to_string());
            }
            let body = match req.uri().path() {
                "/api/system/status" => r#"{"Version":"2.27.0","InstanceID":"i1"}"#.to_string(),
                "/api/endpoints" => r#"[{"Id":3,"Name":"alpha","Type":1}]"#.to_string(),
                "/api/stacks" => r#"[{"Id":7,"Name":"Shop","Type":2,"EndpointId":3,"Status":1,"Env":[{"name":"PORT","value":"8080"}]},{"Id":8,"Name":"swarmy","Type":1,"EndpointId":3,"Status":1}]"#.to_string(),
                "/api/stacks/7/file" => r#"{"StackFileContent":"services:\n  web:\n    image: nginx\n"}"#.to_string(),
                _ => return (StatusCode::NOT_FOUND, "{}".to_string()),
            };
            (StatusCode::OK, body)
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn req(method: &str, path: &str, cookie: &str, csrf: &str, body: Value) -> Request<Body> {
        Request::builder().method(method).uri(path).header(header::COOKIE, cookie).header("x-csrf-token", csrf).header(header::CONTENT_TYPE, "application/json").body(json_body(body)).unwrap()
    }

    #[tokio::test]
    async fn connect_scan_and_import_into_the_cluster_stopped() {
        let (app, asked, _d) = crate::cluster::tests::connected("t0k3n").await;
        let (cookie, csrf) = signed_in(&app).await;
        let url = portainer().await;
        let (s, _, b) = call(&app, req("PUT", "/api/portainer", &cookie, &csrf, json!({ "url": url, "token": "ptr_bad" }))).await;
        assert_eq!((s, b["field"].clone()), (StatusCode::BAD_REQUEST, json!("url")), "a refused token is not saved: {b}");
        let (s, _, b) = call(&app, req("PUT", "/api/portainer", &cookie, &csrf, json!({ "url": url, "token": "ptr_good" }))).await;
        assert_eq!((s, b["version"].clone(), b["has_token"].clone()), (StatusCode::OK, json!("2.27.0"), json!(true)), "{b}");
        assert!(!b.to_string().contains("ptr_good"), "never the token");
        let raw: String = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'portainer'", [], |r| r.get(0))?)).await.unwrap();
        assert!(!raw.contains("ptr_good"), "sealed at rest");

        let (s, _, b) = call(&app, req("POST", "/api/portainer/scan", &cookie, &csrf, json!({}))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!((b["stacks"][0]["target"].clone(), b["stacks"][0]["why"].clone()), (json!("shop"), Value::Null));
        assert!(b["stacks"][1]["why"].as_str().unwrap().contains("Swarm"));

        let (s, _, b) = call(&app, req("POST", "/api/portainer/import", &cookie, &csrf, json!({ "stacks": [7, 8] }))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!((b["results"][0]["ok"].clone(), b["results"][0]["stack"].clone(), b["results"][0]["started"].clone()), (json!(true), json!("shop"), json!(false)), "{b}");
        assert_eq!(b["results"][1]["ok"], false, "the Swarm one says why");
        assert!(asked.lock().unwrap().iter().any(|l| l == "POST /v1/stacks"));
        let log: Vec<(String, String)> = app.db.call(|c| Ok(c.prepare("SELECT action, COALESCE(detail, '') FROM audit WHERE action LIKE 'portainer.%' ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(log.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), vec!["portainer.connect", "portainer.import"]);
        assert!(log[1].1.contains("made stopped"), "{log:?}");
        assert_eq!(call(&app, req("DELETE", "/api/portainer", &cookie, &csrf, json!({}))).await.0, StatusCode::NO_CONTENT);
        let (_, _, b) = call(&app, req("POST", "/api/portainer/scan", &cookie, &csrf, json!({}))).await;
        assert!(b["error"].as_str().unwrap_or_default().contains("Connect to Portainer first"), "{b}");
    }
}
