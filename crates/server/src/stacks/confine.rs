//! What keeps a stack to its own folder: every file Compose or Freightdock reads for a stack must lie inside
//! `<data>/stacks/<id>/`, followed through every link ("Stacks" 14, 15).

use std::path::{Component, Path, PathBuf};

/// `path` made whole without touching the disk past what exists: the longest part of it that exists is resolved
/// through its links, and the rest is added on, `..` taken off as it comes. `None` when that cannot be told.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    let base = loop {
        if existing.symlink_metadata().is_ok() {
            break std::fs::canonicalize(&existing).ok()?;
        }
        rest.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?.to_path_buf();
        if existing.as_os_str().is_empty() {
            existing = PathBuf::from(".");
        }
    };
    let mut out = base;
    for part in rest.into_iter().rev() {
        match Path::new(&part).components().next() {
            Some(Component::ParentDir) => {
                out.pop();
            }
            Some(Component::Normal(_)) => out.push(part),
            _ => {}
        }
    }
    Some(out)
}

/// Whether `path` (relative to `base` when it is not absolute) is in `root` once every link is followed. A path that
/// cannot be told is not.
pub fn inside(root: &Path, base: &Path, path: &Path) -> bool {
    let full = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    match (std::fs::canonicalize(root), resolve(&full)) {
        (Ok(r), Some(p)) => p.starts_with(r),
        _ => false,
    }
}

/// A file's text when it is in `root`, followed through every link; empty otherwise, or when it cannot be read.
pub fn read(root: &Path, path: &Path) -> String {
    if inside(root, root, path) { std::fs::read_to_string(path).unwrap_or_default() } else { String::new() }
}

/// Every file the compose file `file` makes Compose read on Freightdock's server lies in `root`: its `env_file`s,
/// `label_file`s, the files it `include`s or `extends` (and theirs in turn), a build's context, Dockerfile, extra
/// contexts, SSH keys and local caches, and the `file` of its configs and secrets ("Stacks" 15). Relative paths are
/// read from `project_dir`, as Compose reads them. The reason, in words, for the first that does not.
pub fn references(root: &Path, file: &Path, project_dir: &Path) -> Result<(), String> {
    let mut seen = Vec::new();
    Walk { root, seen: &mut seen }.file(file, project_dir, 0)
}

use yaml_serde::Value;

/// How deep includes and extends may go: far past any real project, short of a loop that never ends.
const DEPTH: usize = 16;

struct Walk<'a> {
    root: &'a Path,
    /// The files already looked at, so a loop ends.
    seen: &'a mut Vec<PathBuf>,
}

/// A value with its YAML tags (`!reset`, `!override`) taken off.
fn bare(v: &Value) -> &Value {
    match v {
        Value::Tagged(t) => bare(&t.value),
        v => v,
    }
}

/// One path or a list of them, as Compose writes `env_file` and the like: a string, or a list of strings or of
/// mappings with `key` in them.
fn paths<'v>(v: &'v Value, key: &str) -> Vec<&'v Value> {
    match bare(v) {
        Value::Sequence(s) => s
            .iter()
            .filter_map(|x| match bare(x) {
                Value::Mapping(_) => bare(x).get(key),
                _ => Some(x),
            })
            .collect(),
        Value::Mapping(_) => bare(v).get(key).into_iter().collect(),
        Value::Null => Vec::new(),
        _ => vec![v],
    }
}

/// A list of strings, or a mapping's values (`additional_contexts`, `ssh`), as `key=value` strings.
fn pairs(v: &Value) -> Vec<String> {
    match bare(v) {
        Value::Sequence(s) => s.iter().filter_map(|x| text(x).ok()).collect(),
        Value::Mapping(m) => m.iter().filter_map(|(k, x)| Some(format!("{}={}", text(k).ok()?, text(x).ok()?))).collect(),
        v => text(v).ok().into_iter().collect(),
    }
}

fn text(v: &Value) -> Result<String, String> {
    match bare(v) {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("A file's path in the compose file is not text.".into()),
    }
}

/// An address a build reads from somewhere else, not a folder on this server.
fn remote(s: &str) -> bool {
    s.contains("://") || s.starts_with("git@") || s.starts_with("github.com/")
}

impl Walk<'_> {
    /// A path as written in a compose file, read from `base`: no variables (what they would say is not known until
    /// Compose fills them), no `~`, and inside the stack's folder.
    fn path(&self, v: &Value, base: &Path) -> Result<PathBuf, String> {
        let p = text(v)?;
        if p.contains('$') {
            return Err(format!("\"{p}\": a file's path in a stack may not use variables, so Freightdock can tell that it is in the stack's own folder."));
        }
        if p.starts_with('~') {
            return Err(format!("\"{p}\": a file's path in a stack is in the stack's own folder, not a home folder."));
        }
        if !inside(self.root, base, Path::new(&p)) {
            return Err(format!("\"{p}\" is outside the stack's own folder: a stack may name only files in its folder."));
        }
        Ok(base.join(&p))
    }

    /// A compose file, its project folder `dir`, and the files it names in turn.
    fn file(&mut self, file: &Path, dir: &Path, depth: usize) -> Result<(), String> {
        if !inside(self.root, dir, file) || !inside(self.root, self.root, dir) {
            return Err(format!("{} is outside the stack's own folder: a stack may name only files in its folder.", file.display()));
        }
        let Ok(canon) = std::fs::canonicalize(file) else { return Ok(()) };
        if self.seen.contains(&canon) {
            return Ok(());
        }
        if depth > DEPTH {
            return Err("The compose files include and extend each other too deeply.".into());
        }
        self.seen.push(canon);
        let Ok(text) = std::fs::read_to_string(file) else { return Ok(()) };
        let mut doc: Value = yaml_serde::from_str(&text).map_err(|e| format!("Freightdock could not read {} to check the files it names: {e}", file.display()))?;
        doc.apply_merge().map_err(|e| format!("Freightdock could not read {} to check the files it names: {e}", file.display()))?;

        let includes = match doc.get("include").map(bare) {
            Some(Value::Sequence(s)) => s.iter().collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(v) => vec![v],
        };
        for inc in includes {
            let inc = bare(inc);
            let (files, env, project) = match bare(inc) {
                Value::Mapping(_) => (inc.get("path").map(|p| paths(p, "")).unwrap_or_default(), inc.get("env_file").map(|e| paths(e, "")).unwrap_or_default(), inc.get("project_directory")),
                _ => (vec![inc], Vec::new(), None),
            };
            let files = files.into_iter().map(|f| self.path(f, dir)).collect::<Result<Vec<_>, _>>()?;
            let project = match project {
                Some(p) => self.path(p, dir)?,
                None => files.first().and_then(|f| f.parent()).map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf()),
            };
            for e in env {
                // Read from either folder, whichever Compose takes: both stay in the stack's.
                self.path(e, dir)?;
                self.path(e, &project)?;
            }
            for f in files {
                self.file(&f, &project, depth + 1)?;
            }
        }

        if let Some(Value::Mapping(services)) = doc.get("services").map(bare) {
            for (_, svc) in services {
                let svc = bare(svc);
                for key in ["env_file", "label_file"] {
                    for p in svc.get(key).map(|v| paths(v, "path")).unwrap_or_default() {
                        self.path(p, dir)?;
                    }
                }
                if let Some(ext) = svc.get("extends").map(bare).filter(|e| matches!(e, Value::Mapping(_)))
                    && let Some(f) = ext.get("file")
                {
                    let f = self.path(f, dir)?;
                    let parent = f.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf());
                    self.file(&f, &parent, depth + 1)?;
                }
                if let Some(b) = svc.get("build") {
                    self.build(b, dir)?;
                }
            }
        }

        for top in ["configs", "secrets"] {
            if let Some(Value::Mapping(m)) = doc.get(top).map(bare) {
                for (_, item) in m {
                    let item = bare(item);
                    if let Some(f) = item.get("file") {
                        self.path(f, dir)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A service's `build`: its context (unless it is an address), Dockerfile, extra contexts, SSH keys and local caches.
    fn build(&self, b: &Value, dir: &Path) -> Result<(), String> {
        let b = bare(b);
        let (context, b) = match bare(b) {
            Value::Mapping(_) => (b.get("context"), Some(b)),
            _ => (Some(b), None),
        };
        let context = match context {
            Some(c) if remote(&text(c)?) => None,
            Some(c) => Some(self.path(c, dir)?),
            None => Some(dir.to_path_buf()),
        };
        let Some(b) = b else { return Ok(()) };
        if let (Some(ctx), Some(f)) = (&context, b.get("dockerfile")) {
            self.path(f, ctx)?;
        }
        for c in b.get("additional_contexts").map(pairs).unwrap_or_default() {
            let v = c.split_once('=').map_or(c.as_str(), |(_, v)| v);
            let v = v.strip_prefix("oci-layout://").unwrap_or(v);
            if !(remote(v) || v.starts_with("service:") || v.starts_with("target:")) {
                self.path(&Value::String(v.to_string()), dir)?;
            }
        }
        for k in b.get("ssh").map(pairs).unwrap_or_default() {
            if let Some((_, v)) = k.split_once('=') {
                self.path(&Value::String(v.to_string()), dir)?;
            }
        }
        for key in ["cache_from", "cache_to"] {
            for c in b.get(key).map(pairs).unwrap_or_default() {
                if !c.split(',').any(|kv| kv.trim() == "type=local") {
                    continue;
                }
                for kv in c.split(',') {
                    if let Some(v) = kv.trim().strip_prefix("src=").or_else(|| kv.trim().strip_prefix("dest=")) {
                        self.path(&Value::String(v.to_string()), dir)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stack folder (`<tmp>/stacks/1`) with `compose` as its compose file, beside stack 2's folder with its `.env`.
    fn stack(compose: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("stacks/1");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(d.path().join("stacks/2")).unwrap();
        std::fs::write(d.path().join("stacks/2/.env"), "SECRET=theirs\n").unwrap();
        std::fs::write(d.path().join("stacks/2/compose.yaml"), "services:\n  db:\n    image: postgres\n    env_file: .env\n").unwrap();
        std::fs::write(root.join("compose.yaml"), compose).unwrap();
        (d, root)
    }

    fn check(compose: &str) -> Result<(), String> {
        let (_d, root) = stack(compose);
        references(&root, &root.join("compose.yaml"), &root)
    }

    #[test]
    fn a_compose_file_may_name_files_in_its_own_folder() {
        let ok = [
            "services:\n  app:\n    image: nginx\n",
            "services:\n  app:\n    image: nginx\n    env_file: .env\n",
            "services:\n  app:\n    image: nginx\n    env_file:\n      - ./a.env\n      - path: sub/b.env\n        required: false\n",
            "services:\n  app:\n    build: .\n",
            "services:\n  app:\n    build:\n      context: ./app\n      dockerfile: Dockerfile.prod\n      additional_contexts:\n        base: docker-image://alpine\n        other: service:db\n",
            "services:\n  app:\n    build: https://github.com/docker/buildx.git#master\n",
            "services:\n  app:\n    build:\n      context: git@github.com:me/app.git\n      dockerfile: ../../../anything\n",
            "services:\n  app:\n    image: nginx\n    volumes:\n      - /srv/data:/data\n      - ../shared:/shared\n",
            "configs:\n  c:\n    file: ./c.conf\nsecrets:\n  s:\n    file: s.txt\n  e:\n    environment: TOKEN\n",
            "x-base: &base\n  env_file: .env\nservices:\n  app:\n    <<: *base\n    image: nginx\n",
            "services:\n  app:\n    image: nginx\n    environment:\n      HOME_DIR: ${HOME_DIR:-/srv}\n",
        ];
        for c in ok {
            assert_eq!(check(c), Ok(()), "{c}");
        }
    }

    #[test]
    fn a_compose_file_may_not_make_compose_read_outside_its_folder() {
        let out = [
            "services:\n  app:\n    image: nginx\n    env_file: ../2/.env\n",
            "services:\n  app:\n    image: nginx\n    env_file:\n      - .env\n      - /etc/hostname\n",
            "services:\n  app:\n    image: nginx\n    env_file:\n      - path: ../2/.env\n",
            "services:\n  app:\n    image: nginx\n    env_file: !override ../2/.env\n",
            "services:\n  app:\n    image: nginx\n    env_file: ${WHERE}\n",
            "services:\n  app:\n    image: nginx\n    env_file: ~/x.env\n",
            "services:\n  app:\n    image: nginx\n    label_file: ../2/.env\n",
            "include:\n  - ../2/compose.yaml\n",
            "include:\n  - path: [../2/compose.yaml]\n",
            "include:\n  - path: ./mine.yaml\n    env_file: ../2/.env\n",
            "include:\n  - path: ./mine.yaml\n    project_directory: ..\n",
            "services:\n  app:\n    extends:\n      file: ../2/compose.yaml\n      service: db\n",
            "services:\n  app:\n    build: ..\n",
            "services:\n  app:\n    build:\n      context: ../..\n",
            "services:\n  app:\n    build:\n      context: .\n      dockerfile: ../2/.env\n",
            "services:\n  app:\n    build:\n      context: .\n      additional_contexts:\n        data: ../..\n",
            "services:\n  app:\n    build:\n      context: .\n      additional_contexts:\n        - data=/\n",
            "services:\n  app:\n    build:\n      context: .\n      ssh:\n        - key=/root/.ssh/id_ed25519\n",
            "services:\n  app:\n    build:\n      context: .\n      cache_from:\n        - type=local,src=/var/lib\n",
            "configs:\n  c:\n    file: ../2/.env\n",
            "secrets:\n  s:\n    file: /etc/shadow\n",
            "x-base: &base\n  env_file: ../2/.env\nservices:\n  app:\n    <<: *base\n    image: nginx\n",
            "services:\n  app: !reset\n    image: nginx\n    env_file: ../2/.env\n",
            "services:\n  app:\n    build: !override\n      context: ..\n",
        ];
        for c in out {
            assert!(check(c).is_err(), "{c}");
        }
    }

    #[test]
    fn what_an_included_or_extended_file_names_is_held_to_the_folder_too() {
        let (_d, root) = stack("include:\n  - sub/more.yaml\nservices:\n  app:\n    extends:\n      file: base.yaml\n      service: b\n");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/more.yaml"), "services:\n  more:\n    image: nginx\n    env_file: ../.env\n").unwrap();
        std::fs::write(root.join("base.yaml"), "services:\n  b:\n    image: nginx\n").unwrap();
        assert_eq!(references(&root, &root.join("compose.yaml"), &root), Ok(()), "../.env from sub/ is the stack's own");
        std::fs::write(root.join("sub/more.yaml"), "services:\n  more:\n    image: nginx\n    env_file: ../../2/.env\n").unwrap();
        assert!(references(&root, &root.join("compose.yaml"), &root).is_err(), "an included file's own reference out");
        std::fs::write(root.join("sub/more.yaml"), "services: {}\n").unwrap();
        std::fs::write(root.join("base.yaml"), "include: [../2/compose.yaml]\n").unwrap();
        assert!(references(&root, &root.join("compose.yaml"), &root).is_err(), "an extended file's own include out");
        std::fs::write(root.join("base.yaml"), "include: [compose.yaml]\n").unwrap();
        assert_eq!(references(&root, &root.join("compose.yaml"), &root), Ok(()), "a loop ends");
    }

    #[test]
    fn a_path_is_inside_only_once_every_link_and_dot_dot_is_followed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("stacks/1");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(d.path().join("stacks/2")).unwrap();
        std::fs::write(d.path().join("stacks/2/.env"), "X=1\n").unwrap();
        std::os::unix::fs::symlink(d.path().join("stacks/2/.env"), root.join("link.env")).unwrap();
        std::os::unix::fs::symlink(d.path().join("stacks/2"), root.join("other")).unwrap();
        std::os::unix::fs::symlink(root.join("sub"), root.join("mine")).unwrap();

        assert!(inside(&root, &root, Path::new("compose.yaml")), "a file not yet there, in the folder");
        assert!(inside(&root, &root, Path::new("./sub/../sub/a.env")));
        assert!(inside(&root, &root, Path::new("mine/a.env")), "a link that stays in the folder");
        assert!(inside(&root, &root.join("sub"), Path::new("../x")));
        assert!(inside(&root, &root, &root.join("sub/a")), "an absolute path in the folder");
        assert!(!inside(&root, &root, Path::new("../2/.env")));
        assert!(!inside(&root, &root, Path::new("sub/../../2/.env")));
        assert!(!inside(&root, &root, Path::new("nothing/../../2/.env")), "through a folder that is not there");
        assert!(!inside(&root, &root, Path::new("/etc/passwd")));
        assert!(!inside(&root, &root, Path::new("link.env")), "a link out");
        assert!(!inside(&root, &root, Path::new("other/.env")), "a linked folder out");
        assert!(!inside(&root, &root, Path::new("..")));
        assert_eq!(read(&root, &root.join("link.env")), "");
    }
}
