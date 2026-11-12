//! The activity log: who did what to which thing, from where. Every change made through the API is written here, and
//! so is every sign-in, failed or not. A change made with an API token names the token (`via`).

use crate::auth::User;
use crate::db::{Db, now, rusqlite::params};

pub struct Entry {
    pub action: &'static str,
    pub target: Option<String>,
    pub detail: Option<String>,
}

pub async fn record(db: &Db, user: Option<&User>, ip: Option<String>, e: Entry) {
    let (uid, uname) = user.map_or((None, None), |u| (Some(u.id), Some(u.name.clone())));
    // A change made with an API token says which, so a program's doings can be told from the person's own.
    let via = user.and_then(|u| u.token.as_ref()).map(|t| format!("token {}", t.name));
    let r = db
        .call(move |c| {
            c.execute(
                "INSERT INTO audit (at, user_id, user_name, action, target, detail, ip, via) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![now(), uid, uname, e.action, e.target, e.detail, ip, via],
            )?;
            Ok(())
        })
        .await;
    if let Err(err) = r {
        tracing::error!("the activity log could not be written: {err:#}");
    }
}

/// Who made a change: a person, from an address; or nobody signed in, for what Freightdock does by itself or on a call that
/// needs no session (the Git poller, a webhook), which then says so in the entry's detail.
#[derive(Clone, Debug, Default)]
pub struct Who {
    pub user: Option<User>,
    pub ip: Option<String>,
}

impl From<&crate::api::Allowed> for Who {
    fn from(a: &crate::api::Allowed) -> Self {
        Who { user: Some(a.0.session.user.clone()), ip: a.0.ip.clone() }
    }
}

impl From<&crate::api::Admin> for Who {
    fn from(a: &crate::api::Admin) -> Self {
        Who { user: Some(a.0.session.user.clone()), ip: a.0.ip.clone() }
    }
}

pub async fn record_by(db: &Db, who: &Who, e: Entry) {
    record(db, who.user.as_ref(), who.ip.clone(), e).await
}

#[derive(serde::Deserialize, Default)]
pub struct Filter {
    /// An action, or a family of them: `login` is `login` and `login.failed`.
    action: Option<String>,
    user: Option<String>,
    /// Entries before this id, for the next page.
    before: Option<i64>,
    limit: Option<i64>,
    /// From and to these Unix times.
    since: Option<i64>,
    until: Option<i64>,
    /// `csv` for a file.
    format: Option<String>,
}

/// `GET /api/audit`: the log, newest first, a page at a time, or as CSV (`format=csv`, up to 100 000 rows).
pub async fn list(
    axum::extract::State(app): axum::extract::State<crate::state::App>,
    _: crate::api::Admin,
    axum::extract::Query(f): axum::extract::Query<Filter>,
) -> crate::error::ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let csv = f.format.as_deref() == Some("csv");
    let limit = if csv { 100_000 } else { f.limit.unwrap_or(100).clamp(1, 500) };
    let entries = app
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT id, at, user_id, user_name, action, target, detail, ip, via FROM audit
                 WHERE (?1 IS NULL OR action = ?1 OR action LIKE ?1 || '.%')
                   AND (?2 IS NULL OR user_name = ?2 COLLATE NOCASE)
                   AND (?3 IS NULL OR id < ?3)
                   AND (?5 IS NULL OR at >= ?5)
                   AND (?6 IS NULL OR at <= ?6)
                 ORDER BY id DESC LIMIT ?4",
            )?;
            let rows = st.query_map(params![f.action, f.user, f.before, limit, f.since, f.until], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "user_id": r.get::<_, Option<i64>>(2)?,
                    "user_name": r.get::<_, Option<String>>(3)?, "action": r.get::<_, String>(4)?, "target": r.get::<_, Option<String>>(5)?,
                    "detail": r.get::<_, Option<String>>(6)?, "ip": r.get::<_, Option<String>>(7)?, "via": r.get::<_, Option<String>>(8)?,
                }))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    if csv {
        return Ok(crate::authlog::csv_file("freightdock-activity.csv", &["at", "user_name", "action", "target", "detail", "ip", "via"], &entries));
    }
    Ok(axum::Json(serde_json::json!({ "entries": entries })).into_response())
}

pub fn entry(action: &'static str, target: impl Into<Option<String>>) -> Entry {
    Entry { action, target: target.into(), detail: None }
}

#[cfg(test)]
mod tests {
    use crate::api::tests::{app, call, signed_in};
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};

    #[tokio::test]
    async fn the_log_is_for_administrators_newest_first_and_filtered() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let get = |q: &str| Request::get(format!("/api/audit{q}")).header(header::COOKIE, &cookie).body(Body::empty()).unwrap();
        let (s, _, b) = call(&app, get("")).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["entries"][0]["action"], "setup");
        assert_eq!(b["entries"][0]["user_name"], "admin");
        let (_, _, b) = call(&app, get("?action=login")).await;
        assert_eq!(b["entries"].as_array().unwrap().len(), 0);

        app.db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (9, 'deckhand', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = crate::auth::start(&app.db, 9, None, None).await.unwrap();
        let (s, _, _) = call(&app, Request::get("/api/audit").header(header::COOKIE, format!("freightdock_session={token}")).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
