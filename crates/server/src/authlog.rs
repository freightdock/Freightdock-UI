//! The auth log: every sign-in and failed one, every sign-out, everybody SSO turned away, every account linked to the
//! provider, and API tokens' use (at most once a minute a token, as its last use is noted). Apart from the activity log,
//! which is about what was changed: who came in, how and from where is a question of its own ("Activity" 3).

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Admin;
use crate::auth::User;
use crate::db::{Db, now, rusqlite::params};
use crate::error::ApiResult;
use crate::state::App;

/// One entry: what happened (`login`, `login.failed`, `logout`, `login.sso`, `login.sso.failed`, `sso.link`,
/// `token.use`), how (`password`, `sso`, `token`), whether it let somebody in.
pub struct Event<'a> {
    pub event: &'static str,
    pub method: &'static str,
    pub ok: bool,
    pub user: Option<&'a User>,
    /// The name tried, when nobody was signed in by it.
    pub name: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub detail: Option<String>,
}

pub async fn record(db: &Db, e: Event<'_>) {
    let (uid, uname) = match e.user {
        Some(u) => (Some(u.id), Some(u.name.clone())),
        None => (None, e.name.map(|n| n.chars().take(256).collect::<String>())),
    };
    let ua = e.user_agent.map(|u| u.chars().take(256).collect::<String>());
    let (event, method, ok, ip, detail) = (e.event, e.method, e.ok, e.ip, e.detail);
    let r = db
        .call(move |c| {
            write(c, event, method, ok, uid, uname.as_deref(), ip.as_deref(), ua.as_deref(), detail.as_deref())?;
            Ok(())
        })
        .await;
    if let Err(err) = r {
        tracing::error!("the auth log could not be written: {err:#}");
    }
}

/// The row itself, for a caller already holding a connection (a token's lookup).
#[allow(clippy::too_many_arguments)]
pub fn write(
    c: &crate::db::rusqlite::Connection,
    event: &str,
    method: &str,
    ok: bool,
    uid: Option<i64>,
    uname: Option<&str>,
    ip: Option<&str>,
    ua: Option<&str>,
    detail: Option<&str>,
) -> crate::db::rusqlite::Result<()> {
    c.execute(
        "INSERT INTO auth_log (at, event, method, ok, user_id, user_name, ip, user_agent, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![now(), event, method, ok, uid, uname, ip, ua, detail],
    )?;
    Ok(())
}

/// How long the auth log keeps an entry: a year, so it cannot grow for ever ("Activity" 4).
pub const KEEP_DAYS: i64 = 365;

/// Forgets the entries from before `before`; answers how many went.
pub async fn prune(db: &Db, before: i64) -> anyhow::Result<usize> {
    db.call(move |c| Ok(c.execute("DELETE FROM auth_log WHERE at < ?1", [before])?)).await
}

/// Forgets what is older than `KEEP_DAYS`, at start and every hour, until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    loop {
        if let Err(e) = prune(&app.db, now() - KEEP_DAYS * 86400).await {
            tracing::warn!("could not forget old auth log entries: {e:#}");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
            _ = stop.cancelled() => return,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct Filter {
    /// An event, or a family: `login` is every kind of sign-in.
    event: Option<String>,
    method: Option<String>,
    /// `true` for what let somebody in, `false` for what was refused.
    ok: Option<bool>,
    user: Option<String>,
    ip: Option<String>,
    /// From and to these Unix times.
    since: Option<i64>,
    until: Option<i64>,
    before: Option<i64>,
    limit: Option<i64>,
    /// `csv` for a file.
    format: Option<String>,
}

/// `GET /api/auth-log`: newest first, a page at a time, or as CSV (`format=csv`, up to 100 000 rows).
pub async fn list(State(app): State<App>, _: Admin, Query(f): Query<Filter>) -> ApiResult<Response> {
    let csv = f.format.as_deref() == Some("csv");
    let limit = if csv { 100_000 } else { f.limit.unwrap_or(100).clamp(1, 500) };
    let rows = app
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT id, at, event, method, ok, user_id, user_name, ip, user_agent, detail FROM auth_log
                 WHERE (?1 IS NULL OR event = ?1 OR event LIKE ?1 || '.%')
                   AND (?2 IS NULL OR method = ?2)
                   AND (?3 IS NULL OR ok = ?3)
                   AND (?4 IS NULL OR user_name = ?4 COLLATE NOCASE)
                   AND (?5 IS NULL OR ip = ?5)
                   AND (?6 IS NULL OR at >= ?6)
                   AND (?7 IS NULL OR at <= ?7)
                   AND (?8 IS NULL OR id < ?8)
                 ORDER BY id DESC LIMIT ?9",
            )?;
            let rows = st.query_map(params![f.event, f.method, f.ok, f.user, f.ip, f.since, f.until, f.before, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "event": r.get::<_, String>(2)?, "method": r.get::<_, String>(3)?,
                    "ok": r.get::<_, bool>(4)?, "user_id": r.get::<_, Option<i64>>(5)?, "user_name": r.get::<_, Option<String>>(6)?,
                    "ip": r.get::<_, Option<String>>(7)?, "user_agent": r.get::<_, Option<String>>(8)?, "detail": r.get::<_, Option<String>>(9)?,
                }))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    if csv {
        return Ok(csv_file("freightdock-auth-log.csv", &["at", "event", "method", "ok", "user_name", "ip", "user_agent", "detail"], &rows));
    }
    Ok(axum::Json(json!({ "entries": rows })).into_response())
}

/// One CSV field: quoted when it holds a comma, a quote or a line break, and a formula's first character defused (a
/// spreadsheet would run `=...` written by whoever chose a user name).
pub fn field(v: &Value) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) && !s.parse::<f64>().is_ok() { format!("'{s}") } else { s };
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

/// Rows as a CSV file to download: a header of `columns`, then each row's fields in that order; `at` as an ISO time.
pub fn csv_file(name: &str, columns: &[&str], rows: &[Value]) -> Response {
    let mut out = columns.join(",") + "\r\n";
    for r in rows {
        let line: Vec<String> = columns
            .iter()
            .map(|c| {
                if *c == "at" {
                    r["at"].as_i64().and_then(chrono_like).map(Value::String).map_or_else(String::new, |v| field(&v))
                } else {
                    field(&r[*c])
                }
            })
            .collect();
        out += &line.join(",");
        out += "\r\n";
    }
    (
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))],
        out,
    )
        .into_response()
}

/// A Unix time as `2026-11-09T21:14:09Z`.
fn chrono_like(t: i64) -> Option<String> {
    let d = time::OffsetDateTime::from_unix_timestamp(t).ok()?;
    d.format(&time::format_description::well_known::Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_field_is_quoted_when_it_must_be_and_a_formula_is_defused() {
        assert_eq!(field(&json!("plain")), "plain");
        assert_eq!(field(&json!("a,b")), "\"a,b\"");
        assert_eq!(field(&json!("say \"hi\"")), "\"say \"\"hi\"\"\"");
        assert_eq!(field(&json!("=HYPERLINK(\"x\")")), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(field(&json!(-5)), "-5", "a number stays a number");
        assert_eq!(field(&Value::Null), "");
        assert_eq!(field(&json!(true)), "true");
        assert_eq!(chrono_like(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn the_auth_log_forgets_what_is_older_than_it_keeps() {
        let db = Db::open_in_memory().unwrap();
        let t = now();
        db.call(move |c| {
            c.execute("INSERT INTO auth_log (at, event, method, ok) VALUES (?1, 'login', 'password', 1)", [t - (KEEP_DAYS + 1) * 86400])?;
            c.execute("INSERT INTO auth_log (at, event, method, ok) VALUES (?1, 'login', 'password', 1)", [t - 86400])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(prune(&db, t - KEEP_DAYS * 86400).await.unwrap(), 1, "the one older than a year goes");
        let left: i64 = db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM auth_log", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(left, 1);
    }

    #[tokio::test]
    async fn a_name_tried_is_kept_no_longer_than_a_user_agent() {
        let db = Db::open_in_memory().unwrap();
        record(&db, Event { event: "login.sso.failed", method: "sso", ok: false, user: None, name: Some("y".repeat(10_000)), ip: None, user_agent: None, detail: None }).await;
        let n: i64 = db.call(|c| Ok(c.query_row("SELECT length(user_name) FROM auth_log", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(n, 256);
    }
}
