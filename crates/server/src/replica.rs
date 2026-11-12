//! The database, replicated by the cluster's managers ("Cluster" 9, Freightdock-Manager's G8.1): Freightdock keeps its
//! SQLite and its SQL, and every row it writes is also sent to the managers, who keep a snapshot of the whole database
//! and the changes after it through Raft. A Freightdock starting on any node rebuilds its database from them, so the
//! managers can move the UI from node to node.
//!
//! The changes are recorded by triggers (`install`): each replicated table writes every insert, update and delete, the
//! row's key and its new values as JSON, into `_replog`. After a database call, `drain` takes what is there, in order,
//! and the change goes to the managers numbered; they take only the next number, so a second Freightdock writing
//! beside this one is refused and this one must load again. `apply` replays a change on another database: an update
//! by key, else an insert (never `INSERT OR REPLACE`, which would cascade a delete through foreign keys).
//!
//! The tables of one host (`LOCAL_TABLES`: statistics, the single-host environments) are not replicated.

use anyhow::{Context, Result, bail};
use r2d2_sqlite::rusqlite::{self, Connection, params_from_iter, types::Value as Sql};
use serde_json::{Map, Value, json};

/// Tables that stay on this host.
pub const LOCAL_TABLES: &[&str] = &["container_stats", "environments", "_replog"];

/// A replicated table: its name, its columns, which of them are its key (none: its rowid) and which hold blobs.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<String>,
    pub key: Vec<String>,
    pub blobs: Vec<String>,
}

fn quote(id: &str) -> String {
    format!("\"{}\"", id.replace('"', "\"\""))
}

/// The tables to replicate, from the schema as it is now.
pub fn tables(c: &Connection) -> Result<Vec<Table>> {
    let names: Vec<String> = c
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for name in names.into_iter().filter(|n| !LOCAL_TABLES.contains(&n.as_str())) {
        let mut cols: Vec<(String, String, i64)> = c.prepare(&format!("PRAGMA table_info({})", quote(&name)))?.query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
        let mut key: Vec<(i64, String)> = cols.iter().filter(|c| c.2 > 0).map(|c| (c.2, c.0.clone())).collect();
        key.sort();
        let blobs = cols.iter().filter(|c| c.1.to_ascii_uppercase().contains("BLOB")).map(|c| c.0.clone()).collect();
        out.push(Table { name, columns: cols.drain(..).map(|c| c.0).collect(), key: key.into_iter().map(|k| k.1).collect(), blobs });
    }
    Ok(out)
}

impl Table {
    /// `json_object(...)` of `cols` from the trigger's row (`NEW` or `OLD`), blobs as hex.
    fn object(&self, row: &str, cols: &[String]) -> String {
        let parts: Vec<String> = cols
            .iter()
            .map(|c| {
                let v = format!("{row}.{}", quote(c));
                let v = if self.blobs.contains(c) { format!("hex({v})") } else { v };
                format!("'{}', {v}", c.replace('\'', "''"))
            })
            .collect();
        format!("json_object({})", parts.join(", "))
    }

    fn key_object(&self, row: &str) -> String {
        if self.key.is_empty() { format!("json_object('rowid', {row}.rowid)") } else { self.object(row, &self.key) }
    }
}

/// Makes `_replog` and the triggers that fill it, for the schema as it is now (again after a migration).
pub fn install(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS _replog (id INTEGER PRIMARY KEY, tbl TEXT NOT NULL, op TEXT NOT NULL, k TEXT NOT NULL, r TEXT)")?;
    uninstall(c)?;
    for t in tables(c)? {
        let n = quote(&t.name);
        let lit = t.name.replace('\'', "''");
        let all = t.object("NEW", &t.columns);
        // A table with no key of its own is kept by its rowid, which the row then carries.
        let row = if t.key.is_empty() { format!("json_insert({all}, '$.rowid', NEW.rowid)") } else { all.clone() };
        c.execute_batch(&format!(
            "CREATE TRIGGER {} AFTER INSERT ON {n} BEGIN INSERT INTO _replog (tbl, op, k, r) VALUES ('{lit}', 'u', {}, {}); END;
             CREATE TRIGGER {} AFTER UPDATE ON {n} BEGIN INSERT INTO _replog (tbl, op, k, r) VALUES ('{lit}', 'u', {}, {}); END;
             CREATE TRIGGER {} AFTER DELETE ON {n} BEGIN INSERT INTO _replog (tbl, op, k, r) VALUES ('{lit}', 'd', {}, NULL); END;",
            quote(&format!("_rep_i_{}", t.name)),
            t.key_object("NEW"),
            row,
            quote(&format!("_rep_u_{}", t.name)),
            t.key_object("OLD"),
            row,
            quote(&format!("_rep_d_{}", t.name)),
            t.key_object("OLD"),
        ))?;
    }
    Ok(())
}

/// Drops the triggers (to load a database, or before making them again).
pub fn uninstall(c: &Connection) -> Result<()> {
    let names: Vec<String> = c.prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger' AND name LIKE '\\_rep\\_%' ESCAPE '\\'")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for n in names {
        c.execute_batch(&format!("DROP TRIGGER {}", quote(&n)))?;
    }
    Ok(())
}

/// What `_replog` holds, oldest first: the last id taken and the change as JSON (`[{t, op, k, r}]`), or None.
pub fn drain(c: &Connection) -> Result<Option<(i64, String)>> {
    let rows: Vec<(i64, String, String, String, Option<String>)> = c.prepare("SELECT id, tbl, op, k, r FROM _replog ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    let Some(last) = rows.last().map(|r| r.0) else { return Ok(None) };
    let ops: Vec<Value> = rows
        .into_iter()
        .map(|(_, t, op, k, r)| Ok(json!({ "t": t, "op": op, "k": serde_json::from_str::<Value>(&k)?, "r": r.map(|r| serde_json::from_str::<Value>(&r)).transpose()? })))
        .collect::<Result<_>>()?;
    Ok(Some((last, serde_json::to_string(&ops)?)))
}

/// Forgets what `drain` took, once the managers have it.
pub fn forget(c: &Connection, upto: i64) -> Result<()> {
    c.execute("DELETE FROM _replog WHERE id <= ?1", [upto])?;
    Ok(())
}

fn sql(v: &Value, blob: bool) -> Result<Sql> {
    Ok(match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(Sql::Integer).or_else(|| n.as_f64().map(Sql::Real)).context("a number SQLite cannot hold")?,
        Value::String(s) if blob => Sql::Blob(hex::decode(s).context("a blob that is not hex")?),
        Value::String(s) => Sql::Text(s.clone()),
        _ => bail!("a value SQLite cannot hold"),
    })
}

/// Replays a change `drain` made on this database, whose triggers must be off (`uninstall`).
pub fn apply(c: &Connection, change: &str) -> Result<()> {
    let ops: Vec<Value> = serde_json::from_str(change).context("a change that is not JSON")?;
    let schema: std::collections::HashMap<String, Table> = tables(c)?.into_iter().map(|t| (t.name.clone(), t)).collect();
    for op in ops {
        let name = op["t"].as_str().context("a change with no table")?;
        let t = schema.get(name).with_context(|| format!("a change to {name}, a table this database does not replicate"))?;
        let key = op["k"].as_object().context("a change with no key")?;
        let (kc, kv): (Vec<&String>, Vec<Sql>) = key.iter().map(|(k, v)| Ok((k, sql(v, t.blobs.contains(k))?))).collect::<Result<Vec<_>>>()?.into_iter().unzip();
        let wher = kc.iter().enumerate().map(|(i, k)| format!("{} = ?{}", if *k == "rowid" { "rowid".into() } else { quote(k) }, i + 1)).collect::<Vec<_>>().join(" AND ");
        let n = quote(name);
        match op["op"].as_str() {
            Some("d") => {
                c.execute(&format!("DELETE FROM {n} WHERE {wher}"), params_from_iter(kv))?;
            }
            Some("u") => {
                let row: &Map<String, Value> = op["r"].as_object().context("a change with no row")?;
                let (rc, rv): (Vec<&String>, Vec<Sql>) = row.iter().map(|(k, v)| Ok((k, sql(v, t.blobs.contains(k))?))).collect::<Result<Vec<_>>>()?.into_iter().unzip();
                let col = |k: &String| if k == "rowid" { "rowid".to_string() } else { quote(k) };
                let set = rc.iter().enumerate().map(|(i, k)| format!("{} = ?{}", col(k), i + 1 + kv.len())).collect::<Vec<_>>().join(", ");
                let changed = c.execute(&format!("UPDATE {n} SET {set} WHERE {wher}"), params_from_iter(kv.iter().cloned().chain(rv.iter().cloned())))?;
                if changed == 0 {
                    let cols = rc.iter().map(|k| col(k)).collect::<Vec<_>>().join(", ");
                    let marks = (1..=rv.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
                    c.execute(&format!("INSERT INTO {n} ({cols}) VALUES ({marks})"), params_from_iter(rv))?;
                }
            }
            other => bail!("a change of a kind this Freightdock does not know: {other:?}"),
        }
    }
    Ok(())
}

/// The database as one value, to send as a snapshot: SQLite's own file, made with its backup API (so writers may go
/// on) in `scratch` (a folder of its own), then read and removed.
pub fn serialize(c: &Connection, scratch: &std::path::Path) -> Result<Vec<u8>> {
    let path = scratch.join(format!("snapshot-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let mut to = Connection::open(&path)?;
        rusqlite::backup::Backup::new(c, &mut to)?.run_to_completion(256, std::time::Duration::ZERO, None)?;
        to.execute_batch("DELETE FROM _replog; VACUUM;").ok();
    }
    let bytes = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

/// What the managers hold, as this Freightdock loaded it at start.
#[derive(Debug)]
pub struct Loaded {
    /// The latest change they hold, now in the database.
    pub head: u64,
    /// They held nothing: this Freightdock's own database is the first snapshot.
    pub fresh: bool,
    /// The schema version of what they held (to send a snapshot when a migration changes it).
    pub schema: Option<i64>,
    /// Their snapshot carried the certificate's CA; one from an older Freightdock did not, and is replaced by one that
    /// does, so the CA made here is the one a move takes along.
    pub tls_ca: bool,
}

/// A snapshot as Freightdock sends it: the database file and the key its secrets are sealed with, which must travel
/// with it (the managers keep both; only an admin token reads them).
#[derive(serde::Serialize, serde::Deserialize)]
struct Payload {
    v: u32,
    db: String,
    key: String,
    /// The CA of Freightdock's own HTTPS certificate (`tls/ca.pem`, `tls/ca.key`), so a Freightdock started on another
    /// node makes its certificate from the same CA and a browser that trusted it once still does (G8.5). Absent from
    /// an older snapshot, and when there is none (one's own certificate, or plain HTTP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tls_ca: Option<(String, String)>,
}

/// The CA of Freightdock's own certificate in `data_dir`, when there is one.
fn tls_ca(data_dir: &std::path::Path) -> Option<(String, String)> {
    let dir = data_dir.join("tls");
    Some((std::fs::read_to_string(dir.join("ca.pem")).ok()?, std::fs::read_to_string(dir.join("ca.key")).ok()?))
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Loads the managers' copy of the database into `data_dir` (`freightdock.db`, `secret.key`), before the database
/// is opened; with none there, keeps the local one, which becomes the first snapshot. Waits for the managers.
pub async fn load(cluster: &crate::cluster::Cluster, data_dir: &std::path::Path) -> Result<Loaded> {
    use base64::Engine;
    let v = loop {
        match cluster.call(reqwest::Method::GET, "/v1/ui-db", None).await {
            Ok((_, v)) => break v,
            Err(e) => {
                tracing::warn!("the managers do not give the database yet ({}): trying again", e.message);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    };
    let head = v["head"].as_u64().unwrap_or(0);
    let Some(snapshot) = v["snapshot"].as_str() else {
        anyhow::ensure!(head == 0, "the managers hold changes but no snapshot");
        tracing::info!("the managers hold no database yet: this one is the first");
        return Ok(Loaded { head: 0, fresh: true, schema: None, tls_ca: false });
    };
    let p: Payload = serde_json::from_str(snapshot).context("the managers' snapshot is not one this Freightdock reads")?;
    let path = data_dir.join("freightdock.db");
    for f in ["freightdock.db", "freightdock.db-wal", "freightdock.db-shm"] {
        let _ = std::fs::remove_file(data_dir.join(f));
    }
    std::fs::write(&path, b64().decode(&p.db)?)?;
    crate::tls::write_private(&data_dir.join("secret.key"), &b64().decode(&p.key)?)?;
    let had_ca = p.tls_ca.is_some();
    if let Some((pem, key)) = &p.tls_ca {
        let dir = data_dir.join("tls");
        std::fs::create_dir_all(&dir)?;
        if tls_ca(data_dir).as_ref().is_none_or(|(p0, _)| p0 != pem) {
            // A certificate from another CA is made again from this one (`tls::prepare`).
            let _ = std::fs::remove_file(dir.join("cert.pem"));
        }
        crate::tls::write_private(&dir.join("ca.key"), key.as_bytes())?;
        std::fs::write(dir.join("ca.pem"), pem)?;
    }
    let changes: Vec<(u64, String)> = v["changes"].as_array().into_iter().flatten().filter_map(|c| Some((c["seq"].as_u64()?, c["data"].as_str()?.to_string()))).collect();
    let n = changes.len();
    let schema = tokio::task::spawn_blocking(move || -> Result<i64> {
        let c = Connection::open(&path)?;
        uninstall(&c)?;
        c.execute_batch("PRAGMA foreign_keys = OFF; BEGIN;")?;
        for (seq, data) in &changes {
            apply(&c, data).with_context(|| format!("replaying change {seq}"))?;
        }
        c.execute_batch("COMMIT;")?;
        Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    })
    .await??;
    tracing::info!("loaded the database from the managers: snapshot and {n} changes, at {head}");
    Ok(Loaded { head, fresh: false, schema: Some(schema), tls_ca: had_ca })
}

/// Sends what the database writes to the managers, in order and numbered (see the module).
pub struct Replicator {
    cluster: std::sync::Arc<crate::cluster::Cluster>,
    data_dir: std::path::PathBuf,
    /// The latest change the managers took from here; held while one is sent, so they go one at a time.
    head: tokio::sync::Mutex<u64>,
    since_snapshot: std::sync::atomic::AtomicU64,
    stale: std::sync::atomic::AtomicBool,
}

/// After this many changes a snapshot replaces them at the managers.
const SNAPSHOT_EVERY: u64 = 200;

impl Replicator {
    /// Triggers on, and a first snapshot when the managers held none, a migration changed the schema, or theirs lacks
    /// the certificate's CA there is here.
    pub async fn start(pool: &crate::db::Pool, cluster: std::sync::Arc<crate::cluster::Cluster>, data_dir: std::path::PathBuf, loaded: Loaded) -> Result<std::sync::Arc<Self>> {
        let p = pool.clone();
        let schema = tokio::task::spawn_blocking(move || -> Result<i64> {
            let c = p.get()?;
            install(&c)?;
            c.execute_batch("DELETE FROM _replog")?;
            Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?)
        })
        .await??;
        let r = std::sync::Arc::new(Replicator { cluster, data_dir, head: tokio::sync::Mutex::new(loaded.head), since_snapshot: Default::default(), stale: Default::default() });
        if loaded.fresh || loaded.schema != Some(schema) || (!loaded.tls_ca && tls_ca(&r.data_dir).is_some()) {
            let head = r.head.lock().await;
            r.snapshot(pool, *head).await?;
        }
        Ok(r)
    }

    /// Another Freightdock wrote since: this one must load again (it stops, and is started again).
    pub fn is_stale(&self) -> bool {
        self.stale.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Value) -> Result<()> {
        let mut tries = 0;
        loop {
            match self.cluster.call(method.clone(), path, Some(body.clone())).await {
                Ok(_) => return Ok(()),
                Err(e) if e.status == axum::http::StatusCode::CONFLICT => {
                    self.stale.store(true, std::sync::atomic::Ordering::SeqCst);
                    bail!("another Freightdock wrote to the cluster's database since this one loaded it: {}", e.message);
                }
                // A new leader is being chosen, or the manager tried does not answer: a few seconds, then give up.
                Err(e) if tries < 8 && matches!(e.status.as_u16(), 502 | 503) => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(250 * tries)).await;
                }
                Err(e) => bail!("the managers did not take the change: {}", e.message),
            }
        }
    }

    async fn snapshot(&self, pool: &crate::db::Pool, head: u64) -> Result<()> {
        use base64::Engine;
        let (p, dir) = (pool.clone(), self.data_dir.clone());
        let db = tokio::task::spawn_blocking(move || serialize(&*p.get()?, &dir)).await??;
        let key = std::fs::read(self.data_dir.join("secret.key")).context("the secret key, to send with the snapshot")?;
        let data = serde_json::to_string(&Payload { v: 1, db: b64().encode(db), key: b64().encode(key), tls_ca: tls_ca(&self.data_dir) })?;
        self.send(reqwest::Method::PUT, "/v1/ui-db/snapshot", json!({ "head": head, "data": data })).await?;
        self.since_snapshot.store(0, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("sent the managers a snapshot of the database at {head}");
        Ok(())
    }

    /// Sends what was written since the last push, if anything. A write is only safe once this has returned.
    pub async fn push(&self, pool: &crate::db::Pool) -> Result<()> {
        if self.is_stale() {
            bail!("this Freightdock's database is behind the cluster's; it is starting again");
        }
        let mut head = self.head.lock().await;
        let p = pool.clone();
        let Some((last, data)) = tokio::task::spawn_blocking(move || drain(&*p.get()?)).await?? else { return Ok(()) };
        self.send(reqwest::Method::POST, "/v1/ui-db/changes", json!({ "seq": *head + 1, "data": data })).await?;
        *head += 1;
        let p = pool.clone();
        tokio::task::spawn_blocking(move || forget(&*p.get()?, last)).await??;
        if self.since_snapshot.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= SNAPSHOT_EVERY {
            self.snapshot(pool, *head).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        PRAGMA foreign_keys = ON;
        CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, avatar BLOB);
        CREATE TABLE sessions (token TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, seen INTEGER) WITHOUT ROWID;
        CREATE TABLE notes (body TEXT, score REAL);
        CREATE TABLE team_members (team INTEGER, user INTEGER, PRIMARY KEY (team, user));
        CREATE TABLE container_stats (at INTEGER, cpu REAL);";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn dump(c: &Connection) -> Vec<String> {
        let mut out = vec![];
        for t in ["users", "sessions", "notes", "team_members"] {
            let mut s = c.prepare(&format!("SELECT rowid, * FROM {t} ORDER BY 1")).or_else(|_| c.prepare(&format!("SELECT * FROM {t} ORDER BY 1"))).unwrap();
            let n = s.column_count();
            let rows: Vec<String> = s.query_map([], |r| Ok((0..n).map(|i| format!("{:?}", r.get::<_, Sql>(i).unwrap())).collect::<Vec<_>>().join("|"))).unwrap().collect::<Result<_, _>>().unwrap();
            out.push(format!("{t}: {}", rows.join("; ")));
        }
        out
    }

    #[test]
    fn the_tables_their_keys_and_blobs_and_not_the_local_ones() {
        let t = tables(&db()).unwrap();
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["notes", "sessions", "team_members", "users"]);
        let by = |n: &str| t.iter().find(|t| t.name == n).unwrap().clone();
        assert_eq!(by("users").key, ["id"]);
        assert_eq!(by("users").blobs, ["avatar"]);
        assert_eq!(by("team_members").key, ["team", "user"]);
        assert!(by("notes").key.is_empty(), "no key: its rowid");
    }

    #[test]
    fn every_write_replays_on_another_database_as_it_was() {
        let a = db();
        install(&a).unwrap();
        a.execute_batch(
            "INSERT INTO users (id, name, avatar) VALUES (1, 'ann', x'00ff10'), (2, 'bob', NULL);
             INSERT INTO sessions VALUES ('s1', 1, 10), ('s2', 2, 20);
             INSERT INTO notes VALUES ('it''s \"quoted\"', 1.5), ('two', NULL);
             INSERT INTO team_members VALUES (7, 1), (7, 2);
             INSERT INTO container_stats VALUES (1, 0.5);",
        )
        .unwrap();
        let (last, first) = drain(&a).unwrap().unwrap();
        forget(&a, last).unwrap();
        a.execute_batch(
            "UPDATE users SET name = 'ann b' WHERE id = 1;
             DELETE FROM users WHERE id = 2;
             UPDATE notes SET score = 2 WHERE body = 'two';
             UPDATE team_members SET user = 9 WHERE team = 7 AND user = 2;
             DELETE FROM team_members WHERE team = 7 AND user = 1;",
        )
        .unwrap();
        let (_, second) = drain(&a).unwrap().unwrap();
        assert!(!first.contains("container_stats"), "statistics stay here");

        let b = db();
        b.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        apply(&b, &first).unwrap();
        apply(&b, &second).unwrap();
        assert_eq!(dump(&b), dump(&a));
        let a_now = dump(&a);
        assert!(a_now[3].contains("Integer(9)") && !a_now[1].contains("s2"), "a key changed, a delete cascaded: {a_now:?}");
    }

    #[test]
    fn nothing_written_is_nothing_to_send_and_a_snapshot_carries_no_log() {
        let a = db();
        install(&a).unwrap();
        assert_eq!(drain(&a).unwrap(), None);
        a.execute("INSERT INTO users (id, name) VALUES (1, 'ann')", []).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let bytes = serialize(&a, dir.path()).unwrap();
        std::fs::write(dir.path().join("x.db"), &bytes).unwrap();
        let b = Connection::open(dir.path().join("x.db")).unwrap();
        assert_eq!(b.query_row("SELECT count(*) FROM _replog", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.query_row("SELECT name FROM users", [], |r| r.get::<_, String>(0)).unwrap(), "ann");
    }

    /// A manager's `/v1/ui-db` as far as Freightdock uses it, with the managers' rules: the next number only, a
    /// snapshot only at the head.
    async fn fake_managers() -> String {
        use axum::{Json, Router, extract::State, http::StatusCode, routing::{get, post, put}};
        use std::sync::{Arc, Mutex};
        #[derive(Default)]
        struct Fake { head: u64, snapshot: Option<String>, changes: Vec<(u64, String)> }
        type S = Arc<Mutex<Fake>>;
        let app = Router::new()
            .route("/v1/ui-db", get(|State(s): State<S>| async move {
                let f = s.lock().unwrap();
                Json(json!({ "head": f.head, "snapshot": f.snapshot, "changes": f.changes.iter().map(|(q, d)| json!({ "seq": q, "data": d })).collect::<Vec<_>>() }))
            }))
            .route("/v1/ui-db/changes", post(|State(s): State<S>, Json(b): Json<Value>| async move {
                let mut f = s.lock().unwrap();
                let seq = b["seq"].as_u64().unwrap();
                if seq != f.head + 1 || f.snapshot.is_none() { return StatusCode::CONFLICT; }
                f.head = seq;
                f.changes.push((seq, b["data"].as_str().unwrap().to_string()));
                StatusCode::NO_CONTENT
            }))
            .route("/v1/ui-db/snapshot", put(|State(s): State<S>, Json(b): Json<Value>| async move {
                let mut f = s.lock().unwrap();
                let head = b["head"].as_u64().unwrap();
                if head != f.head { return StatusCode::CONFLICT; }
                f.snapshot = Some(b["data"].as_str().unwrap().to_string());
                f.changes.retain(|c| c.0 > head);
                StatusCode::NO_CONTENT
            }))
            .with_state(S::default());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn node(api: &str, dir: &std::path::Path) -> (crate::db::Db, std::sync::Arc<Replicator>) {
        let token = dir.join("api.token");
        std::fs::write(&token, "t").unwrap();
        let c = std::sync::Arc::new(crate::cluster::Cluster::new(api, token, None).unwrap());
        let loaded = load(&c, dir).await.unwrap();
        crate::secret::Sealer::open_or_make(&dir.join("secret.key")).unwrap();
        // As `main` does: the HTTPS certificate's CA made (or the one loaded kept) before the first snapshot.
        crate::ca::Ca::open_or_make_named(&dir.join("tls"), "Freightdock").unwrap();
        let mut db = crate::db::Db::open(&dir.join("freightdock.db")).unwrap();
        let r = db.replicate(c, dir.to_path_buf(), loaded).await.unwrap();
        (db, r)
    }

    fn add_user(name: &'static str) -> impl FnOnce(&mut Connection) -> Result<()> + Send + 'static {
        move |c| Ok(c.execute("INSERT INTO users (name, password_hash, role, created_at) VALUES (?1, 'x', 'user', 1)", [name]).map(|_| ())?)
    }

    async fn users(db: &crate::db::Db) -> Vec<String> {
        db.call(|c| Ok(c.prepare("SELECT name FROM users ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
    }

    #[tokio::test]
    async fn a_freightdock_on_another_node_loads_what_the_first_wrote_and_only_one_writes() {
        let api = fake_managers().await;
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, ra) = node(&api, a_dir.path()).await;
        a.call(add_user("ann")).await.unwrap();

        let (b, _) = node(&api, b_dir.path()).await;
        assert_eq!(users(&b).await, ["ann"], "the first's snapshot and change, loaded on the second");
        assert_eq!(std::fs::read(a_dir.path().join("secret.key")).unwrap(), std::fs::read(b_dir.path().join("secret.key")).unwrap(), "its secrets open there too");
        for f in ["tls/ca.pem", "tls/ca.key"] {
            assert_eq!(std::fs::read(a_dir.path().join(f)).unwrap(), std::fs::read(b_dir.path().join(f)).unwrap(), "{f}: a browser that trusted the first trusts the second");
        }
        b.call(add_user("bob")).await.unwrap();

        assert!(a.call(add_user("cat")).await.is_err(), "the first is behind now: its write is refused");
        assert!(ra.is_stale());

        let c_dir = tempfile::tempdir().unwrap();
        let (c, _) = node(&api, c_dir.path()).await;
        assert_eq!(users(&c).await, ["ann", "bob"]);
    }

    #[tokio::test]
    async fn a_snapshot_from_before_the_certificate_s_ca_travelled_is_replaced_by_one_with_it() {
        use base64::Engine;
        let api = fake_managers().await;
        let c = crate::cluster::Cluster::new(&api, crate::cluster::Given::Value("t".into()), None).unwrap();
        let old = tempfile::tempdir().unwrap();
        let src = crate::db::Db::open(&old.path().join("freightdock.db")).unwrap();
        let dir_ = old.path().to_path_buf();
        let bytes = src.call(move |c| serialize(c, &dir_)).await.unwrap();
        let data = json!({ "v": 1, "db": b64().encode(bytes), "key": b64().encode([7u8; 32]) }).to_string();
        c.call(reqwest::Method::PUT, "/v1/ui-db/snapshot", Some(json!({ "head": 0, "data": data }))).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        node(&api, dir.path()).await;
        let (_, v) = c.call(reqwest::Method::GET, "/v1/ui-db", None).await.unwrap();
        let p: Payload = serde_json::from_str(v["snapshot"].as_str().unwrap()).unwrap();
        assert_eq!(p.tls_ca.map(|t| t.0), Some(std::fs::read_to_string(dir.path().join("tls/ca.pem")).unwrap()), "the CA made here, at the managers now");
    }
}
