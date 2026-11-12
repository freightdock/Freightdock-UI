//! Docker's own command line, for what only it does (`docker stack deploy` reads a Compose file into services). How the
//! CLI reaches an environment is one function, `docker_host`: the same endpoint bollard speaks to, the local Docker's
//! address or the unix socket of a remote host ("Endpoints" 1).

use std::process::Stdio;
use std::time::Duration;

use axum::http::StatusCode;
use tokio::io::AsyncWriteExt;

use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// `DOCKER_HOST` for environment `env`: `unix:///...` or `tcp://...`. A TLS host's socket is made here if it is not yet.
/// Every Docker program Freightdock runs takes its address from here: this CLI and Docker Compose (`stacks::compose`).
pub async fn docker_host(app: &App, env: i64) -> ApiResult<String> {
    Ok(super::endpoint(app, env).await?.url().to_string())
}

/// The `docker` binary: `FREIGHTDOCK_DOCKER_CLI`, else `docker` on the path (the image has Docker's static CLI there).
pub fn binary() -> String {
    std::env::var("FREIGHTDOCK_DOCKER_CLI").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "docker".into())
}

/// What of Freightdock's own environment the CLI is given: what finds programs, a home and a temporary folder, the time
/// zone, and where Docker's client configuration is. As Compose's ("Stacks" 3).
const KEPT: [&str; 5] = ["PATH", "HOME", "TZ", "TMPDIR", "DOCKER_CONFIG"];

/// The CLI pointed at `host` and at nothing else, with an emptied environment: a context, TLS settings or another host
/// from Freightdock's own environment must not send it elsewhere, and none of Freightdock's variables may fill a `${...}`
/// in a stack's file ("Swarm" 8).
pub fn command(cli: &str, host: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(cli);
    c.env_clear();
    for k in KEPT {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    c.env("DOCKER_HOST", host)
        .env("DOCKER_CLI_HINTS", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}

pub struct Ran {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cli args...` against `host` with `stdin` fed in, in folder `cwd` when one is given, within `within`.
pub async fn run(cli: &str, host: &str, args: &[&str], stdin: Option<&str>, cwd: Option<&std::path::Path>, within: Duration) -> ApiResult<Ran> {
    let mut c = command(cli, host);
    c.args(args);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    let mut child = c.spawn().map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, format!("Docker's command line ({cli}) could not be started: {e}. Set FREIGHTDOCK_DOCKER_CLI to where it is.")))?;
    let mut input = child.stdin.take();
    if let (Some(text), Some(pipe)) = (stdin, input.as_mut()) {
        pipe.write_all(text.as_bytes()).await.map_err(|e| ApiError::from(anyhow::anyhow!("writing to {cli}: {e}")))?;
    }
    drop(input);
    let out = tokio::time::timeout(within, child.wait_with_output())
        .await
        .map_err(|_| ApiError::new(StatusCode::GATEWAY_TIMEOUT, format!("{cli} {} took longer than {} s.", args.first().copied().unwrap_or(""), within.as_secs())))?
        .map_err(|e| ApiError::from(anyhow::anyhow!("waiting for {cli}: {e}")))?;
    Ok(Ran { ok: out.status.success(), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr: String::from_utf8_lossy(&out.stderr).into_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_goes_to_the_environment_s_host_and_nowhere_else() {
        let c = command("docker", "unix:///data/run/env-2.sock");
        let envs: Vec<(String, Option<String>)> = c.as_std().get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        assert!(envs.contains(&("DOCKER_HOST".into(), Some("unix:///data/run/env-2.sock".into()))));
        assert!(!envs.iter().any(|(k, _)| k == "DOCKER_CONTEXT" || k == "DOCKER_TLS_VERIFY"), "a context would win over DOCKER_HOST");
    }

    #[tokio::test]
    async fn a_run_feeds_its_input_and_keeps_both_outputs_and_whether_it_worked() {
        let r = run("sh", "unix:///x.sock", &["-c", "cat; echo \"$DOCKER_HOST\" >&2; exit 3"], Some("compose"), None, Duration::from_secs(10)).await.unwrap();
        assert!(!r.ok);
        assert_eq!(r.stdout, "compose");
        assert_eq!(r.stderr.trim(), "unix:///x.sock");
    }

    #[tokio::test]
    async fn the_cli_gets_none_of_freightdock_s_own_environment() {
        // The CLI fills a `${...}` in a stack's file from its environment: Freightdock's own variables must never reach it
        // ("Stacks" 3, "Swarm" 8). The test runner's (CARGO_*) stand in for Freightdock's.
        assert!(std::env::vars().any(|(k, _)| k.starts_with("CARGO")), "the runner sets variables to look for");
        let r = run("sh", "unix:///x.sock", &["-c", "env"], None, None, Duration::from_secs(10)).await.unwrap();
        let names: Vec<&str> = r.stdout.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
        let shell = ["PWD", "SHLVL", "_", "OLDPWD"];
        let unexpected: Vec<&&str> = names.iter().filter(|k| !KEPT.contains(k) && !shell.contains(k) && !["DOCKER_HOST", "DOCKER_CLI_HINTS"].contains(k)).collect();
        assert!(unexpected.is_empty(), "{unexpected:?}");
        assert!(names.contains(&"DOCKER_HOST"));
    }

    #[tokio::test]
    async fn a_cli_that_is_not_there_says_so() {
        let e = run("/nowhere/docker", "unix:///x.sock", &["stack", "ls"], None, None, Duration::from_secs(5)).await.err().unwrap();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(e.message.contains("FREIGHTDOCK_DOCKER_CLI"), "{}", e.message);
    }
}
