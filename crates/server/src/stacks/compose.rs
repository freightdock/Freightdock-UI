//! `docker compose`, Docker's own binary (the Compose plugin, run on its own), so a stack behaves exactly as it would on
//! the command line ("Endpoints" 2). Freightdock builds the arguments, hands the binary the environment it reaches Docker by
//! and nothing else of its own, and reads what it says line by line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::Sender;

use crate::docker::images::Line;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Where the Compose plugin is looked for when `FREIGHTDOCK_COMPOSE` does not say: where Docker's packages and the image
/// put it. The plugin runs on its own as well as under `docker`.
const SEARCH: &[&str] = &[
    "/usr/local/libexec/docker/cli-plugins/docker-compose",
    "/usr/libexec/docker/cli-plugins/docker-compose",
    "/usr/local/lib/docker/cli-plugins/docker-compose",
    "/usr/lib/docker/cli-plugins/docker-compose",
];

/// A compose job may run this long before it is stopped: a pull of big images on a slow line, not one that hangs.
const LIMIT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug)]
pub struct Compose {
    bin: Option<PathBuf>,
}

impl Compose {
    /// The binary `given` names (`FREIGHTDOCK_COMPOSE`), else the first of the usual places that has one, else
    /// `docker-compose` on the PATH.
    pub fn find(given: Option<&str>) -> Self {
        if let Some(g) = given.filter(|g| !g.is_empty()) {
            return Compose { bin: Some(PathBuf::from(g)) };
        }
        let home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker/cli-plugins/docker-compose"));
        let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).map(|d| d.join("docker-compose")).collect::<Vec<_>>());
        let bin = SEARCH.iter().map(PathBuf::from).chain(home).chain(on_path).find(|p| p.is_file());
        Compose { bin }
    }

    #[cfg(test)]
    pub fn none() -> Self {
        Compose { bin: None }
    }

    #[cfg(test)]
    pub fn at(bin: impl Into<PathBuf>) -> Self {
        Compose { bin: Some(bin.into()) }
    }

    pub fn path(&self) -> Option<&Path> {
        self.bin.as_deref()
    }

    /// The binary, or a 503 that says how to get one.
    pub fn bin(&self) -> ApiResult<&Path> {
        self.bin.as_deref().ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Docker Compose is not installed where Freightdock runs. The image has it; elsewhere, install the Compose plugin or set FREIGHTDOCK_COMPOSE to it.",
            )
        })
    }
}

/// A Compose project name as Compose takes it: lower-case letters, digits, `-` and `_`, starting with a letter or digit.
pub fn valid_project(name: &str) -> bool {
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f.is_ascii_digit())
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '-' || x == '_')
        && name.len() <= 63
}

/// A `.env` file Compose reads without surprise: each line blank, a `#` comment, or `NAME=value` with a name of letters,
/// digits and `_` that does not start with a digit (an `export ` in front is allowed, as Compose allows it). The first
/// line that is not is the error, by number.
pub fn check_env(text: &str) -> Result<(), String> {
    for (i, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let l = l.strip_prefix("export ").unwrap_or(l);
        let Some((name, _)) = l.split_once('=') else { return Err(format!("Line {} of the variables has no = in it: write NAME=value.", i + 1)) };
        let name = name.trim();
        let mut c = name.chars();
        let ok = c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_');
        if !ok {
            return Err(format!("Line {} of the variables: \"{name}\" is not a variable name (letters, digits and _, not starting with a digit).", i + 1));
        }
    }
    Ok(())
}

/// What a compose job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// `up -d --remove-orphans`: deploy, update and redeploy; `pull` asks for every image again first.
    Up { pull: bool },
    Stop,
    Start,
    Pull,
    /// `down`: the containers and networks go; `volumes` takes the stack's volumes with them.
    Down { volumes: bool },
    /// `config --quiet`: is the file one Compose takes.
    Check,
    /// `config --format json`: the file as Compose runs it, `${...}`, `.env` and `env_file` filled in ("Stacks" 11).
    Resolved,
}

impl Job {
    /// Whether the job reads the compose file. The ones that do not work by the project's name alone, so a stack whose
    /// file is broken can still be stopped and removed.
    pub fn needs_file(self) -> bool {
        matches!(self, Job::Up { .. } | Job::Pull | Job::Check | Job::Resolved)
    }

    /// Whether the job may pull images, and so is given the saved registries' sign-ins ("Stacks" 10).
    pub fn pulls(self) -> bool {
        matches!(self, Job::Up { .. } | Job::Pull)
    }
}

/// The project a job is about: its name and, for the jobs that read it, its file, the folder relative paths in it are
/// read from, and its variables.
pub struct Project<'a> {
    pub name: &'a str,
    pub file: &'a Path,
    pub dir: &'a Path,
    pub env_file: Option<&'a Path>,
}

/// The arguments for `job` on `p`: plain progress and no colours (they are read as text), then the project, then the job.
pub fn args(p: &Project, job: Job) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["--ansi", "never", "--progress", "plain", "-p", p.name].iter().map(OsString::from).collect();
    if job.needs_file() {
        a.extend(["-f".into(), p.file.into(), "--project-directory".into(), p.dir.into()]);
        if let Some(e) = p.env_file {
            a.extend(["--env-file".into(), e.into()]);
        }
    }
    let rest: &[&str] = match job {
        Job::Up { pull: false } => &["up", "--detach", "--remove-orphans"],
        Job::Up { pull: true } => &["up", "--detach", "--remove-orphans", "--pull", "always"],
        Job::Stop => &["stop"],
        Job::Start => &["start"],
        Job::Pull => &["pull"],
        Job::Down { volumes: false } => &["down", "--remove-orphans"],
        Job::Down { volumes: true } => &["down", "--remove-orphans", "--volumes"],
        Job::Check => &["config", "--quiet"],
        Job::Resolved => &["config", "--format", "json"],
    };
    a.extend(rest.iter().map(OsString::from));
    a
}

/// One service's variables as Compose would start it: name and value, in name order.
#[derive(serde::Serialize, Debug, PartialEq)]
pub struct ServiceEnv {
    pub service: String,
    pub environment: Vec<(String, String)>,
}

/// Every service's variables out of `config --format json`, services in name order. Compose writes `environment` as a
/// map; a variable it has no value for is `null`, shown as empty.
pub fn environments(json: &str) -> Result<Vec<ServiceEnv>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("Docker Compose's answer was not the JSON asked for: {e}"))?;
    let mut out: Vec<ServiceEnv> = v["services"]
        .as_object()
        .map(|services| {
            services
                .iter()
                .map(|(name, svc)| {
                    let mut environment: Vec<(String, String)> = svc["environment"]
                        .as_object()
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() }))).collect())
                        .unwrap_or_default();
                    environment.sort();
                    ServiceEnv { service: name.clone(), environment }
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(out)
}

/// The arguments that make one compose file of a project started elsewhere, from its files where it was started:
/// variables left as `${...}` and `env_file`s named, not copied in, but relative paths made absolute, so the stack still
/// mounts the folders it mounted from wherever its file now lives.
pub fn merge_args(name: &str, files: &[String], dir: &str, env_file: Option<&str>) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["--ansi", "never", "-p", name].iter().map(OsString::from).collect();
    for f in files {
        a.extend(["-f".into(), f.into()]);
    }
    a.extend(["--project-directory".into(), dir.into()]);
    if let Some(e) = env_file {
        a.extend(["--env-file".into(), e.into()]);
    }
    a.extend(["config", "--no-interpolate", "--no-env-resolution"].iter().map(OsString::from));
    a
}

/// How `docker compose` reaches environment `env`: the environment variables it is run with. The address is
/// `docker::cli::docker_host`, the one function every Docker program Freightdock runs takes it from ("Endpoints" 1, 5): the
/// endpoint bollard uses, the local Docker by its address, and an agent's tunnel or a TLS host as the unix socket
/// Freightdock offers for it (`docker/sockets.rs`), so `DOCKER_HOST=unix://<data>/run/env-<id>.sock`. Only `DOCKER_HOST`:
/// a TLS host's socket does the TLS, so Compose is handed no certificate. An agent that is not connected answers 503.
pub async fn docker_env(app: &App, env: i64) -> ApiResult<Vec<(&'static str, String)>> {
    Ok(vec![("DOCKER_HOST", crate::docker::cli::docker_host(app, env).await?)])
}

/// A Docker client configuration written for one compose job, with the saved registries' sign-ins, so the images
/// Compose pulls come from them signed in ("Stacks" 10). It lives in a folder of its own beside the stacks' folders
/// (never in a stack's, which a build could read), readable by Freightdock alone, and goes when this is dropped.
pub struct DockerConfig {
    dir: PathBuf,
}

impl DockerConfig {
    /// Writes `config` as `<dir>/config.json`, the folder made afresh and private.
    pub fn write(dir: PathBuf, config: &serde_json::Value) -> std::io::Result<Self> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let made = DockerConfig { dir };
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(made.dir.join("config.json"))?;
        f.write_all(config.to_string().as_bytes())?;
        Ok(made)
    }

    /// What Compose is run with to read it.
    pub fn env(&self) -> (&'static str, String) {
        ("DOCKER_CONFIG", self.dir.display().to_string())
    }
}

impl Drop for DockerConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The saved registries' sign-ins as a Docker configuration in `dir`, for a job that pulls; none when no registry with
/// a sign-in is saved, so Compose keeps the configuration it would have had. A registry whose sign-in fails (ECR that
/// will not give a token) is left out and said on `tx`: its pulls go unsigned, as they would on the command line.
pub async fn registry_config(app: &App, dir: PathBuf, tx: Option<&Sender<Line>>) -> Result<Option<DockerConfig>, String> {
    let mut auths = Vec::new();
    for host in crate::registries::hosts(app).await.map_err(|e| e.message)? {
        match crate::registries::auth_for(app, &host).await {
            Ok(Some(a)) => auths.push(a),
            Ok(None) => {}
            Err(e) => {
                if let Some(tx) = tx {
                    let _ = tx.send(Line { stream: Some(format!("{}; pulls from {host} go without signing in.\n", e.message)), ..Default::default() }).await;
                }
            }
        }
    }
    if auths.is_empty() {
        return Ok(None);
    }
    let config = crate::registries::docker_config(&auths);
    DockerConfig::write(dir, &config).map(Some).map_err(|e| format!("The registries' sign-in for Compose could not be written: {e}"))
}

/// Runs the binary with `args` in `cwd`, with only `env` and what a process needs (PATH, HOME, the time zone) in its
/// environment: Freightdock's own variables must not reach `${...}` in somebody's compose file. Each line it writes goes to
/// `tx` as it comes (when there is one, and somebody still reads). The answer is what it wrote to standard output, or the
/// reason it failed:
/// its last words on standard error, which is where Compose says what went wrong.
pub async fn run(bin: &Path, args: &[OsString], env: &[(&str, String)], cwd: &Path, tx: Option<&Sender<Line>>) -> Result<Vec<String>, String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).current_dir(cwd).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for k in ["PATH", "HOME", "TZ", "TMPDIR", "DOCKER_CONFIG"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let what = args.iter().find(|a| ["up", "stop", "start", "pull", "down", "config"].iter().any(|w| a.to_str() == Some(w))).and_then(|a| a.to_str()).unwrap_or("compose").to_string();
    let mut child = cmd.spawn().map_err(|e| format!("Docker Compose could not be started ({}): {e}", bin.display()))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let (ltx, mut lrx) = tokio::sync::mpsc::channel::<(bool, String)>(256);
    for (is_err, pipe) in [(false, out.map(|o| Box::new(o) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)), (true, err.map(|e| Box::new(e) as Box<dyn tokio::io::AsyncRead + Send + Unpin>))] {
        let Some(pipe) = pipe else { continue };
        let ltx = ltx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if ltx.send((is_err, l)).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(ltx);
    let mut all = Vec::new();
    let mut last_err = String::new();
    let read = async {
        while let Some((is_err, l)) = lrx.recv().await {
            if is_err && !l.trim().is_empty() {
                last_err = l.trim().to_string();
            }
            if let Some(tx) = tx {
                let _ = tx.send(Line { stream: Some(format!("{l}\n")), ..Default::default() }).await;
            }
            if !is_err {
                all.push(l);
            }
        }
        child.wait().await
    };
    let status = match tokio::time::timeout(LIMIT, read).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("Docker Compose stopped answering: {e}")),
        Err(_) => return Err(format!("docker compose {what} took longer than {} minutes and was stopped.", LIMIT.as_secs() / 60)),
    };
    if status.success() {
        Ok(all)
    } else if last_err.is_empty() {
        Err(format!("docker compose {what} failed ({status})."))
    } else {
        Err(last_err)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in for the Compose binary: it writes its arguments, a line a call, to `calls` beside it, and says them
    /// back; the Docker configuration it was given (its folder, then its `config.json`) goes to `configs`; a compose file
    /// with `broken` in it fails `config` the way Compose does.
    pub fn fake(dir: &Path) -> PathBuf {
        let bin = dir.join("fake-compose");
        let script = r#"#!/bin/sh
here=$(dirname "$0")
echo "$*" >> "$here/calls"
if [ -f "$DOCKER_CONFIG/config.json" ]; then { echo "$DOCKER_CONFIG"; cat "$DOCKER_CONFIG/config.json"; echo; } >> "$here/configs"; fi
case " $* " in *" --no-interpolate "*) echo "name: merged"; echo "services: {}"; exit 0;; esac
case " $* " in *" --format json "*) if [ -f "$here/config.json" ]; then cat "$here/config.json"; exit 0; fi; echo '{"name":"web","services":{"db":{"image":"postgres","environment":{"POSTGRES_USER":"postgres","POSTGRES_PASSWORD":"s3cret","EMPTY":null}},"app":{"image":"nginx"}}}'; exit 0;; esac
echo "DOCKER_HOST=$DOCKER_HOST FREIGHTDOCK_SECRET=${FREIGHTDOCK_SECRET:-}"
file=
while [ $# -gt 0 ]; do [ "$1" = "-f" ] && file=$2; shift; done
if [ -n "$file" ] && grep -q broken "$file"; then echo "services.web Additional property brokn is not allowed" >&2; exit 15; fi
echo "Container done" >&2
"#;
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn project<'a>(file: &'a Path, dir: &'a Path, env: Option<&'a Path>) -> Project<'a> {
        Project { name: "web", file, dir, env_file: env }
    }

    fn strs(a: Vec<OsString>) -> Vec<String> {
        a.into_iter().map(|s| s.into_string().unwrap()).collect()
    }

    /// Environments of every kind, in the test app's database: 1 local, 2 an agent, 3 a TLS host.
    async fn every_kind(app: &App) {
        app.db
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO environments (id, name, kind, url, created_at) VALUES (1, 'local', 'local', 'unix:///var/run/docker.sock', 0);
                     INSERT INTO environments (id, name, kind, url, created_at) VALUES (2, 'vm-1', 'agent', '', 0);
                     INSERT INTO environments (id, name, kind, url, created_at) VALUES (3, 'nas', 'tls', 'tcp://nas:2376', 0);",
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// A dial that never reaches a host: the socket is there, which is all Compose's address needs.
    fn nowhere() -> impl crate::docker::sockets::Dial {
        || -> crate::docker::sockets::DialFuture { Box::pin(async { Err(std::io::Error::other("no host in a test")) }) }
    }

    #[tokio::test]
    async fn compose_reaches_every_kind_of_environment_by_its_socket() {
        let app = crate::api::tests::app();
        every_kind(&app).await;
        let host = |e: Vec<(&'static str, String)>| {
            assert_eq!(e.iter().map(|(k, _)| *k).collect::<Vec<_>>(), ["DOCKER_HOST"], "Compose is told where Docker is, and nothing else: {e:?}");
            e[0].1.clone()
        };

        assert_eq!(host(docker_env(&app, 1).await.unwrap()), app.docker.url(), "the local Docker, as Freightdock reaches it");

        let err = docker_env(&app, 2).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE, "an agent that is not connected: {}", err.message);
        app.sockets.offer(2, None, nowhere()).await.unwrap();
        assert_eq!(host(docker_env(&app, 2).await.unwrap()), format!("unix://{}", app.sockets.path(2).display()), "an agent: its tunnel's socket");

        // A TLS host's socket does the TLS: Compose gets no certificate, so the key is never written out for it.
        app.sockets.offer(3, Some("nas:2376".into()), nowhere()).await.unwrap();
        assert_eq!(host(docker_env(&app, 3).await.unwrap()), format!("unix://{}", app.sockets.path(3).display()), "a TLS host: its socket");

        assert_eq!(docker_env(&app, 9).await.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn each_services_variables_come_out_of_composes_resolved_file_by_name() {
        let json = r#"{"name":"web","services":{
            "db":{"image":"postgres","environment":{"POSTGRES_USER":"postgres","POSTGRES_DB":"statsapp","EMPTY":null}},
            "app":{"image":"nginx"}}}"#;
        let got = environments(json).unwrap();
        assert_eq!(got, vec![
            ServiceEnv { service: "app".into(), environment: vec![] },
            ServiceEnv { service: "db".into(), environment: vec![("EMPTY".into(), String::new()), ("POSTGRES_DB".into(), "statsapp".into()), ("POSTGRES_USER".into(), "postgres".into())] },
        ], "services and their variables in name order; a variable with no value is empty");
        assert!(environments("not json").is_err());
    }

    #[test]
    fn the_resolved_file_is_asked_for_with_the_stacks_files_and_variables() {
        let (f, d, e) = (Path::new("/s/1/compose.yaml"), Path::new("/s/1"), Path::new("/s/1/.env"));
        let a = strs(args(&project(f, d, Some(e)), Job::Resolved));
        assert!(a.ends_with(&["config".to_string(), "--format".to_string(), "json".to_string()]), "{a:?}");
        assert!(a.windows(2).any(|w| w == ["--env-file", "/s/1/.env"]), "{a:?}");
    }

    #[test]
    fn project_names_are_what_compose_takes() {
        assert!(valid_project("web") && valid_project("my-app_2") && valid_project("9lives"));
        assert!(!valid_project("") && !valid_project("Web") && !valid_project("-x") && !valid_project("a b") && !valid_project("a.b"));
        assert!(!valid_project(&"a".repeat(64)));
    }

    #[test]
    fn a_variables_file_is_names_and_values() {
        assert!(check_env("").is_ok());
        assert!(check_env("# a comment\n\nTAG=1.2\nexport _X=a=b\nEMPTY=\n").is_ok());
        assert_eq!(check_env("A=1\nnot a line").unwrap_err(), "Line 2 of the variables has no = in it: write NAME=value.");
        assert!(check_env("1A=x").unwrap_err().contains("\"1A\" is not a variable name"));
        assert!(check_env("A-B=x").is_err());
    }

    #[test]
    fn up_reads_the_file_and_its_variables_and_the_rest_go_by_name() {
        let (f, d, e) = (Path::new("/s/1/compose.yaml"), Path::new("/s/1"), Path::new("/s/1/.env"));
        assert_eq!(
            strs(args(&project(f, d, Some(e)), Job::Up { pull: false })),
            ["--ansi", "never", "--progress", "plain", "-p", "web", "-f", "/s/1/compose.yaml", "--project-directory", "/s/1", "--env-file", "/s/1/.env", "up", "--detach", "--remove-orphans"]
        );
        assert_eq!(strs(args(&project(f, d, None), Job::Up { pull: true })).join(" "), "--ansi never --progress plain -p web -f /s/1/compose.yaml --project-directory /s/1 up --detach --remove-orphans --pull always");
        assert_eq!(strs(args(&project(f, d, Some(e)), Job::Stop)).join(" "), "--ansi never --progress plain -p web stop");
        assert_eq!(strs(args(&project(f, d, Some(e)), Job::Start)).join(" "), "--ansi never --progress plain -p web start");
        assert_eq!(strs(args(&project(f, d, Some(e)), Job::Down { volumes: true })).join(" "), "--ansi never --progress plain -p web down --remove-orphans --volumes");
        assert!(strs(args(&project(f, d, Some(e)), Job::Pull)).ends_with(&["pull".to_string()]));
        assert!(strs(args(&project(f, d, None), Job::Check)).ends_with(&["config".to_string(), "--quiet".to_string()]));
    }

    #[test]
    fn a_project_from_elsewhere_is_merged_into_one_file_from_where_it_was_started() {
        let a = strs(merge_args("blog", &["/srv/blog/compose.yaml".into(), "/srv/blog/override.yaml".into()], "/srv/blog", Some("/srv/blog/.env")));
        assert_eq!(
            a.join(" "),
            "--ansi never -p blog -f /srv/blog/compose.yaml -f /srv/blog/override.yaml --project-directory /srv/blog --env-file /srv/blog/.env config --no-interpolate --no-env-resolution"
        );
    }

    #[tokio::test]
    async fn the_binary_gets_the_docker_host_and_none_of_freightdock_s_variables() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake(dir.path());
        // SAFETY: no other test reads this variable.
        unsafe { std::env::set_var("FREIGHTDOCK_SECRET", "leaked") };
        let file = dir.path().join("compose.yaml");
        std::fs::write(&file, "services: {}\n").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let a = args(&project(&file, dir.path(), None), Job::Up { pull: false });
        let out = run(&bin, &a, &[("DOCKER_HOST", "tcp://127.0.0.1:2375".into())], dir.path(), Some(&tx)).await.unwrap();
        drop(tx);
        assert!(out.iter().any(|l| l == "DOCKER_HOST=tcp://127.0.0.1:2375 FREIGHTDOCK_SECRET="), "{out:?}");
        let mut streamed = Vec::new();
        while let Some(l) = rx.recv().await {
            streamed.push(l.stream.unwrap());
        }
        assert!(streamed.contains(&"Container done\n".to_string()), "standard error is streamed too: {streamed:?}");
    }

    #[tokio::test]
    async fn a_failure_is_compose_s_last_words() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake(dir.path());
        let file = dir.path().join("compose.yaml");
        std::fs::write(&file, "services: broken\n").unwrap();
        let a = args(&project(&file, dir.path(), None), Job::Check);
        let e = run(&bin, &a, &[], dir.path(), None).await.unwrap_err();
        assert_eq!(e, "services.web Additional property brokn is not allowed");
        let e = run(Path::new("/nowhere/compose"), &a, &[], dir.path(), None).await.unwrap_err();
        assert!(e.starts_with("Docker Compose could not be started"), "{e}");
    }

    #[test]
    fn a_compose_binary_that_is_not_there_is_a_503_that_says_so() {
        let e = Compose { bin: None }.bin().unwrap_err();
        assert_eq!(e.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(Compose::find(Some("/x/compose")).path() == Some(Path::new("/x/compose")));
    }
}
