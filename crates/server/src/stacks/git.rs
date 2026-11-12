//! Git stacks: a repository, a branch (or tag), and the path of the compose file inside it. Freightdock keeps a shallow
//! clone in the stack's folder and asks the remote what its branch points at (`git ls-remote`) to see whether there is
//! anything new, so polling costs one small request, not a fetch.
//!
//! git runs with an emptied environment, no system or global configuration, no prompt, and only the https and http
//! transports ("Stacks" 9). Credentials are a user name and a token sent as an HTTP Basic header through git's
//! environment (`GIT_CONFIG_*`), so they are never written into the clone's configuration nor seen in the process list.

use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;

/// The transports git may use. Tests clone from a bare repository in a folder of their own.
const PROTOCOLS: &str = if cfg!(test) { "https:http:file" } else { "https:http" };

/// No git command may take longer than this; a remote that does not answer is a failed check, not a stuck stack.
const LIMIT: Duration = Duration::from_secs(300);

/// Where a Git stack comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remote {
    pub url: String,
    /// A branch or a tag; empty for the remote's default branch.
    pub reference: String,
    pub user: Option<String>,
    pub token: Option<String>,
}

/// An address Freightdock clones from: https or http, nothing a shell or git would read as an option.
pub fn valid_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && url::Url::parse(url).is_ok_and(|u| u.host_str().is_some_and(|h| !h.is_empty()))
}

/// An address the API takes for a Git stack: `valid_url`, and in tests a bare repository in a folder too.
pub fn allowed_url(url: &str) -> bool {
    valid_url(url) || (cfg!(test) && url.starts_with("file:///"))
}

/// Whether two repository addresses are on one origin (scheme, host and port): a saved token is good only for the
/// origin it was saved for.
pub fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default(),
        _ => false,
    }
}

/// A branch or tag name git takes, roughly `git check-ref-format`: no spaces, no `..`, no leading `-`, nothing special.
pub fn valid_ref(r: &str) -> bool {
    r.is_empty()
        || (r.len() <= 255
            && !r.starts_with('-')
            && !r.starts_with('/')
            && !r.ends_with('/')
            && !r.ends_with(".lock")
            && !r.contains("..")
            && !r.contains("@{")
            && !r.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c)))
}

/// The compose file's path inside the repository: relative, and never out of it.
pub fn valid_path(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty() && p.len() <= 1024 && path.components().all(|c| matches!(c, Component::Normal(_))) && !p.chars().any(char::is_control)
}

/// Whether a stack checked at `checked` (never, when `None`) every `interval` seconds is due at `now`.
pub fn due(now: i64, checked: Option<i64>, interval: i64) -> bool {
    interval > 0 && checked.is_none_or(|c| now - c >= interval)
}

/// The variables that hand git the credentials as a header for this one run, and keep everything else of git's own
/// configuration out.
fn env_for(r: &Remote) -> Vec<(String, String)> {
    let mut e = vec![
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_ALLOW_PROTOCOL".into(), PROTOCOLS.into()),
        ("GIT_ASKPASS".into(), "/bin/false".into()),
    ];
    // A link in the repository is checked out as a plain file holding its target's name, so nothing in a clone leads
    // out of it ("Stacks" 14).
    let mut config = vec![("core.symlinks".to_string(), "false".to_string())];
    if let Some(token) = r.token.as_deref().filter(|t| !t.is_empty()) {
        let user = r.user.as_deref().filter(|u| !u.is_empty()).unwrap_or("x-access-token");
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
        config.push(("http.extraHeader".into(), format!("Authorization: Basic {basic}")));
    }
    e.push(("GIT_CONFIG_COUNT".into(), config.len().to_string()));
    for (i, (k, v)) in config.into_iter().enumerate() {
        e.push((format!("GIT_CONFIG_KEY_{i}"), k));
        e.push((format!("GIT_CONFIG_VALUE_{i}"), v));
    }
    e
}

/// Runs git with `args`; its standard output, or its last words on standard error (with the token taken out, should a
/// remote ever say it back).
async fn git(args: &[&str], cwd: Option<&Path>, r: &Remote) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for k in ["PATH", "HOME", "TZ", "TMPDIR"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.envs(env_for(r));
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = match tokio::time::timeout(LIMIT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("git could not be started: {e}")),
        Err(_) => return Err(format!("git took longer than {} minutes and was stopped.", LIMIT.as_secs() / 60)),
    };
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let mut last = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("git failed").to_string();
    if let Some(t) = r.token.as_deref().filter(|t| !t.is_empty()) {
        last = last.replace(t, "***");
    }
    Err(last.trim_start_matches("fatal: ").to_string())
}

/// What the remote's branch (or tag, or default branch) points at now: a commit id.
pub async fn head(r: &Remote) -> Result<String, String> {
    let wanted = if r.reference.is_empty() { "HEAD".to_string() } else { r.reference.clone() };
    let out = git(&["ls-remote", "--", &r.url, &wanted], None, r).await?;
    // A branch and a tag of one name: the branch. An annotated tag: the commit it names (`^{}`).
    let lines: Vec<(&str, &str)> = out.lines().filter_map(|l| l.split_once('\t')).collect();
    let pick = |suffix: &str| lines.iter().find(|(_, n)| *n == suffix).map(|(id, _)| id.to_string());
    let found = if wanted == "HEAD" {
        pick("HEAD")
    } else {
        pick(&format!("refs/heads/{wanted}")).or_else(|| pick(&format!("refs/tags/{wanted}^{{}}"))).or_else(|| pick(&format!("refs/tags/{wanted}")))
    };
    found.ok_or_else(|| if wanted == "HEAD" { "The repository has no default branch.".to_string() } else { format!("The repository has no branch or tag \"{wanted}\".") })
}

/// Brings the clone in `dir` to what the remote's reference points at now (cloning it first when it is not there), and
/// answers the commit it is at. Files Compose or a container wrote in the folder are left alone; only the repository's
/// own files are put back as they are upstream.
pub async fn sync(dir: &Path, r: &Remote) -> Result<String, String> {
    if !dir.join(".git").exists() {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| format!("The old clone could not be cleared: {e}"))?;
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("The stack's folder could not be made: {e}"))?;
        }
        let target = dir.to_string_lossy().to_string();
        let mut args = vec!["clone", "--depth", "1", "--single-branch", "--no-tags"];
        if !r.reference.is_empty() {
            args.extend(["--branch", r.reference.as_str()]);
        }
        args.extend(["--", r.url.as_str(), target.as_str()]);
        git(&args, None, r).await?;
    } else {
        git(&["remote", "set-url", "origin", "--", &r.url], Some(dir), r).await?;
        let wanted = if r.reference.is_empty() { "HEAD" } else { r.reference.as_str() };
        git(&["fetch", "--depth", "1", "--no-tags", "origin", "--", wanted], Some(dir), r).await?;
        git(&["reset", "--hard", "FETCH_HEAD"], Some(dir), r).await?;
    }
    git(&["rev-parse", "HEAD"], Some(dir), r).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bare repository with one commit on `main`, and a work tree to push more from.
    pub fn upstream(dir: &Path) -> (String, std::path::PathBuf) {
        let bare = dir.join("upstream.git");
        let work = dir.join("work");
        let run = |args: &[&str], cwd: &Path| {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "qa")
                .env("GIT_AUTHOR_EMAIL", "qa@example.com")
                .env("GIT_COMMITTER_NAME", "qa")
                .env("GIT_COMMITTER_EMAIL", "qa@example.com")
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "--bare", "--initial-branch", "main", bare.to_str().unwrap()], dir);
        run(&["init", "--initial-branch", "main", work.to_str().unwrap()], dir);
        std::fs::create_dir_all(work.join("deploy")).unwrap();
        std::fs::write(work.join("deploy/compose.yaml"), "services:\n  web: {image: nginx:1}\n").unwrap();
        run(&["add", "."], &work);
        run(&["commit", "-m", "first"], &work);
        run(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
        run(&["push", "origin", "main"], &work);
        (format!("file://{}", bare.display()), work)
    }

    pub fn commit(work: &Path, text: &str) {
        std::fs::write(work.join("deploy/compose.yaml"), text).unwrap();
        for args in [&["commit", "-am", "next"][..], &["push", "origin", "main"][..]] {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(work)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "qa")
                .env("GIT_AUTHOR_EMAIL", "qa@example.com")
                .env("GIT_COMMITTER_NAME", "qa")
                .env("GIT_COMMITTER_EMAIL", "qa@example.com")
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }

    /// Changes the work tree with `change`, then commits everything and pushes it.
    pub fn push(work: &Path, change: impl FnOnce(&Path)) {
        change(work);
        for args in [&["add", "-A"][..], &["commit", "-m", "next"][..], &["push", "origin", "main"][..]] {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(work)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "qa")
                .env("GIT_AUTHOR_EMAIL", "qa@example.com")
                .env("GIT_COMMITTER_NAME", "qa")
                .env("GIT_COMMITTER_EMAIL", "qa@example.com")
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
    }

    #[test]
    fn addresses_refs_and_paths_are_held_to_what_is_safe() {
        assert!(valid_url("https://github.com/owner/repo.git") && valid_url("http://git.lan:3000/a/b"));
        assert!(!valid_url("file:///etc") && !valid_url("ext::sh -c id") && !valid_url("-uhttps://x") && !valid_url("https://") && !valid_url("https://a b/c"));
        assert!(valid_ref("") && valid_ref("main") && valid_ref("release/1.2") && valid_ref("v1.0.0"));
        assert!(!valid_ref("-x") && !valid_ref("a..b") && !valid_ref("a b") && !valid_ref("a:b") && !valid_ref("x.lock"));
        assert!(valid_path("docker-compose.yml") && valid_path("deploy/prod/compose.yaml"));
        assert!(!valid_path("") && !valid_path("../secret") && !valid_path("/etc/passwd") && !valid_path("a/../../b") && !valid_path("./a"));
    }

    #[test]
    fn an_origin_is_scheme_host_and_port() {
        assert!(same_origin("https://github.com/a/b.git", "https://github.com/c/d.git"));
        assert!(same_origin("https://github.com/a.git", "https://github.com:443/a.git"), "the known port is the same port");
        assert!(!same_origin("https://github.com/a.git", "https://evil.example/a.git"));
        assert!(!same_origin("https://github.com/a.git", "http://github.com/a.git"));
        assert!(!same_origin("https://git.example/a.git", "https://git.example:8443/a.git"));
        assert!(!same_origin("https://github.com/a.git", "not an address"));
    }

    #[test]
    fn a_stack_is_due_when_its_interval_has_passed() {
        assert!(!due(1000, None, 0), "polling off");
        assert!(due(1000, None, 300), "never checked");
        assert!(!due(1000, Some(800), 300));
        assert!(due(1100, Some(800), 300));
    }

    #[test]
    fn credentials_go_as_a_header_in_git_s_environment_and_nowhere_else() {
        let e = env_for(&Remote { token: Some("s3cret".into()), user: Some("me".into()), ..Default::default() });
        let config = |e: &[(String, String)], key: &str| {
            let k = e.iter().find(|(_, v)| v == key).map(|(k, _)| k.replace("KEY", "VALUE"))?;
            e.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        let header = config(&e, "http.extraHeader").unwrap();
        assert_eq!(header, format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode("me:s3cret")));
        assert!(e.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
        let none = env_for(&Remote::default());
        assert_eq!(config(&none, "http.extraHeader"), None, "no token, no header");
        assert_eq!(config(&none, "core.symlinks").as_deref(), Some("false"), "links come out as plain files, token or not");
        assert_eq!(config(&e, "core.symlinks").as_deref(), Some("false"));
    }

    #[tokio::test]
    async fn polling_sees_a_new_commit_and_a_sync_brings_it() {
        let dir = tempfile::tempdir().unwrap();
        let (url, work) = upstream(dir.path());
        let r = Remote { url, reference: "main".into(), ..Default::default() };
        let clone = dir.path().join("stack/repo");
        let first = sync(&clone, &r).await.unwrap();
        assert_eq!(head(&r).await.unwrap(), first, "nothing new yet");
        assert_eq!(std::fs::read_to_string(clone.join("deploy/compose.yaml")).unwrap(), "services:\n  web: {image: nginx:1}\n");
        std::fs::write(clone.join("deploy/data.txt"), "written by a container").unwrap();

        commit(&work, "services:\n  web: {image: nginx:2}\n");
        let now = head(&r).await.unwrap();
        assert_ne!(now, first, "the poller sees the new commit");
        assert_eq!(sync(&clone, &r).await.unwrap(), now);
        assert_eq!(std::fs::read_to_string(clone.join("deploy/compose.yaml")).unwrap(), "services:\n  web: {image: nginx:2}\n");
        assert!(clone.join("deploy/data.txt").exists(), "what was written in the folder stays");

        let default = Remote { reference: String::new(), ..r.clone() };
        assert_eq!(head(&default).await.unwrap(), now, "no branch named: the default branch");
    }

    #[tokio::test]
    async fn a_branch_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (url, _) = upstream(dir.path());
        let e = head(&Remote { url: url.clone(), reference: "nope".into(), ..Default::default() }).await.unwrap_err();
        assert_eq!(e, "The repository has no branch or tag \"nope\".");
        let e = sync(&dir.path().join("x"), &Remote { url: format!("{url}-missing"), reference: "main".into(), ..Default::default() }).await.unwrap_err();
        assert!(!e.is_empty() && !e.starts_with("fatal"), "{e}");
    }
}
