//! What an import from Portainer would do, worked out without touching anything: which of Freightdock's environments each
//! of Portainer's is ("Portainer" 4), and for each stack whether it can come, where, and what to know about it
//! ("Portainer" 5, 6), and which team each of Portainer's teams becomes, with which members ("Portainer" 10). Pure, so
//! the rules are tested on recorded answers.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use super::client::{EnvVar, Membership, Stack, Team, User};

/// One of Freightdock's environments, as far as matching goes: its Docker engine's ID and host name (when it answered),
/// and the addresses it is known by (a TLS host's, where an agent last dialled from).
#[derive(Clone, Debug, Default)]
pub struct ShipEnv {
    pub id: i64,
    pub name: String,
    pub engine: Option<String>,
    pub hostname: Option<String>,
    pub addresses: Vec<String>,
}

/// One of Portainer's environments, as far as matching goes: its name, its address, and its Docker engine's ID and
/// host name as Portainer's Docker proxy gave them.
#[derive(Clone, Debug, Default)]
pub struct PortEnv {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub engine: Option<String>,
    pub hostname: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Match {
    pub env: i64,
    pub by: &'static str,
}

/// The host in an address such as `tcp://192.0.2.5:9001` or `https://docker.example:2376`; none for a socket.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(scheme, rest)| if scheme == "unix" || scheme == "npipe" { "" } else { rest });
    let hostport = rest.split('/').next().unwrap_or("");
    let host = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => hostport,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_lowercase();
    (!host.is_empty()).then_some(host)
}

fn same(a: &Option<String>, b: &str) -> bool {
    a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// Each of Portainer's environments matched to one of Freightdock's, or none. By the Docker engine's ID first: it is the
/// same host whatever either calls it. A cloned machine keeps its engine's ID, so when two of Portainer's share one, or
/// two of Freightdock's do, the host name has to agree as well. Then by name, then by address ("Portainer" 4).
pub fn map_all(ports: &[PortEnv], envs: &[ShipEnv]) -> Vec<Option<Match>> {
    let shared = |engine: &str| ports.iter().filter(|p| p.engine.as_deref() == Some(engine)).count() > 1;
    ports
        .iter()
        .map(|p| {
            if let Some(engine) = p.engine.as_deref().filter(|e| !e.is_empty()) {
                let by_engine: Vec<&ShipEnv> = envs.iter().filter(|e| e.engine.as_deref() == Some(engine)).collect();
                if by_engine.len() == 1 && !shared(engine) {
                    return Some(Match { env: by_engine[0].id, by: "engine ID" });
                }
                let by_host: Vec<&&ShipEnv> = by_engine.iter().filter(|e| p.hostname.as_deref().is_some_and(|h| same(&e.hostname, h))).collect();
                if by_host.len() == 1 {
                    return Some(Match { env: by_host[0].id, by: "engine ID and host name" });
                }
            }
            let by_name: Vec<&ShipEnv> = envs.iter().filter(|e| e.name.eq_ignore_ascii_case(&p.name)).collect();
            if by_name.len() == 1 {
                return Some(Match { env: by_name[0].id, by: "name" });
            }
            let host = host_of(&p.url)?;
            let by_address: Vec<&ShipEnv> = envs.iter().filter(|e| e.addresses.iter().any(|a| host_of(a).as_deref() == Some(host.as_str()) || a.eq_ignore_ascii_case(&host))).collect();
            (by_address.len() == 1).then(|| Match { env: by_address[0].id, by: "address" })
        })
        .collect()
}

/// A variable name Compose takes.
fn var_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// A stack's variables as a `.env`, written as Portainer writes its `stack.env` (`name=value`, as it is), so Compose reads
/// them as it did under Portainer. A value over several lines is put in double quotes with its line breaks as `\n` (and
/// `\`, `"` and `$` escaped), as one line could not hold it otherwise. Names Compose would not take are left out, and
/// answered.
pub fn env_text(vars: &[EnvVar]) -> (String, Vec<String>) {
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

/// A Git reference as Freightdock takes it: `refs/heads/main` is the branch `main`, `refs/tags/v1` the tag `v1`.
pub fn reference(r: &str) -> String {
    r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/tags/")).unwrap_or(r).to_string()
}

/// Portainer's polling interval (a Go duration: `5m`, `1h30m`, `90s`) as Freightdock's minutes, rounded up and held to 1 to
/// 1440; 0 (off) when there is none.
pub fn poll_minutes(interval: &str) -> i64 {
    let (mut secs, mut num) = (0f64, String::new());
    let mut chars = interval.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let n: f64 = num.parse().unwrap_or(0.0);
        num.clear();
        secs += match c {
            'h' => n * 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                n / 1000.0
            }
            'm' => n * 60.0,
            's' => n,
            _ => 0.0,
        };
    }
    if secs <= 0.0 {
        return 0;
    }
    ((secs / 60.0).ceil() as i64).clamp(1, 24 * 60)
}

/// Freightdock's kind of registry for Portainer's type: 1 Quay, 4 GitLab, 6 Docker Hub, 7 ECR, 8 GitHub; Azure (2), ProGet
/// (5) and any other (3) are registries like any other.
pub fn registry_kind(t: i64) -> &'static str {
    match t {
        1 => "quay",
        4 => "gitlab",
        6 => "dockerhub",
        7 => "ecr",
        8 => "ghcr",
        _ => "custom",
    }
}

/// What one of Freightdock's environments already has: its stacks by name, and its Compose projects with their
/// containers (how many, how many running) and where they were started.
#[derive(Clone, Debug, Default)]
pub struct EnvSide {
    pub stacks: HashMap<String, i64>,
    pub projects: HashMap<String, (usize, usize, Option<String>)>,
    /// Why its Docker could not be asked, when it could not: then nothing is known of what runs there.
    pub error: Option<String>,
}

/// A stack's line in the preview.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StackPlan {
    pub id: i64,
    pub name: String,
    pub endpoint_id: i64,
    /// `compose`, `git`, `swarm` or `kubernetes`.
    pub kind: &'static str,
    pub active: bool,
    pub vars: usize,
    /// The Freightdock environment it goes to.
    pub env: Option<i64>,
    /// Whether it can be imported; `reason` says why not.
    pub importable: bool,
    /// Ticked to begin with.
    pub checked: bool,
    pub reason: Option<String>,
    pub notes: Vec<String>,
    /// The Freightdock stack an earlier import made of it.
    pub imported: Option<i64>,
    /// Containers of its project in the environment it goes to, and how many run.
    pub containers: usize,
    pub running: usize,
    /// Where its project was started, from its containers' labels.
    pub working_dir: Option<String>,
    /// A Git stack whose repository Portainer signs in to: Portainer does not give the token out.
    pub needs_token: bool,
}

/// Every stack's line: `mapping` is where each of Portainer's environments goes, `known` the environments Portainer
/// still has, `sides` what each of Freightdock's has, `imported` the Freightdock stack made of each Portainer stack before.
pub fn stacks(stacks: &[Stack], known: &[i64], mapping: &HashMap<i64, i64>, sides: &HashMap<i64, EnvSide>, imported: &HashMap<i64, i64>) -> Vec<StackPlan> {
    // Two of Portainer's stacks by one name going to one environment: the active one, else the newer, is ticked.
    let mut first: BTreeMap<(i64, String), &Stack> = BTreeMap::new();
    for s in stacks.iter().filter(|s| s.kind == 2) {
        if let Some(&env) = mapping.get(&s.endpoint_id) {
            let k = (env, s.name.clone());
            let better = |a: &Stack, b: &Stack| (a.status == 1, a.update_date, -a.id) > (b.status == 1, b.update_date, -b.id);
            if first.get(&k).is_none_or(|b| better(s, b)) {
                first.insert(k, s);
            }
        }
    }
    let mut out: Vec<StackPlan> = stacks
        .iter()
        .map(|s| {
            let env = mapping.get(&s.endpoint_id).copied();
            let side = env.and_then(|e| sides.get(&e));
            let git = s.git_config.as_ref().filter(|g| !g.url.is_empty());
            let kind = match s.kind {
                1 => "swarm",
                3 => "kubernetes",
                _ if git.is_some() => "git",
                _ => "compose",
            };
            let mut p = StackPlan {
                id: s.id,
                name: s.name.clone(),
                endpoint_id: s.endpoint_id,
                kind,
                active: s.status == 1,
                vars: s.env.len(),
                env,
                importable: false,
                checked: false,
                reason: None,
                notes: Vec::new(),
                imported: None,
                containers: 0,
                running: 0,
                working_dir: None,
                needs_token: git.and_then(|g| g.authentication.as_ref()).is_some_and(|a| !a.username.is_empty()),
            };
            if let Some((c, r, wd)) = side.and_then(|sd| sd.projects.get(&s.name)) {
                (p.containers, p.running, p.working_dir) = (*c, *r, wd.clone());
            }
            let freightdock_stack = side.and_then(|sd| sd.stacks.get(&s.name)).copied();
            let mut why = |m: String| p.reason = Some(m);
            if let Some(&id) = imported.get(&s.id).filter(|&&id| freightdock_stack == Some(id)) {
                p.imported = Some(id);
                why("Imported already.".into());
            } else if s.kind == 1 {
                why("Not imported yet: Portainer's Swarm stacks are not brought over yet.".into());
            } else if s.kind == 3 {
                why("Kubernetes is not something Freightdock manages.".into());
            } else if !known.contains(&s.endpoint_id) {
                why(format!("Its environment ({}) is no longer in Portainer.", s.endpoint_id));
            } else if env.is_none() {
                why("Its environment is not matched to one of Freightdock's.".into());
            } else if !s.additional_files.is_empty() {
                why(format!("It has more compose files ({}), and Portainer's API gives out only the first.", s.additional_files.join(", ")));
            } else if freightdock_stack.is_some() {
                why(format!("Freightdock has a stack named {} there already.", s.name));
            } else {
                p.importable = true;
                p.checked = first.get(&(env.unwrap_or(0), s.name.clone())).is_some_and(|b| b.id == s.id);
                if !p.checked {
                    p.notes.push(format!("Portainer has another stack named {} for the same environment; that one is ticked, and only one can be imported.", s.name));
                }
            }
            if p.importable {
                p.notes.push(match (p.containers, p.running) {
                    _ if side.is_some_and(|sd| sd.error.is_some()) => format!("Freightdock could not ask that environment's Docker what runs there ({}).", side.and_then(|sd| sd.error.as_deref()).unwrap_or_default()),
                    (0, _) if p.active => "Active in Portainer, but none of its containers are there.".into(),
                    (0, _) => "Nothing of it is running (inactive in Portainer).".into(),
                    (c, r) => format!("Its project is there ({r} of {c} running): adopted in place, its containers kept as they are."),
                });
                if let Some(g) = git {
                    let mut said = format!("From Git: {} at {}, {}.", g.url, if g.reference_name.is_empty() { "the default branch".into() } else { reference(&g.reference_name) }, if g.config_file_path.is_empty() { "docker-compose.yml" } else { &g.config_file_path });
                    if let Some(m) = s.auto_update.as_ref().map(|a| poll_minutes(&a.interval)).filter(|m| *m > 0) {
                        said.push_str(&format!(" Checked for changes every {m} minutes."));
                    }
                    p.notes.push(said);
                    if p.needs_token {
                        p.notes.push("Portainer signs in to the repository and does not give the token out: give it here, unless the repository is public.".into());
                    }
                    if g.tls_skip_verify {
                        p.notes.push("Portainer skips the repository's certificate check; Freightdock does not.".into());
                    }
                }
                if !s.webhook.is_empty() || s.auto_update.as_ref().is_some_and(|a| !a.webhook.is_empty()) {
                    p.notes.push("Portainer's webhook address does not carry over: make a new one on the stack.".into());
                }
            }
            p
        })
        .collect();
    out.sort_by(|a, b| (a.endpoint_id, &a.name, a.id).cmp(&(b.endpoint_id, &b.name, b.id)));
    out
}

/// One of Freightdock's teams, as far as an import goes: its name and who is in it.
#[derive(Clone, Debug, Default)]
pub struct ShipTeam {
    pub id: i64,
    pub name: String,
    pub members: BTreeSet<i64>,
}

/// A member of Portainer's team in the preview: the Freightdock user of the same name, or none (left out), and whether
/// they are in the Freightdock team already.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MemberPlan {
    pub name: String,
    pub freightdock_user_id: Option<i64>,
    pub already: bool,
}

/// A team's line in the preview.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TeamPlan {
    pub id: i64,
    pub name: String,
    /// The Freightdock team it is: the one an earlier import made, or the one of its name; none when it would be made.
    pub freightdock_id: Option<i64>,
    /// `new` (made), `exists` (Freightdock has a team of its name) or `imported` (an earlier import made it).
    pub outcome: &'static str,
    pub importable: bool,
    pub reason: Option<String>,
    pub members: Vec<MemberPlan>,
}

/// Each of Portainer's teams as a Freightdock team: the one an earlier import made (`imported`, Portainer's team id to
/// Freightdock's) while it is still there, else Freightdock's of the same name (case-blind, as `teams.name` is), else a new
/// one; and each of its members as the Freightdock user of the same name (case-blind, as `users.name` is), or none.
pub fn teams(teams: &[Team], users: &[User], memberships: &[Membership], ship_teams: &[ShipTeam], ship_users: &[(i64, String)], imported: &HashMap<i64, i64>) -> Vec<TeamPlan> {
    let mut out: Vec<TeamPlan> = teams
        .iter()
        .map(|t| {
            let name = t.name.trim().to_string();
            let by_record = imported.get(&t.id).and_then(|s| ship_teams.iter().find(|st| st.id == *s));
            let by_name = || ship_teams.iter().find(|st| st.name.eq_ignore_ascii_case(&name));
            let (outcome, there) = match (by_record, by_name()) {
                (Some(st), _) => ("imported", Some(st)),
                (None, Some(st)) => ("exists", Some(st)),
                (None, None) => ("new", None),
            };
            let fits = !name.is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control);
            let reason = (there.is_none() && !fits).then(|| "Freightdock's team names are 1 to 64 characters.".to_string());
            let mut seen = BTreeSet::new();
            let mut members: Vec<MemberPlan> = memberships
                .iter()
                .filter(|m| m.team_id == t.id && seen.insert(m.user_id))
                .filter_map(|m| users.iter().find(|u| u.id == m.user_id))
                .map(|u| {
                    let sid = ship_users.iter().find(|(_, n)| n.eq_ignore_ascii_case(&u.username)).map(|(id, _)| *id);
                    MemberPlan { name: u.username.clone(), freightdock_user_id: sid, already: sid.is_some_and(|s| there.is_some_and(|st| st.members.contains(&s))) }
                })
                .collect();
            members.sort_by_key(|m| m.name.to_lowercase());
            TeamPlan { id: t.id, name: t.name.clone(), freightdock_id: there.map(|st| st.id), outcome, importable: reason.is_none(), reason, members }
        })
        .collect();
    out.sort_by_key(|t| (t.name.to_lowercase(), t.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portainer::client::{GitAuth, GitConfig};

    fn port(id: i64, name: &str, url: &str, engine: Option<&str>, host: Option<&str>) -> PortEnv {
        PortEnv { id, name: name.into(), url: url.into(), engine: engine.map(Into::into), hostname: host.map(Into::into) }
    }
    fn ship(id: i64, name: &str, engine: Option<&str>, host: Option<&str>, addr: &[&str]) -> ShipEnv {
        ShipEnv { id, name: name.into(), engine: engine.map(Into::into), hostname: host.map(Into::into), addresses: addr.iter().map(|a| a.to_string()).collect() }
    }

    #[test]
    fn hosts_come_out_of_addresses() {
        assert_eq!(host_of("tcp://192.0.2.5:9001").as_deref(), Some("192.0.2.5"));
        assert_eq!(host_of("https://Docker.Example:2376/").as_deref(), Some("docker.example"));
        assert_eq!(host_of("192.0.2.9").as_deref(), Some("192.0.2.9"));
        assert_eq!(host_of("tcp://[fd00::1]:9001").as_deref(), Some("fd00::1"));
        assert_eq!(host_of("unix:///var/run/docker.sock"), None);
    }

    #[test]
    fn environments_match_by_engine_id_then_name_then_address() {
        let ports = [
            port(2, "alpha", "unix:///var/run/docker.sock", Some("E1"), Some("host-a")),
            port(3, "proxy", "tcp://192.0.2.6:9001", Some("E3"), Some("host-b")),
            port(6, "Nas", "tcp://192.0.2.3:9001", None, None),
            port(7, "apps", "tcp://192.0.2.9:9001", None, None),
            port(8, "spare", "tcp://192.0.2.10:9001", None, None),
        ];
        let envs = [
            ship(1, "local", Some("E1"), Some("host-a"), &[]),
            ship(4, "Traefik box", Some("E3"), Some("host-b"), &[]),
            ship(5, "nas", None, None, &[]),
            ship(9, "arr", None, None, &["192.0.2.9"]),
        ];
        let m = map_all(&ports, &envs);
        assert_eq!(m[0], Some(Match { env: 1, by: "engine ID" }));
        assert_eq!(m[1], Some(Match { env: 4, by: "engine ID" }), "whatever either calls it");
        assert_eq!(m[2], Some(Match { env: 5, by: "name" }), "case-blind");
        assert_eq!(m[3], Some(Match { env: 9, by: "address" }), "where the agent dials from");
        assert_eq!(m[4], None);
    }

    #[test]
    fn a_cloned_machine_s_engine_id_needs_its_host_name_too() {
        // Recorded: two of the real Portainer's hosts are one VM cloned, and report the same engine ID.
        let ports = [port(2, "alpha", "unix:///var/run/docker.sock", Some("E1"), Some("host-a")), port(5, "media", "tcp://192.0.2.5:9001", Some("E1"), Some("host-c"))];
        let only_one = [ship(1, "local", Some("E1"), Some("host-a"), &[])];
        let m = map_all(&ports, &only_one);
        assert_eq!(m[0], Some(Match { env: 1, by: "engine ID and host name" }));
        assert_eq!(m[1], None, "the other clone is not this host");
        let both = [ship(1, "local", Some("E1"), Some("host-a"), &[]), ship(3, "Media", Some("E1"), Some("host-c"), &["192.0.2.5"])];
        let m = map_all(&ports, &both);
        assert_eq!(m[1], Some(Match { env: 3, by: "engine ID and host name" }));
    }

    #[test]
    fn variables_are_written_as_portainer_writes_them() {
        let v = |n: &str, val: &str| EnvVar { name: n.into(), value: val.into() };
        let (text, left) = env_text(&[v("TOKEN", "a b#c $x"), v("EMPTY", ""), v("bad-name", "1"), v("PEM", "line1\nline2")]);
        assert_eq!(text, "TOKEN=a b#c $x\nEMPTY=\nPEM=\"line1\\nline2\"\n");
        assert_eq!(left, ["bad-name"]);
        assert!(crate::stacks::compose::check_env(&text).is_ok(), "one line a variable, as Freightdock's own check wants");
    }

    #[test]
    fn git_settings_come_over() {
        assert_eq!(reference("refs/heads/master"), "master");
        assert_eq!(reference("refs/tags/v1.2"), "v1.2");
        assert_eq!(reference(""), "");
        assert_eq!(poll_minutes("5m"), 5);
        assert_eq!(poll_minutes("1h30m"), 90);
        assert_eq!(poll_minutes("90s"), 2);
        assert_eq!(poll_minutes("30s"), 1);
        assert_eq!(poll_minutes("500ms"), 1);
        assert_eq!(poll_minutes("48h"), 1440);
        assert_eq!(poll_minutes(""), 0);
        assert_eq!(registry_kind(6), "dockerhub");
        assert_eq!(registry_kind(5), "custom");
    }

    fn recorded() -> Vec<Stack> {
        serde_json::from_str(include_str!("fixtures/stacks.json")).unwrap()
    }

    #[test]
    fn the_plan_says_what_comes_and_why_not() {
        let mut all = recorded();
        all.push(Stack { id: 900, name: "swarmy".into(), kind: 1, endpoint_id: 2, ..Default::default() });
        all.push(Stack { id: 901, name: "two-files".into(), kind: 2, endpoint_id: 2, status: 1, additional_files: vec!["extra.yml".into()], ..Default::default() });
        let known = [2, 3, 5, 6, 7, 8, 9];
        let mapping = HashMap::from([(2, 1), (3, 4), (5, 3)]);
        let mut one = EnvSide::default();
        one.stacks.insert("arr".into(), 77);
        one.projects.insert("webapp".into(), (2, 2, Some("/data/compose/259/8d57".into())));
        let sides = HashMap::from([(1, one), (4, EnvSide::default()), (3, EnvSide { error: Some("The agent for media is not connected.".into()), ..Default::default() })]);
        let p = stacks(&all, &known, &mapping, &sides, &HashMap::new());
        let by = |id: i64| p.iter().find(|s| s.id == id).unwrap();

        assert_eq!((by(3).importable, by(3).reason.as_deref()), (false, Some("Freightdock has a stack named arr there already.")));
        let sw = by(259);
        assert_eq!((sw.kind, sw.importable, sw.checked, sw.env, sw.needs_token), ("git", true, true, Some(1), true));
        assert_eq!((sw.containers, sw.running, sw.working_dir.as_deref()), (2, 2, Some("/data/compose/259/8d57")));
        assert!(sw.notes.iter().any(|n| n == "From Git: http://git.example.test/owner/webapp at master, compose.yaml. Checked for changes every 5 minutes."), "{:?}", sw.notes);
        assert!(sw.notes.iter().any(|n| n.starts_with("Portainer's webhook address does not carry over")));
        assert_eq!(by(24).reason.as_deref(), Some("Its environment (4) is no longer in Portainer."));
        assert_eq!(by(252).kind, "compose");
        assert_eq!(by(900).reason.as_deref(), Some("Not imported yet: Portainer's Swarm stacks are not brought over yet."));
        assert!(by(901).reason.as_deref().unwrap().contains("extra.yml"));
        assert!(by(4).importable && by(4).notes[0] == "Nothing of it is running (inactive in Portainer).", "{:?}", by(4));
        // Recorded: the real Portainer has two active stacks named statsapp on one environment.
        let (a, b) = (by(237), by(238));
        assert!(a.importable && b.importable);
        assert_eq!(a.checked as u8 + b.checked as u8, 1, "only one of the two is ticked");
        assert!(a.notes.iter().any(|n| n == "Freightdock could not ask that environment's Docker what runs there (The agent for media is not connected.)."), "{:?}", a.notes);
        // Environment 6 (nas) is not matched.
        assert!(p.iter().filter(|s| s.endpoint_id == 6).all(|s| !s.importable));
    }

    #[test]
    fn what_was_imported_before_is_not_imported_again() {
        let all = recorded();
        let mapping = HashMap::from([(3, 4)]);
        let mut side = EnvSide::default();
        side.stacks.insert("nginx".into(), 12);
        let sides = HashMap::from([(4, side)]);
        let p = stacks(&all, &[3], &mapping, &sides, &HashMap::from([(147, 12)]));
        let n = p.iter().find(|s| s.id == 147).unwrap();
        assert_eq!((n.importable, n.imported, n.reason.as_deref()), (false, Some(12), Some("Imported already.")));
        // Imported once, then removed from Freightdock: it can come again.
        let p = stacks(&all, &[3], &mapping, &HashMap::new(), &HashMap::from([(147, 12)]));
        assert!(p.iter().find(|s| s.id == 147).unwrap().importable);
    }

    #[test]
    fn a_git_stack_without_a_user_needs_no_token() {
        let s = Stack {
            id: 1,
            name: "pub".into(),
            kind: 2,
            endpoint_id: 2,
            git_config: Some(GitConfig { url: "https://example.test/r.git".into(), authentication: Some(GitAuth { username: String::new() }), ..Default::default() }),
            ..Default::default()
        };
        let p = stacks(&[s], &[2], &HashMap::from([(2, 1)]), &HashMap::new(), &HashMap::new());
        assert_eq!((p[0].kind, p[0].needs_token), ("git", false));
    }

    #[test]
    fn teams_are_matched_by_the_record_then_by_name_and_members_by_name() {
        let users = [
            User { id: 1, username: "admin".into(), role: 1 },
            User { id: 2, username: "Viewer".into(), role: 2 },
            User { id: 3, username: "ghost".into(), role: 2 },
        ];
        let pteams = [
            Team { id: 1, name: "readers".into() },
            Team { id: 2, name: "Ops".into() },
            Team { id: 3, name: "renamed".into() },
            Team { id: 4, name: " ".into() },
        ];
        let m = |u: i64, t: i64| Membership { user_id: u, team_id: t, role: 2 };
        let memberships = [m(2, 1), m(3, 1), m(1, 2), m(2, 2), m(9, 2), m(1, 3)];
        let ship_users = [(10, "admin".to_string()), (11, "viewer".to_string())];
        let ship_teams = [
            ShipTeam { id: 20, name: "OPS".into(), members: BTreeSet::from([10]) },
            ShipTeam { id: 21, name: "Readers here".into(), members: BTreeSet::new() },
        ];
        // Team 3 was imported before as Freightdock's 21, renamed since; team 1 was imported as 99, removed since.
        let imported = HashMap::from([(3, 21), (1, 99)]);
        let p = teams(&pteams, &users, &memberships, &ship_teams, &ship_users, &imported);
        let by = |id: i64| p.iter().find(|t| t.id == id).unwrap();

        let r = by(1);
        assert_eq!((r.outcome, r.freightdock_id, r.importable), ("new", None, true), "removed since: made again");
        assert_eq!(r.members, [MemberPlan { name: "ghost".into(), freightdock_user_id: None, already: false }, MemberPlan { name: "Viewer".into(), freightdock_user_id: Some(11), already: false }]);
        let o = by(2);
        assert_eq!((o.outcome, o.freightdock_id), ("exists", Some(20)), "by name, case-blind");
        assert_eq!(o.members, [MemberPlan { name: "admin".into(), freightdock_user_id: Some(10), already: true }, MemberPlan { name: "Viewer".into(), freightdock_user_id: Some(11), already: false }], "a membership of a user Portainer does not list is left out");
        assert_eq!((by(3).outcome, by(3).freightdock_id), ("imported", Some(21)), "by the record, whatever it is called now");
        assert_eq!((by(4).importable, by(4).reason.as_deref()), (false, Some("Freightdock's team names are 1 to 64 characters.")));
        assert_eq!(p.iter().map(|t| t.id).collect::<Vec<_>>(), [4, 2, 1, 3], "by name");
    }
}
