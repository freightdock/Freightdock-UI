//! The agent end to end, in one process: Freightdock's agent endpoint on loopback, a real `freightdock-agent` dialling it,
//! and a fake Docker (a small HTTP responder on a unix socket) behind the agent. Requests go from the API through
//! bollard, the environment's socket, yamux over the WebSocket over mTLS, to the fake Docker and back.

use std::path::Path;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use http_body_util::Full;
use hyper::body::Bytes;
use serde_json::json;

use crate::api::tests::{app, call, signed_in};
use crate::environments::tests::req;
use crate::state::App;

/// A Docker that knows `/_ping`, `/version`, `/info`, its one container and no volumes, as Docker 28.1 answers them, on
/// a unix socket.
fn fake_docker(path: &Path) {
    let l = tokio::net::UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let service = hyper::service::service_fn(|r: hyper::Request<hyper::body::Incoming>| async move {
                    let p = r.uri().path().to_string();
                    let body = if p.ends_with("/_ping") {
                        "OK".to_string()
                    } else if p.ends_with("/version") {
                        json!({ "Version": "28.1.1", "ApiVersion": "1.49", "MinAPIVersion": "1.24", "Os": "linux", "Arch": "amd64" }).to_string()
                    } else if p.ends_with("/info") {
                        json!({ "Name": "far-host", "Containers": 1, "OperatingSystem": "Ubuntu 24.04.2 LTS", "NCPU": 4, "MemTotal": 8_000_000_000_i64, "ServerVersion": "28.1.1" }).to_string()
                    } else if p.ends_with("/containers/json") {
                        json!([{ "Id": "a", "State": "running", "Status": "Up 1 minute (unhealthy)", "Labels": { "com.docker.compose.project": "web" } }]).to_string()
                    } else if p.ends_with("/volumes") {
                        json!({ "Volumes": [], "Warnings": [] }).to_string()
                    } else {
                        return Ok::<_, std::convert::Infallible>(hyper::Response::builder().status(404).body(Full::new(Bytes::from("{\"message\":\"page not found\"}"))).unwrap());
                    };
                    Ok(hyper::Response::builder().header("content-type", "application/json").header("api-version", "1.49").body(Full::new(Bytes::from(body))).unwrap())
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(s), service).await;
            });
        }
    });
}

/// The agent endpoint on a free loopback port.
async fn endpoint(app: &App) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let tls = Some(tokio_rustls::TlsAcceptor::from(app.agents.tls.clone()));
    tokio::spawn(crate::serve(l, super::endpoint::router(app.clone()), tls, std::future::pending::<()>()));
    addr
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..200 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} did not happen within 10s");
}

async fn audits(app: &App, action: &'static str) -> i64 {
    app.db.call(move |c| Ok(c.query_row("SELECT COUNT(*) FROM audit WHERE action = ?1", [action], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_enrols_once_tunnels_docker_through_and_is_cut_off_when_removed() {
    let app = app();
    let (cookie, csrf) = signed_in(&app).await;
    let dir = tempfile::tempdir().unwrap();
    let docker_sock = dir.path().join("docker.sock");
    fake_docker(&docker_sock);
    let server = endpoint(&app).await;

    let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "vm", "kind": "agent" })))).await;
    assert_eq!(s, StatusCode::CREATED);
    let id = b["environment"]["id"].as_i64().unwrap();
    let token = b["join"]["token"].as_str().unwrap().to_string();

    let config = freightdock_agent::Config { server: server.clone(), token: Some(token.clone()), data_dir: dir.path().join("agent"), docker: docker_sock.clone() };
    let agent = tokio::spawn(freightdock_agent::run(config.clone()));
    until("the tunnel coming up", || app.sockets.get(id).is_some()).await;

    // Through the API, bollard, the socket, the tunnel and the agent to the fake Docker.
    let (s, _, b) = call(&app, req(Method::GET, &format!("/api/envs/{id}/info"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["info"]["Name"], "far-host");
    assert_eq!(b["version"]["Version"], "28.1.1");
    assert_eq!(b["version"]["ApiVersion"], "1.49");
    // Many at once share the one tunnel.
    let many = (0..16).map(|_| {
        let app = app.clone();
        async move { crate::docker::endpoint(&app, id).await.unwrap().client().ping().await.unwrap() }
    });
    assert!(futures_util::future::join_all(many).await.iter().all(|p| p == "OK"));

    // Enrolled: the token is spent, the certificate known, the environment up, all in the log.
    let (fp, hash): (Option<String>, Option<String>) = app.db.call(move |c| Ok(c.query_row("SELECT agent_cert_sha256, join_token_hash FROM environments WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
    assert!(fp.is_some() && hash.is_none());
    assert_eq!(audits(&app, "agent.enrol").await, 1);
    for _ in 0..100 {
        let (_, _, e) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
        if e["status"] == "up" {
            assert_eq!(e["docker_version"], "28.1.1");
            assert!(e["agent"]["connected_since"].is_i64());
            assert_eq!(e["agent"]["peer"], "127.0.0.1");
            assert_eq!(e["agent"]["last_peer"], "127.0.0.1", "kept for when it is gone");
            assert_eq!(e["agent"]["version"], env!("CARGO_PKG_VERSION"), "as the agent says in its user agent");
            assert_eq!(e["agent"]["outdated"], false);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (_, _, e) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
    assert_eq!(e["status"], "up", "{e}");

    // Its numbers come through the tunnel for the home page, and the health check keeps them as the last known.
    let (s, _, sum) = call(&app, req(Method::GET, &format!("/api/envs/{id}/summary"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(sum["containers"], json!({ "total": 1, "running": 1, "paused": 0, "stopped": 0, "unhealthy": 1 }));
    assert_eq!((sum["stacks"].clone(), sum["cpus"].clone(), sum["nodes"].clone()), (json!(1), json!(4), json!(1)));
    app.db.call(move |c| Ok(c.execute("UPDATE environments SET snapshot = NULL WHERE id = ?1", [id])?)).await.unwrap();
    crate::environments::check(&app, id).await;
    let (_, _, e) = call(&app, req(Method::GET, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
    assert_eq!(e["snapshot"]["memory"], 8_000_000_000_i64, "{e}");
    assert!(e["snapshot"]["at"].is_i64());

    // The token is good once: another agent with it gets nothing.
    let other = tempfile::tempdir().unwrap();
    let c2 = freightdock_agent::Config { data_dir: other.path().into(), ..config.clone() };
    let err = freightdock_agent::enrol::enrol(&c2, &freightdock_agent::Files::in_dir(other.path()), &token).await.unwrap_err();
    assert!(format!("{err:#}").contains("spent"), "{err:#}");

    // A tunnel that breaks comes back by itself, on the certificate, without enrolling again.
    app.sockets.close(id);
    assert!(app.sockets.get(id).is_none());
    until("the tunnel coming back", || app.sockets.get(id).is_some()).await;
    assert_eq!(crate::docker::endpoint(&app, id).await.unwrap().client().ping().await.unwrap(), "OK");
    assert_eq!(audits(&app, "agent.enrol").await, 1);

    // Removing the environment cuts the tunnel, and the certificate opens nothing any more.
    let (s, _, _) = call(&app, req(Method::DELETE, &format!("/api/environments/{id}"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(app.sockets.get(id).is_none());
    assert!(!app.sockets.path(id).exists());
    let files = freightdock_agent::Files::in_dir(&config.data_dir);
    match freightdock_agent::tunnel::once(&config, &files).await.unwrap() {
        freightdock_agent::Ended::Revoked(why) => assert!(why.contains("not one Freightdock knows"), "{why}"),
        other => panic!("a removed environment's agent got in: {other:?}"),
    }
    agent.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_join_token_revokes_the_old_certificate_and_the_agent_enrols_again_with_it() {
    let app = app();
    let (cookie, csrf) = signed_in(&app).await;
    let dir = tempfile::tempdir().unwrap();
    let docker_sock = dir.path().join("docker.sock");
    fake_docker(&docker_sock);
    let server = endpoint(&app).await;
    let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "vm", "kind": "agent" })))).await;
    let id = b["environment"]["id"].as_i64().unwrap();
    let config = freightdock_agent::Config { server, token: b["join"]["token"].as_str().map(str::to_string), data_dir: dir.path().join("agent"), docker: docker_sock };
    let first = tokio::spawn(freightdock_agent::run(config.clone()));
    until("the tunnel coming up", || app.sockets.get(id).is_some()).await;

    let (s, _, b) = call(&app, req(Method::POST, &format!("/api/environments/{id}/join"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(app.sockets.get(id).is_none(), "a new token cuts the tunnel at once");
    first.abort();

    // The agent started again with the new token: refused on its old certificate, it enrols with the new token.
    let again = freightdock_agent::Config { token: b["token"].as_str().map(str::to_string), ..config };
    let second = tokio::spawn(freightdock_agent::run(again));
    until("the tunnel coming back", || app.sockets.get(id).is_some()).await;
    assert_eq!(crate::docker::endpoint(&app, id).await.unwrap().client().ping().await.unwrap(), "OK");
    assert_eq!(audits(&app, "agent.enrol").await, 2);
    second.abort();
}

#[tokio::test]
async fn a_token_for_another_freightdock_is_refused_before_its_secret_is_sent() {
    let app = app();
    let (cookie, csrf) = signed_in(&app).await;
    let server = endpoint(&app).await;
    let (_, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(json!({ "name": "vm", "kind": "agent" })))).await;
    let real = freightdock_tunnel::token::JoinToken::parse(b["join"]["token"].as_str().unwrap()).unwrap();
    // The same secret, pinned to another CA: the agent must not trust this endpoint with it.
    let forged = freightdock_tunnel::token::JoinToken { ca_sha256: "0".repeat(64), secret: real.secret.clone() }.to_string();
    let dir = tempfile::tempdir().unwrap();
    let config = freightdock_agent::Config { server, token: Some(forged.clone()), data_dir: dir.path().into(), docker: dir.path().join("none") };
    let err = freightdock_agent::enrol::enrol(&config, &freightdock_agent::Files::in_dir(dir.path()), &forged).await.unwrap_err();
    assert!(format!("{err:#}").contains("not the Freightdock the join token is for"), "{err:#}");
    assert_eq!(audits(&app, "agent.enrol").await, 0);
}

/// A Docker on its TLS port, as `dockerd --tlsverify` is: its own certificate, and only clients from `client_ca`.
async fn fake_tls_docker(dir: &Path, client_ca: &str) -> (u16, String) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    use std::sync::Arc;
    let sock = dir.join("tls-docker.sock");
    fake_docker(&sock);
    let (server_ca, cert, key) = crate::docker::tls_host::tests::pki("localhost", false);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from_pem_slice(client_ca.as_bytes()).unwrap()).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![CertificateDer::from_pem_slice(cert.as_bytes()).unwrap()], PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap())
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            let (acceptor, sock) = (acceptor.clone(), sock.clone());
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    freightdock_tunnel::mux::join(tls, tokio::net::UnixStream::connect(sock).await.unwrap()).await;
                }
            });
        }
    });
    (port, server_ca)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tls_host_is_reached_with_its_client_certificate() {
    let app = app();
    let (cookie, csrf) = signed_in(&app).await;
    let dir = tempfile::tempdir().unwrap();
    let (client_ca, client_cert, client_key) = crate::docker::tls_host::tests::pki("freightdock", true);
    let (port, server_ca) = fake_tls_docker(dir.path(), &client_ca).await;
    let body = json!({ "name": "tls", "kind": "tls", "url": format!("tcp://localhost:{port}"), "tls_ca": server_ca, "tls_cert": client_cert, "tls_key": client_key });
    let (s, _, b) = call(&app, req(Method::POST, "/api/environments", &cookie, &csrf, Some(body))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let id = b["environment"]["id"].as_i64().unwrap();
    let (s, _, b) = call(&app, req(Method::GET, &format!("/api/envs/{id}/info"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["info"]["Name"], "far-host");
    assert!(app.sockets.path(id).exists(), "a TLS host is a socket of its own too");

    // A certificate the host does not take: the bridge is made again with it, and Docker cannot be reached.
    let (_, other_cert, other_key) = crate::docker::tls_host::tests::pki("freightdock", true);
    let (s, _, _) = call(&app, req(Method::PUT, &format!("/api/environments/{id}"), &cookie, &csrf, Some(json!({ "tls_cert": other_cert, "tls_key": other_key })))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = call(&app, req(Method::GET, &format!("/api/envs/{id}/info"), &cookie, &csrf, None)).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
}
