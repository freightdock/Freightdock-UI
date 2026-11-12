//! The database: one SQLite file, `freightdock.db` in the data folder. Pooled through r2d2, every write transaction
//! IMMEDIATE (it takes the write lock as it begins, so a read before a write can never be refused at commit), and the
//! schema brought up to date by `MIGRATIONS`, which are only ever appended to ("Database" 1, 2 in DECISIONS.md).

use std::path::Path;

use anyhow::{Context, Result};
use r2d2_sqlite::SqliteConnectionManager;
pub use r2d2_sqlite::rusqlite;
use rusqlite::Connection;

pub type Pool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone)]
pub struct Db {
    pool: Pool,
    /// In a cluster: what is written goes to the managers too ("Cluster" 9).
    replica: Option<std::sync::Arc<crate::replica::Replicator>>,
}

/// The schema, one step per release that changed it. A step that has shipped is never edited or reordered: a database
/// out there has already run it. Add a new one at the end.
pub(crate) const MIGRATIONS: &[&str] = &[
    // 1: the foundation (1.0.0): settings, people, their sessions, the environments and the activity log.
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE users (
        id            INTEGER PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT NOT NULL,
        role          TEXT NOT NULL CHECK (role IN ('admin', 'user')),
        disabled      INTEGER NOT NULL DEFAULT 0,
        created_at    INTEGER NOT NULL,
        last_login_at INTEGER
    );

    CREATE TABLE sessions (
        token_hash   TEXT PRIMARY KEY,
        user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        csrf         TEXT NOT NULL,
        created_at   INTEGER NOT NULL,
        expires_at   INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL,
        ip           TEXT,
        user_agent   TEXT
    ) WITHOUT ROWID;
    CREATE INDEX idx_sessions_user ON sessions(user_id);

    CREATE TABLE environments (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        kind       TEXT NOT NULL CHECK (kind IN ('local', 'tls', 'agent')),
        url        TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );

    CREATE TABLE audit (
        id        INTEGER PRIMARY KEY,
        at        INTEGER NOT NULL,
        user_id   INTEGER,
        user_name TEXT,
        action    TEXT NOT NULL,
        target    TEXT,
        detail    TEXT,
        ip        TEXT
    );
    CREATE INDEX idx_audit_at ON audit(at);
    "#,
    // 2: single sign-on (1.0.0): who a user is at their OpenID Connect provider, and their e-mail.
    r#"
    ALTER TABLE users ADD COLUMN oidc_issuer TEXT;
    ALTER TABLE users ADD COLUMN oidc_sub TEXT;
    ALTER TABLE users ADD COLUMN email TEXT;
    CREATE UNIQUE INDEX idx_users_oidc ON users(oidc_issuer, oidc_sub) WHERE oidc_sub IS NOT NULL;
    "#,
    // 3: many hosts (1.0.0): each environment's health, an agent's join token (its hash) and certificate (its
    // fingerprint), a TLS host's CA, client certificate and key (sealed).
    r#"
    ALTER TABLE environments ADD COLUMN status TEXT NOT NULL DEFAULT 'unknown' CHECK (status IN ('unknown', 'up', 'down'));
    ALTER TABLE environments ADD COLUMN last_seen_at INTEGER;
    ALTER TABLE environments ADD COLUMN last_error TEXT;
    ALTER TABLE environments ADD COLUMN docker_version TEXT;
    ALTER TABLE environments ADD COLUMN join_token_hash TEXT;
    ALTER TABLE environments ADD COLUMN join_token_expires_at INTEGER;
    ALTER TABLE environments ADD COLUMN agent_cert_sha256 TEXT;
    ALTER TABLE environments ADD COLUMN tls_ca TEXT;
    ALTER TABLE environments ADD COLUMN tls_cert TEXT;
    ALTER TABLE environments ADD COLUMN tls_key TEXT;
    CREATE UNIQUE INDEX idx_environments_join ON environments(join_token_hash) WHERE join_token_hash IS NOT NULL;
    CREATE UNIQUE INDEX idx_environments_agent ON environments(agent_cert_sha256) WHERE agent_cert_sha256 IS NOT NULL;
    "#,
    // 4: stacks (1.0.0): the Compose projects Freightdock keeps. Their files live in <data>/stacks/<id>/; a Git stack's
    // repository, branch, compose file's path, credentials (the token sealed) and how often it is checked are here.
    r#"
    CREATE TABLE stacks (
        id             INTEGER PRIMARY KEY,
        env_id         INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        name           TEXT NOT NULL,
        source         TEXT NOT NULL CHECK (source IN ('editor', 'git')),
        git_url        TEXT,
        git_ref        TEXT,
        git_path       TEXT,
        git_user       TEXT,
        git_token      TEXT,
        git_poll_s     INTEGER NOT NULL DEFAULT 0,
        git_commit     TEXT,
        git_checked_at INTEGER,
        git_error      TEXT,
        created_by     TEXT,
        created_at     INTEGER NOT NULL,
        updated_at     INTEGER NOT NULL,
        deployed_at    INTEGER,
        UNIQUE (env_id, name)
    );
    "#,
    // 5: custom app templates (1.0.0): a compose file and the variables it asks for (Portainer's template `env`, JSON).
    r#"
    CREATE TABLE templates (
        id          INTEGER PRIMARY KEY,
        title       TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        note        TEXT NOT NULL DEFAULT '',
        categories  TEXT NOT NULL DEFAULT '[]',
        logo        TEXT NOT NULL DEFAULT '',
        compose     TEXT NOT NULL,
        env         TEXT NOT NULL DEFAULT '[]',
        created_by  TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    "#,
    // 6: webhooks (1.0.0): an address that redeploys a stack (target: its id) or a container (target: its name), kept
    // as the SHA-256 of its token.
    r#"
    CREATE TABLE webhooks (
        id           INTEGER PRIMARY KEY,
        token_hash   TEXT NOT NULL UNIQUE,
        env_id       INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        kind         TEXT NOT NULL CHECK (kind IN ('stack', 'container')),
        target       TEXT NOT NULL,
        created_by   TEXT,
        created_at   INTEGER NOT NULL,
        last_used_at INTEGER,
        UNIQUE (env_id, kind, target)
    );
    "#,
    // 7: environment groups and tags, and the home page (1.0.0): an environment is in at most one group (its
    // `group_id`, what roles per group will hang on in M4) and has any number of tags; an agent's version and the
    // address it last dialled from; the last summary of what each host holds, for when it cannot be asked.
    r#"
    ALTER TABLE environments ADD COLUMN agent_version TEXT;
    ALTER TABLE environments ADD COLUMN agent_peer TEXT;
    ALTER TABLE environments ADD COLUMN snapshot TEXT;
    ALTER TABLE environments ADD COLUMN snapshot_at INTEGER;

    CREATE TABLE environment_groups (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
        description TEXT NOT NULL DEFAULT '',
        created_at  INTEGER NOT NULL
    );
    ALTER TABLE environments ADD COLUMN group_id INTEGER REFERENCES environment_groups(id) ON DELETE SET NULL;
    CREATE INDEX idx_environments_group ON environments(group_id);

    CREATE TABLE tags (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE environment_tags (
        environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        tag_id         INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
        PRIMARY KEY (environment_id, tag_id)
    ) WITHOUT ROWID;
    CREATE INDEX idx_environment_tags_tag ON environment_tags(tag_id);
    "#,
    // 8: registries (1.0.0): where images come from and go to, and how Freightdock signs in there. `host` is what image
    // references are matched by (Docker Hub as docker.io), one registry a host; `secret` is sealed (a password, a
    // token, or ECR's secret access key, whose access key is `username`).
    r#"
    CREATE TABLE registries (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        kind       TEXT NOT NULL CHECK (kind IN ('dockerhub', 'ghcr', 'gitlab', 'ecr', 'quay', 'custom')),
        url        TEXT NOT NULL,
        host       TEXT NOT NULL UNIQUE,
        username   TEXT NOT NULL DEFAULT '',
        secret     TEXT,
        region     TEXT,
        created_at INTEGER NOT NULL
    );
    "#,
    // 9: API tokens (1.0.0): a person's tokens for programs (the SHA-256 of each, its last four characters to tell it
    // by), and which token an entry in the activity log was made with.
    r#"
    CREATE TABLE api_tokens (
        id           INTEGER PRIMARY KEY,
        user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        name         TEXT NOT NULL COLLATE NOCASE,
        token_hash   TEXT NOT NULL UNIQUE,
        hint         TEXT NOT NULL,
        read_only    INTEGER NOT NULL DEFAULT 0,
        created_at   INTEGER NOT NULL,
        expires_at   INTEGER,
        last_used_at INTEGER,
        last_used_ip TEXT,
        UNIQUE (user_id, name)
    );
    ALTER TABLE audit ADD COLUMN via TEXT;
    "#,
    // 10: Swarm stacks (1.0.0): the Compose file each was last deployed with, per environment, to edit and deploy again.
    r#"
    CREATE TABLE swarm_stacks (
        env        INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        name       TEXT NOT NULL,
        compose    TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        updated_by TEXT,
        PRIMARY KEY (env, name)
    ) WITHOUT ROWID;
    "#,
    // 11: resource history (1.0.0): a sample of each running container's use every so often, by its name, so a
    // container made again (an edit, an update) keeps its history.
    r#"
    CREATE TABLE container_stats (
        env_id     INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        container  TEXT NOT NULL,
        at         INTEGER NOT NULL,
        cpu        REAL NOT NULL,
        mem        INTEGER NOT NULL,
        mem_limit  INTEGER NOT NULL,
        net_rx     INTEGER NOT NULL,
        net_tx     INTEGER NOT NULL,
        disk_read  INTEGER NOT NULL,
        disk_write INTEGER NOT NULL,
        pids       INTEGER NOT NULL
    );
    CREATE INDEX idx_container_stats ON container_stats(env_id, container, at);
    CREATE INDEX idx_container_stats_at ON container_stats(at);
    "#,
    // 12: notifications (1.0.0): the channels Freightdock tells things through, their settings (the one secret in them
    // sealed), the events each wants, and how the last delivery went.
    r#"
    CREATE TABLE notification_channels (
        id           INTEGER PRIMARY KEY,
        name         TEXT NOT NULL,
        kind         TEXT NOT NULL CHECK (kind IN ('webhook', 'email', 'ntfy', 'gotify')),
        config       TEXT NOT NULL,
        events       TEXT NOT NULL,
        enabled      INTEGER NOT NULL DEFAULT 1,
        created_at   INTEGER NOT NULL,
        last_sent_at INTEGER,
        last_error   TEXT
    );
    "#,
    // 13: image updates (1.0.0): what each registry last said a tag is (asked at most every so often), the verdicts
    // the badges read, and the containers switched to automatic updates on their page, by name.
    r#"
    CREATE TABLE image_checks (
        reference  TEXT PRIMARY KEY,
        digest     TEXT,
        is_index   INTEGER NOT NULL DEFAULT 0,
        platforms  TEXT,
        checked_at INTEGER NOT NULL,
        error      TEXT
    ) WITHOUT ROWID;
    CREATE TABLE update_status (
        env_id     INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        kind       TEXT NOT NULL CHECK (kind IN ('container', 'image')),
        id         TEXT NOT NULL,
        name       TEXT NOT NULL,
        reference  TEXT NOT NULL,
        status     TEXT NOT NULL,
        detail     TEXT,
        remote     TEXT,
        labelled   INTEGER NOT NULL DEFAULT 0,
        checked_at INTEGER NOT NULL,
        PRIMARY KEY (env_id, kind, id)
    ) WITHOUT ROWID;
    CREATE TABLE auto_updates (
        env_id    INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        container TEXT NOT NULL,
        PRIMARY KEY (env_id, container)
    ) WITHOUT ROWID;
    "#,
    // 14: imports from Portainer (1.0.0): what each import made of which of Portainer's things, so a second import does
    // not make it again. `instance` is Portainer's InstanceID; `kind` is stack, group, tag, registry or template.
    r#"
    CREATE TABLE portainer_imports (
        instance     TEXT NOT NULL,
        kind         TEXT NOT NULL,
        portainer_id INTEGER NOT NULL,
        freightdock_id  INTEGER NOT NULL,
        at           INTEGER NOT NULL,
        PRIMARY KEY (instance, kind, portainer_id)
    ) WITHOUT ROWID;
    "#,
    // 15: access (1.0.0, M4): teams and their members; roles given to a user or a team on an environment or a group;
    // who each container and stack belongs to; the auth log. Users who were not administrators could read everything
    // and change nothing, which is Helpdesk: they keep that on every environment there is.
    r#"
    CREATE TABLE teams (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE team_members (
        team_id INTEGER NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        PRIMARY KEY (team_id, user_id)
    ) WITHOUT ROWID;
    CREATE INDEX idx_team_members_user ON team_members(user_id);

    CREATE TABLE grants (
        id       INTEGER PRIMARY KEY,
        user_id  INTEGER REFERENCES users(id) ON DELETE CASCADE,
        team_id  INTEGER REFERENCES teams(id) ON DELETE CASCADE,
        env_id   INTEGER REFERENCES environments(id) ON DELETE CASCADE,
        group_id INTEGER REFERENCES environment_groups(id) ON DELETE CASCADE,
        role     TEXT NOT NULL CHECK (role IN ('env_admin', 'operator', 'helpdesk', 'standard', 'read_only')),
        CHECK ((user_id IS NULL) <> (team_id IS NULL)),
        CHECK ((env_id IS NULL) <> (group_id IS NULL))
    );
    CREATE UNIQUE INDEX idx_grants_one ON grants(IFNULL(user_id, 0), IFNULL(team_id, 0), IFNULL(env_id, 0), IFNULL(group_id, 0));
    CREATE INDEX idx_grants_env ON grants(env_id);
    CREATE INDEX idx_grants_group ON grants(group_id);

    CREATE TABLE resource_access (
        env_id  INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        kind    TEXT NOT NULL CHECK (kind IN ('container', 'stack')),
        key     TEXT NOT NULL,
        level   TEXT NOT NULL CHECK (level IN ('public', 'team', 'private')),
        user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
        team_id INTEGER REFERENCES teams(id) ON DELETE SET NULL,
        PRIMARY KEY (env_id, kind, key)
    ) WITHOUT ROWID;

    CREATE TABLE auth_log (
        id         INTEGER PRIMARY KEY,
        at         INTEGER NOT NULL,
        event      TEXT NOT NULL,
        method     TEXT NOT NULL,
        ok         INTEGER NOT NULL,
        user_id    INTEGER,
        user_name  TEXT,
        ip         TEXT,
        user_agent TEXT,
        detail     TEXT
    );
    CREATE INDEX idx_auth_log_at ON auth_log(at);

    INSERT INTO grants (user_id, env_id, role) SELECT u.id, e.id, 'helpdesk' FROM users u, environments e WHERE u.role = 'user';
    "#,
    // 16: per-environment security settings (1.0.0, M4): what non-administrators may not make a container with, and
    // whether they may open consoles, set on an environment or on its group (JSON, `access::security::Rules`). None is
    // everything allowed, as before.
    r#"
    CREATE TABLE security_rules (
        env_id     INTEGER UNIQUE REFERENCES environments(id) ON DELETE CASCADE,
        group_id   INTEGER UNIQUE REFERENCES environment_groups(id) ON DELETE CASCADE,
        rules      TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        updated_by TEXT,
        CHECK ((env_id IS NULL) <> (group_id IS NULL))
    );
    "#,
    // 17: ownership of volumes, networks and images (1.0.0, M4): `resource_access` takes them as kinds, a volume and a
    // network by name, an image by its id. SQLite cannot change a CHECK, so the table is made again with what it held.
    r#"
    CREATE TABLE resource_access_17 (
        env_id  INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
        kind    TEXT NOT NULL CHECK (kind IN ('container', 'stack', 'volume', 'network', 'image')),
        key     TEXT NOT NULL,
        level   TEXT NOT NULL CHECK (level IN ('public', 'team', 'private')),
        user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
        team_id INTEGER REFERENCES teams(id) ON DELETE SET NULL,
        PRIMARY KEY (env_id, kind, key)
    ) WITHOUT ROWID;
    INSERT INTO resource_access_17 (env_id, kind, key, level, user_id, team_id) SELECT env_id, kind, key, level, user_id, team_id FROM resource_access;
    DROP TABLE resource_access;
    ALTER TABLE resource_access_17 RENAME TO resource_access;
    "#,
    // 18: more ways in (1.0.0, M4): a local account's TOTP seed (sealed), the one it is enrolling and the last time step
    // a code was taken for (no code is good twice); its recovery codes, as hashes; which provider put a person in a team
    // (NULL: an administrator did, and a sign-in never takes them out); and the one OpenID Connect setting becomes the
    // first of a list of providers, with the id `oidc`, so its callback and the people linked to it carry on as they were.
    r#"
    ALTER TABLE users ADD COLUMN totp_secret TEXT;
    ALTER TABLE users ADD COLUMN totp_pending TEXT;
    ALTER TABLE users ADD COLUMN totp_last_step INTEGER;
    CREATE TABLE recovery_codes (
        user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        code_hash TEXT NOT NULL,
        PRIMARY KEY (user_id, code_hash)
    ) WITHOUT ROWID;
    ALTER TABLE team_members ADD COLUMN synced_by TEXT;
    INSERT INTO settings (key, value)
        SELECT 'sso', json_object('providers', json_array(json_set(json(value), '$.id', 'oidc', '$.kind', 'oidc'))) FROM settings WHERE key = 'oidc';
    DELETE FROM settings WHERE key = 'oidc';
    "#,
    // 19: a person's picture from their sign-in provider, and the copy Freightdock keeps of it ("Users" 1).
    r#"
    ALTER TABLE users ADD COLUMN picture TEXT;
    CREATE TABLE avatars (
        user_id      INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        content_type TEXT NOT NULL,
        data         BLOB NOT NULL,
        fetched_at   INTEGER NOT NULL
    );
    "#,
    // 20: a Forgejo's or a Gitea's package registry as kinds of registry ("Registries" 8). SQLite cannot change a CHECK,
    // so the table is made again with the wider one and every row copied across; nothing refers to it.
    r#"
    CREATE TABLE registries_new (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        kind       TEXT NOT NULL CHECK (kind IN ('dockerhub', 'ghcr', 'gitlab', 'ecr', 'quay', 'forgejo', 'gitea', 'custom')),
        url        TEXT NOT NULL,
        host       TEXT NOT NULL UNIQUE,
        username   TEXT NOT NULL DEFAULT '',
        secret     TEXT,
        region     TEXT,
        created_at INTEGER NOT NULL
    );
    INSERT INTO registries_new (id, name, kind, url, host, username, secret, region, created_at)
        SELECT id, name, kind, url, host, username, secret, region, created_at FROM registries;
    DROP TABLE registries;
    ALTER TABLE registries_new RENAME TO registries;
    "#,
];

impl Db {
    /// The database at `path`, made if it is not there, migrated to this binary's schema.
    pub fn open(path: &Path) -> Result<Self> {
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 15000;
                 PRAGMA temp_store = MEMORY;",
            )?;
            c.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(6).min_idle(Some(1)).build(manager).context("opening the database")?;
        let db = Db { pool, replica: None };
        db.migrate()?;
        Ok(db)
    }

    /// A migrated database that lives only as long as a test.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let manager = SqliteConnectionManager::memory().with_init(|c| {
            c.execute_batch("PRAGMA foreign_keys = ON;")?;
            c.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
            Ok(())
        });
        // One connection: every caller has to see the same in-memory database.
        let db = Db { pool: r2d2::Pool::builder().max_size(1).build(manager)?, replica: None };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.pool.get()?;
        let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            current as usize <= MIGRATIONS.len(),
            "the database is at schema {current}, newer than this Freightdock knows ({}); run the newer Freightdock, or restore a backup",
            MIGRATIONS.len()
        );
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
            tracing::info!("applied database migration {}", i + 1);
        }
        Ok(())
    }

    /// From here on, what is written is replicated by the cluster's managers (`replica::Replicator::start`).
    pub async fn replicate(&mut self, cluster: std::sync::Arc<crate::cluster::Cluster>, data_dir: std::path::PathBuf, loaded: crate::replica::Loaded) -> Result<std::sync::Arc<crate::replica::Replicator>> {
        let r = crate::replica::Replicator::start(&self.pool, cluster, data_dir, loaded).await?;
        self.replica = Some(r.clone());
        Ok(r)
    }

    /// Sends what is written and not yet replicated (after a failure to: `call` does it after every write).
    pub async fn push(&self) -> Result<()> {
        match &self.replica {
            Some(r) => r.push(&self.pool).await,
            None => Ok(()),
        }
    }

    /// Runs `f` with a pooled connection on a blocking thread. All database work goes through here. In a cluster, a
    /// call that wrote returns only once the managers have the change, and fails when they did not take it (the
    /// write stays here and is sent again).
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();
        let (out, wrote) = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("database pool exhausted")?;
            let before = conn.total_changes();
            let out = f(&mut conn)?;
            Ok::<_, anyhow::Error>((out, conn.total_changes() != before))
        })
        .await
        .context("database task panicked")??;
        if wrote {
            self.push().await?;
        }
        Ok(out)
    }
}

impl Db {
    /// The setting `key` as JSON, when there is one.
    pub async fn setting<T: serde::de::DeserializeOwned + Send + 'static>(&self, key: &'static str) -> Result<Option<T>> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = self.call(move |c| Ok(c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)).await?;
        raw.map(|r| serde_json::from_str(&r).with_context(|| format!("the setting {key} is not what this Freightdock reads"))).transpose()
    }

    /// Keeps `value` as the setting `key`, as JSON.
    pub async fn put_setting<T: serde::Serialize>(&self, key: &'static str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        self.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", rusqlite::params![key, raw])?))
            .await?;
        Ok(())
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_new_database_is_migrated_to_the_last_step() {
        let db = Db::open_in_memory().unwrap();
        let v: i64 = db.call(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }

    #[tokio::test]
    async fn opening_twice_runs_no_step_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("freightdock.db");
        let db = Db::open(&path).unwrap();
        db.call(|c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('a', 'b')", [])?)).await.unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let kept: String = db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = 'a'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(kept, "b");
    }

    /// The step that lets a registry be a Forgejo or a Gitea makes `registries` again: every saved registry stays, its
    /// sealed secret and its one-a-host rule with it.
    #[tokio::test]
    async fn saved_registries_survive_the_table_being_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("freightdock.db");
        let step = MIGRATIONS.iter().position(|m| m.contains("registries_new")).expect("the step");
        {
            let mut c = Connection::open(&path).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(step) {
                let tx = c.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            c.execute("INSERT INTO registries (id, name, kind, url, host, username, secret, created_at) VALUES (3, 'gh', 'ghcr', 'ghcr.io', 'ghcr.io', 'owner', 'sealed1:x', 7)", []).unwrap();
            let refused = c.execute("INSERT INTO registries (name, kind, url, host, created_at) VALUES ('git', 'forgejo', 'git.lan', 'git.lan', 0)", []);
            assert!(refused.is_err(), "not a kind before the step");
        }
        let db = Db::open(&path).unwrap();
        db.call(|c| {
            let kept: (i64, String, String, String, i64) =
                c.query_row("SELECT id, kind, username, secret, created_at FROM registries WHERE name = 'GH'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            assert_eq!(kept, (3, "ghcr".into(), "owner".into(), "sealed1:x".into(), 7), "the name still case-blind");
            for kind in ["forgejo", "gitea"] {
                c.execute("INSERT INTO registries (name, kind, url, host, created_at) VALUES (?1, ?1, ?1, ?1, 0)", [kind])?;
            }
            let twice = c.execute("INSERT INTO registries (name, kind, url, host, created_at) VALUES ('again', 'custom', 'ghcr.io', 'ghcr.io', 0)", []);
            assert!(twice.is_err(), "one registry a host");
            Ok(())
        })
        .await
        .unwrap();
    }

    /// The step that lets volumes, networks and images be owned makes `resource_access` again: what it held stays, and
    /// a user's removal still empties their records' user.
    #[tokio::test]
    async fn ownership_records_survive_the_table_being_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("freightdock.db");
        let step = MIGRATIONS.iter().position(|m| m.contains("resource_access_17")).expect("the step");
        {
            let mut c = Connection::open(&path).unwrap();
            c.pragma_update(None, "foreign_keys", true).unwrap();
            for (i, sql) in MIGRATIONS.iter().enumerate().take(step) {
                let tx = c.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            c.execute_batch(
                "INSERT INTO users (id, name, password_hash, role, created_at) VALUES (5, 'sam', 'x', 'user', 0);
                 INSERT INTO environments (id, name, kind, url, created_at) VALUES (1, 'local', 'local', '', 0);
                 INSERT INTO resource_access (env_id, kind, key, level, user_id) VALUES (1, 'container', 'web', 'private', 5);",
            )
            .unwrap();
            assert!(c.execute("INSERT INTO resource_access (env_id, kind, key, level) VALUES (1, 'volume', 'v', 'public')", []).is_err(), "not a kind before the step");
        }
        let db = Db::open(&path).unwrap();
        db.call(|c| {
            let kept: (String, Option<i64>) = c.query_row("SELECT level, user_id FROM resource_access WHERE kind = 'container' AND key = 'web'", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            assert_eq!(kept, ("private".to_string(), Some(5)));
            for kind in ["volume", "network", "image"] {
                c.execute("INSERT INTO resource_access (env_id, kind, key, level) VALUES (1, ?1, 'k', 'public')", [kind])?;
            }
            c.execute("DELETE FROM users WHERE id = 5", [])?;
            let user: Option<i64> = c.query_row("SELECT user_id FROM resource_access WHERE key = 'web'", [], |r| r.get(0))?;
            assert_eq!(user, None);
            c.execute("DELETE FROM environments WHERE id = 1", [])?;
            let left: i64 = c.query_row("SELECT COUNT(*) FROM resource_access", [], |r| r.get(0))?;
            assert_eq!(left, 0, "they go with their environment");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_database_newer_than_the_binary_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("freightdock.db");
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", (MIGRATIONS.len() + 1) as i64).unwrap();
        }
        let err = Db::open(&path).err().expect("a newer schema must be refused").to_string();
        assert!(err.contains("newer than this Freightdock"), "{err}");
    }

    #[tokio::test]
    async fn removing_an_environment_forgets_its_swarm_stacks() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute("INSERT INTO environments (id, name, kind, url, created_at) VALUES (5, 'far', 'agent', '', 0)", [])?;
            c.execute("INSERT INTO swarm_stacks (env, name, compose, updated_at) VALUES (5, 'app', 'services: {}', 0)", [])?;
            c.execute("DELETE FROM environments WHERE id = 5", [])?;
            let left: i64 = c.query_row("SELECT COUNT(*) FROM swarm_stacks", [], |r| r.get(0))?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn removing_a_user_ends_their_sessions() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| {
            c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'a', 'x', 'admin', 0)", [])?;
            c.execute("INSERT INTO sessions (token_hash, user_id, csrf, created_at, expires_at, last_seen_at) VALUES ('t', 1, 'c', 0, 1, 0)", [])?;
            c.execute("DELETE FROM users WHERE id = 1", [])?;
            let left: i64 = c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
            assert_eq!(left, 0);
            Ok(())
        })
        .await
        .unwrap();
    }
}
