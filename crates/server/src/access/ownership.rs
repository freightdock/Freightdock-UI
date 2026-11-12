//! Who a container or a stack belongs to: public, a team's, or one person's ("Access" 4). Kept on Freightdock's side
//! (`resource_access`), by environment and a container's name or a stack's id; a container made through Freightdock also
//! carries a `freightdock.owner` label with its maker's name, for whoever looks at it with Docker's own tools. A container
//! with no record of its own belongs as its Compose project's Freightdock stack does; with neither, it is the
//! administrators' (made on the command line, or before roles), which Standard and Read-only users do not see.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{Db, rusqlite::{self, params}};

/// The label a container made through Freightdock carries, with its maker's name.
pub const LABEL: &str = "freightdock.owner";
/// Compose's label for the project a container is in.
pub const PROJECT: &str = "com.docker.compose.project";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Share {
    Public,
    Team,
    Private,
}

impl Share {
    pub fn as_str(self) -> &'static str {
        match self {
            Share::Public => "public",
            Share::Team => "team",
            Share::Private => "private",
        }
    }
    fn parse(s: &str) -> Option<Share> {
        [Share::Public, Share::Team, Share::Private].into_iter().find(|x| x.as_str() == s)
    }
}

/// One container's or stack's ownership: shared how, made by whom (or the person it is private to), which team.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub level: Share,
    pub user: Option<i64>,
    pub team: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Container,
    Stack,
    /// By its name.
    Volume,
    /// By its name.
    Network,
    /// By its id (`sha256:...`).
    Image,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Container => "container",
            Kind::Stack => "stack",
            Kind::Volume => "volume",
            Kind::Network => "network",
            Kind::Image => "image",
        }
    }
}

/// Docker's own networks, which every container may be on: public, whoever looks.
pub const PREDEFINED_NETWORKS: [&str; 5] = ["bridge", "host", "none", "ingress", "docker_gwbridge"];

static PUBLIC: Owner = Owner { level: Share::Public, user: None, team: None };

/// Every ownership record of one environment, and its stacks by name, read once for a list or a check.
#[derive(Default, Debug)]
pub struct Owners {
    containers: HashMap<String, Owner>,
    stacks: HashMap<i64, Owner>,
    volumes: HashMap<String, Owner>,
    networks: HashMap<String, Owner>,
    images: HashMap<String, Owner>,
    stack_ids: HashMap<String, i64>,
    names: HashMap<i64, String>,
    team_names: HashMap<i64, String>,
}

impl Owners {
    pub async fn load(db: &Db, env: i64) -> anyhow::Result<Owners> {
        db.call(move |c| {
            let mut o = Owners::default();
            let mut st = c.prepare("SELECT kind, key, level, user_id, team_id FROM resource_access WHERE env_id = ?1")?;
            let rows = st.query_map([env], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?)))?;
            for row in rows {
                let (kind, key, level, user, team) = row?;
                let Some(level) = Share::parse(&level) else { continue };
                let owner = Owner { level, user, team };
                match kind.as_str() {
                    "container" => {
                        o.containers.insert(key, owner);
                    }
                    "stack" => {
                        if let Ok(id) = key.parse() {
                            o.stacks.insert(id, owner);
                        }
                    }
                    "volume" => {
                        o.volumes.insert(key, owner);
                    }
                    "network" => {
                        o.networks.insert(key, owner);
                    }
                    "image" => {
                        o.images.insert(key, owner);
                    }
                    _ => {}
                }
            }
            let mut st = c.prepare("SELECT id, name FROM stacks WHERE env_id = ?1")?;
            for row in st.query_map([env], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, name) = row?;
                o.stack_ids.insert(name.to_lowercase(), id);
            }
            let mut st = c.prepare("SELECT id, name FROM users")?;
            for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, name) = row?;
                o.names.insert(id, name);
            }
            let mut st = c.prepare("SELECT id, name FROM teams")?;
            for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, name) = row?;
                o.team_names.insert(id, name);
            }
            Ok(o)
        })
        .await
    }

    /// A container's ownership by its name and Compose project: its own record, else its Freightdock stack's.
    pub fn container(&self, name: &str, project: Option<&str>) -> Option<&Owner> {
        let name = name.trim_start_matches('/');
        self.containers.get(name).or_else(|| project.and_then(|p| self.stack_ids.get(&p.to_lowercase())).and_then(|id| self.stacks.get(id)))
    }

    pub fn stack(&self, id: i64) -> Option<&Owner> {
        self.stacks.get(&id)
    }

    fn of_project(&self, project: Option<&str>) -> Option<&Owner> {
        project.and_then(|p| self.stack_ids.get(&p.to_lowercase())).and_then(|id| self.stacks.get(id))
    }

    /// A volume's ownership by its name and Compose project: its own record, else its Freightdock stack's.
    pub fn volume(&self, name: &str, project: Option<&str>) -> Option<&Owner> {
        self.volumes.get(name).or_else(|| self.of_project(project))
    }

    /// A network's: its own record, else its Freightdock stack's; Docker's own networks are public.
    pub fn network(&self, name: &str, project: Option<&str>) -> Option<&Owner> {
        self.networks.get(name).or_else(|| self.of_project(project)).or_else(|| PREDEFINED_NETWORKS.contains(&name).then_some(&PUBLIC))
    }

    /// An image's, by its id.
    pub fn image(&self, id: &str) -> Option<&Owner> {
        self.images.get(id)
    }

    /// An ownership as the API shows it: `{level, user, team}` with names, or `{level: "admins"}` for none.
    pub fn show(&self, o: Option<&Owner>) -> Value {
        match o {
            None => json!({ "level": "admins" }),
            Some(o) => json!({
                "level": o.level.as_str(),
                "user": o.user.map(|u| json!({ "id": u, "name": self.names.get(&u) })),
                "team": o.team.map(|t| json!({ "id": t, "name": self.team_names.get(&t) })),
            }),
        }
    }
}

/// A container in Docker's list or inspect (either shape): its name and its Compose project.
pub fn container_keys(c: &Value) -> (String, Option<String>) {
    let name = c["Name"].as_str().map(str::to_string).or_else(|| c["Names"][0].as_str().map(str::to_string)).unwrap_or_default();
    let labels = if c["Labels"].is_object() { &c["Labels"] } else { &c["Config"]["Labels"] };
    (name.trim_start_matches('/').to_string(), labels[PROJECT].as_str().map(str::to_string))
}

/// What a limited person sees through the containers they can see: the volumes those mount, the networks they are
/// on and the images they run ("Access" 18). A volume, network or image is seen when its own ownership lets the
/// person see it, or when a container they see uses it.
#[derive(Default, Debug)]
pub struct Seen {
    pub containers: std::collections::HashSet<String>,
    pub volumes: std::collections::HashSet<String>,
    /// Names and ids.
    pub networks: std::collections::HashSet<String>,
    pub images: std::collections::HashSet<String>,
}

impl Seen {
    /// From Docker's container list (`all`), as JSON or as bollard's summaries.
    pub fn of(access: &super::Access, owners: &Owners, containers: &[Value]) -> Seen {
        let mut s = Seen::default();
        for c in containers {
            let (name, project) = container_keys(c);
            if !access.sees(owners.container(&name, project.as_deref())) {
                continue;
            }
            for m in c["Mounts"].as_array().into_iter().flatten() {
                if m["Type"] == "volume" {
                    s.volumes.extend(m["Name"].as_str().map(str::to_string));
                }
            }
            for (net, e) in c["NetworkSettings"]["Networks"].as_object().into_iter().flatten() {
                s.networks.insert(net.clone());
                s.networks.extend(e["NetworkID"].as_str().map(str::to_string));
            }
            if let Some(m) = c["HostConfig"]["NetworkMode"].as_str() {
                s.networks.insert(m.to_string());
            }
            s.images.extend(c["ImageID"].as_str().map(str::to_string));
            s.containers.insert(name);
        }
        s
    }

    pub fn volume(&self, access: &super::Access, owners: &Owners, name: &str, project: Option<&str>) -> bool {
        !access.limited() || access.sees(owners.volume(name, project)) || self.volumes.contains(name)
    }

    pub fn network(&self, access: &super::Access, owners: &Owners, name: &str, id: &str, project: Option<&str>) -> bool {
        !access.limited() || access.sees(owners.network(name, project)) || self.networks.contains(name) || self.networks.contains(id)
    }

    pub fn image(&self, access: &super::Access, owners: &Owners, id: &str) -> bool {
        !access.limited() || access.sees(owners.image(id)) || self.images.contains(id)
    }

    /// The containers among `names` the person sees (all of them for those who see everything).
    pub fn only<'a>(&self, access: &super::Access, names: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        names.into_iter().filter(|n| !access.limited() || self.containers.contains(n.trim_start_matches('/'))).cloned().collect()
    }
}

/// The Compose project in Docker's labels of a volume or a network (`Labels`).
pub fn project_of(v: &Value) -> Option<String> {
    v["Labels"][PROJECT].as_str().map(str::to_string)
}

/// Sets (or, with `None`, forgets) a container's or stack's ownership.
pub fn set(c: &rusqlite::Connection, env: i64, kind: Kind, key: &str, o: Option<&Owner>) -> rusqlite::Result<()> {
    match o {
        None => c.execute("DELETE FROM resource_access WHERE env_id = ?1 AND kind = ?2 AND key = ?3", params![env, kind.as_str(), key]).map(|_| ()),
        Some(o) => c
            .execute(
                "INSERT INTO resource_access (env_id, kind, key, level, user_id, team_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (env_id, kind, key) DO UPDATE SET level = excluded.level, user_id = excluded.user_id, team_id = excluded.team_id",
                params![env, kind.as_str(), key, o.level.as_str(), o.user, o.team],
            )
            .map(|_| ()),
    }
}

pub async fn save(db: &Db, env: i64, kind: Kind, key: String, o: Option<Owner>) -> anyhow::Result<()> {
    db.call(move |c| Ok(set(c, env, kind, &key, o.as_ref())?)).await
}

/// A container's record follows it to its new name.
pub async fn rename(db: &Db, env: i64, from: String, to: String) -> anyhow::Result<()> {
    db.call(move |c| {
        if from != to {
            c.execute("DELETE FROM resource_access WHERE env_id = ?1 AND kind = 'container' AND key = ?2", params![env, to])?;
            c.execute("UPDATE resource_access SET key = ?3 WHERE env_id = ?1 AND kind = 'container' AND key = ?2", params![env, from, to])?;
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
pub async fn get(db: &Db, env: i64, kind: Kind, key: String) -> anyhow::Result<Option<Owner>> {
    use rusqlite::OptionalExtension;
    db.call(move |c| {
        Ok(c.query_row("SELECT level, user_id, team_id FROM resource_access WHERE env_id = ?1 AND kind = ?2 AND key = ?3", params![env, kind.as_str(), key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?))
        })
        .optional()?
        .and_then(|(l, user, team)| Share::parse(&l).map(|level| Owner { level, user, team })))
    })
    .await
}

/// What a request asks a new or changed resource's ownership to be: `public`, `team` (with `team_id`), `private`, or
/// `admins` (no record). Left out on a create: private to its maker.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Wanted {
    pub level: Option<String>,
    pub team_id: Option<i64>,
    /// Private to somebody else: only for those who see everything.
    pub user_id: Option<i64>,
}

impl Wanted {
    /// The ownership `who` may give, or why not: a limited person may only share with a team they are in and make a
    /// thing private to themselves, and may not give it to the administrators (they would lose it).
    pub fn owner(&self, who: &super::Access) -> Result<Option<Owner>, crate::error::ApiError> {
        use crate::error::ApiError;
        let me = who.user.id;
        let level = self.level.as_deref().unwrap_or("private");
        let o = match level {
            "admins" if who.limited() => return Err(ApiError::bad("Only an administrator keeps a thing to the administrators.").field("level")),
            "admins" => None,
            "public" => Some(Owner { level: Share::Public, user: Some(me), team: None }),
            "team" => {
                let t = self.team_id.ok_or_else(|| ApiError::bad("Choose the team.").field("team_id"))?;
                if who.limited() && !who.teams.contains(&t) {
                    return Err(ApiError::bad("You can only share with a team you are in.").field("team_id"));
                }
                Some(Owner { level: Share::Team, user: Some(me), team: Some(t) })
            }
            "private" => {
                let u = self.user_id.unwrap_or(me);
                if who.limited() && u != me {
                    return Err(ApiError::bad("You can only keep a thing to yourself.").field("user_id"));
                }
                Some(Owner { level: Share::Private, user: Some(u), team: None })
            }
            _ => return Err(ApiError::bad("Access is public, team, private or admins.").field("level")),
        };
        Ok(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_container_belongs_as_its_record_says_else_as_its_stack_does() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute_batch(
                "INSERT INTO users (id, name, password_hash, role, created_at) VALUES (5, 'sam', 'x', 'user', 0);
                 INSERT INTO environments (id, name, kind, url, created_at) VALUES (1, 'local', 'local', '', 0);
                 INSERT INTO stacks (id, env_id, name, source, created_at, updated_at) VALUES (3, 1, 'Shop', 'editor', 0, 0);",
            )?;
            set(c, 1, Kind::Container, "web", Some(&Owner { level: Share::Private, user: Some(5), team: None }))?;
            set(c, 1, Kind::Stack, "3", Some(&Owner { level: Share::Public, user: None, team: None }))?;
            Ok(())
        })
        .await
        .unwrap();
        let o = Owners::load(&db, 1).await.unwrap();
        assert_eq!(o.container("/web", None).unwrap().user, Some(5));
        assert_eq!(o.container("shop-app-1", Some("shop")).unwrap().level, Share::Public, "the stack's, its project matched without case");
        assert!(o.container("other", None).is_none(), "neither: the administrators'");
        assert_eq!(o.show(o.container("web", None))["user"]["name"], "sam");
        assert_eq!(o.show(None)["level"], "admins");

        rename(&db, 1, "web".into(), "web2".into()).await.unwrap();
        assert!(get(&db, 1, Kind::Container, "web".into()).await.unwrap().is_none());
        assert!(get(&db, 1, Kind::Container, "web2".into()).await.unwrap().is_some());
        save(&db, 1, Kind::Container, "web2".into(), None).await.unwrap();
        assert!(get(&db, 1, Kind::Container, "web2".into()).await.unwrap().is_none());
    }

    #[test]
    fn a_container_s_keys_come_from_either_shape_docker_gives() {
        let listed = json!({ "Names": ["/web"], "Labels": { PROJECT: "shop" } });
        assert_eq!(container_keys(&listed), ("web".into(), Some("shop".into())));
        let inspected = json!({ "Name": "/web", "Config": { "Labels": {} } });
        assert_eq!(container_keys(&inspected), ("web".into(), None));
    }

    #[test]
    fn a_limited_person_shares_only_with_their_teams_and_keeps_only_to_themselves() {
        use super::super::{Access, Level, Role};
        let user = crate::auth::User { id: 5, name: "s".into(), role: "user".into(), token: None };
        let std = Access { user, env: 1, level: Level::Role(Role::Standard), teams: vec![7] };
        let w = |level: &str, team: Option<i64>, user: Option<i64>| Wanted { level: Some(level.into()), team_id: team, user_id: user };
        assert_eq!(Wanted::default().owner(&std).unwrap(), Some(Owner { level: Share::Private, user: Some(5), team: None }), "private to its maker by default");
        assert!(w("team", Some(7), None).owner(&std).is_ok());
        assert!(w("team", Some(8), None).owner(&std).is_err());
        assert!(w("private", None, Some(6)).owner(&std).is_err());
        assert!(w("admins", None, None).owner(&std).is_err());
        assert!(w("everyone", None, None).owner(&std).is_err());
        let admin = Access { level: Level::Admin, ..std };
        assert_eq!(w("admins", None, None).owner(&admin).unwrap(), None);
        assert!(w("team", Some(8), None).owner(&admin).is_ok());
        assert_eq!(w("private", None, Some(6)).owner(&admin).unwrap().unwrap().user, Some(6));
    }
}
