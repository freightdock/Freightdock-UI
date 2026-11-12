//! Images: listed (with what uses them), inspected with their layers, tagged, removed, pruned; pulled, pushed, built,
//! imported and exported, the long ones answered as a stream of progress lines (NDJSON) the page reads as they come.
//! Paths name an image by its id; a reference, which has slashes, travels in the body.

use std::collections::HashMap;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bollard::query_parameters as q;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::containers::split_tag;
use super::{api_error, endpoint, valid_ref};
use crate::access::Access;
use crate::access::ownership::{self, Kind, Owners, Wanted};
use crate::api::{Allowed, Authed};
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

fn check_ref(id: &str) -> ApiResult<()> {
    if valid_ref(id) { Ok(()) } else { Err(ApiError::bad("That is not an image's id.")) }
}

/// An image reference as Docker takes it: no spaces or control characters, a sane length.
pub fn valid_image(r: &str) -> bool {
    !r.is_empty() && r.len() <= 512 && !r.chars().any(|c| c.is_whitespace() || c.is_control()) && !r.starts_with('-')
}

async fn log(app: &App, admin: &Allowed, action: &'static str, target: String, detail: Option<String>) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail }).await;
}

/// Every image the person can see ("Access" 18): theirs, shared with them, and those a container they see runs; with
/// how many containers use each, and whom it belongs to (`freightdock_access`).
pub async fn list(State(app): State<App>, access: Access, Path(env): Path<i64>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let c = d.client();
    let (images, containers) = tokio::try_join!(
        c.list_images(Some(q::ListImagesOptions { all: false, digests: true, ..Default::default() })),
        c.list_containers(Some(q::ListContainersOptions { all: true, ..Default::default() })),
    )
    .map_err(api_error)?;
    let mut used: HashMap<String, usize> = HashMap::new();
    for ct in &containers {
        if let Some(id) = &ct.image_id {
            *used.entry(id.clone()).or_default() += 1;
        }
    }
    let (owners, seen) = crate::access::routes::sight(&app, &access).await?;
    let out: Vec<Value> = images
        .into_iter()
        .filter(|i| seen.image(&access, &owners, &i.id))
        .map(|i| {
            let n = used.get(&i.id).copied().unwrap_or(0);
            let shown = owners.show(owners.image(&i.id));
            let mut v = json!(i);
            v["containers"] = json!(n);
            v["freightdock_access"] = shown;
            v
        })
        .collect();
    Ok(Json(json!(out)))
}

pub async fn inspect(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Json<Value>> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let (i, h) = tokio::try_join!(d.client().inspect_image(&id), d.client().image_history(&id)).map_err(api_error)?;
    let owners = Owners::load(&app.db, env).await?;
    let shown = owners.show(i.id.as_deref().and_then(|x| owners.image(x)));
    Ok(Json(json!({ "image": i, "history": h, "freightdock_access": shown })))
}

/// `PUT .../images/{id}/access`: `{level, team_id, user_id}`, as a container's ("Access" 4, 18).
pub async fn set_access(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(w): Json<Wanted>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    let o = w.owner(&admin.1)?;
    let d = endpoint(&app, env).await?;
    let i = d.client().inspect_image(&id).await.map_err(api_error)?;
    let key = i.id.ok_or_else(|| ApiError::not_found("image"))?;
    ownership::save(&app.db, env, Kind::Image, key.clone(), o.clone()).await?;
    log(&app, &admin, "image.access", key, Some(o.map_or("admins", |o| o.level.as_str()).to_string())).await;
    Ok(StatusCode::NO_CONTENT)
}

/// The person who pulled or built `reference` becomes its owner, private to them, unless it has an owner they can
/// see already: so what they fetched is there for them in the list ("Access" 18).
pub async fn claim(app: &App, access: &Access, env: i64, reference: &str) {
    let found = async {
        let d = endpoint(app, env).await.ok()?;
        d.client().inspect_image(reference).await.ok()?.id
    };
    let Some(id) = found.await else { return };
    let Ok(owners) = Owners::load(&app.db, env).await else { return };
    let keep = owners.image(&id).is_some_and(|o| access.sees(Some(o)));
    if !keep {
        let mine = ownership::Owner { level: ownership::Share::Private, user: Some(access.user.id), team: None };
        if let Err(e) = ownership::save(&app.db, env, Kind::Image, id, Some(mine)).await {
            tracing::warn!("recording who pulled {reference}: {e:#}");
        }
    }
}

#[derive(Deserialize)]
pub struct Tag {
    /// `repo:tag`, or a repository alone for `latest`.
    reference: String,
}

pub async fn tag(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Json(t): Json<Tag>) -> ApiResult<StatusCode> {
    check_ref(&id)?;
    if !valid_image(&t.reference) || t.reference.contains('@') {
        return Err(ApiError::bad("A tag is a repository and a tag, such as myapp:1.0.").field("reference"));
    }
    let d = endpoint(&app, env).await?;
    let (repo, tag) = split_tag(&t.reference);
    d.client().tag_image(&id, Some(q::TagImageOptions { repo: Some(repo.into()), tag: Some(tag.into()) })).await.map_err(api_error)?;
    log(&app, &admin, "image.tag", id, Some(t.reference)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct RemoveQuery {
    force: Option<bool>,
}

pub async fn remove(State(app): State<App>, admin: Allowed, Path((env, id)): Path<(i64, String)>, Query(rq): Query<RemoveQuery>) -> ApiResult<Json<Value>> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let key = d.client().inspect_image(&id).await.ok().and_then(|i| i.id);
    let r = d.client().remove_image(&id, Some(q::RemoveImageOptions { force: rq.force.unwrap_or(false), noprune: false }), None).await.map_err(api_error)?;
    // Removing one tag of several leaves the image; its record goes only with the image.
    if let Some(k) = key
        && r.iter().any(|x| x.deleted.as_deref() == Some(k.as_str())) {
            ownership::save(&app.db, env, Kind::Image, k, None).await?;
        }
    log(&app, &admin, "image.remove", id, rq.force.unwrap_or(false).then(|| "forced".into())).await;
    Ok(Json(json!(r)))
}

#[derive(Deserialize, Default)]
pub struct Prune {
    /// Every image no container uses, not only the untagged ones.
    #[serde(default)]
    all: bool,
}

pub async fn prune(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(p): Json<Prune>) -> ApiResult<Json<Value>> {
    let d = endpoint(&app, env).await?;
    let filters = HashMap::from([("dangling".to_string(), vec![(!p.all).to_string()])]);
    let r = d.client().prune_images(Some(q::PruneImagesOptions { filters: Some(filters) })).await.map_err(api_error)?;
    let n = r.images_deleted.as_ref().map_or(0, Vec::len);
    for k in r.images_deleted.iter().flatten().filter_map(|x| x.deleted.clone()) {
        ownership::save(&app.db, env, Kind::Image, k, None).await?;
    }
    log(&app, &admin, "image.prune", format!("{n} images"), r.space_reclaimed.map(|b| format!("{b} bytes freed"))).await;
    Ok(Json(json!(r)))
}

/// One line of progress, as the page reads it: Docker's status (and the layer it is about), or its build output, or an
/// error, which ends the stream.
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct Line {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

/// A stream of progress lines as a response, one JSON object a line, the last `{"done":true}` when it went well, or an
/// `error` line when it did not. When it ends well, `then` runs (the activity log).
fn ndjson<S, F>(s: S, then: F) -> Response
where
    S: Stream<Item = Result<Line, bollard::errors::Error>> + Send + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let body = async_stream(s, then);
    ([(header::CONTENT_TYPE, "application/x-ndjson"), (header::CACHE_CONTROL, "no-cache")], Body::from_stream(body)).into_response()
}

fn async_stream<S, F>(s: S, then: F) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Line, bollard::errors::Error>> + Send + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let enc = |l: &Line| Bytes::from(serde_json::to_string(l).unwrap_or_default() + "\n");
    let state = (Box::pin(s), Some(Box::pin(then)), false);
    futures_util::stream::unfold(state, move |(mut s, mut then, ended)| async move {
        if ended {
            return None;
        }
        match s.next().await {
            Some(Ok(l)) => {
                let failed = l.error.is_some();
                Some((Ok(enc(&l)), (s, then, failed)))
            }
            Some(Err(e)) => {
                let msg = api_error(e).message;
                Some((Ok(enc(&Line { error: Some(msg), ..Default::default() })), (s, then, true)))
            }
            None => {
                if let Some(t) = then.take() {
                    t.await;
                }
                Some((Ok(enc(&Line { done: Some(true), ..Default::default() })), (s, None, true)))
            }
        }
    })
}

fn from_progress(id: Option<String>, status: Option<String>, progress: Option<String>, detail: Option<bollard::models::ProgressDetail>, error: Option<String>) -> Line {
    Line { id, status, progress, current: detail.as_ref().and_then(|d| d.current), total: detail.and_then(|d| d.total), error, ..Default::default() }
}

fn build_line(b: bollard::models::BuildInfo) -> Line {
    Line { id: b.id, stream: b.stream, status: b.status, progress: b.progress, error: b.error_detail.and_then(|e| e.message).or(b.error), ..Default::default() }
}

#[derive(Deserialize)]
pub struct Pull {
    image: String,
}

pub async fn pull(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(p): Json<Pull>) -> ApiResult<Response> {
    let image = p.image.trim().to_string();
    if !valid_image(&image) {
        return Err(ApiError::bad("Say which image, such as nginx:latest or ghcr.io/owner/app:1.0.").field("image"));
    }
    let d = endpoint(&app, env).await?;
    let auth = crate::registries::auth_for_image(&app, &image).await?;
    let (from, tag) = split_tag(&image);
    let opts = q::CreateImageOptions { from_image: Some(from.into()), tag: (!tag.is_empty()).then(|| tag.into()), ..Default::default() };
    let c = d.client().clone();
    let detail = signed_in_to(&auth);
    let creds = auth.map(|a| a.credentials);
    let s = spawned(move |tx| async move {
        forward(c.create_image(Some(opts), None, creds).map(|r| r.map(|i| from_progress(i.id, i.status, i.progress, i.progress_detail, i.error_detail.and_then(|e| e.message)))), tx).await
    });
    let done = async move {
        claim(&app, &admin.1, env, &image).await;
        log(&app, &admin, "image.pull", image, detail).await
    };
    Ok(ndjson(s, done))
}

pub async fn push(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Json(p): Json<Pull>) -> ApiResult<Response> {
    let image = p.image.trim().to_string();
    if !valid_image(&image) || image.contains('@') {
        return Err(ApiError::bad("Say which tag to push, such as registry.example.com/team/app:1.0.").field("image"));
    }
    let d = endpoint(&app, env).await?;
    let auth = crate::registries::auth_for_image(&app, &image).await?;
    let (repo, tag) = split_tag(&image);
    let (repo, tag) = (repo.to_string(), tag.to_string());
    let c = d.client().clone();
    let detail = signed_in_to(&auth);
    let creds = auth.map(|a| a.credentials);
    let s = spawned(move |tx| async move {
        let s = c.push_image(&repo, Some(q::PushImageOptions { tag: Some(tag), platform: None }), creds);
        forward(s.map(|r| r.map(|i| from_progress(None, i.status, i.progress, i.progress_detail, i.error_detail.and_then(|e| e.message)))), tx).await
    });
    let done = async move { log(&app, &admin, "image.push", image, detail).await };
    Ok(ndjson(s, done))
}

/// The activity log's word on which saved registry a pull or push signed in to.
pub fn signed_in_to(auth: &Option<crate::registries::Auth>) -> Option<String> {
    auth.as_ref().map(|a| format!("signed in to {}", a.registry))
}

type Lines = std::pin::Pin<Box<dyn Stream<Item = Result<Line, bollard::errors::Error>> + Send>>;
type Tx = tokio::sync::mpsc::Sender<Result<Line, bollard::errors::Error>>;

/// A stream driven by a task of its own. The task's future holds the client and the stream that borrows it (bollard's
/// build stream does), and it stops as soon as nobody reads: a page closed mid-pull stops pulling.
fn spawned<Fut>(run: impl FnOnce(Tx) -> Fut) -> Lines
where
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(run(tx));
    Box::pin(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) }))
}

async fn forward(s: impl Stream<Item = Result<Line, bollard::errors::Error>>, tx: Tx) {
    futures_util::pin_mut!(s);
    while let Some(x) = s.next().await {
        if tx.send(x).await.is_err() {
            break;
        }
    }
}

#[derive(Deserialize, Default)]
pub struct BuildQuery {
    /// The tag the built image gets.
    tag: Option<String>,
    /// A Git repository (or a tarball's address) to build from, instead of a body.
    remote: Option<String>,
    /// The Dockerfile's path inside the context.
    dockerfile: Option<String>,
    #[serde(default)]
    nocache: bool,
    #[serde(default)]
    pull: bool,
}

/// A build context holding only this Dockerfile.
pub fn context_of(dockerfile: &str) -> std::io::Result<Vec<u8>> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(dockerfile.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_cksum();
    b.append_data(&mut h, "Dockerfile", dockerfile.as_bytes())?;
    b.into_inner()
}

/// `POST .../images/build`: from a Dockerfile (a JSON body, `{"dockerfile": "FROM ..."}`), a tar of a whole context
/// (`application/x-tar`), or `?remote=` a Git address. The output streams as it builds.
pub async fn build(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, Query(bq): Query<BuildQuery>, headers: axum::http::HeaderMap, body: Body) -> ApiResult<Response> {
    let tag = bq.tag.clone().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if let Some(t) = &tag
        && (!valid_image(t) || t.contains('@')) {
            return Err(ApiError::bad("A tag is a repository and a tag, such as myapp:1.0.").field("tag"));
        }
    let d = endpoint(&app, env).await?;
    let json = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/json"));
    let tar = if bq.remote.is_some() {
        None
    } else if json {
        #[derive(Deserialize)]
        struct Df {
            dockerfile: String,
        }
        let bytes = axum::body::to_bytes(body, 1 << 20).await.map_err(|_| ApiError::bad("A Dockerfile is at most 1 MB."))?;
        let df: Df = serde_json::from_slice(&bytes).map_err(|_| ApiError::bad("Send the Dockerfile as {\"dockerfile\": \"...\"}."))?;
        if df.dockerfile.trim().is_empty() {
            return Err(ApiError::bad("The Dockerfile is empty.").field("dockerfile"));
        }
        Some(bollard::body_full(Bytes::from(context_of(&df.dockerfile).map_err(anyhow::Error::from)?)))
    } else {
        Some(bollard::body_stream(body.into_data_stream().filter_map(|c| async move { c.ok() })))
    };
    let opts = q::BuildImageOptions {
        dockerfile: bq.dockerfile.clone().filter(|f| !f.is_empty()).unwrap_or_else(|| "Dockerfile".into()),
        t: tag.clone(),
        remote: bq.remote.clone(),
        nocache: bq.nocache,
        pull: bq.pull.then(|| "1".into()),
        rm: true,
        forcerm: true,
        ..Default::default()
    };
    let c = d.client().clone();
    let s = spawned(move |tx| async move { forward(c.build_image(opts, None, tar).map(|r| r.map(build_line)), tx).await });
    let what = tag.clone().or(bq.remote).unwrap_or_else(|| "untagged".into());
    let done = async move {
        if let Some(t) = &tag {
            claim(&app, &admin.1, env, t).await;
        }
        log(&app, &admin, "image.build", what, None).await
    };
    Ok(ndjson(s, done))
}

/// `POST .../images/load`: a tar made by `docker save` (or Export here).
pub async fn load(State(app): State<App>, admin: Allowed, Path(env): Path<i64>, body: Body) -> ApiResult<Response> {
    let d = endpoint(&app, env).await?;
    let tar = bollard::body_stream(body.into_data_stream().filter_map(|c| async move { c.ok() }));
    let c = d.client().clone();
    let s = spawned(move |tx| async move { forward(c.import_image(q::ImportImageOptions { quiet: false, platform: None }, tar, None).map(|r| r.map(build_line)), tx).await });
    let done = async move { log(&app, &admin, "image.load", "an image archive".into(), None).await };
    Ok(ndjson(s, done))
}

/// `GET .../images/{id}/export`: the image as `docker save` writes it, to keep or to load elsewhere.
pub async fn export(State(app): State<App>, a: Authed, Path((env, id)): Path<(i64, String)>) -> ApiResult<Response> {
    check_ref(&id)?;
    let d = endpoint(&app, env).await?;
    let i = d.client().inspect_image(&id).await.map_err(api_error)?;
    let name = i.repo_tags.as_ref().and_then(|t| t.first()).cloned().unwrap_or_else(|| id.clone());
    let file: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let client = d.client().clone();
    let target = id.clone();
    let s = futures_util::stream::once(async move { client.export_image(&target).map(|r| r.map_err(std::io::Error::other)) }).flatten();
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "image.export", target: Some(name), detail: None }).await;
    Ok((
        [(header::CONTENT_TYPE, "application/x-tar".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}.tar\""))],
        Body::from_stream(s),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dockerfile_alone_is_a_context_with_one_file() {
        let t = context_of("FROM alpine\nRUN true\n").unwrap();
        let mut a = tar::Archive::new(&t[..]);
        let names: Vec<String> = a.entries().unwrap().map(|e| e.unwrap().path().unwrap().display().to_string()).collect();
        assert_eq!(names, vec!["Dockerfile"]);
    }

    #[test]
    fn image_references_are_held_to_docker_s_shape() {
        assert!(valid_image("nginx") && valid_image("ghcr.io/a/b:1.0") && valid_image("alpine@sha256:abc"));
        assert!(!valid_image("") && !valid_image("a b") && !valid_image("--help"));
    }

    #[tokio::test]
    async fn a_stream_ends_done_when_it_went_well_and_with_the_error_when_not() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = ran.clone();
        let good = futures_util::stream::iter(vec![Ok(Line { status: Some("Pulling".into()), ..Default::default() })]);
        let out: Vec<_> = async_stream(good, async move { r.store(true, std::sync::atomic::Ordering::SeqCst) }).collect().await;
        let text: Vec<String> = out.into_iter().map(|b| String::from_utf8(b.unwrap().to_vec()).unwrap()).collect();
        assert_eq!(text, vec!["{\"status\":\"Pulling\"}\n", "{\"done\":true}\n"]);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst), "the activity log is written when it went well");

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = ran.clone();
        let bad = futures_util::stream::iter(vec![Ok(Line { error: Some("manifest unknown".into()), ..Default::default() }), Ok(Line::default())]);
        let out: Vec<_> = async_stream(bad, async move { r.store(true, std::sync::atomic::Ordering::SeqCst) }).collect().await;
        assert_eq!(out.len(), 1, "nothing after the error");
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst), "a failed pull is not logged as done");
    }
}
