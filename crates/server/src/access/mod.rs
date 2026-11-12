//! Who may do what, where: roles as Portainer Business has them, given per environment or per environment group to a
//! user or a team, and one table of role against action that every route is checked by ("Access" in DECISIONS.md).
//!
//! An Administrator (`users.role = 'admin'`) may do everything everywhere. Everybody else has, on each environment, the
//! one role [`resolve`] works out from their grants, or none, and then does not see that environment at all.

#[cfg(test)]
pub mod api_tests;
pub mod ownership;
pub mod routes;
pub mod security;

use serde::{Deserialize, Serialize};

/// A role on an environment, lowest first: `Ord` is "higher", for picking the highest of several team grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Reads what is theirs, their team's or public.
    ReadOnly,
    /// Makes things, and manages what is theirs, their team's or public.
    Standard,
    /// Reads everything in the environment; no console.
    Helpdesk,
    /// Starts, stops and opens consoles on everything; makes and removes nothing.
    Operator,
    /// Everything in the environment.
    EnvAdmin,
}

impl Role {
    pub const ALL: [Role; 5] = [Role::EnvAdmin, Role::Operator, Role::Helpdesk, Role::Standard, Role::ReadOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::EnvAdmin => "env_admin",
            Role::Operator => "operator",
            Role::Helpdesk => "helpdesk",
            Role::Standard => "standard",
            Role::ReadOnly => "read_only",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// What a request does in an environment. Every route under `/api/envs/{env}` is one of these (`routes::RULES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Look: lists, inspect, logs, stats, events, files.
    Read,
    /// Start, stop, restart, kill, pause, unpause a container; stop and start a stack.
    Operate,
    /// A console, an attach, a command run in a container.
    Console,
    /// Make something new: a container, a stack, a volume, a network; pull or build an image.
    Create,
    /// Change or remove a container or a stack: edit, recreate, rename, redeploy, remove, its webhook, its access.
    Manage,
    /// What reaches past one person's things: prune, remove images, volumes and networks, Swarm, adopting a project
    /// started elsewhere.
    Administer,
}

impl Action {
    pub const ALL: [Action; 6] = [Action::Read, Action::Operate, Action::Console, Action::Create, Action::Manage, Action::Administer];

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Operate => "operate",
            Action::Console => "console",
            Action::Create => "create",
            Action::Manage => "manage",
            Action::Administer => "administer",
        }
    }

    /// Whether a read-only API token may do it.
    pub fn reads(self) -> bool {
        self == Action::Read
    }
}

/// How far a role may do an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allow {
    No,
    /// Only on the containers and stacks the person can see: public, their team's, their own ("Access" 4). Anything
    /// else the action reaches (an image, a volume) is not owned, and is allowed.
    Visible,
    /// On everything in the environment.
    All,
}

use Allow::{All, No, Visible};

/// The permission table: each role's answer for each action, in `Action::ALL`'s order (read, operate, console, create,
/// manage, administer). This is the one place it is written; DECISIONS "Access" 2 has it as a table for people.
pub const TABLE: [(Role, [Allow; 6]); 5] = [
    (Role::EnvAdmin, [All, All, All, All, All, All]),
    (Role::Operator, [All, All, All, No, No, No]),
    (Role::Helpdesk, [All, No, No, No, No, No]),
    (Role::Standard, [Visible, Visible, Visible, All, Visible, No]),
    (Role::ReadOnly, [Visible, No, No, No, No, No]),
];

pub fn allows(role: Role, action: Action) -> Allow {
    let row = TABLE.iter().find(|(r, _)| *r == role).map(|(_, a)| a).expect("every role has a row");
    row[Action::ALL.iter().position(|a| *a == action).expect("every action has a column")]
}

/// Who a grant is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    User,
    Team,
}

/// What a grant is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    Environment,
    Group,
}

/// One grant that applies to a person on one environment: given to them or to a team they are in, on the environment
/// or on its group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub subject: Subject,
    pub on: On,
    pub role: Role,
}

/// A person's role on an environment from the grants that reach it ("Access" 3), the first of these that has any:
/// 1. their own grant on the environment;
/// 2. their own grant on its group;
/// 3. the highest of their teams' grants on the environment;
/// 4. the highest of their teams' grants on its group.
///
/// None: no access, and the environment is not shown to them.
pub fn resolve(grants: &[Grant]) -> Option<Role> {
    let best = |s: Subject, o: On| grants.iter().filter(|g| g.subject == s && g.on == o).map(|g| g.role).max();
    best(Subject::User, On::Environment)
        .or_else(|| best(Subject::User, On::Group))
        .or_else(|| best(Subject::Team, On::Environment))
        .or_else(|| best(Subject::Team, On::Group))
}

/// What one person has on one environment: everything (an Administrator) or a role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Admin,
    Role(Role),
}

impl Level {
    pub fn allows(self, a: Action) -> Allow {
        match self {
            Level::Admin => All,
            Level::Role(r) => allows(r, a),
        }
    }

    /// Whether this level sees only some containers and stacks (Standard, Read-only).
    pub fn limited(self) -> bool {
        Action::ALL.iter().any(|a| self.allows(*a) == Visible)
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Admin => "admin",
            Level::Role(r) => r.as_str(),
        }
    }

    /// The actions this level may do at all (on something), for the UI to hide the rest.
    pub fn actions(self) -> Vec<&'static str> {
        Action::ALL.iter().filter(|a| self.allows(**a) != No).map(|a| a.as_str()).collect()
    }
}

/// A person's access in one environment, as a request checked by the guard carries it (`routes::guard`).
#[derive(Clone, Debug)]
pub struct Access {
    pub user: crate::auth::User,
    pub env: i64,
    pub level: Level,
    /// The teams they are in, for team resources.
    pub teams: Vec<i64>,
}

impl Access {
    pub fn allows(&self, a: Action) -> Allow {
        self.level.allows(a)
    }

    pub fn limited(&self) -> bool {
        self.level.limited()
    }

    /// Whether they see a container or stack with this ownership (`None`: no record, administrators only).
    pub fn sees(&self, o: Option<&ownership::Owner>) -> bool {
        if !self.limited() {
            return true;
        }
        match o {
            None => false,
            Some(o) => match o.level {
                ownership::Share::Public => true,
                ownership::Share::Team => o.team.is_some_and(|t| self.teams.contains(&t)),
                ownership::Share::Private => o.user == Some(self.user.id),
            },
        }
    }

    /// Whether they may do `a` to a container or stack with this ownership.
    pub fn may(&self, a: Action, o: Option<&ownership::Owner>) -> bool {
        match self.allows(a) {
            No => false,
            All => true,
            Visible => self.sees(o),
        }
    }
}

// ---------------------------------------------------------------- from the database

use crate::db::{Db, rusqlite::params};

/// The teams a person is in.
pub fn teams_of(c: &crate::db::rusqlite::Connection, user: i64) -> crate::db::rusqlite::Result<Vec<i64>> {
    let mut st = c.prepare("SELECT team_id FROM team_members WHERE user_id = ?1 ORDER BY team_id")?;
    st.query_map([user], |r| r.get(0))?.collect()
}

/// Every environment's role for a person who is not an Administrator: environments they have no role on are left out.
pub fn roles(c: &crate::db::rusqlite::Connection, user: i64) -> crate::db::rusqlite::Result<std::collections::HashMap<i64, Role>> {
    let mut st = c.prepare(
        "SELECT e.id, g.user_id IS NOT NULL, g.env_id IS NOT NULL, g.role FROM environments e
         JOIN grants g ON (g.env_id = e.id OR (e.group_id IS NOT NULL AND g.group_id = e.group_id))
         WHERE g.user_id = ?1 OR g.team_id IN (SELECT team_id FROM team_members WHERE user_id = ?1)",
    )?;
    let rows = st.query_map([user], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, bool>(2)?, r.get::<_, String>(3)?)))?;
    let mut by: std::collections::HashMap<i64, Vec<Grant>> = Default::default();
    for row in rows {
        let (env, user, on_env, role) = row?;
        let Some(role) = Role::parse(&role) else { continue };
        let g = Grant { subject: if user { Subject::User } else { Subject::Team }, on: if on_env { On::Environment } else { On::Group }, role };
        by.entry(env).or_default().push(g);
    }
    Ok(by.into_iter().filter_map(|(e, g)| resolve(&g).map(|r| (e, r))).collect())
}

/// A person's level on every environment they may see (an Administrator: every one).
pub async fn levels(db: &Db, user: &crate::auth::User) -> anyhow::Result<std::collections::HashMap<i64, Level>> {
    let (uid, admin) = (user.id, user.is_admin());
    db.call(move |c| {
        if admin {
            let mut st = c.prepare("SELECT id FROM environments")?;
            return Ok(st.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?.into_iter().map(|e| (e, Level::Admin)).collect());
        }
        Ok(roles(c, uid)?.into_iter().map(|(e, r)| (e, Level::Role(r))).collect())
    })
    .await
}

/// A person's access in environment `env`; `None` when the environment is not there or they have no role on it.
pub async fn load(db: &Db, user: &crate::auth::User, env: i64) -> anyhow::Result<Option<Access>> {
    let u = user.clone();
    db.call(move |c| {
        let exists: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM environments WHERE id = ?1)", params![env], |r| r.get(0))?;
        if !exists {
            return Ok(None);
        }
        let level = if u.is_admin() {
            Level::Admin
        } else {
            match roles(c, u.id)?.get(&env) {
                Some(r) => Level::Role(*r),
                None => return Ok(None),
            }
        };
        let teams = teams_of(c, u.id)?;
        Ok(Some(Access { user: u, env, level, teams }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table as DECISIONS "Access" 2 writes it, cell by cell, so a change to one is a change to both.
    #[test]
    fn the_table_is_what_decisions_says_for_every_role_and_action() {
        use Action::*;
        let expect = |r: Role, a: Action| -> Allow {
            match (r, a) {
                (Role::EnvAdmin, _) => All,
                (Role::Operator, Read | Operate | Console) => All,
                (Role::Operator, Create | Manage | Administer) => No,
                (Role::Helpdesk, Read) => All,
                (Role::Helpdesk, _) => No,
                (Role::Standard, Read | Operate | Console | Manage) => Visible,
                (Role::Standard, Create) => All,
                (Role::Standard, Administer) => No,
                (Role::ReadOnly, Read) => Visible,
                (Role::ReadOnly, _) => No,
            }
        };
        let mut cells = 0;
        for r in Role::ALL {
            for a in Action::ALL {
                assert_eq!(allows(r, a), expect(r, a), "{r:?} × {a:?}");
                assert_eq!(Level::Role(r).allows(a), expect(r, a));
                assert_eq!(Level::Admin.allows(a), All, "an Administrator may {a:?}");
                cells += 1;
            }
        }
        assert_eq!(cells, 30);
    }

    #[test]
    fn only_standard_and_read_only_users_are_limited_to_what_they_can_see() {
        let limited: Vec<Role> = Role::ALL.into_iter().filter(|r| Level::Role(*r).limited()).collect();
        assert_eq!(limited, vec![Role::Standard, Role::ReadOnly]);
        assert!(!Level::Admin.limited());
    }

    #[test]
    fn a_role_reads_and_parses_as_its_name() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("admin"), None, "an Administrator is Freightdock-wide, not a role on an environment");
        assert!(Role::EnvAdmin > Role::Operator && Role::Operator > Role::Helpdesk && Role::Helpdesk > Role::Standard && Role::Standard > Role::ReadOnly);
    }

    #[test]
    fn only_reading_is_a_read() {
        let reads: Vec<Action> = Action::ALL.into_iter().filter(|a| a.reads()).collect();
        assert_eq!(reads, vec![Action::Read]);
    }

    fn g(subject: Subject, on: On, role: Role) -> Grant {
        Grant { subject, on, role }
    }

    #[test]
    fn the_environment_wins_over_the_group_the_user_over_the_team_and_the_highest_team() {
        use On::*;
        use Role::*;
        use Subject::*;
        assert_eq!(resolve(&[]), None, "no grant, no access");
        assert_eq!(resolve(&[g(User, Group, Operator), g(User, Environment, ReadOnly)]), Some(ReadOnly), "the environment's grant wins, even lower");
        assert_eq!(resolve(&[g(Team, Environment, EnvAdmin), g(User, Group, Helpdesk)]), Some(Helpdesk), "the user's own, even on the group, wins over a team's");
        assert_eq!(resolve(&[g(Team, Environment, ReadOnly), g(Team, Environment, Operator), g(Team, Environment, Standard)]), Some(Operator), "the highest team grant");
        assert_eq!(resolve(&[g(Team, Group, EnvAdmin), g(Team, Environment, Helpdesk)]), Some(Helpdesk), "a team's grant on the environment wins over one on the group");
        assert_eq!(resolve(&[g(Team, Group, Standard), g(Team, Group, Helpdesk)]), Some(Helpdesk));
        assert_eq!(resolve(&[g(User, Group, Standard)]), Some(Standard));
    }

    /// Every combination of the four kinds of grant, each absent or any role: the answer is always the first kind
    /// present in the order of `resolve`, and the highest within it.
    #[test]
    fn resolve_follows_its_order_for_every_combination() {
        let kinds = [(Subject::User, On::Environment), (Subject::User, On::Group), (Subject::Team, On::Environment), (Subject::Team, On::Group)];
        let choices: Vec<Option<Role>> = std::iter::once(None).chain(Role::ALL.into_iter().map(Some)).collect();
        let mut n = 0;
        for a in &choices {
            for b in &choices {
                for c in &choices {
                    for d in &choices {
                        let picked = [a, b, c, d];
                        let mut grants: Vec<Grant> = kinds.iter().zip(picked).filter_map(|((s, o), r)| r.map(|r| g(*s, *o, r))).collect();
                        // A second team grant, lower, never changes the answer.
                        if let Some(r) = c {
                            grants.push(g(Subject::Team, On::Environment, Role::ReadOnly.min(*r)));
                        }
                        let want = picked.iter().find_map(|r| **r);
                        assert_eq!(resolve(&grants), want, "{grants:?}");
                        n += 1;
                    }
                }
            }
        }
        assert_eq!(n, 6 * 6 * 6 * 6);
    }

    #[tokio::test]
    async fn roles_come_from_the_database_through_groups_and_teams() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute_batch(
                "INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'ann', 'x', 'user', 0), (2, 'bob', 'x', 'user', 0), (3, 'root', 'x', 'admin', 0);
                 INSERT INTO environment_groups (id, name, created_at) VALUES (10, 'prod', 0);
                 INSERT INTO environments (id, name, kind, url, created_at, group_id) VALUES (1, 'local', 'local', '', 0, NULL), (2, 'web', 'agent', '', 0, 10), (3, 'db', 'agent', '', 0, 10);
                 INSERT INTO teams (id, name, created_at) VALUES (7, 'ops', 0);
                 INSERT INTO team_members (team_id, user_id) VALUES (7, 1);
                 INSERT INTO grants (team_id, group_id, role) VALUES (7, 10, 'helpdesk');
                 INSERT INTO grants (user_id, env_id, role) VALUES (1, 3, 'read_only');
                 INSERT INTO grants (team_id, env_id, role) VALUES (7, 1, 'operator');",
            )?;
            let ann = roles(c, 1)?;
            assert_eq!(ann.get(&1), Some(&Role::Operator), "the team's on the environment");
            assert_eq!(ann.get(&2), Some(&Role::Helpdesk), "the team's on the group");
            assert_eq!(ann.get(&3), Some(&Role::ReadOnly), "her own wins over the team's");
            assert!(roles(c, 2)?.is_empty(), "bob is in no team and has no grant");
            Ok(())
        })
        .await
        .unwrap();
        let root = crate::auth::User { id: 3, name: "root".into(), role: "admin".into(), token: None };
        assert_eq!(levels(&db, &root).await.unwrap().len(), 3, "an Administrator sees every environment");
        let bob = crate::auth::User { id: 2, name: "bob".into(), role: "user".into(), token: None };
        assert!(load(&db, &bob, 1).await.unwrap().is_none());
        let ann = crate::auth::User { id: 1, name: "ann".into(), role: "user".into(), token: None };
        let a = load(&db, &ann, 2).await.unwrap().unwrap();
        assert_eq!((a.level, a.teams.clone()), (Level::Role(Role::Helpdesk), vec![7]));
    }

    #[test]
    fn a_limited_role_sees_public_its_team_s_and_its_own() {
        use ownership::{Owner, Share};
        let user = crate::auth::User { id: 5, name: "s".into(), role: "user".into(), token: None };
        let a = Access { user, env: 1, level: Level::Role(Role::Standard), teams: vec![7] };
        let o = |level, user, team| Owner { level, user, team };
        assert!(!a.sees(None), "no record: administrators only");
        assert!(a.sees(Some(&o(Share::Public, None, None))));
        assert!(a.sees(Some(&o(Share::Team, Some(9), Some(7)))));
        assert!(!a.sees(Some(&o(Share::Team, Some(5), Some(8)))), "another team's, even made by them");
        assert!(a.sees(Some(&o(Share::Private, Some(5), None))));
        assert!(!a.sees(Some(&o(Share::Private, Some(6), None))));
        assert!(a.may(Action::Manage, Some(&o(Share::Public, None, None))));
        assert!(!a.may(Action::Administer, Some(&o(Share::Private, Some(5), None))));
        let helpdesk = Access { level: Level::Role(Role::Helpdesk), ..a.clone() };
        assert!(helpdesk.sees(None), "Helpdesk reads everything");
        assert!(!helpdesk.may(Action::Operate, None));
    }
}
