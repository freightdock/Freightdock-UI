//! Freightdock: one binary serving the web UI and the API, over HTTPS by default.

mod access;
mod agent;
mod api;
mod appearance;
mod audit;
mod auth;
mod avatars;
mod authlog;
mod backup;
mod changelog;
mod cluster;
mod db;
mod docker;
mod environments;
mod mcp;
mod error;
mod groups;
mod history;
mod notify;
mod oidc;
mod outbound;
mod portainer;
mod registries;
mod replica;
mod secret;
mod signin;
mod stacks;
mod state;
mod templates;
mod tls;
mod tokens;
mod users;
mod updates;
mod web;
mod webhooks;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::get;

/// What the environment says, with Freightdock's defaults.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    data_dir: PathBuf,
    bind: SocketAddr,
    /// `None`: plain HTTP (behind a proxy that does TLS). Otherwise the certificate and key.
    tls: Option<tls::Files>,
    /// `FREIGHTDOCK_TLS_NAMES`: more names for the certificate Freightdock makes, comma-separated.
    tls_names: Option<String>,
    docker: String,
    /// The agent endpoint's listener; `None` when it is off.
    agent_bind: Option<SocketAddr>,
    /// Where agents are told to dial, when not this server's name with the endpoint's port.
    agent_address: Option<String>,
    agent_image: String,
    /// The Compose binary, when `FREIGHTDOCK_COMPOSE` names it; else it is looked for where Docker puts it.
    compose: Option<String>,
    /// `FREIGHTDOCK_SETUP_TOKEN`: the first run's setup token, given rather than made (automated installs, `qa/`).
    setup_token: Option<String>,
    /// `FREIGHTDOCK_CLUSTER_API`, `FREIGHTDOCK_CLUSTER_TOKEN_FILE` and `FREIGHTDOCK_CLUSTER_CA_FILE`: the managers whose
    /// cluster this Freightdock shows, the token it signs in with and the cluster CA (over HTTPS).
    cluster: Option<(String, PathBuf, Option<PathBuf>)>,
}

impl Config {
    fn from(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let data_dir = PathBuf::from(get("FREIGHTDOCK_DATA_DIR").unwrap_or_else(|| "/data".into()));
        let bind = get("FREIGHTDOCK_BIND").unwrap_or_else(|| "0.0.0.0:7443".into());
        let bind = bind.parse().with_context(|| format!("FREIGHTDOCK_BIND \"{bind}\" is not an address and port"))?;
        let tls = match get("FREIGHTDOCK_TLS").as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("off" | "0" | "false" | "no") => None,
            None | Some("" | "on" | "auto" | "1" | "true" | "yes") => {
                let (cert, key) = (get("FREIGHTDOCK_TLS_CERT").filter(|c| !c.is_empty()), get("FREIGHTDOCK_TLS_KEY").filter(|k| !k.is_empty()));
                Some(tls::Files {
                    own: cert.is_some() || key.is_some(),
                    cert: cert.map(PathBuf::from).unwrap_or_else(|| data_dir.join("tls/cert.pem")),
                    key: key.map(PathBuf::from).unwrap_or_else(|| data_dir.join("tls/key.pem")),
                })
            }
            Some(other) => anyhow::bail!("FREIGHTDOCK_TLS is on or off, not \"{other}\""),
        };
        let docker = get("FREIGHTDOCK_DOCKER_HOST").or_else(|| get("DOCKER_HOST")).unwrap_or_else(|| "unix:///var/run/docker.sock".into());
        let agent_bind = match get("FREIGHTDOCK_AGENT_BIND").as_deref().map(str::trim) {
            Some("off" | "0" | "false" | "no") => None,
            None | Some("") => Some(SocketAddr::from(([0, 0, 0, 0], 7444))),
            Some(a) => Some(a.parse().with_context(|| format!("FREIGHTDOCK_AGENT_BIND \"{a}\" is not an address and port, or off"))?),
        };
        let agent_address = get("FREIGHTDOCK_AGENT_ADDRESS").map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        let agent_image = get("FREIGHTDOCK_AGENT_IMAGE").map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).unwrap_or_else(|| "freightdock-agent:latest".into());
        let tls_names = get("FREIGHTDOCK_TLS_NAMES").filter(|n| !n.trim().is_empty());
        let setup_token = get("FREIGHTDOCK_SETUP_TOKEN").map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if setup_token.as_ref().is_some_and(|t| t.chars().count() < 16) {
            anyhow::bail!("FREIGHTDOCK_SETUP_TOKEN is at least 16 characters: it is all that stands between the first visitor and the first administrator");
        }
        let cluster = get("FREIGHTDOCK_CLUSTER_API").map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).map(|api| {
            let token = get("FREIGHTDOCK_CLUSTER_TOKEN_FILE").filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "/var/lib/freightdock-manager/admin.token".into());
            (api, PathBuf::from(token.trim()), get("FREIGHTDOCK_CLUSTER_CA_FILE").map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).map(PathBuf::from))
        });
        Ok(Config { data_dir, bind, tls, tls_names, docker, agent_bind, agent_address, agent_image, compose: get("FREIGHTDOCK_COMPOSE"), setup_token, cluster })
    }
}

fn main() -> Result<()> {
    // Local time with its offset ("2026-11-09T21:14:09.485+01:00"): the zone TZ names, else /etc/localtime, which is how
    // a container is told its zone. tracing's own clock is UTC only.
    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new("%Y-%m-%dT%H:%M:%S%.3f%:z".into()))
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "freightdock=info,warn".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match command(&args)? {
        Command::Serve => {}
        Command::Version => {
            println!("freightdock {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Command::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Command::Healthcheck => return healthcheck(),
        Command::SetupToken => return print_setup_token(),
        Command::Restore(archive) => return restore_offline(archive.map(PathBuf::from)),
    }
    let config = Config::from(|k| std::env::var(k).ok())?;
    let restart = tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run(config))?;
    if restart {
        return Err(start_again());
    }
    Ok(())
}

const USAGE: &str = "\
Usage: freightdock [command]

With no command, Freightdock serves (configured by FREIGHTDOCK_* in the environment).

Commands:
  setup-token        print the first run's setup token, while nobody is set up yet
  restore <archive>  stage a backup for the next start, while Freightdock is stopped
  healthcheck        whether Freightdock answers on its port (the image's HEALTHCHECK)
  --version          the version
  --help             this
";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Command {
    Serve,
    Version,
    Help,
    Healthcheck,
    SetupToken,
    Restore(Option<String>),
}

/// Reads the command line. Anything it does not know is refused: started as a server instead, a mistyped command
/// beside a running Freightdock would print a setup token of its own and die on the taken port.
fn command(args: &[String]) -> Result<Command> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match a.as_slice() {
        [] => Command::Serve,
        ["--version" | "-V" | "version"] => Command::Version,
        ["--help" | "-h" | "help"] => Command::Help,
        ["healthcheck"] => Command::Healthcheck,
        ["setup-token"] => Command::SetupToken,
        ["restore"] => Command::Restore(None),
        ["restore", archive] => Command::Restore(Some(archive.to_string())),
        _ => anyhow::bail!("freightdock does not know `{}`.\n\n{USAGE}", a.join(" ")),
    })
}

/// Runs this binary anew in this process (same id, arguments and environment), so a restore staged in the data folder
/// is put in place as it starts. Answers only if that could not be done.
fn start_again() -> anyhow::Error {
    tracing::info!("starting again, to put the restored backup in place");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // /proc/self/exe is this very binary even when the file was replaced since it started.
        let exe = if std::path::Path::new("/proc/self/exe").exists() { PathBuf::from("/proc/self/exe") } else { std::env::current_exe().unwrap_or_default() };
        let e = std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec();
        anyhow::anyhow!("could not start again ({e}); start Freightdock by hand and the restore is put in place")
    }
    #[cfg(not(unix))]
    anyhow::anyhow!("start Freightdock again: the restore is put in place as it starts")
}

/// `freightdock restore <archive>`: while Freightdock is stopped, stages a backup (the passphrase from
/// `FREIGHTDOCK_BACKUP_PASSPHRASE`, else the first line of standard input) for its next start.
fn restore_offline(archive: Option<PathBuf>) -> Result<()> {
    let archive = archive.context("say which archive: freightdock restore <file>")?;
    let config = Config::from(|k| std::env::var(k).ok())?;
    let data = std::fs::read(&archive).with_context(|| format!("reading {}", archive.display()))?;
    let passphrase = match std::env::var("FREIGHTDOCK_BACKUP_PASSPHRASE") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("The backup's passphrase:");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    std::fs::create_dir_all(&config.data_dir)?;
    let from = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let m = backup::stage(&config.data_dir, &data, &passphrase, "freightdock restore", &from)?;
    println!("Staged the backup made {} by Freightdock {}; it is put in place when Freightdock next starts with FREIGHTDOCK_DATA_DIR={}.", notify::utc_text(m.made_at), m.freightdock, config.data_dir.display());
    Ok(())
}

/// `freightdock setup-token` (in a container, `docker exec freightdock freightdock setup-token`): the first run's setup token,
/// alone on standard output, while it waits in the data folder ("Sign-in" 16).
fn print_setup_token() -> Result<()> {
    let config = Config::from(|k| std::env::var(k).ok())?;
    match auth::read_setup_token(&config.data_dir) {
        Some(t) => {
            println!("{t}");
            Ok(())
        }
        None if config.setup_token.is_some() => anyhow::bail!("FREIGHTDOCK_SETUP_TOKEN gives the setup token: it is the one you set"),
        None => anyhow::bail!("no setup token waits in {}: Freightdock is set up already (sign in), or has not started yet", config.data_dir.display()),
    }
}

/// `freightdock healthcheck`: for the image's HEALTHCHECK, without curl in the image.
fn healthcheck() -> Result<()> {
    let config = Config::from(|k| std::env::var(k).ok())?;
    let port = config.bind.port();
    let mut s = std::net::TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    // Only that it answers: TLS or not, a listening, accepting server is a healthy one at this level.
    use std::io::Write;
    s.write_all(b"")?;
    Ok(())
}

/// Serves until Freightdock stops; answers whether it is to start again (a restore was staged).
async fn run(config: Config) -> Result<bool> {
    std::fs::create_dir_all(&config.data_dir).with_context(|| format!("making the data folder {}", config.data_dir.display()))?;
    let restored = backup::apply_staged(&config.data_dir).context("putting a staged restore in place")?;
    if let Some(r) = &restored {
        tracing::info!("restored a backup {r}");
    }
    // In a cluster the database is the managers' ("Cluster" 9): loaded from them before it is opened, and every write
    // sent to them after.
    let cluster = match &config.cluster {
        Some((api, token, ca)) => {
            let c = std::sync::Arc::new(cluster::Cluster::new(api, token.clone(), ca.clone())?);
            tracing::info!("the UI of the cluster whose managers are at {}", c.api);
            Some(c)
        }
        None => None,
    };
    let loaded = match &cluster {
        Some(c) => Some(replica::load(c, &config.data_dir).await.context("loading the database from the managers")?),
        None => None,
    };
    // The key that seals secrets: the managers' (loaded above) in a cluster, and sent with every snapshot.
    let sealer = secret::Sealer::open_or_make(&config.data_dir.join("secret.key"))?;
    let mut db = db::Db::open(&config.data_dir.join("freightdock.db"))?;
    if let (Some(c), Some(l)) = (&cluster, loaded) {
        let r = db.replicate(c.clone(), config.data_dir.clone(), l).await.context("replicating the database through the managers")?;
        let db = db.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if r.is_stale() {
                    tracing::error!("another Freightdock writes to the cluster's database: stopping, to load it again");
                    std::process::exit(75);
                }
                if let Err(e) = db.push().await {
                    tracing::warn!("a database change not yet with the managers: {e:#}");
                }
            }
        });
    }
    let docker = docker::Docker::connect(&config.docker)?;
    docker.negotiate().await;
    match docker.client().version().await {
        Ok(v) => tracing::info!("Docker {} (API {}) at {}", v.version.unwrap_or_default(), v.api_version.unwrap_or_default(), config.docker),
        Err(e) => tracing::warn!("Docker at {} does not answer yet: {e}", config.docker),
    }
    let (acceptor, tls_ca) = match &config.tls {
        Some(files) => {
            let names = tls::names(config.bind.ip(), tls::hostname().as_deref(), &tls::interfaces(), config.tls_names.as_deref())?;
            let ca = tls::prepare(files, &config.data_dir.join("tls"), &names)?;
            (Some(tls::acceptor(&files.cert, &files.key)?), ca)
        }
        None => (None, None),
    };
    let sockets = docker::sockets::Sockets::new(&config.data_dir.join("run"))?;
    let ca = agent::ca::Ca::open_or_make(&config.data_dir.join("ca"))?;
    let agents = agent::Agents::new(ca, config.agent_address.clone(), config.agent_bind.map(|b| b.port()), config.agent_image.clone())?;
    let compose = stacks::compose::Compose::find(config.compose.as_deref());
    match compose.path() {
        Some(p) => tracing::info!("Docker Compose at {}", p.display()),
        None => tracing::warn!("no Docker Compose found: stacks can be written but not deployed (set FREIGHTDOCK_COMPOSE)"),
    }
    let stacks = stacks::Stacks::new(config.data_dir.join("stacks"), compose);
    let app = state::AppState::new(db, acceptor.is_some(), docker, sealer, sockets, agents, stacks);
    let _ = app.backups.data_dir.set(config.data_dir.clone());
    if let Some(ca) = tls_ca {
        let _ = app.tls_ca.set(ca);
    }
    if let Some(own) = docker::own::detect(&app.docker).await {
        tracing::info!("Freightdock runs in container {}: a console there is an administrator's alone", &own[..12.min(own.len())]);
        let _ = app.own_container.set(own);
    }
    // The token waits in the data folder while nobody exists, the same across restarts, for `freightdock setup-token`
    // ("Sign-in" 16); one given in FREIGHTDOCK_SETUP_TOKEN is never written down.
    if api::configured(&app).await.unwrap_or(true) {
        auth::forget_setup_token(&config.data_dir);
        let _ = app.setup_token.set(config.setup_token.clone().unwrap_or_else(auth::setup_token));
    } else if let Some(given) = config.setup_token.clone() {
        auth::forget_setup_token(&config.data_dir);
        let _ = app.setup_token.set(given);
        tracing::warn!("Freightdock is not set up yet: open it and make the first administrator with the setup token FREIGHTDOCK_SETUP_TOKEN gives");
    } else {
        let token = auth::kept_setup_token(&config.data_dir).context("keeping the setup token in the data folder")?;
        let _ = app.setup_token.set(token.clone());
        tracing::warn!("Freightdock is not set up yet: open it and make the first administrator with this setup token: {token} (`freightdock setup-token` shows it again)");
    }
    if let Some(c) = cluster {
        let _ = app.cluster.set(c);
    }
    if let Some(r) = restored {
        audit::record(&app.db, None, None, audit::Entry { action: "backup.restored", target: None, detail: Some(r) }).await;
    }
    stacks::spawn_poller(app.clone());
    let ui = axum::Router::new().fallback(web::serve).with_state(app.clone());
    let router = web::harden(api::router(app.clone()).route("/api/health", get(|| async { "ok" })).fallback_service(ui));
    let listener = tokio::net::TcpListener::bind(config.bind).await.with_context(|| format!("listening on {}", config.bind))?;
    tracing::info!("Freightdock {} on {}://{}", env!("CARGO_PKG_VERSION"), if acceptor.is_some() { "https" } else { "http" }, config.bind);
    let stop = tokio_util::sync::CancellationToken::new();
    let agents = match config.agent_bind {
        Some(bind) => {
            let l = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("listening for agents on {bind}"))?;
            tracing::info!("the agent endpoint on {bind} (TLS from Freightdock's own CA)");
            let tls = Some(tokio_rustls::TlsAcceptor::from(app.agents.tls.clone()));
            Some(tokio::spawn(serve(l, agent::endpoint::router(app.clone()), tls, stop.clone().cancelled_owned())))
        }
        None => None,
    };
    tokio::spawn(environments::watch(app.clone(), stop.clone()));
    tokio::spawn(history::watch(app.clone(), stop.clone()));
    tokio::spawn(notify::watch(app.clone(), stop.clone()));
    tokio::spawn(updates::watch(app.clone(), stop.clone()));
    tokio::spawn(backup::watch(app.clone(), stop.clone()));
    tokio::spawn(authlog::watch(app.clone(), stop.clone()));
    let s = stop.clone();
    let restart = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (a, r) = (app.clone(), restart.clone());
    tokio::spawn(async move {
        tokio::select! {
            _ = shutdown() => {}
            _ = a.backups.restart.notified() => r.store(true, std::sync::atomic::Ordering::SeqCst),
        }
        s.cancel();
    });
    serve(listener, router, acceptor, stop.cancelled_owned()).await;
    if let Some(a) = agents {
        let _ = a.await;
    }
    Ok(restart.load(std::sync::atomic::Ordering::SeqCst))
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("stopping");
}

/// A request head must arrive within this; a TLS handshake too.
const HEAD_WITHIN: Duration = Duration::from_secs(15);
/// A request's body may stop coming for this long, and no longer ("HTTPS" 6).
const BODY_IDLE: Duration = HEAD_WITHIN;
/// At most this many connections at once.
const CONNECTIONS: usize = 1024;
/// How long the requests under way get to finish when Freightdock stops.
const DRAIN_WITHIN: Duration = Duration::from_secs(10);

/// hyper driven directly, as in FinStats: `axum::serve` sets no timer, so a request head could take for ever.
async fn serve(listener: tokio::net::TcpListener, app: axum::Router, tls: Option<tokio_rustls::TlsAcceptor>, shutdown: impl std::future::Future<Output = ()>) {
    serve_with(listener, app, tls, shutdown, BODY_IDLE).await
}

/// `serve`, with how long a request's body may stop coming before it is given up (`BODY_IDLE`; a test's is shorter).
async fn serve_with(listener: tokio::net::TcpListener, app: axum::Router, tls: Option<tokio_rustls::TlsAcceptor>, shutdown: impl std::future::Future<Output = ()>, body_idle: Duration) {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use std::sync::Arc;
    let permits = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS));
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(HEAD_WITHIN);
    tokio::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            p = permits.clone().acquire_owned() => p.expect("the semaphore is never closed"),
            _ = &mut shutdown => break,
        };
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("could not accept a connection: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        keepalive(&stream);
        let app = app.clone();
        // The certificate a client showed, for the agent endpoint (the web server asks for none).
        let service = move |cert: agent::endpoint::ClientCert| {
            hyper::service::service_fn(move |mut req: axum::http::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
                req.extensions_mut().insert(cert.clone());
                tower_service::Service::call(&mut app.clone(), req.map(|b| axum::body::Body::new(IdleBody::new(b, body_idle))))
            })
        };
        let builder = builder.clone();
        let tls = tls.clone();
        let mut stopping = stopping.clone();
        tokio::spawn(async move {
            let _permit = permit;
            macro_rules! drive {
                ($io:expr, $cert:expr) => {{
                    let conn = builder.serve_connection(TokioIo::new($io), service($cert)).with_upgrades();
                    tokio::pin!(conn);
                    tokio::select! {
                        _ = conn.as_mut() => return,
                        _ = stopping.wait_for(|s| *s) => conn.as_mut().graceful_shutdown(),
                    }
                    let _ = conn.await;
                }};
            }
            match tls {
                Some(acceptor) => {
                    if let Ok(Ok(s)) = tokio::time::timeout(HEAD_WITHIN, acceptor.accept(stream)).await {
                        let cert = s.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| freightdock_tunnel::sha256_hex(c));
                        drive!(s, agent::endpoint::ClientCert(cert))
                    }
                }
                None => drive!(stream, agent::endpoint::ClientCert(None)),
            }
        });
    }
    drop(listener);
    let _ = stop.send(true);
    let all = u32::try_from(CONNECTIONS).unwrap_or(u32::MAX);
    if tokio::time::timeout(DRAIN_WITHIN, permits.acquire_many(all)).await.is_err() {
        tracing::warn!("stopping with requests still under way after {}s", DRAIN_WITHIN.as_secs());
    }
}

/// A request's body that ends in an error once it has stopped coming for `idle`: the handler waiting on it gets the error
/// (a 400) and the connection, with its slot, is let go. Only time spent waiting on the client counts: while the
/// handler does not ask for more, nothing is timed, and each piece that comes starts the clock again.
struct IdleBody<B> {
    inner: B,
    idle: Duration,
    waiting: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<B> IdleBody<B> {
    fn new(inner: B, idle: Duration) -> Self {
        IdleBody { inner, idle, waiting: None }
    }
}

impl<B> hyper::body::Body for IdleBody<B>
where
    B: hyper::body::Body + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = B::Data;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;
        let this = &mut *self;
        match std::pin::Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                this.waiting = None;
                Poll::Ready(frame.map(|f| f.map_err(Into::into)))
            }
            Poll::Pending => {
                let idle = this.idle;
                let sleep = this.waiting.get_or_insert_with(|| Box::pin(tokio::time::sleep(idle)));
                match sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Some(Err(format!("the request's body stopped coming for {}s", idle.as_secs_f32()).into()))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// TCP keepalive, so a peer that vanished (an agent's host switched off) is noticed within about a minute, not hours.
fn keepalive(stream: &tokio::net::TcpStream) {
    let k = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30)).with_interval(Duration::from_secs(10));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let k = k.with_retries(3);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&k);
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_command_freightdock_does_not_know_is_refused_rather_than_serving() {
        let parse = |a: &[&str]| super::command(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(parse(&[]).unwrap(), super::Command::Serve);
        assert_eq!(parse(&["setup-token"]).unwrap(), super::Command::SetupToken);
        assert_eq!(parse(&["healthcheck"]).unwrap(), super::Command::Healthcheck);
        assert_eq!(parse(&["--version"]).unwrap(), super::Command::Version);
        assert_eq!(parse(&["restore", "a.backup"]).unwrap(), super::Command::Restore(Some("a.backup".into())));
        assert_eq!(parse(&["--help"]).unwrap(), super::Command::Help);
        let e = parse(&["setup-tokn"]).unwrap_err().to_string();
        assert!(e.contains("setup-tokn") && e.contains("setup-token"), "{e}");
        assert!(parse(&["setup-token", "extra"]).is_err());
    }

    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn the_defaults_are_https_on_7443_with_the_local_docker() {
        let c = Config::from(env(&[])).unwrap();
        assert_eq!(c.bind, "0.0.0.0:7443".parse().unwrap());
        assert_eq!(c.data_dir, PathBuf::from("/data"));
        assert_eq!(c.tls, Some(tls::Files { cert: PathBuf::from("/data/tls/cert.pem"), key: PathBuf::from("/data/tls/key.pem"), own: false }), "Freightdock's own, made and kept");
        assert_eq!(c.tls_names, None);
        assert_eq!(c.docker, "unix:///var/run/docker.sock");
        assert_eq!(c.agent_bind, Some("0.0.0.0:7444".parse().unwrap()), "agents dial 7444, on its own TLS");
        assert_eq!(c.agent_address, None);
        assert_eq!(c.agent_image, "freightdock-agent:latest");
        assert_eq!(c.setup_token, None, "made at start");
    }

    #[test]
    fn a_cluster_is_an_address_and_its_manager_s_token() {
        assert_eq!(Config::from(env(&[])).unwrap().cluster, None, "no cluster: the single-host Freightdock");
        let c = Config::from(env(&[("FREIGHTDOCK_CLUSTER_API", "http://127.0.0.1:7451")])).unwrap();
        assert_eq!(c.cluster, Some(("http://127.0.0.1:7451".into(), PathBuf::from("/var/lib/freightdock-manager/admin.token"), None)));
        let c = Config::from(env(&[("FREIGHTDOCK_CLUSTER_API", "http://m:7451"), ("FREIGHTDOCK_CLUSTER_TOKEN_FILE", "/run/admin.token"), ("FREIGHTDOCK_CLUSTER_CA_FILE", "/run/ca.pem")])).unwrap();
        let c = c.cluster.unwrap();
        assert_eq!((c.1, c.2), (PathBuf::from("/run/admin.token"), Some(PathBuf::from("/run/ca.pem"))));
    }

    #[test]
    fn a_setup_token_can_be_given_but_not_a_short_one() {
        assert_eq!(Config::from(env(&[("FREIGHTDOCK_SETUP_TOKEN", "a-token-for-the-installer")])).unwrap().setup_token.as_deref(), Some("a-token-for-the-installer"));
        assert!(Config::from(env(&[("FREIGHTDOCK_SETUP_TOKEN", "short")])).is_err());
        assert_eq!(Config::from(env(&[("FREIGHTDOCK_SETUP_TOKEN", " ")])).unwrap().setup_token, None);
    }

    #[test]
    fn the_agent_endpoint_can_be_moved_or_turned_off() {
        let c = Config::from(env(&[("FREIGHTDOCK_AGENT_BIND", "127.0.0.1:9444"), ("FREIGHTDOCK_AGENT_ADDRESS", "agents.example.com:443")])).unwrap();
        assert_eq!(c.agent_bind, Some("127.0.0.1:9444".parse().unwrap()));
        assert_eq!(c.agent_address.as_deref(), Some("agents.example.com:443"));
        assert_eq!(Config::from(env(&[("FREIGHTDOCK_AGENT_BIND", "off")])).unwrap().agent_bind, None);
        assert!(Config::from(env(&[("FREIGHTDOCK_AGENT_BIND", "nowhere")])).is_err());
    }

    #[test]
    fn tls_can_be_turned_off_or_given_and_nonsense_stops_the_start() {
        assert_eq!(Config::from(env(&[("FREIGHTDOCK_TLS", "off")])).unwrap().tls, None);
        let c = Config::from(env(&[("FREIGHTDOCK_TLS_CERT", "/c.pem"), ("FREIGHTDOCK_TLS_KEY", "/k.pem")])).unwrap();
        assert_eq!(c.tls, Some(tls::Files { cert: PathBuf::from("/c.pem"), key: PathBuf::from("/k.pem"), own: true }), "the person's own, never touched");
        assert_eq!(Config::from(env(&[("FREIGHTDOCK_TLS_NAMES", "boat.lan, 198.51.100.9")])).unwrap().tls_names.as_deref(), Some("boat.lan, 198.51.100.9"));
        assert!(Config::from(env(&[("FREIGHTDOCK_TLS", "maybe")])).is_err());
        assert!(Config::from(env(&[("FREIGHTDOCK_BIND", "nowhere")])).is_err());
    }

    /// A client that sends a request's head and then no body must not hold its connection, and so one of the
    /// `CONNECTIONS` slots, for ever ("HTTPS" 6).
    #[tokio::test]
    async fn a_body_that_stops_coming_lets_its_connection_go() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/in", axum::routing::post(|b: String| async move { b }));
        tokio::spawn(serve_with(listener, router, None, std::future::pending(), Duration::from_millis(300)));
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"POST /in HTTP/1.1\r\nHost: x\r\nContent-Length: 64\r\n\r\nonly a part").await.unwrap();
        let mut got = Vec::new();
        let r = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut got)).await;
        assert!(r.is_ok(), "the connection is let go");
        assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 400"), "{}", String::from_utf8_lossy(&got));

        // A body that keeps coming, however slowly, is read to its end.
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"POST /in HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nConnection: close\r\n\r\n").await.unwrap();
        for b in [b"a", b"b", b"c", b"d"] {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c.write_all(b).await.unwrap();
        }
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut got)).await.unwrap().unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.starts_with("HTTP/1.1 200") && got.ends_with("abcd"), "{got}");
    }

    #[test]
    fn freightdock_s_own_docker_host_wins_over_docker_s() {
        let c = Config::from(env(&[("DOCKER_HOST", "tcp://a:2375"), ("FREIGHTDOCK_DOCKER_HOST", "unix:///b.sock")])).unwrap();
        assert_eq!(c.docker, "unix:///b.sock");
    }
}
