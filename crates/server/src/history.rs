//! Resource history: every so often (`every_s`, a minute by default) one sample of every running container on every
//! environment that answers is written to `container_stats`, by the container's name, and samples older than
//! `keep_days` go. The container page draws them as charts ("History" in DECISIONS.md).

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{Admin, Authed};
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::params};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

const SETTINGS_KEY: &str = "history";
/// At most this many points in one answer: a week of minutes is ten thousand, more than a chart has pixels for.
pub const MAX_POINTS: usize = 360;
/// Two samples further apart than this have a gap between them (the container was stopped, Freightdock was), so no rate is
/// counted across it.
const GAP_S: i64 = 600;

pub fn router() -> Router<App> {
    Router::new()
        .route("/api/settings/history", get(get_settings).put(put_settings))
        .route("/api/envs/{env}/containers/{id}/history", get(history))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Seconds between two samples, 10 to 3600.
    pub every_s: u64,
    /// Days a sample is kept, 1 to 365.
    pub keep_days: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, every_s: 60, keep_days: 7 }
    }
}

impl Settings {
    fn check(&self) -> ApiResult<()> {
        if !(10..=3600).contains(&self.every_s) {
            return Err(ApiError::bad("A sample every 10 to 3600 seconds.").field("every_s"));
        }
        if !(1..=365).contains(&self.keep_days) {
            return Err(ApiError::bad("Kept for 1 to 365 days.").field("keep_days"));
        }
        Ok(())
    }
}

pub async fn settings(app: &App) -> Settings {
    app.db.setting(SETTINGS_KEY).await.ok().flatten().unwrap_or_default()
}

async fn get_settings(State(app): State<App>, _: Admin) -> Json<Settings> {
    Json(settings(&app).await)
}

async fn put_settings(State(app): State<App>, admin: Admin, Json(s): Json<Settings>) -> ApiResult<StatusCode> {
    s.check()?;
    app.db.put_setting(SETTINGS_KEY, &s).await?;
    let a = &admin.0;
    let detail = if s.enabled { format!("every {} s, kept {} days", s.every_s, s.keep_days) } else { "off".into() };
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.history", target: None, detail: Some(detail) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// One sample as it is kept: the counters (network, disk) as Docker gives them, since the container started.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub at: i64,
    pub cpu: f64,
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub pids: u64,
}

/// One point of a chart: CPU and memory as sampled, network and disk as bytes a second since the sample before.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Point {
    pub at: i64,
    pub cpu: f64,
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub disk_read: f64,
    pub disk_write: f64,
    pub pids: u64,
}

/// What a counter grew by a second between two samples; nothing across a gap, or when it went back (the container
/// started again and its counters with it).
fn rate(prev: Option<&Row>, cur: &Row, f: impl Fn(&Row) -> u64) -> f64 {
    match prev {
        Some(p) if cur.at > p.at && cur.at - p.at <= GAP_S => f(cur).saturating_sub(f(p)) as f64 / (cur.at - p.at) as f64,
        _ => 0.0,
    }
}

/// The samples, oldest first, as at most `max` points: rates worked out between neighbours, then averaged over equal
/// stretches of time when there are more samples than points.
pub fn series(rows: &[Row], max: usize) -> Vec<Point> {
    let points: Vec<Point> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let prev = i.checked_sub(1).map(|j| &rows[j]);
            Point {
                at: r.at,
                cpu: r.cpu,
                mem: r.mem,
                mem_limit: r.mem_limit,
                net_rx: rate(prev, r, |x| x.net_rx),
                net_tx: rate(prev, r, |x| x.net_tx),
                disk_read: rate(prev, r, |x| x.disk_read),
                disk_write: rate(prev, r, |x| x.disk_write),
                pids: r.pids,
            }
        })
        .collect();
    if points.len() <= max || max == 0 {
        return points;
    }
    let (first, last) = (points[0].at, points[points.len() - 1].at);
    let width = ((last - first) as f64 / max as f64).max(1.0);
    let mut out: Vec<Point> = Vec::with_capacity(max);
    let mut bucket: Vec<&Point> = Vec::new();
    let mut index = 0usize;
    let flush = |bucket: &mut Vec<&Point>, out: &mut Vec<Point>| {
        if bucket.is_empty() {
            return;
        }
        let n = bucket.len() as f64;
        let avg = |f: &dyn Fn(&Point) -> f64| bucket.iter().map(|p| f(p)).sum::<f64>() / n;
        out.push(Point {
            at: bucket[0].at,
            cpu: (avg(&|p| p.cpu) * 10.0).round() / 10.0,
            mem: avg(&|p| p.mem as f64).round() as u64,
            mem_limit: bucket.iter().map(|p| p.mem_limit).max().unwrap_or(0),
            net_rx: avg(&|p| p.net_rx),
            net_tx: avg(&|p| p.net_tx),
            disk_read: avg(&|p| p.disk_read),
            disk_write: avg(&|p| p.disk_write),
            pids: bucket.iter().map(|p| p.pids).max().unwrap_or(0),
        });
        bucket.clear();
    };
    for p in &points {
        let i = (((p.at - first) as f64 / width) as usize).min(max - 1);
        if i != index {
            flush(&mut bucket, &mut out);
            index = i;
        }
        bucket.push(p);
    }
    flush(&mut bucket, &mut out);
    out
}

/// Writes one pass of samples for environment `env`.
pub async fn record(app: &App, env: i64, at: i64, samples: Vec<(String, crate::docker::streams::Sample)>) -> anyhow::Result<()> {
    app.db
        .call(move |c| {
            let tx = c.transaction()?;
            {
                let mut st = tx.prepare(
                    "INSERT INTO container_stats (env_id, container, at, cpu, mem, mem_limit, net_rx, net_tx, disk_read, disk_write, pids)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )?;
                for (name, s) in samples {
                    st.execute(params![env, name, at, s.cpu, s.mem as i64, s.mem_limit as i64, s.net_rx as i64, s.net_tx as i64, s.disk_read as i64, s.disk_write as i64, s.pids as i64])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

/// Forgets the samples taken before `before`; answers how many went.
pub async fn prune(app: &App, before: i64) -> anyhow::Result<usize> {
    app.db.call(move |c| Ok(c.execute("DELETE FROM container_stats WHERE at < ?1", [before])?)).await
}

/// The samples of `container` on `env` from `from` on, oldest first.
pub async fn rows(app: &App, env: i64, container: String, from: i64) -> anyhow::Result<Vec<Row>> {
    app.db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT at, cpu, mem, mem_limit, net_rx, net_tx, disk_read, disk_write, pids FROM container_stats
                 WHERE env_id = ?1 AND container = ?2 AND at >= ?3 ORDER BY at",
            )?;
            let rows = st.query_map(params![env, container, from], |r| {
                let u = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
                Ok(Row { at: r.get(0)?, cpu: r.get(1)?, mem: u(2)?, mem_limit: u(3)?, net_rx: u(4)?, net_tx: u(5)?, disk_read: u(6)?, disk_write: u(7)?, pids: u(8)? })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
}

#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    /// How far back, in hours: 1 to a year.
    hours: Option<i64>,
}

/// `GET .../containers/{id}/history?hours=24`: the container's samples as chart points. A container is found by its
/// name, so a name that is no container now (it was removed) still has its history.
async fn history(State(app): State<App>, _: Authed, Path((env, id)): Path<(i64, String)>, Query(hq): Query<HistoryQuery>) -> ApiResult<Json<Value>> {
    if !crate::docker::valid_ref(&id) {
        return Err(ApiError::bad("That is not a container's id or name."));
    }
    let hours = hq.hours.unwrap_or(24).clamp(1, 24 * 366);
    let name = match crate::docker::endpoint(&app, env).await {
        Ok(d) => match d.client().inspect_container(&id, None::<bollard::query_parameters::InspectContainerOptions>).await {
            Ok(c) => c.name.unwrap_or_default().trim_start_matches('/').to_string(),
            Err(_) => id.clone(),
        },
        Err(e) if e.status == StatusCode::NOT_FOUND => return Err(e),
        Err(_) => id.clone(),
    };
    let s = settings(&app).await;
    let rows = rows(&app, env, name.clone(), now() - hours * 3600).await?;
    Ok(Json(json!({ "container": name, "every_s": s.every_s, "enabled": s.enabled, "points": series(&rows, MAX_POINTS) })))
}

/// One pass over environment `env`: a sample of each running container, written down. An environment that does not
/// answer is passed over quietly; its health check says why.
async fn sample_env(app: &App, env: i64, at: i64) {
    let Ok(d) = crate::docker::endpoint(app, env).await else { return };
    match tokio::time::timeout(Duration::from_secs(30), crate::docker::streams::sample_running(&d)).await {
        Ok(Ok(samples)) => {
            let samples = samples.into_iter().map(|(_, name, s)| (name, s)).filter(|(n, _)| !n.is_empty()).collect();
            if let Err(e) = record(app, env, at, samples).await {
                tracing::warn!("could not write environment {env}'s samples: {e:#}");
            }
        }
        Ok(Err(e)) => tracing::debug!("no samples from environment {env}: {}", e.message),
        Err(_) => tracing::debug!("environment {env} took too long to sample"),
    }
}

/// Whether a pass is due: none yet, or the last one `every_s` ago.
pub fn due(last: Option<i64>, now: i64, every_s: u64) -> bool {
    last.is_none_or(|l| now - l >= every_s as i64)
}

/// How often the watch looks at the clock (and the settings): a changed interval holds within this.
const TICK: Duration = Duration::from_secs(5);

/// Samples every environment every `every_s` and forgets what is older than `keep_days`, until Freightdock stops.
pub async fn watch(app: App, stop: tokio_util::sync::CancellationToken) {
    let mut last: Option<i64> = None;
    loop {
        let s = settings(&app).await;
        let t = now();
        if s.enabled && due(last, t, s.every_s) {
            last = Some(t);
            let ids = app.db.call(|c| Ok(c.prepare("SELECT id FROM environments")?.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap_or_default();
            futures_util::future::join_all(ids.into_iter().map(|id| {
                let app = app.clone();
                async move { sample_env(&app, id, t).await }
            }))
            .await;
            if let Err(e) = prune(&app, now() - s.keep_days as i64 * 86400).await {
                tracing::warn!("could not forget old samples: {e:#}");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = stop.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, signed_in};
    use crate::environments::tests::req;
    use axum::http::Method;

    fn row(at: i64, cpu: f64, net_rx: u64) -> Row {
        Row { at, cpu, mem: 100, mem_limit: 1000, net_rx, ..Default::default() }
    }

    #[test]
    fn network_and_disk_are_rates_between_neighbours() {
        let p = series(&[row(0, 1.0, 0), row(60, 2.0, 6000), row(120, 3.0, 12000)], 100);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].net_rx, 0.0, "the first has nothing before it");
        assert_eq!(p[1].net_rx, 100.0, "6000 bytes in a minute is 100 a second");
        assert_eq!(p[2].cpu, 3.0);
    }

    #[test]
    fn no_rate_is_counted_across_a_restart_or_a_gap() {
        let p = series(&[row(0, 0.0, 9000), row(60, 0.0, 100), row(60 + GAP_S + 1, 0.0, 99_999)], 100);
        assert_eq!(p[1].net_rx, 0.0, "the counter went back: it started again");
        assert_eq!(p[2].net_rx, 0.0, "nothing is known about the gap");
    }

    #[test]
    fn many_samples_are_averaged_into_at_most_max_points() {
        let rows: Vec<Row> = (0..1000).map(|i| row(i * 60, (i % 2) as f64 * 2.0, 0)).collect();
        let p = series(&rows, 100);
        assert!(p.len() <= 100 && p.len() >= 90, "{} points", p.len());
        assert!(p.iter().all(|x| (x.cpu - 1.0).abs() < 0.2), "alternating 0 and 2 averages to about 1: {:?}", p.iter().map(|x| x.cpu).collect::<Vec<_>>());
        assert_eq!(p[0].at, 0);
        assert!(p.windows(2).all(|w| w[0].at < w[1].at), "oldest first");
    }

    #[test]
    fn a_pass_is_due_first_and_then_every_so_often() {
        assert!(due(None, 1000, 60));
        assert!(!due(Some(1000), 1059, 60));
        assert!(due(Some(1000), 1060, 60));
    }

    #[test]
    fn the_settings_are_held_to_their_bounds() {
        assert!(Settings::default().check().is_ok());
        assert_eq!(Settings { every_s: 5, ..Default::default() }.check().unwrap_err().field, Some("every_s"));
        assert_eq!(Settings { keep_days: 0, ..Default::default() }.check().unwrap_err().field, Some("keep_days"));
    }

    fn sample(cpu: f64) -> crate::docker::streams::Sample {
        crate::docker::streams::Sample { cpu, mem: 5 << 20, mem_limit: 1 << 30, ..Default::default() }
    }

    #[tokio::test]
    async fn samples_are_kept_by_name_and_old_ones_go() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let t = now();
        record(&app, 1, t - 10 * 86400, vec![("web".into(), sample(9.0))]).await.unwrap();
        record(&app, 1, t - 120, vec![("web".into(), sample(1.5)), ("db".into(), sample(3.0))]).await.unwrap();
        record(&app, 1, t - 60, vec![("web".into(), sample(2.5))]).await.unwrap();
        assert_eq!(prune(&app, t - 7 * 86400).await.unwrap(), 1, "the one ten days old goes");

        // No Docker answers in the tests, so the name in the path is taken as the container's name.
        let (s, _, b) = call(&app, req(Method::GET, "/api/envs/1/containers/web/history?hours=1", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["container"], "web");
        let cpu: Vec<f64> = b["points"].as_array().unwrap().iter().map(|p| p["cpu"].as_f64().unwrap()).collect();
        assert_eq!(cpu, vec![1.5, 2.5]);
        assert_eq!(b["every_s"], 60);
        let (s, _, _) = call(&app, req(Method::GET, "/api/envs/1/containers/a%2Fb/history", &cookie, &csrf, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_settings_are_an_administrator_s_and_logged() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let (s, _, b) = call(&app, req(Method::GET, "/api/settings/history", &cookie, &csrf, None)).await;
        assert_eq!((s, b["every_s"].as_u64()), (StatusCode::OK, Some(60)));
        let (s, _, b) = call(&app, req(Method::PUT, "/api/settings/history", &cookie, &csrf, Some(json!({ "enabled": true, "every_s": 1, "keep_days": 7 })))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("every_s")));
        let (s, _, _) = call(&app, req(Method::PUT, "/api/settings/history", &cookie, &csrf, Some(json!({ "enabled": true, "every_s": 30, "keep_days": 30 })))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert_eq!(settings(&app).await, Settings { enabled: true, every_s: 30, keep_days: 30 });
        let logged: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit WHERE action = 'settings.history'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(logged, 1);
    }
}
