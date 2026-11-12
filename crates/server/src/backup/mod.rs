//! Backups of Freightdock's own data: the database (copied with SQLite's online backup API, so Freightdock keeps running),
//! `secret.key`, the agents' CA, the TLS certificate and the stacks' files when there are any, in one tar.gz sealed
//! with the passphrase an administrator sets (`crypt`), to a folder or to S3 (`s3`), on a schedule, the oldest beyond
//! `keep` removed. A restore is staged into `<data>/restore/` and applied as Freightdock starts again, which it does at
//! once by running itself anew; `freightdock restore <archive>` stages one while it is stopped ("Backups" in DECISIONS.md).

pub mod crypt;
pub mod s3;

use std::io::Read;
use std::path::{Path as FsPath, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::{Json, Router, routing::get, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

const SETTINGS_KEY: &str = "backups";
const STATE_KEY: &str = "backups.state";
/// What goes into an archive from the data folder, when it is there. Everything else (the sockets, older backups, a
/// restore on its way) stays out.
pub const KEPT: &[&str] = &["secret.key", "ca", "tls", "stacks"];
/// What a restore puts in place, and moves aside first.
const REPLACED: &[&str] = &["freightdock.db", "freightdock.db-wal", "freightdock.db-shm", "secret.key", "ca", "tls", "stacks"];
/// The largest archive a restore takes as an upload.
pub const UPLOAD_MAX: usize = 1 << 30;
const UPLOADED: &str = "uploaded";
const MARKER: &str = ".restore.json";

/// What every request and the schedule share: where the data folder is, and the way to have Freightdock start again.
#[derive(Default)]
pub struct Backups {
    pub data_dir: OnceLock<PathBuf>,
    pub restart: tokio::sync::Notify,
}

impl Backups {
    fn dir(&self) -> ApiResult<PathBuf> {
        self.data_dir.get().cloned().ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Freightdock does not know its data folder."))
    }
}

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/settings/backups", get(get_settings).put(put_settings))
        .route("/api/backups", get(list))
        .route("/api/backups/run", post(run_now))
        .route("/api/backups/restore", post(restore))
        .route("/api/backups/upload", post(upload).layer(axum::extract::DefaultBodyLimit::max(UPLOAD_MAX)))
        .route("/api/backups/{name}/download", get(download))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Hours between two backups, 1 to 720.
    pub every_hours: u64,
    /// How many archives are kept at the destination, 1 to 365; the oldest beyond go.
    pub keep: usize,
    /// `folder` or `s3`.
    pub destination: String,
    /// The folder, on Freightdock's side; empty for `<data>/backups`.
    pub folder: String,
    pub s3: s3::Config,
    /// Sealed at rest, never sent back.
    pub passphrase: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: false, every_hours: 24, keep: 7, destination: "folder".into(), folder: String::new(), s3: s3::Config { prefix: "freightdock/".into(), ..Default::default() }, passphrase: String::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Status {
    pub last_run_at: Option<i64>,
    pub last_name: Option<String>,
    pub last_size: Option<u64>,
    pub last_error: Option<String>,
}

pub async fn settings(app: &App) -> Settings {
    app.db.setting(SETTINGS_KEY).await.ok().flatten().unwrap_or_default()
}

async fn status(app: &App) -> Status {
    app.db.setting(STATE_KEY).await.ok().flatten().unwrap_or_default()
}

/// The settings as the page sees them: no passphrase, no secret key, only whether there is one.
fn shown(s: &Settings) -> Value {
    let mut v = json!(s);
    v["has_passphrase"] = json!(!s.passphrase.is_empty());
    v["s3"]["has_secret"] = json!(!s.s3.secret_key.is_empty());
    v.as_object_mut().map(|o| o.remove("passphrase"));
    v["s3"].as_object_mut().map(|o| o.remove("secret_key"));
    v
}

async fn get_settings(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let mut v = shown(&settings(&app).await);
    v["default_folder"] = json!(app.backups.dir().ok().map(|d| d.join("backups").display().to_string()));
    Ok(Json(v))
}

fn check(s: &Settings) -> ApiResult<()> {
    if !(1..=720).contains(&s.every_hours) {
        return Err(ApiError::bad("A backup every 1 to 720 hours.").field("every_hours"));
    }
    if !(1..=365).contains(&s.keep) {
        return Err(ApiError::bad("Keep 1 to 365 backups.").field("keep"));
    }
    match s.destination.as_str() {
        "folder" => {
            if !s.folder.trim().is_empty() && !FsPath::new(s.folder.trim()).is_absolute() {
                return Err(ApiError::bad("A folder's whole path, from /.").field("folder"));
            }
        }
        "s3" => {
            s3::Client::new(s.s3.clone(), String::new()).map_err(|e| ApiError::bad(format!("{e}.")).field("endpoint"))?;
            if s.s3.bucket.trim().is_empty() || s.s3.bucket.contains('/') {
                return Err(ApiError::bad("Name the bucket.").field("bucket"));
            }
            if s.s3.access_key.trim().is_empty() {
                return Err(ApiError::bad("The access key's id.").field("access_key"));
            }
        }
        _ => return Err(ApiError::bad("A backup goes to a folder or to S3.").field("destination")),
    }
    Ok(())
}

async fn put_settings(State(app): State<App>, admin: Admin, Json(mut s): Json<Settings>) -> ApiResult<StatusCode> {
    let old = settings(&app).await;
    check(&s)?;
    s.passphrase = match s.passphrase.as_str() {
        "" => old.passphrase.clone(),
        p if p.chars().count() < 12 => return Err(ApiError::bad("A passphrase of at least 12 characters: it is all that guards the archives.").field("passphrase")),
        p => app.sealer.seal(p)?,
    };
    s.s3.secret_key = match s.s3.secret_key.trim() {
        "" => old.s3.secret_key.clone(),
        k => app.sealer.seal(k)?,
    };
    if s.enabled && s.passphrase.is_empty() {
        return Err(ApiError::bad("Set a passphrase first: archives are always sealed.").field("passphrase"));
    }
    if s.destination == "s3" && s.s3.secret_key.is_empty() {
        return Err(ApiError::bad("The secret access key.").field("secret_key"));
    }
    s.folder = s.folder.trim().to_string();
    app.db.put_setting(SETTINGS_KEY, &s).await?;
    let detail = format!("{} to {}, every {} h, keep {}", if s.enabled { "on" } else { "off" }, s.destination, s.every_hours, s.keep);
    log(&app, Some(&admin), "settings.backups", None, Some(detail)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn log(app: &App, admin: Option<&Admin>, action: &'static str, target: Option<String>, detail: Option<String>) {
    let (user, ip) = admin.map_or((None, None), |a| (Some(&a.0.session.user), a.0.ip.clone()));
    audit::record(&app.db, user, ip, Entry { action, target, detail }).await;
}

/// `freightdock-20261109-211409.backup` for a Unix time.
pub fn name_at(at: i64) -> String {
    let (y, m, d, hh, mm, ss) = crate::db::utc(at);
    format!("freightdock-{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}.backup")
}

/// Whether `n` is the name of an archive Freightdock made (and so one it may remove).
pub fn is_archive_name(n: &str) -> bool {
    let Some(core) = n.strip_prefix("freightdock-").and_then(|r| r.strip_suffix(".backup")) else { return false };
    let b = core.as_bytes();
    b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit())
}

/// The archives to remove so that `keep` are left: the oldest, by their names, which sort by time.
pub fn to_prune(mut names: Vec<String>, keep: usize) -> Vec<String> {
    names.retain(|n| is_archive_name(n));
    names.sort();
    let over = names.len().saturating_sub(keep);
    names.into_iter().take(over).collect()
}

/// Where archives go.
enum Dest {
    Folder(PathBuf),
    S3(Box<s3::Client>),
}

impl Dest {
    fn of(app: &App, s: &Settings) -> ApiResult<Dest> {
        Ok(match s.destination.as_str() {
            "s3" => {
                let secret = if s.s3.secret_key.is_empty() { String::new() } else { app.sealer.open(&s.s3.secret_key)? };
                Dest::S3(Box::new(s3::Client::new(s.s3.clone(), secret).map_err(|e| ApiError::bad(e.to_string()))?))
            }
            _ => Dest::Folder(if s.folder.is_empty() { app.backups.dir()?.join("backups") } else { PathBuf::from(&s.folder) }),
        })
    }

    async fn put(&self, name: &str, data: Vec<u8>) -> Result<()> {
        match self {
            Dest::Folder(dir) => {
                let (dir, name) = (dir.clone(), name.to_string());
                tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
                    let part = dir.join(format!(".{name}.part"));
                    crate::tls::write_private(&part, &data)?;
                    std::fs::rename(&part, dir.join(&name)).with_context(|| format!("putting {name} in {}", dir.display()))
                })
                .await?
            }
            Dest::S3(c) => c.put(name, data).await,
        }
    }

    async fn get(&self, name: &str) -> Result<Vec<u8>> {
        match self {
            Dest::Folder(dir) => Ok(tokio::fs::read(dir.join(name)).await.with_context(|| format!("reading {name}"))?),
            Dest::S3(c) => c.get(name).await,
        }
    }

    /// Freightdock's archives there, oldest first, with their sizes.
    async fn list(&self) -> Result<Vec<(String, u64)>> {
        let mut out = match self {
            Dest::Folder(dir) => {
                let mut out = Vec::new();
                match std::fs::read_dir(dir) {
                    Ok(rd) => {
                        for e in rd.flatten() {
                            let name = e.file_name().to_string_lossy().to_string();
                            if is_archive_name(&name) {
                                out.push((name, e.metadata().map(|m| m.len()).unwrap_or(0)));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
                }
                out
            }
            Dest::S3(c) => c.list().await?.into_iter().filter(|o| is_archive_name(&o.key)).map(|o| (o.key, o.size)).collect(),
        };
        out.sort();
        Ok(out)
    }

    async fn delete(&self, name: &str) -> Result<()> {
        match self {
            Dest::Folder(dir) => Ok(tokio::fs::remove_file(dir.join(name)).await?),
            Dest::S3(c) => c.delete(name).await,
        }
    }
}

/// What an archive says of itself.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub freightdock: String,
    pub schema: usize,
    pub made_at: i64,
    pub files: Vec<String>,
}

/// The archive's contents before it is sealed: `manifest.json`, the database copy as `freightdock.db`, and what of `KEPT`
/// the data folder has, as a tar.gz.
pub fn archive(data_dir: &FsPath, db_copy: &FsPath, at: i64) -> Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut t = tar::Builder::new(gz);
    t.follow_symlinks(false);
    let mut files = vec!["freightdock.db".to_string()];
    for k in KEPT {
        if data_dir.join(k).exists() {
            files.push(k.to_string());
        }
    }
    let manifest = serde_json::to_vec_pretty(&Manifest { freightdock: env!("CARGO_PKG_VERSION").into(), schema: crate::db::MIGRATIONS.len(), made_at: at, files: files.clone() })?;
    let mut h = tar::Header::new_gnu();
    h.set_size(manifest.len() as u64);
    h.set_mode(0o600);
    h.set_mtime(at.max(0) as u64);
    t.append_data(&mut h, "manifest.json", &manifest[..])?;
    t.append_path_with_name(db_copy, "freightdock.db")?;
    for k in &files[1..] {
        let p = data_dir.join(k);
        if p.is_dir() { t.append_dir_all(k, &p)? } else { t.append_path_with_name(&p, k)? }
    }
    Ok(t.into_inner()?.finish()?)
}

/// Copies the live database to `to` with SQLite's online backup API: a consistent copy while Freightdock goes on writing.
pub async fn copy_database(app: &App, to: PathBuf) -> Result<()> {
    app.db
        .call(move |c| {
            let mut dst = crate::db::rusqlite::Connection::open(&to)?;
            let b = crate::db::rusqlite::backup::Backup::new(c, &mut dst)?;
            b.run_to_completion(1024, Duration::from_millis(5), None)?;
            Ok(())
        })
        .await
}

/// Makes a backup now: answers the archive's name and size.
pub async fn make(app: &App) -> ApiResult<(String, u64)> {
    let s = settings(app).await;
    if s.passphrase.is_empty() {
        return Err(ApiError::bad("Set a passphrase in the backup settings first: archives are always sealed.").field("passphrase"));
    }
    let passphrase = app.sealer.open(&s.passphrase)?;
    let dest = Dest::of(app, &s)?;
    let dir = app.backups.dir()?;
    let at = now();
    let mut suffix = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut suffix);
    let copy = dir.join(format!(".backup-{}.db", hex::encode(suffix)));
    let result = async {
        copy_database(app, copy.clone()).await?;
        let (d, c) = (dir.clone(), copy.clone());
        let sealed = tokio::task::spawn_blocking(move || crypt::seal(&archive(&d, &c, at)?, &passphrase, crypt::COST)).await??;
        let name = name_at(at);
        let size = sealed.len() as u64;
        dest.put(&name, sealed).await?;
        let names = dest.list().await?.into_iter().map(|(n, _)| n).collect();
        for old in to_prune(names, s.keep) {
            if let Err(e) = dest.delete(&old).await {
                tracing::warn!("could not remove the old backup {old}: {e:#}");
            }
        }
        anyhow::Ok((name, size))
    }
    .await;
    let _ = std::fs::remove_file(&copy);
    let st = match &result {
        Ok((name, size)) => Status { last_run_at: Some(at), last_name: Some(name.clone()), last_size: Some(*size), last_error: None },
        Err(e) => Status { last_run_at: Some(at), last_error: Some(format!("{e:#}")), ..status(app).await },
    };
    let _ = app.db.put_setting(STATE_KEY, &st).await;
    result.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("The backup failed: {e:#}")))
}

/// `GET /api/backups`: the archives at the destination, newest first, and how the last backup went.
async fn list(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let s = settings(&app).await;
    let st = status(&app).await;
    let archives = match Dest::of(&app, &s) {
        Ok(d) => d.list().await.map_err(|e| format!("{e:#}")),
        Err(e) => Err(e.message),
    };
    let (archives, error) = match archives {
        Ok(mut a) => {
            a.reverse();
            (a.into_iter().map(|(name, size)| json!({ "name": name, "size": size })).collect::<Vec<_>>(), None)
        }
        Err(e) => (Vec::new(), Some(e)),
    };
    let uploaded = app.backups.dir().ok().and_then(|d| std::fs::metadata(d.join("restore-upload.backup")).ok()).map(|m| m.len());
    Ok(Json(json!({ "archives": archives, "error": error, "status": st, "uploaded": uploaded, "destination": s.destination })))
}

async fn run_now(State(app): State<App>, admin: Admin) -> ApiResult<Json<Value>> {
    let r = make(&app).await;
    let detail = match &r {
        Ok((name, size)) => format!("{name}, {size} bytes"),
        Err(e) => format!("failed: {}", e.message),
    };
    log(&app, Some(&admin), "backup.make", None, Some(detail)).await;
    let (name, size) = r?;
    Ok(Json(json!({ "name": name, "size": size })))
}

/// `GET /api/backups/{name}/download`: an archive as it is kept, sealed, to keep somewhere else.
async fn download(State(app): State<App>, admin: Admin, Path(name): Path<String>) -> ApiResult<Response> {
    if !is_archive_name(&name) {
        return Err(ApiError::not_found("backup"));
    }
    let data = Dest::of(&app, &settings(&app).await)?.get(&name).await.map_err(|e| ApiError::new(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    log(&app, Some(&admin), "backup.download", Some(name.clone()), None).await;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
        .body(Body::from(data))
        .map_err(|e| anyhow::anyhow!(e).into())
}

/// `POST /api/backups/upload`: an archive from elsewhere, kept beside the data folder until it is restored.
async fn upload(State(app): State<App>, admin: Admin, body: Bytes) -> ApiResult<Json<Value>> {
    if !crypt::is_backup(&body) {
        return Err(ApiError::bad("That is not a Freightdock backup.").field("file"));
    }
    let dir = app.backups.dir()?;
    let size = body.len();
    tokio::task::spawn_blocking(move || crate::tls::write_private(&dir.join("restore-upload.backup"), &body)).await.map_err(anyhow::Error::from)??;
    log(&app, Some(&admin), "backup.upload", None, Some(format!("{size} bytes"))).await;
    Ok(Json(json!({ "name": UPLOADED, "size": size })))
}

#[derive(Deserialize)]
struct Restore {
    /// An archive at the destination, or `uploaded`.
    name: String,
    passphrase: String,
}

/// `POST /api/backups/restore {name, passphrase}`: opens and checks the archive, stages it, and has Freightdock start
/// again, which puts it in place. Answers 202 before the restart.
async fn restore(State(app): State<App>, admin: Admin, Json(r): Json<Restore>) -> ApiResult<(StatusCode, Json<Value>)> {
    let dir = app.backups.dir()?;
    let data = if r.name == UPLOADED {
        tokio::fs::read(dir.join("restore-upload.backup")).await.map_err(|_| ApiError::not_found("uploaded backup"))?
    } else if is_archive_name(&r.name) {
        Dest::of(&app, &settings(&app).await)?.get(&r.name).await.map_err(|e| ApiError::new(StatusCode::NOT_FOUND, format!("{e:#}")))?
    } else {
        return Err(ApiError::not_found("backup"));
    };
    let by = admin.0.session.user.name.clone();
    let (d, from, name) = (dir.clone(), r.name.clone(), r.name.clone());
    let staged = tokio::task::spawn_blocking(move || stage(&d, &data, &r.passphrase, &by, &name)).await.map_err(anyhow::Error::from)?;
    let manifest = match staged {
        Ok(m) => m,
        Err(e) => {
            log(&app, Some(&admin), "backup.restore", Some(from), Some(format!("refused: {e:#}"))).await;
            return Err(ApiError::bad(format!("{e:#}.")).field("passphrase"));
        }
    };
    let _ = std::fs::remove_file(dir.join("restore-upload.backup"));
    log(&app, Some(&admin), "backup.restore", Some(from), Some(format!("made {}, staged; Freightdock starts again", crate::notify::utc_text(manifest.made_at)))).await;
    let a = app.clone();
    tokio::spawn(async move {
        // The answer goes out first.
        tokio::time::sleep(Duration::from_millis(500)).await;
        a.backups.restart.notify_one();
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "restarting": true, "made_at": manifest.made_at, "freightdock": manifest.freightdock }))))
}

/// Opens an archive, checks it, and stages it in `<data>/restore/` for the next start: the database must be whole
/// and not newer than this Freightdock, and nothing in it may land outside the folder.
pub fn stage(data_dir: &FsPath, sealed: &[u8], passphrase: &str, by: &str, from: &str) -> Result<Manifest> {
    let plain = crypt::open(sealed, passphrase)?;
    let tmp = data_dir.join("restore.tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let result = (|| {
        let mut manifest: Option<Manifest> = None;
        let mut a = tar::Archive::new(flate2::read::GzDecoder::new(&plain[..]));
        for e in a.entries()? {
            let mut e = e?;
            let path = e.path()?.into_owned();
            let top = path.components().next().map(|c| c.as_os_str().to_string_lossy().to_string()).unwrap_or_default();
            if top == "manifest.json" {
                let mut s = String::new();
                e.read_to_string(&mut s)?;
                manifest = Some(serde_json::from_str(&s).context("the archive's manifest")?);
                continue;
            }
            if top != "freightdock.db" && !KEPT.contains(&top.as_str()) {
                bail!("the archive holds {}, which is not Freightdock's", path.display());
            }
            if !e.unpack_in(&tmp)? {
                bail!("the archive holds a path out of its folder: {}", path.display());
            }
        }
        let manifest = manifest.context("the archive has no manifest: it was not made by Freightdock")?;
        let db = tmp.join("freightdock.db");
        anyhow::ensure!(db.exists(), "the archive has no database");
        anyhow::ensure!(std::fs::metadata(tmp.join("secret.key")).map(|m| m.len()).unwrap_or(0) == 32, "the archive has no 32-byte secret.key");
        {
            let c = crate::db::rusqlite::Connection::open_with_flags(&db, crate::db::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            anyhow::ensure!(ok == "ok", "the archive's database is damaged: {ok}");
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            anyhow::ensure!(v as usize <= crate::db::MIGRATIONS.len(), "the archive is from a newer Freightdock (schema {v}); restore it with that one");
        }
        let marker = json!({ "by": by, "from": from, "at": now(), "made_at": manifest.made_at, "freightdock": manifest.freightdock });
        std::fs::write(tmp.join(MARKER), serde_json::to_vec(&marker)?)?;
        Ok(manifest)
    })();
    match result {
        Ok(m) => {
            let staged = data_dir.join("restore");
            let _ = std::fs::remove_dir_all(&staged);
            std::fs::rename(&tmp, &staged)?;
            Ok(m)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// Folders 0700 and files 0600, all the way down, never through a link: a Git stack's clone keeps the links its
/// repository commits, and one may point anywhere on Freightdock's side ("Backups" 6). A link is left as it is, and so is
/// what it points at.
#[cfg(unix)]
fn private(p: &FsPath) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(kind) = std::fs::symlink_metadata(p).map(|m| m.file_type()) else { return };
    if kind.is_dir() {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            private(&e.path());
        }
    } else if kind.is_file() {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn private(_: &FsPath) {}

/// At start, before the database is opened: a restore staged in `<data>/restore/` is put in place, what it replaces
/// moved to `<data>/before-restore-<time>/`. Answers what the restore's marker says, for the activity log.
pub fn apply_staged(data_dir: &FsPath) -> Result<Option<String>> {
    let staged = data_dir.join("restore");
    if !staged.join(MARKER).exists() {
        if staged.exists() {
            let _ = std::fs::remove_dir_all(&staged);
        }
        return Ok(None);
    }
    let aside = data_dir.join(format!("before-restore-{}", name_at(now()).trim_start_matches("freightdock-").trim_end_matches(".backup")));
    std::fs::create_dir_all(&aside)?;
    for item in REPLACED {
        let p = data_dir.join(item);
        if p.exists() {
            std::fs::rename(&p, aside.join(item)).with_context(|| format!("moving {} aside", p.display()))?;
        }
    }
    let marker: Value = serde_json::from_slice(&std::fs::read(staged.join(MARKER))?).unwrap_or(json!({}));
    for item in REPLACED {
        let p = staged.join(item);
        if p.exists() {
            std::fs::rename(&p, data_dir.join(item)).with_context(|| format!("putting {item} in place"))?;
            private(&data_dir.join(item));
        }
    }
    std::fs::remove_dir_all(&staged)?;
    let made = marker["made_at"].as_i64().map(crate::notify::utc_text).unwrap_or_else(|| "?".into());
    Ok(Some(format!("from {}, made {made}, by {}; what it replaced is in {}", marker["from"].as_str().unwrap_or("?"), marker["by"].as_str().unwrap_or("?"), aside.display())))
}

/// Backs up every `every_hours` while backups are on, until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    loop {
        let s = settings(&app).await;
        if s.enabled && !s.passphrase.is_empty() && crate::history::due(status(&app).await.last_run_at, now(), s.every_hours * 3600) {
            let r = make(&app).await;
            let detail = match &r {
                Ok((name, size)) => format!("{name}, {size} bytes, on schedule"),
                Err(e) => format!("failed on schedule: {}", e.message),
            };
            if let Err(e) = &r {
                tracing::warn!("{}", e.message);
            }
            log(&app, None, "backup.make", None, Some(detail)).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = stop.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;

    #[test]
    fn archives_are_named_by_time_and_the_oldest_beyond_keep_go() {
        assert_eq!(name_at(1_762_722_849), "freightdock-20251109-211409.backup");
        assert!(is_archive_name("freightdock-20251109-211409.backup"));
        assert!(!is_archive_name("freightdock-2025110-211409.backup") && !is_archive_name("notes.txt") && !is_archive_name("freightdock-20251109-211409.backup.part"));
        let names = vec!["freightdock-20261103-000000.backup".to_string(), "freightdock-20261101-000000.backup".into(), "mine.tar".into(), "freightdock-20261102-000000.backup".into()];
        assert_eq!(to_prune(names.clone(), 2), vec!["freightdock-20261101-000000.backup"]);
        assert!(to_prune(names, 7).is_empty());
    }

    fn data_folder() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("secret.key"), [7u8; 32]).unwrap();
        std::fs::create_dir_all(d.path().join("ca")).unwrap();
        std::fs::write(d.path().join("ca/ca.pem"), "CA").unwrap();
        std::fs::create_dir_all(d.path().join("run")).unwrap();
        std::fs::write(d.path().join("run/env-2.sock"), "").unwrap();
        d
    }

    #[tokio::test]
    async fn an_archive_holds_the_database_the_key_and_the_ca_and_restores_into_place() {
        let app = app();
        let from = data_folder();
        app.db.call(|c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('marker', '\"before\"')", [])?)).await.unwrap();
        let copy = from.path().join("copy.db");
        copy_database(&app, copy.clone()).await.unwrap();
        let plain = archive(from.path(), &copy, 1_762_722_849).unwrap();
        let sealed = crypt::seal(&plain, "a passphrase", crypt::Cost { m: 64, t: 1, p: 1 }).unwrap();

        let to = tempfile::tempdir().unwrap();
        std::fs::write(to.path().join("freightdock.db"), "the old database").unwrap();
        std::fs::write(to.path().join("secret.key"), [1u8; 32]).unwrap();
        assert!(stage(to.path(), &sealed, "not the passphrase", "admin", "x").is_err());
        assert!(!to.path().join("restore").exists(), "nothing is staged from what does not open");
        let m = stage(to.path(), &sealed, "a passphrase", "admin", "freightdock-20251109-211409.backup").unwrap();
        assert_eq!((m.made_at, m.schema), (1_762_722_849, crate::db::MIGRATIONS.len()));
        assert!(m.files.contains(&"ca".to_string()) && !m.files.contains(&"run".to_string()), "{:?}", m.files);
        assert_eq!(std::fs::read(to.path().join("freightdock.db")).unwrap(), b"the old database", "staged, not yet in place");

        let said = apply_staged(to.path()).unwrap().unwrap();
        assert!(said.contains("by admin") && said.contains("before-restore-"), "{said}");
        assert_eq!(std::fs::read(to.path().join("secret.key")).unwrap(), [7u8; 32]);
        assert_eq!(std::fs::read_to_string(to.path().join("ca/ca.pem")).unwrap(), "CA");
        assert!(!to.path().join("run").exists() && !to.path().join("restore").exists());
        let db = crate::db::Db::open(&to.path().join("freightdock.db")).unwrap();
        let v: String = db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'marker'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(v, "\"before\"");
        let aside: Vec<_> = std::fs::read_dir(to.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("before-restore-")).collect();
        assert_eq!(std::fs::read(aside[0].path().join("freightdock.db")).unwrap(), b"the old database", "what was replaced is kept aside");
        assert_eq!(apply_staged(to.path()).unwrap(), None, "applied once");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(to.path().join("secret.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn making_a_restore_private_never_follows_a_link_out_of_the_data_folder() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &FsPath| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("bin")).unwrap();
        std::fs::write(outside.path().join("bin/tool"), "x").unwrap();
        std::fs::set_permissions(outside.path().join("bin"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(outside.path().join("bin/tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let data = tempfile::tempdir().unwrap();
        let repo = data.path().join("stacks/1/repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("compose.yaml"), "services: {}").unwrap();
        // What a Git stack's repository can commit: links to a folder and to a file on Freightdock's side.
        std::os::unix::fs::symlink(outside.path().join("bin"), repo.join("dir-link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("bin/tool"), repo.join("file-link")).unwrap();
        private(&data.path().join("stacks"));
        assert_eq!(mode(&repo.join("compose.yaml")), 0o600, "what is in the data folder is made private");
        assert_eq!(mode(&repo), 0o700);
        assert_eq!(mode(&outside.path().join("bin")), 0o755, "a linked folder is left alone");
        assert_eq!(mode(&outside.path().join("bin/tool")), 0o755, "a linked file is left alone");
    }

    #[test]
    fn an_archive_with_anything_else_in_it_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut t = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        t.append_data(&mut h, "etc/cron.d/x", &b"hi"[..]).unwrap();
        let plain = t.into_inner().unwrap().finish().unwrap();
        let sealed = crypt::seal(&plain, "p", crypt::Cost { m: 64, t: 1, p: 1 }).unwrap();
        let e = stage(d.path(), &sealed, "p", "a", "b").unwrap_err().to_string();
        assert!(e.contains("not Freightdock's"), "{e}");
        assert!(!d.path().join("restore").exists() && !d.path().join("restore.tmp").exists());
    }

    #[tokio::test]
    async fn a_backup_to_a_folder_keeps_the_newest_and_a_restore_asks_for_a_restart() {
        let app = app();
        let dir = data_folder();
        app.backups.data_dir.set(dir.path().to_path_buf()).unwrap();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::POST, "/api/backups/run", &cookie, &csrf, None)).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("passphrase")), "no passphrase, no backup");
        let body = json!({ "enabled": false, "every_hours": 24, "keep": 2, "destination": "folder", "passphrase": "short" });
        let (s, _, b) = call(&app, req(Method::PUT, "/api/settings/backups", &cookie, &csrf, Some(body))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("passphrase")));
        let body = json!({ "enabled": false, "every_hours": 24, "keep": 2, "destination": "folder", "passphrase": "a long backup passphrase" });
        let (s, _, _) = call(&app, req(Method::PUT, "/api/settings/backups", &cookie, &csrf, Some(body))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, b) = call(&app, req(Method::GET, "/api/settings/backups", &cookie, &csrf, None)).await;
        assert_eq!(b["has_passphrase"], true);
        assert!(b.get("passphrase").is_none() && !b.to_string().contains("sealed1:"), "{b}");
        let raw: String = app.db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'backups'", [], |r| r.get(0))?)).await.unwrap();
        assert!(!raw.contains("a long backup passphrase"));

        // Three backups a second apart (names are by the second); two are kept.
        for _ in 0..3 {
            let (s, _, b) = call(&app, req(Method::POST, "/api/backups/run", &cookie, &csrf, None)).await;
            assert_eq!(s, StatusCode::OK, "{b}");
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        let (_, _, b) = call(&app, req(Method::GET, "/api/backups", &cookie, &csrf, None)).await;
        let archives = b["archives"].as_array().unwrap().clone();
        assert_eq!(archives.len(), 2, "{b}");
        assert!(archives[0]["name"].as_str() > archives[1]["name"].as_str(), "newest first");
        assert!(b["status"]["last_error"].is_null());
        let newest = archives[0]["name"].as_str().unwrap().to_string();
        assert!(std::fs::read_dir(dir.path()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".backup-")), "the database copy is gone");

        let (s, _, b) = call(&app, req(Method::POST, "/api/backups/restore", &cookie, &csrf, Some(json!({ "name": newest, "passphrase": "wrong" })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("passphrase")));
        let restart = app.backups.restart.notified();
        let (s, _, b) = call(&app, req(Method::POST, "/api/backups/restore", &cookie, &csrf, Some(json!({ "name": newest, "passphrase": "a long backup passphrase" })))).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{b}");
        tokio::time::timeout(Duration::from_secs(5), restart).await.expect("a restart is asked for");
        assert!(dir.path().join("restore/freightdock.db").exists());
        let actions: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action FROM audit WHERE action LIKE 'backup.%' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap();
        assert_eq!(actions.iter().filter(|a| *a == "backup.make").count(), 4);
        assert_eq!(actions.iter().filter(|a| *a == "backup.restore").count(), 2, "the refused one too");
        let (s, _, _) = call(&app, req(Method::GET, "/api/backups/..%2Fsecret.key/download", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "only an archive's name");
    }
}
