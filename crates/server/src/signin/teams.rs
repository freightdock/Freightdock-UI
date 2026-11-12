//! Teams from a provider's groups, at every sign-in through it (Portainer's automatic team membership, "SSO" 11). Each
//! rule maps a group to a team: by its name exactly, or by a regular expression between slashes whose captures the
//! team's name may use (`/^dev-(.+)$/` to `$1`). A person is put in every team their groups map to and taken out of
//! the ones this provider put them in that they no longer map to; a membership an administrator made is never taken
//! away by a sign-in.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::{now, rusqlite::{self, OptionalExtension, params}};
use crate::error::ApiError;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TeamSync {
    pub enabled: bool,
    /// A team a rule names that is not there yet is made; otherwise that rule waits until an administrator makes it.
    pub create_missing: bool,
    pub rules: Vec<TeamRule>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct TeamRule {
    /// A group's name, or `/regex/`.
    pub group: String,
    /// The team's name; with a regular expression, `$1` or `${name}` are its captures.
    pub team: String,
}

/// The longest a compiled expression may be: a rule is the administrator's, but it is matched at every sign-in.
const REGEX_LIMIT: usize = 1 << 20;

enum Matcher {
    Exact(String),
    Regex(regex::Regex),
}

fn matcher(group: &str) -> Result<Matcher, String> {
    let g = group.trim();
    match g.strip_prefix('/').and_then(|r| r.strip_suffix('/')) {
        Some(re) if g.len() >= 2 => regex::RegexBuilder::new(re).size_limit(REGEX_LIMIT).build().map(Matcher::Regex).map_err(|e| format!("{g} is not a regular expression Freightdock reads: {e}")),
        _ => Ok(Matcher::Exact(g.to_string())),
    }
}

/// A team's name as `users.rs` holds it: 1 to 64 characters, no control characters.
fn team_name(n: &str) -> Option<String> {
    let n = n.trim();
    (!n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control)).then(|| n.to_string())
}

/// Every rule must read before it is kept.
pub fn check(sync: &TeamSync) -> Result<(), ApiError> {
    for r in &sync.rules {
        if r.group.trim().is_empty() || r.team.trim().is_empty() {
            return Err(ApiError::bad("Each team rule needs a group and a team.").field("teams"));
        }
        matcher(&r.group).map_err(|e| ApiError::bad(e).field("teams"))?;
    }
    Ok(())
}

/// The teams `groups` map to, by name without regard to case (as teams are named), each once.
pub fn wanted(groups: &[String], sync: &TeamSync) -> Vec<String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for r in &sync.rules {
        let Ok(m) = matcher(&r.group) else { continue };
        for g in groups {
            let name = match &m {
                Matcher::Exact(e) if e == g => Some(r.team.clone()),
                Matcher::Exact(_) => None,
                Matcher::Regex(re) => re.captures(g).map(|c| {
                    let mut s = String::new();
                    c.expand(&r.team, &mut s);
                    s
                }),
            };
            if let Some(n) = name.as_deref().and_then(team_name) {
                out.entry(n.to_lowercase()).or_insert(n);
            }
        }
    }
    out.into_values().collect()
}

/// What a sign-in changed of a person's teams.
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    pub joined: Vec<String>,
    pub left: Vec<String>,
    /// Made for this sign-in (create_missing).
    pub made: Vec<String>,
    /// Named by a rule but not there, and not made.
    pub missing: Vec<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.joined.is_empty() && self.left.is_empty() && self.made.is_empty() && self.missing.is_empty()
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = [("joined", &self.joined), ("left", &self.left), ("made", &self.made), ("no such team", &self.missing)]
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k} {}", v.join(", ")))
            .collect();
        f.write_str(&parts.join("; "))
    }
}

/// Puts user `uid` in the teams `wanted`, as provider `provider`'s doing, and out of the ones that provider put them in
/// before and `wanted` no longer names. One transaction.
pub fn apply(c: &mut rusqlite::Connection, uid: i64, provider: &str, wanted: &[String], create_missing: bool) -> Result<Change> {
    let tx = c.transaction()?;
    let mut ch = Change::default();
    let mut keep = Vec::new();
    for name in wanted {
        let id: Option<(i64, String)> = tx.query_row("SELECT id, name FROM teams WHERE name = ?1", [name], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let (tid, tname) = match id {
            Some(t) => t,
            None if create_missing => {
                tx.execute("INSERT INTO teams (name, created_at) VALUES (?1, ?2)", params![name, now()])?;
                ch.made.push(name.clone());
                (tx.last_insert_rowid(), name.clone())
            }
            None => {
                ch.missing.push(name.clone());
                continue;
            }
        };
        keep.push(tid);
        // A membership already there, by hand or by this sign-in before, stays as it is.
        if tx.execute("INSERT OR IGNORE INTO team_members (team_id, user_id, synced_by) VALUES (?1, ?2, ?3)", params![tid, uid, provider])? == 1 {
            ch.joined.push(tname);
        }
    }
    let mut st = tx.prepare("SELECT t.id, t.name FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = ?1 AND m.synced_by = ?2")?;
    let had: Vec<(i64, String)> = st.query_map(params![uid, provider], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    drop(st);
    for (tid, tname) in had {
        if !keep.contains(&tid) {
            tx.execute("DELETE FROM team_members WHERE team_id = ?1 AND user_id = ?2", params![tid, uid])?;
            ch.left.push(tname);
        }
    }
    tx.commit()?;
    Ok(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    fn sync(rules: &[(&str, &str)]) -> TeamSync {
        TeamSync { enabled: true, create_missing: false, rules: rules.iter().map(|(g, t)| TeamRule { group: g.to_string(), team: t.to_string() }).collect() }
    }

    #[test]
    fn rules_map_groups_to_teams_by_name_or_by_expression() {
        let s = sync(&[("devs", "Developers"), ("/^team-(.+)$/", "Team $1"), ("/^(?P<org>[a-z]+)/ops$/", "${org} operators")]);
        assert_eq!(wanted(&g(&["devs", "team-blue", "acme/ops", "other"]), &s), ["acme operators", "Developers", "Team blue"]);
        assert!(wanted(&g(&["Devs"]), &s).is_empty(), "a group's name is matched exactly");
        assert_eq!(wanted(&g(&["team-a", "team-A"]), &sync(&[("/^team-(.+)$/", "$1")])), ["a"], "teams are named without case, so once");
        assert!(wanted(&g(&["team-"]), &sync(&[("/^team-(.*)$/", "$1")])).is_empty(), "an empty name is no team");
        assert!(wanted(&g(&["x"]), &sync(&[("/x/", &"y".repeat(65))])).is_empty(), "nor is one too long");
    }

    #[test]
    fn a_rule_that_does_not_read_is_refused_when_it_is_kept() {
        assert!(check(&sync(&[("devs", "Developers"), ("/^a(.+)$/", "$1")])).is_ok());
        assert_eq!(check(&sync(&[("/(unclosed/", "x")])).unwrap_err().field, Some("teams"));
        assert_eq!(check(&sync(&[("devs", " ")])).unwrap_err().field, Some("teams"));
        assert!(check(&sync(&[("/a{1000}{1000}{1000}/", "x")])).is_err(), "an expression too big to match at every sign-in");
        assert!(matches!(matcher("/").unwrap(), Matcher::Exact(_)), "a lone slash is a name");
    }

    #[tokio::test]
    async fn a_sign_in_puts_a_person_in_their_teams_and_takes_them_out_of_only_the_ones_it_put_them_in() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute_batch(
                "INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'ola', '!', 'user', 0);
                 INSERT INTO teams (id, name, created_at) VALUES (1, 'Developers', 0), (2, 'Operators', 0), (3, 'Hand-picked', 0);
                 INSERT INTO team_members (team_id, user_id) VALUES (3, 1);",
            )?;
            let ch = apply(c, 1, "authentik", &g(&["developers", "Operators", "Hand-picked", "Nowhere"]), false)?;
            assert_eq!(ch.joined, ["Developers", "Operators"], "found without regard to case");
            assert_eq!(ch.missing, ["Nowhere"]);
            assert!(ch.left.is_empty() && ch.made.is_empty());
            assert_eq!(ch.to_string(), "joined Developers, Operators; no such team Nowhere");

            let ch = apply(c, 1, "authentik", &g(&["Developers"]), false)?;
            assert_eq!(ch.left, ["Operators"], "out of what the provider put them in and no longer names");
            let teams: Vec<String> = c.prepare("SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = 1 ORDER BY t.name")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            assert_eq!(teams, ["Developers", "Hand-picked"], "what an administrator gave stays");

            let ch = apply(c, 1, "github", &[], false)?;
            assert!(ch.is_empty(), "another provider takes nothing it did not give");
            let ch = apply(c, 1, "authentik", &g(&["New team"]), true)?;
            assert_eq!((ch.made.as_slice(), ch.joined.as_slice(), ch.left.as_slice()), (&g(&["New team"])[..], &g(&["New team"])[..], &g(&["Developers"])[..]));
            let again = apply(c, 1, "authentik", &g(&["New team"]), true)?;
            assert!(again.is_empty(), "nothing changes the second time");
            Ok(())
        })
        .await
        .unwrap();
    }
}
