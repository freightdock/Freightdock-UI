//! The volume file browser: list, download, upload, make a folder and delete, inside a volume. Each request is a
//! short-lived helper container (`busybox`) that mounts the volume at `/volume` and nothing else: no network, no other
//! mount. Listing, making and deleting run a command in it; download and upload are Docker's archive API on it, created
//! and never started. It is Docker's own API all the way, so a TLS host and an agent's host work as the local one does,
//! with no second protocol through the tunnel ("Volume files" in DECISIONS.md). Administrators only, every change logged.

use std::collections::HashMap;
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountTypeEnum};
use bollard::query_parameters as q;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{api_error, endpoint, valid_ref};
use crate::api::Allowed;
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The helper's image: small, everywhere, with `sh`, `stat`, `mkdir` and `rm`. Pulled on a host the first time.
pub const HELPER: &str = "busybox:1.37";
/// Where the helper mounts the volume.
const ROOT: &str = "/volume";
/// The largest file an upload takes.
pub const UPLOAD_MAX: usize = 256 << 20;
/// At most this many entries in one folder's listing.
pub const LIST_MAX: usize = 5000;
/// How long a helper's command may run.
const RUN_WITHIN: Duration = Duration::from_secs(120);

/// A path inside a volume, cleaned: its parts joined by `/`, without `.` or empty parts, `""` for the volume's top.
/// Refused: `..` (out of the volume), a control character, a part over 255 bytes, more than 4096 in all.
pub fn clean(path: &str) -> ApiResult<String> {
    if path.len() > 4096 {
        return Err(ApiError::bad("That path is too long.").field("path"));
    }
    let mut parts = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => continue,
            ".." => return Err(ApiError::bad("A path stays inside the volume: no \"..\".").field("path")),
            p if p.chars().any(char::is_control) || p.len() > 255 => return Err(ApiError::bad("A file name without control characters, at most 255 bytes.").field("path")),
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

/// One name, for a file uploaded: a single part, not `.` or `..`.
pub fn file_name(name: &str) -> ApiResult<String> {
    let n = clean(name)?;
    if n.is_empty() || n.contains('/') {
        return Err(ApiError::bad("A file's name is one name, without a slash.").field("name"));
    }
    Ok(n)
}

/// The path as the helper sees it.
fn inside(rel: &str) -> String {
    if rel.is_empty() { ROOT.to_string() } else { format!("{ROOT}/{rel}") }
}

/// One entry of a folder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub name: String,
    /// `dir`, `file`, `link` or `other`.
    pub kind: &'static str,
    pub size: u64,
    pub modified: i64,
    /// The permission bits, as `ls` writes them in octal: `644`.
    pub mode: String,
}

/// What the listing command prints: per entry `<mode hex> <size> <mtime>\n<name>\0`, so a name may hold anything but NUL.
pub fn parse_listing(out: &[u8], max: usize) -> (Vec<Item>, bool) {
    let mut items = Vec::new();
    let mut rest = out;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else { break };
        let head = String::from_utf8_lossy(&rest[..nl]).to_string();
        rest = &rest[nl + 1..];
        let Some(nul) = rest.iter().position(|b| *b == 0) else { break };
        let name = String::from_utf8_lossy(&rest[..nul]).to_string();
        rest = &rest[nul + 1..];
        let mut f = head.split_whitespace();
        let (Some(mode), Some(size), Some(mtime)) = (f.next().and_then(|m| u32::from_str_radix(m, 16).ok()), f.next().and_then(|s| s.parse().ok()), f.next().and_then(|t| t.parse().ok())) else { continue };
        if items.len() == max {
            return (items, true);
        }
        let kind = match mode & 0o170000 {
            0o040000 => "dir",
            0o100000 => "file",
            0o120000 => "link",
            _ => "other",
        };
        items.push(Item { name, kind, size, modified: mtime, mode: format!("{:o}", mode & 0o7777) });
    }
    (items, false)
}

/// What a tar from Docker's archive API starts with.
#[derive(Debug, PartialEq)]
pub enum First {
    File { name: String, size: u64 },
    Dir { name: String },
    Other,
}

fn octal(field: &[u8]) -> u64 {
    let s: String = field.iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
    u64::from_str_radix(s.trim(), 8).unwrap_or(0)
}

fn cstr(field: &[u8]) -> String {
    String::from_utf8_lossy(&field[..field.iter().position(|b| *b == 0).unwrap_or(field.len())]).to_string()
}

/// Reads the first real entry of a tar from the bytes that came so far: what it is and how many bytes its headers
/// took (a PAX or GNU long-name record in front included), or None while more bytes are needed.
pub fn first_entry(buf: &[u8]) -> Result<Option<(First, usize)>, String> {
    let mut off = 0;
    let mut long_name: Option<String> = None;
    loop {
        if buf.len() < off + 512 {
            return Ok(None);
        }
        let h = &buf[off..off + 512];
        if h.iter().all(|b| *b == 0) {
            return Err("the archive is empty".into());
        }
        let size = octal(&h[124..136]);
        let padded = size.div_ceil(512) as usize * 512;
        let mut name = cstr(&h[0..100]);
        if &h[257..262] == b"ustar" {
            let prefix = cstr(&h[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        match h[156] {
            b'x' | b'g' | b'L' => {
                if buf.len() < off + 512 + padded {
                    return Ok(None);
                }
                let data = &buf[off + 512..off + 512 + size as usize];
                if h[156] == b'L' {
                    long_name = Some(cstr(data));
                } else if h[156] == b'x' {
                    // PAX records: "<len> key=value\n".
                    for rec in String::from_utf8_lossy(data).split('\n') {
                        if let Some((_, kv)) = rec.split_once(' ')
                            && let Some(p) = kv.strip_prefix("path=") {
                                long_name = Some(p.to_string());
                            }
                    }
                }
                off += 512 + padded;
            }
            t => {
                let name = long_name.take().unwrap_or(name).trim_end_matches('/').to_string();
                let first = match t {
                    b'0' | 0 | b'7' => First::File { name, size },
                    b'5' => First::Dir { name },
                    _ => First::Other,
                };
                return Ok(Some((first, off + 512)));
            }
        }
    }
}

/// A tar of one file, `name`, holding `data`.
pub fn tar_of(name: &str, data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(crate::db::now().max(0) as u64);
    h.set_entry_type(tar::EntryType::Regular);
    b.append_data(&mut h, name, data)?;
    b.into_inner()
}

/// Removes the helper when it goes out of scope, however the request ended.
struct Helper {
    client: bollard::Docker,
    id: String,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let (c, id) = (self.client.clone(), std::mem::take(&mut self.id));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = c.remove_container(&id, Some(q::RemoveContainerOptions { force: true, v: false, link: false })).await;
            });
        }
    }
}

fn check_volume(name: &str) -> ApiResult<()> {
    if valid_ref(name) { Ok(()) } else { Err(ApiError::bad("That is not a volume's name.")) }
}

/// A helper container with `volume` at `/volume` (read-only unless `write`), made and not started. The volume must be
/// there: Docker would make a new one for a name it does not know.
async fn helper(d: &super::Docker, volume: &str, write: bool, cmd: Option<Vec<String>>) -> ApiResult<Helper> {
    let c = d.client();
    c.inspect_volume(volume).await.map_err(api_error)?;
    if c.inspect_image(HELPER).await.is_err() {
        super::containers::pull_with(d, HELPER, None).await.map_err(|e| ApiError::new(e.status, format!("The helper image {HELPER} could not be pulled on this host: {}", e.message)))?;
    }
    let mut suffix = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut suffix);
    let body = ContainerCreateBody {
        image: Some(HELPER.into()),
        cmd,
        labels: Some(HashMap::from([("freightdock.helper".to_string(), "volume-files".to_string())])),
        network_disabled: Some(true),
        host_config: Some(HostConfig {
            mounts: Some(vec![Mount { target: Some(ROOT.into()), source: Some(volume.into()), typ: Some(MountTypeEnum::VOLUME), read_only: Some(!write), ..Default::default() }]),
            network_mode: Some("none".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let opts = q::CreateContainerOptions { name: Some(format!("freightdock-files-{}", hex::encode(suffix))), ..Default::default() };
    let made = c.create_container(Some(opts), body).await.map_err(api_error)?;
    Ok(Helper { client: c.clone(), id: made.id })
}

/// Runs `script` (with `args` as `$1`...) in a helper; answers its exit code, what it printed, and its errors.
async fn run(d: &super::Docker, volume: &str, write: bool, script: &str, args: &[String]) -> ApiResult<(i64, Vec<u8>, String)> {
    let mut cmd = vec!["sh".to_string(), "-c".into(), script.into(), "sh".into()];
    cmd.extend(args.iter().cloned());
    let h = helper(d, volume, write, Some(cmd)).await?;
    let c = d.client();
    c.start_container(&h.id, None::<q::StartContainerOptions>).await.map_err(api_error)?;
    let waited = tokio::time::timeout(RUN_WITHIN, c.wait_container(&h.id, None::<q::WaitContainerOptions>).next()).await;
    let code = match waited {
        Ok(Some(Ok(w))) => w.status_code,
        // Docker answers a non-zero exit as an error carrying the code.
        Ok(Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. }))) => code,
        Ok(Some(Err(e))) => return Err(api_error(e)),
        Ok(None) => -1,
        Err(_) => return Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT, "The volume took too long to answer.")),
    };
    let (mut out, mut err) = (Vec::new(), String::new());
    let mut logs = c.logs(&h.id, Some(q::LogsOptions { stdout: true, stderr: true, ..Default::default() }));
    while let Some(Ok(l)) = logs.next().await {
        match l {
            bollard::container::LogOutput::StdOut { message } => out.extend_from_slice(&message),
            bollard::container::LogOutput::StdErr { message } => err.push_str(&String::from_utf8_lossy(&message)),
            _ => {}
        }
    }
    Ok((code, out, err.trim().to_string()))
}

const LIST: &str = r#"cd "$1" 2>/dev/null || { [ -e "$1" ] && exit 4; exit 3; }
for f in .[!.]* ..?* *; do
  if [ -e "$f" ] || [ -L "$f" ]; then stat -c '%f %s %Y' "./$f" && printf '%s\0' "$f"; fi
done"#;

#[derive(Deserialize, Default)]
pub struct At {
    #[serde(default)]
    path: String,
    /// An upload's file name.
    name: Option<String>,
}

async fn log(app: &App, admin: &Allowed, action: &'static str, target: String, detail: String) {
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action, target: Some(target), detail: Some(detail) }).await;
}

/// `GET .../volumes/{name}/files?path=`: a folder's entries, folders first.
pub async fn list(State(app): State<App>, _: Allowed, Path((env, volume)): Path<(i64, String)>, Query(at): Query<At>) -> ApiResult<Json<Value>> {
    check_volume(&volume)?;
    let rel = clean(&at.path)?;
    let d = endpoint(&app, env).await?;
    let (code, out, err) = run(&d, &volume, false, LIST, &[inside(&rel)]).await?;
    match code {
        0 => {}
        3 => return Err(ApiError::not_found("folder")),
        4 => return Err(ApiError::bad("That is a file, not a folder.").field("path")),
        _ => return Err(ApiError::new(StatusCode::BAD_GATEWAY, if err.is_empty() { format!("Listing failed (exit code {code}).") } else { err })),
    }
    let (mut items, truncated) = parse_listing(&out, LIST_MAX);
    items.sort_by(|a, b| (a.kind != "dir").cmp(&(b.kind != "dir")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Json(json!({ "volume": volume, "path": rel, "entries": items, "truncated": truncated })))
}

fn attachment(name: &str) -> String {
    let plain: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let encoded: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")
}

/// `GET .../volumes/{name}/files/download?path=`: a file as itself, a folder as a tar of it. Streamed: nothing is held
/// whole in memory, and the helper goes when the download ends.
pub async fn download(State(app): State<App>, admin: Allowed, Path((env, volume)): Path<(i64, String)>, Query(at): Query<At>) -> ApiResult<Response> {
    check_volume(&volume)?;
    let rel = clean(&at.path)?;
    let d = endpoint(&app, env).await?;
    let h = helper(&d, &volume, false, None).await?;
    let mut s = Box::pin(d.client().download_from_container(&h.id, Some(q::DownloadFromContainerOptions { path: inside(&rel) })));
    let mut buf = Vec::new();
    let (first, used) = loop {
        match s.next().await {
            Some(Ok(b)) => {
                buf.extend_from_slice(&b);
                if let Some(x) = first_entry(&buf).map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))? {
                    break x;
                }
                if buf.len() > 1 << 20 {
                    return Err(ApiError::new(StatusCode::BAD_GATEWAY, "Docker's archive did not start as one."));
                }
            }
            Some(Err(e)) => return Err(api_error(e)),
            None => return Err(ApiError::new(StatusCode::BAD_GATEWAY, "Docker's archive ended early.")),
        }
    };
    let shown = if rel.is_empty() { volume.clone() } else { rel.rsplit('/').next().unwrap_or(&rel).to_string() };
    let (left, rest, name, ctype) = match first {
        First::File { size, .. } => (size, Bytes::from(buf.split_off(used)), shown, "application/octet-stream"),
        First::Dir { .. } => (u64::MAX, Bytes::from(buf), format!("{shown}.tar"), "application/x-tar"),
        First::Other => return Err(ApiError::bad("Only a file or a folder can be downloaded.").field("path")),
    };
    log(&app, &admin, "volume.download", volume, format!("/{rel}")).await;
    let body = futures_util::stream::unfold((s, left, Some(rest), h), |(mut s, left, first, h)| async move {
        if left == 0 {
            return None;
        }
        let chunk = match first {
            Some(c) => c,
            None => match s.next().await {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Some((Err(std::io::Error::other(e)), (s, 0, None, h))),
                None => return None,
            },
        };
        let take = (chunk.len() as u64).min(left) as usize;
        Some((Ok::<_, std::io::Error>(chunk.slice(..take)), (s, left - take as u64, None, h)))
    });
    let mut r = Response::new(Body::from_stream(body));
    let hd = r.headers_mut();
    hd.insert(header::CONTENT_TYPE, ctype.parse().expect("a content type"));
    if let Ok(v) = attachment(&name).parse() {
        hd.insert(header::CONTENT_DISPOSITION, v);
    }
    if left != u64::MAX
        && let Ok(v) = left.to_string().parse() {
            hd.insert(header::CONTENT_LENGTH, v);
        }
    Ok(r)
}

/// `POST .../volumes/{name}/files/upload?path=<folder>&name=<file>`: the body is the file, at most `UPLOAD_MAX`. A
/// file there already of that name is replaced; a folder of that name is not.
pub async fn upload(State(app): State<App>, admin: Allowed, Path((env, volume)): Path<(i64, String)>, Query(at): Query<At>, body: Bytes) -> ApiResult<StatusCode> {
    check_volume(&volume)?;
    let rel = clean(&at.path)?;
    let name = file_name(at.name.as_deref().unwrap_or(""))?;
    if body.len() > UPLOAD_MAX {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, format!("A file of at most {} MB.", UPLOAD_MAX >> 20)));
    }
    let d = endpoint(&app, env).await?;
    let h = helper(&d, &volume, true, None).await?;
    let tar = tar_of(&name, &body).map_err(anyhow::Error::from)?;
    let opts = q::UploadToContainerOptions { path: inside(&rel), no_overwrite_dir_non_dir: Some("true".into()), ..Default::default() };
    d.client().upload_to_container(&h.id, Some(opts), bollard::body_full(tar.into())).await.map_err(|e| match e {
        bollard::errors::Error::DockerResponseServerError { status_code: 404, .. } => ApiError::not_found("folder"),
        e => api_error(e),
    })?;
    let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
    log(&app, &admin, "volume.upload", volume, format!("/{path}, {} bytes", body.len())).await;
    Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
pub struct MakeDir {
    path: String,
}

/// `POST .../volumes/{name}/files/mkdir {path}`: a folder, and those above it.
pub async fn mkdir(State(app): State<App>, admin: Allowed, Path((env, volume)): Path<(i64, String)>, Json(m): Json<MakeDir>) -> ApiResult<StatusCode> {
    check_volume(&volume)?;
    let rel = clean(&m.path)?;
    if rel.is_empty() {
        return Err(ApiError::bad("Name the folder to make.").field("path"));
    }
    let d = endpoint(&app, env).await?;
    let (code, _, err) = run(&d, &volume, true, r#"mkdir -p "$1""#, &[inside(&rel)]).await?;
    if code != 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, if err.is_empty() { "The folder could not be made.".to_string() } else { err.replace(ROOT, "") }));
    }
    log(&app, &admin, "volume.mkdir", volume, format!("/{rel}")).await;
    Ok(StatusCode::CREATED)
}

/// `DELETE .../volumes/{name}/files?path=`: a file, or a folder with all in it. The volume's top is not deleted.
pub async fn delete(State(app): State<App>, admin: Allowed, Path((env, volume)): Path<(i64, String)>, Query(at): Query<At>) -> ApiResult<StatusCode> {
    check_volume(&volume)?;
    let rel = clean(&at.path)?;
    if rel.is_empty() {
        return Err(ApiError::bad("Name what to delete; the volume itself is removed from the volumes list.").field("path"));
    }
    let d = endpoint(&app, env).await?;
    let (code, _, err) = run(&d, &volume, true, r#"[ -e "$1" ] || [ -L "$1" ] || exit 3; rm -rf "$1""#, &[inside(&rel)]).await?;
    match code {
        0 => {}
        3 => return Err(ApiError::not_found("file or folder")),
        _ => return Err(ApiError::new(StatusCode::CONFLICT, if err.is_empty() { "It could not be deleted.".to_string() } else { err.replace(ROOT, "") })),
    }
    log(&app, &admin, "volume.delete_path", volume, format!("/{rel}")).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;

    #[test]
    fn a_path_stays_inside_the_volume() {
        assert_eq!(clean("").unwrap(), "");
        assert_eq!(clean("/").unwrap(), "");
        assert_eq!(clean("./data//logs/").unwrap(), "data/logs");
        assert_eq!(clean("a/./b").unwrap(), "a/b");
        assert_eq!(clean("..").unwrap_err().field, Some("path"));
        assert!(clean("a/../../etc/passwd").is_err());
        assert!(clean("a/..").is_err(), "even .. that would stay inside is refused: there is no need for it");
        assert!(clean("bad\nname").is_err());
        assert!(clean(&"x".repeat(256)).is_err());
        assert_eq!(clean("-rf").unwrap(), "-rf", "a name like an option is a name: the helper is given /volume/-rf");
        assert_eq!(inside(&clean("-rf").unwrap()), "/volume/-rf");
        assert_eq!(inside(""), "/volume");
        assert_eq!(file_name("notes.txt").unwrap(), "notes.txt");
        assert!(file_name("a/b").is_err() && file_name("..").is_err() && file_name("").is_err());
    }

    #[test]
    fn a_listing_is_read_names_and_all() {
        let out = b"41ed 4096 1700000000\ndocs\x0081a4 12 1700000001\nhello world.txt\x00a1ff 7 1700000002\nlink\x0081a4 3 1700000003\nnew\nline\x00";
        let (items, truncated) = parse_listing(out, 100);
        assert!(!truncated);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0], Item { name: "docs".into(), kind: "dir", size: 4096, modified: 1_700_000_000, mode: "755".into() });
        assert_eq!((items[1].name.as_str(), items[1].kind, items[1].mode.as_str()), ("hello world.txt", "file", "644"));
        assert_eq!(items[2].kind, "link");
        assert_eq!(items[3].name, "new\nline", "a name with a newline in it survives");
        let (few, truncated) = parse_listing(out, 2);
        assert_eq!((few.len(), truncated), (2, true));
    }

    #[test]
    fn a_tar_s_first_entry_is_found_from_its_first_bytes() {
        let t = tar_of("hello.txt", b"hello volume").unwrap();
        assert_eq!(first_entry(&t[..100]).unwrap(), None, "a header is 512 bytes");
        let (first, used) = first_entry(&t).unwrap().unwrap();
        assert_eq!(first, First::File { name: "hello.txt".into(), size: 12 });
        assert_eq!(&t[used..used + 12], b"hello volume");

        // A long name comes in a record of its own first, as Docker's archive writes it.
        let long = format!("{}/file.bin", "d".repeat(150));
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, &long, &b"abc"[..]).unwrap();
        let t = b.into_inner().unwrap();
        let (first, used) = first_entry(&t).unwrap().unwrap();
        assert_eq!(first, First::File { name: long, size: 3 });
        assert_eq!(&t[used..used + 3], b"abc");

        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_size(0);
        b.append_data(&mut h, "docs/", &b""[..]).unwrap();
        let t = b.into_inner().unwrap();
        assert_eq!(first_entry(&t).unwrap().unwrap().0, First::Dir { name: "docs".into() });
        assert!(first_entry(&[0u8; 1024]).is_err());
    }

    #[test]
    fn a_download_s_name_is_safe_in_a_header() {
        assert_eq!(attachment("notes.txt"), "attachment; filename=\"notes.txt\"; filename*=UTF-8''notes.txt");
        assert_eq!(attachment("a \"b\".txt"), "attachment; filename=\"a__b_.txt\"; filename*=UTF-8''a%20%22b%22.txt");
    }

    #[tokio::test]
    async fn the_browser_is_an_administrator_s_and_its_paths_are_checked_first() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::GET, "/api/envs/1/volumes/data/files?path=../etc", &cookie, &csrf, None)).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("path")));
        let (s, _, _) = call(&app, req(Method::DELETE, "/api/envs/1/volumes/data/files?path=/", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "the volume's top is not deleted");
        let (s, _, b) = call(&app, req(Method::POST, "/api/envs/1/volumes/data/files/upload?path=&name=a%2Fb", &cookie, &csrf, None)).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("name")));

        app.db.call(|c| Ok(c.execute_batch("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0); INSERT INTO grants (user_id, env_id, role) VALUES (9, 1, 'helpdesk');")?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let (s, _, _) = call(&app, req(Method::GET, "/api/envs/1/volumes/data/files", &format!("freightdock_session={token}"), "", None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "reading a volume's files is an environment administrator's too, not Helpdesk's");
    }
}
