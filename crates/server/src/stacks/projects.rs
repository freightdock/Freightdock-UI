//! Compose projects as Docker knows them: containers grouped by their `com.docker.compose.project` label, whoever started
//! them (Freightdock, `docker compose` on the command line, Portainer). A project with no stack of Freightdock's by its name is
//! shown as external, and can be adopted.

use std::collections::BTreeMap;

use bollard::models::ContainerSummary;
use serde::Serialize;

pub const PROJECT: &str = "com.docker.compose.project";
pub const SERVICE: &str = "com.docker.compose.service";
pub const WORKING_DIR: &str = "com.docker.compose.project.working_dir";
pub const CONFIG_FILES: &str = "com.docker.compose.project.config_files";
pub const ENV_FILE: &str = "com.docker.compose.project.environment_file";

/// One project: how many containers it has and how many run, its services, and where it was started from.
#[derive(Serialize, Debug, Default, PartialEq, Clone)]
pub struct Project {
    pub name: String,
    pub containers: usize,
    pub running: usize,
    pub services: Vec<String>,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
    pub env_file: Option<String>,
    /// Started by Portainer: its stacks live under `/data/compose/<id>` in its own volume.
    pub portainer: bool,
}

impl Project {
    /// `running` (all), `partial`, `stopped` (none), or `down` (no containers at all).
    pub fn state(&self) -> &'static str {
        state(self.containers, self.running)
    }
}

pub fn state(containers: usize, running: usize) -> &'static str {
    match (containers, running) {
        (0, _) => "down",
        (n, r) if r == n => "running",
        (_, 0) => "stopped",
        _ => "partial",
    }
}

/// Every project among `containers`, by name.
pub fn group(containers: &[ContainerSummary]) -> BTreeMap<String, Project> {
    let mut out: BTreeMap<String, Project> = BTreeMap::new();
    for c in containers {
        let Some(labels) = &c.labels else { continue };
        let Some(name) = labels.get(PROJECT).filter(|n| !n.is_empty()) else { continue };
        let p = out.entry(name.clone()).or_insert_with(|| Project { name: name.clone(), ..Default::default() });
        p.containers += 1;
        if c.state == Some(bollard::models::ContainerSummaryStateEnum::RUNNING) {
            p.running += 1;
        }
        if let Some(s) = labels.get(SERVICE)
            && !p.services.contains(s)
        {
            p.services.push(s.clone());
            p.services.sort();
        }
        if p.working_dir.is_none() {
            p.working_dir = labels.get(WORKING_DIR).cloned();
            p.portainer = p.working_dir.as_deref().is_some_and(|d| d.starts_with("/data/compose/"));
        }
        if p.config_files.is_empty()
            && let Some(f) = labels.get(CONFIG_FILES)
        {
            p.config_files = f.split(',').map(str::trim).filter(|f| !f.is_empty()).map(str::to_string).collect();
        }
        if p.env_file.is_none() {
            p.env_file = labels.get(ENV_FILE).filter(|f| !f.is_empty()).cloned();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ContainerSummaryStateEnum as S;
    use std::collections::HashMap;

    fn c(labels: &[(&str, &str)], state: S) -> ContainerSummary {
        ContainerSummary { labels: Some(labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>()), state: Some(state), ..Default::default() }
    }

    #[test]
    fn containers_group_into_their_projects() {
        let list = vec![
            c(&[(PROJECT, "web"), (SERVICE, "app"), (WORKING_DIR, "/srv/web"), (CONFIG_FILES, "/srv/web/compose.yaml,/srv/web/override.yaml")], S::RUNNING),
            c(&[(PROJECT, "web"), (SERVICE, "db")], S::EXITED),
            c(&[(PROJECT, "web"), (SERVICE, "app")], S::RUNNING),
            c(&[(PROJECT, "portainer-made"), (SERVICE, "x"), (WORKING_DIR, "/data/compose/12")], S::RUNNING),
            c(&[("maintainer", "nobody")], S::RUNNING),
            c(&[(PROJECT, "")], S::RUNNING),
        ];
        let g = group(&list);
        assert_eq!(g.keys().collect::<Vec<_>>(), ["portainer-made", "web"]);
        let web = &g["web"];
        assert_eq!((web.containers, web.running), (3, 2));
        assert_eq!(web.services, ["app", "db"]);
        assert_eq!(web.working_dir.as_deref(), Some("/srv/web"));
        assert_eq!(web.config_files, ["/srv/web/compose.yaml", "/srv/web/override.yaml"]);
        assert!(!web.portainer);
        assert_eq!(web.state(), "partial");
        assert!(g["portainer-made"].portainer, "Portainer keeps its stacks under /data/compose");
        assert_eq!(g["portainer-made"].state(), "running");
    }

    #[test]
    fn a_project_s_state_is_what_its_containers_do() {
        assert_eq!(state(0, 0), "down");
        assert_eq!(state(2, 2), "running");
        assert_eq!(state(2, 0), "stopped");
        assert_eq!(state(3, 1), "partial");
    }
}
