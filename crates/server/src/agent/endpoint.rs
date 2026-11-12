//! The agent endpoint's routes, on its own listener: enrolment (token and certificate request in, certificate out) and
//! the tunnel (a WebSocket, with the agent's certificate).

use std::io;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt, future::ready};
use serde::Deserialize;
use serde_json::json;
use freightdock_tunnel::{PATH_ENROL, PATH_TUNNEL, mux, token::JoinToken, ws::WsIo};

use crate::api::Peer;
use crate::audit::{self, Entry};
use crate::db::{now, rusqlite::OptionalExtension, rusqlite::params};
use crate::docker::sockets::{DialFuture, Io};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The fingerprint of the certificate the client showed in the TLS handshake, if it showed one. Put on every request
/// by the listener.
#[derive(Clone, Debug, Default)]
pub struct ClientCert(pub Option<String>);

pub fn router(app: App) -> Router {
    Router::new().route(PATH_ENROL, post(enrol)).route(PATH_TUNNEL, get(tunnel)).with_state(app)
}

#[derive(Deserialize)]
struct Enrol {
    token: String,
    /// A PEM certificate request for the key the agent made and keeps.
    csr: String,
    /// The agent's host name, for the activity log.
    #[serde(default)]
    hostname: Option<String>,
}

/// `POST /agent/v1/enrol`: spends a join token on a certificate. The token is good once: the certificate's fingerprint
/// takes its place, in the same statement that checks it is still there.
async fn enrol(State(app): State<App>, peer: Peer, Json(e): Json<Enrol>) -> ApiResult<Json<serde_json::Value>> {
    let t = JoinToken::parse(&e.token).ok_or_else(|| ApiError::bad("That is not a join token."))?;
    if t.ca_sha256 != app.agents.ca.sha256() {
        return Err(ApiError::bad("That join token is for another Freightdock."));
    }
    let hash = t.secret_hash();
    let spent = || ApiError::new(StatusCode::UNAUTHORIZED, "That join token is spent, expired or replaced. Make a new one in Freightdock.");
    let h = hash.clone();
    let (env, name) = app
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT id, name FROM environments WHERE kind = 'agent' AND join_token_hash = ?1 AND join_token_expires_at > ?2", params![h, now()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .optional()?)
        })
        .await?
        .ok_or_else(spent)?;
    let issued = app.agents.ca.issue_agent(&e.csr, env).map_err(|_| ApiError::bad("That is not a certificate request Freightdock can sign.").field("csr"))?;
    let fp = issued.sha256.clone();
    let took = app
        .db
        .call(move |c| {
            Ok(c.execute(
                "UPDATE environments SET agent_cert_sha256 = ?1, join_token_hash = NULL, join_token_expires_at = NULL WHERE id = ?2 AND join_token_hash = ?3",
                params![fp, env, hash],
            )?)
        })
        .await?;
    if took != 1 {
        return Err(spent());
    }
    // A certificate it had before is no good from now on, and its tunnel goes with it.
    app.sockets.close(env);
    let hostname = e.hostname.filter(|h| !h.is_empty() && h.len() <= 255 && !h.chars().any(char::is_control));
    audit::record(&app.db, None, peer.ip, Entry { action: "agent.enrol", target: Some(name.clone()), detail: hostname.map(|h| format!("host {h}")) }).await;
    tracing::info!("an agent enrolled for {name}");
    Ok(Json(json!({ "cert": issued.pem, "ca": app.agents.ca.pem(), "environment": name })))
}

/// `GET /agent/v1/tunnel`: the WebSocket the tunnel runs over, for an agent whose certificate is an environment's.
async fn tunnel(State(app): State<App>, peer: Peer, Extension(cert): Extension<ClientCert>, ws: WebSocketUpgrade) -> ApiResult<Response> {
    let fp = cert.0.ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "This agent has no certificate yet: start it with a join token."))?;
    let env = app
        .db
        .call({
            let fp = fp.clone();
            move |c| Ok(c.query_row("SELECT id, name FROM environments WHERE kind = 'agent' AND agent_cert_sha256 = ?1", [fp], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).optional()?)
        })
        .await?;
    let Some((env, name)) = env else {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "This agent's certificate is not one Freightdock knows: its environment was removed or given a new join token."));
    };
    let version = agent_version(peer.user_agent.as_deref());
    Ok(ws.max_message_size(1 << 20).on_upgrade(move |socket| run(app, env, fp, name, peer.ip, version, socket)))
}

/// The version an agent gives in its user agent (`freightdock-agent/1.0.0`), when it is one.
fn agent_version(user_agent: Option<&str>) -> Option<String> {
    let v = user_agent?.strip_prefix("freightdock-agent/")?.split_whitespace().next()?;
    (!v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))).then(|| v.to_string())
}

/// A WebSocket as a stream of byte messages and a sink of them, for `WsIo`.
fn as_bytes(ws: WebSocket) -> impl futures_util::Stream<Item = io::Result<Vec<u8>>> + futures_util::Sink<Vec<u8>, Error = io::Error> + Unpin + Send {
    ws.sink_map_err(io::Error::other).with(|b: Vec<u8>| ready(Ok::<_, io::Error>(Message::Binary(b.into())))).filter_map(|m| {
        ready(match m {
            Ok(Message::Binary(b)) if !b.is_empty() => Some(Ok(b.to_vec())),
            Ok(_) => None,
            Err(e) => Some(Err(io::Error::other(e))),
        })
    })
}

/// Offers a tunnel let in with certificate `fp` as environment `env`'s socket, then asks again whether `fp` is still
/// that environment's, and takes the socket away when not ("Agent" 9).
///
/// The tunnel was let in by a look at the database some time before; a new join token or the environment's removal may
/// have come since. Each of those changes the database first and closes the socket after, so with the offer made
/// before this second look, either the look sees the change or the close comes after the offer and takes it away.
async fn offer_if_still(app: &App, env: i64, fp: &str, peer: Option<String>, dial: impl crate::docker::sockets::Dial) -> Option<crate::docker::sockets::Offered> {
    let offered = match app.sockets.offer(env, peer, dial).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("the tunnel for environment {env} could not be offered: {e:#}");
            return None;
        }
    };
    let f = fp.to_string();
    let still = app.db.call(move |c| Ok(c.query_row("SELECT EXISTS (SELECT 1 FROM environments WHERE id = ?1 AND kind = 'agent' AND agent_cert_sha256 = ?2)", params![env, f], |r| r.get::<_, bool>(0))?)).await;
    if !matches!(still, Ok(true)) {
        app.sockets.close_generation(env, offered.generation);
        tracing::info!("a tunnel for environment {env} was let in with a certificate revoked before it was offered; it is cut");
        return None;
    }
    Some(offered)
}

/// Runs one tunnel: offers it as the environment's socket, and when it ends (the agent went, or the tunnel was cut),
/// takes the socket away and says so.
async fn run(app: App, env: i64, fp: String, name: String, peer: Option<String>, version: Option<String>, socket: WebSocket) {
    let (open, drive) = mux::connection(WsIo::new(as_bytes(socket)), mux::Mode::Client, |_| {});
    let mut drive = tokio::spawn(drive);
    let dial = move || -> DialFuture {
        let open = open.clone();
        Box::pin(async move { Ok(Box::new(open.open().await?) as Box<dyn Io>) })
    };
    let Some(offered) = offer_if_still(&app, env, &fp, peer.clone(), dial).await else {
        drive.abort();
        return;
    };
    tracing::info!("the agent for {name} connected from {}", peer.as_deref().unwrap_or("somewhere"));
    let p = peer.clone();
    let _ = app.db.call(move |c| Ok(c.execute("UPDATE environments SET agent_version = ?1, agent_peer = COALESCE(?2, agent_peer) WHERE id = ?3", params![version, p, env])?)).await;
    let (a, docker) = (app.clone(), offered.docker.clone());
    tokio::spawn(async move {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), docker.negotiate()).await;
        crate::environments::check(&a, env).await;
    });
    let why = tokio::select! {
        r = &mut drive => match r {
            Ok(Ok(())) => "The agent disconnected.".to_string(),
            Ok(Err(e)) => format!("The tunnel broke: {e}"),
            Err(_) => "The tunnel broke.".to_string(),
        },
        _ = offered.stop.cancelled() => {
            drive.abort();
            "The tunnel was cut.".to_string()
        }
    };
    app.sockets.close_generation(env, offered.generation);
    tracing::info!("the agent for {name} is gone: {why}");
    // A newer tunnel for the same environment may already be up; only this one's end is recorded.
    if app.sockets.get(env).is_none() {
        let w = why.clone();
        let was = app
            .db
            .call(move |c| {
                let was: Option<String> = c.query_row("SELECT status FROM environments WHERE id = ?1", [env], |r| r.get(0)).optional()?;
                c.execute("UPDATE environments SET status = 'down', last_error = ?1, last_seen_at = ?2 WHERE id = ?3", params![w, now(), env])?;
                Ok(was)
            })
            .await;
        if matches!(was.as_ref().map(|w| w.as_deref()), Ok(Some("up"))) {
            crate::environments::went_down(&app, env, name, why);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::docker::sockets::{DialFuture, Io};

    fn nowhere() -> impl Fn() -> DialFuture + Send + Sync + 'static {
        || -> DialFuture { Box::pin(async { Ok(Box::new(tokio::io::duplex(64).0) as Box<dyn Io>) }) }
    }

    #[tokio::test]
    async fn a_tunnel_whose_certificate_was_revoked_before_it_was_offered_is_not_offered() {
        let app = crate::api::tests::app();
        app.db.call(|c| Ok(c.execute_batch("INSERT INTO environments (id, name, kind, url, created_at, agent_cert_sha256) VALUES (2, 'far', 'agent', '', 0, 'fp-1'), (3, 'near', 'agent', '', 0, 'fp-3');")?)).await.unwrap();
        assert!(super::offer_if_still(&app, 3, "fp-3", None, nowhere()).await.is_some(), "a certificate still the environment's is offered");
        assert!(app.sockets.get(3).is_some());

        // The tunnel was let in with fp-1, then a new join token was made before its socket was offered.
        app.db.call(|c| Ok(c.execute("UPDATE environments SET agent_cert_sha256 = NULL WHERE id = 2", [])?)).await.unwrap();
        assert!(super::offer_if_still(&app, 2, "fp-1", None, nowhere()).await.is_none());
        assert!(app.sockets.get(2).is_none(), "the revoked certificate's tunnel serves nothing");

        // Or the environment was removed, and its id given to a new one.
        app.db.call(|c| Ok(c.execute_batch("DELETE FROM environments WHERE id = 2; INSERT INTO environments (id, name, kind, url, created_at, agent_cert_sha256) VALUES (2, 'fresh', 'agent', '', 0, 'fp-2');")?)).await.unwrap();
        assert!(super::offer_if_still(&app, 2, "fp-1", None, nowhere()).await.is_none());
        assert!(app.sockets.get(2).is_none(), "the new environment is never served by the old one's agent");
    }

    #[test]
    fn an_agent_s_version_is_read_from_its_user_agent_and_nothing_else_is() {
        assert_eq!(super::agent_version(Some("freightdock-agent/1.0.0")), Some("1.0.0".into()));
        assert_eq!(super::agent_version(Some("freightdock-agent/0.2.0-rc.1 (linux)")), Some("0.2.0-rc.1".into()));
        assert_eq!(super::agent_version(Some("curl/8.0")), None);
        assert_eq!(super::agent_version(Some("freightdock-agent/<b>")), None, "kept short and plain");
        assert_eq!(super::agent_version(None), None);
    }
}
