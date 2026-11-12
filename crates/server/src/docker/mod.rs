//! Docker, as Freightdock reaches it. Every environment is one Docker endpoint (a unix socket, or TCP), and bollard speaks
//! the Engine API to it. The local one is `FREIGHTDOCK_DOCKER_HOST`; an agent's tunnel and a TLS host are offered as unix
//! sockets of Freightdock's own (`sockets`), so there is no second code path for remote hosts ("Endpoints" in
//! DECISIONS.md).

pub mod api;
pub mod cli;
pub mod containers;
pub mod images;
pub mod networks;
pub mod own;
pub mod raw;
pub mod sockets;
pub mod streams;
pub mod summary;
pub mod swarm;
pub mod system;
pub mod tls_host;
pub mod volume_files;
pub mod volumes;

use anyhow::{Context, Result};

#[derive(Clone)]
pub struct Docker {
    url: String,
    client: bollard::Docker,
}

/// How long a request to Docker may take before it is given up on. Pulls, logs and consoles stream and are not held
/// to it.
pub const TIMEOUT_S: u64 = 120;

impl Docker {
    /// An endpoint by its address: `unix:///var/run/docker.sock`, or `tcp://host:2375` / `http://host:2375`.
    pub fn connect(url: &str) -> Result<Self> {
        let client = if let Some(path) = url.strip_prefix("unix://") {
            bollard::Docker::connect_with_socket(path, TIMEOUT_S, bollard::API_DEFAULT_VERSION)
        } else if url.starts_with("tcp://") || url.starts_with("http://") {
            bollard::Docker::connect_with_http(&url.replacen("tcp://", "http://", 1), TIMEOUT_S, bollard::API_DEFAULT_VERSION)
        } else {
            anyhow::bail!("\"{url}\" is not a Docker address Freightdock knows: it starts unix://, tcp:// or http://")
        }
        .with_context(|| format!("connecting to Docker at {url}"))?;
        Ok(Docker { url: url.to_string(), client })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn client(&self) -> &bollard::Docker {
        &self.client
    }

    /// Settles on the API version Docker speaks, when it is older than bollard's: Docker 28.1 speaks 1.49, and an older
    /// daemon refuses a request in a version newer than its own. Every clone of this endpoint follows.
    pub async fn negotiate(&self) {
        match self.client.clone().negotiate_version().await {
            Ok(c) => tracing::debug!("Docker at {} speaks API {}", self.url, c.client_version()),
            Err(e) => tracing::debug!("could not ask Docker at {} for its API version: {e}", self.url),
        }
    }
}

/// Docker's own answer as the API's: its status and its sentence (Docker's messages are written for people: "No such
/// container: x", "conflict: unable to remove repository reference"). A Docker that does not answer is a 502.
pub fn api_error(e: bollard::errors::Error) -> crate::error::ApiError {
    use axum::http::StatusCode;
    use bollard::errors::Error as E;
    match e {
        E::DockerResponseServerError { status_code, message } => {
            let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
            let status = if status.is_server_error() { StatusCode::BAD_GATEWAY } else { status };
            let mut m = message.trim().to_string();
            if let Some(first) = m.get(..1) {
                m = first.to_uppercase() + &m[1..];
            }
            crate::error::ApiError::new(status, m)
        }
        E::RequestTimeoutError => crate::error::ApiError::new(StatusCode::GATEWAY_TIMEOUT, "Docker took too long to answer."),
        other => {
            tracing::warn!("Docker: {other}");
            crate::error::ApiError::new(StatusCode::BAD_GATEWAY, format!("Docker did not answer: {other}"))
        }
    }
}

/// The endpoint of environment `env`, whatever kind it is: the local Docker, the socket of an agent's tunnel (503 while
/// the agent is not connected), or a TLS host's socket, made on first use.
pub async fn endpoint(app: &crate::state::App, env: i64) -> crate::error::ApiResult<Docker> {
    use crate::error::ApiError;
    use axum::http::StatusCode;
    let Some(row) = row_of(app, env).await? else { return Err(ApiError::not_found("environment")) };
    match row.0.as_str() {
        "local" => Ok(app.docker.clone()),
        "agent" => app.sockets.get(env).ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, format!("The agent for {} is not connected.", row.1))),
        "tls" => match app.sockets.get(env) {
            Some(d) => Ok(d),
            None => tls_socket(app, env, row).await,
        },
        _ => Err(ApiError::new(StatusCode::NOT_IMPLEMENTED, "Freightdock does not know that kind of environment.")),
    }
}

/// An environment's kind, name, address and TLS material, as they are kept.
type Row = (String, String, String, Option<String>, Option<String>, Option<String>);

async fn row_of(app: &crate::state::App, env: i64) -> crate::error::ApiResult<Option<Row>> {
    use crate::db::rusqlite::OptionalExtension;
    Ok(app
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT kind, name, url, tls_ca, tls_cert, tls_key FROM environments WHERE id = ?1", [env], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                .optional()?)
        })
        .await?)
}

/// The socket for TLS host `env`, made from `row`, what was read of it. The host may have been edited or removed (and
/// its id given to another) since it was read; each of those changes the database, then closes the socket. So the
/// row is read again once the socket is offered: when it changed, the socket goes and the request is refused
/// ("Endpoints" 8).
async fn tls_socket(app: &crate::state::App, env: i64, row: Row) -> crate::error::ApiResult<Docker> {
    use crate::error::ApiError;
    use axum::http::StatusCode;
    let _one = app.sockets.making.lock().await;
    if let Some(d) = app.sockets.get(env) {
        return Ok(d);
    }
    let (_, name, url, ca, cert, key) = &row;
    let broken = |m: String| ApiError::new(StatusCode::BAD_GATEWAY, format!("{name}: {m}"));
    let (host, port) = tls_host::host_port(url).map_err(|e| broken(e.to_string()))?;
    let opened = app.sealer.open(key.as_deref().unwrap_or_default())?;
    let config = tls_host::client_config(ca.as_deref().unwrap_or_default(), cert.as_deref().unwrap_or_default(), &opened).map_err(|(_, m)| broken(m))?;
    let offered = app.sockets.offer(env, Some(format!("{host}:{port}")), tls_host::dial(host, port, config)?).await?;
    if row_of(app, env).await?.as_ref() != Some(&row) {
        app.sockets.close_generation(env, offered.generation);
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, format!("{name} was changed or removed just now. Try again.")));
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), offered.docker.negotiate()).await;
    Ok(offered.docker)
}

/// A name Docker takes for a container, volume or network: `[a-zA-Z0-9][a-zA-Z0-9_.-]*`.
pub fn valid_name(name: &str) -> bool {
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric()) && c.all(|x| x.is_ascii_alphanumeric() || "_.-".contains(x)) && name.len() <= 128
}

/// An id or a name as a path segment of Docker's API: no slashes, no dots alone, nothing to escape out of it.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty() && r.len() <= 256 && r != "." && r != ".." && !r.contains(['/', '\\', '?', '#', '%']) && !r.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_tls_host_s_socket_is_never_made_from_settings_changed_while_it_was_made() {
        let app = crate::api::tests::app();
        let (ca, cert, key) = tls_host::tests::pki("freightdock", true);
        let sealed = app.sealer.seal(&key).unwrap();
        app.db
            .call(move |c| Ok(c.execute("INSERT INTO environments (id, name, kind, url, created_at, tls_ca, tls_cert, tls_key) VALUES (4, 'nas', 'tls', 'tcp://127.0.0.1:9', 0, ?1, ?2, ?3)", crate::db::rusqlite::params![ca, cert, sealed])?))
            .await
            .unwrap();
        let read = row_of(&app, 4).await.unwrap().unwrap();
        // An administrator points it elsewhere (or removes it, and the id goes to another host) meanwhile.
        app.db.call(|c| Ok(c.execute("UPDATE environments SET url = 'tcp://127.0.0.1:8' WHERE id = 4", [])?)).await.unwrap();
        assert!(tls_socket(&app, 4, read).await.is_err());
        assert!(app.sockets.get(4).is_none(), "no socket made from what it was before");
        let fresh = row_of(&app, 4).await.unwrap().unwrap();
        assert!(tls_socket(&app, 4, fresh).await.is_ok());
        assert_eq!(app.sockets.info(4).unwrap().1.as_deref(), Some("127.0.0.1:8"));
    }

    #[test]
    fn docker_s_answer_keeps_its_status_and_words() {
        let e = api_error(bollard::errors::Error::DockerResponseServerError { status_code: 404, message: "no such container: x".into() });
        assert_eq!(e.status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(e.message, "No such container: x");
        let e = api_error(bollard::errors::Error::DockerResponseServerError { status_code: 500, message: "boom".into() });
        assert_eq!(e.status, axum::http::StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn names_and_refs_are_what_docker_takes() {
        assert!(valid_name("web-1") && valid_name("a.b_c") && valid_name("9lives"));
        assert!(!valid_name("") && !valid_name("-x") && !valid_name("a b") && !valid_name("a/b"));
        assert!(valid_ref("3f2a9c") && valid_ref("my_container") && valid_ref("nginx:latest") && valid_ref("sha256:abc"));
        assert!(!valid_ref("..") && !valid_ref("a/b") && !valid_ref("a%2Fb") && !valid_ref(""));
    }

    #[test]
    fn addresses_are_unix_sockets_or_tcp() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("docker.sock");
        std::fs::write(&sock, b"").unwrap();
        assert!(Docker::connect(&format!("unix://{}", sock.display())).is_ok());
        assert!(Docker::connect("tcp://127.0.0.1:2375").is_ok());
        assert!(Docker::connect("ftp://example").is_err());
    }
}
