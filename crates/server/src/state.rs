//! What every request and every background task shares: the database, the sign-in limiter, how the server is reached,
//! the Docker endpoints, and the stacks with their Compose.

use std::sync::Arc;

use crate::auth::Limiter;
use crate::db::Db;

pub type App = Arc<AppState>;

pub struct AppState {
    pub db: Db,
    pub limiter: Limiter,
    /// The server answers over HTTPS, so its cookies are `Secure`.
    pub secure: bool,
    pub docker: crate::docker::Docker,
    pub sealer: crate::secret::Sealer,
    pub oidc: crate::oidc::Oidc,
    /// Sign-ins whose password was right and whose second factor is to come.
    pub tickets: crate::signin::totp::Tickets,
    /// The unix sockets that agents' tunnels and TLS hosts are offered on.
    pub sockets: crate::docker::sockets::Sockets,
    pub agents: crate::agent::Agents,
    pub stacks: crate::stacks::Stacks,
    pub templates: crate::templates::Cache,
    pub webhooks: crate::webhooks::Gate,
    /// The MCP sessions `/mcp` began.
    pub mcp: crate::mcp::Sessions,
    /// What was told lately, so the same news is not told twice in a row.
    pub notifier: crate::notify::Notifier,
    /// The data folder, for backups, and how a restore has Freightdock start again.
    pub backups: crate::backup::Backups,
    /// Held by an import from Portainer: one at a time.
    pub importing: tokio::sync::Mutex<()>,
    /// The CA that signed the web server's certificate, when Freightdock made it (`GET /api/tls/ca.pem`).
    pub tls_ca: std::sync::OnceLock<String>,
    /// The id of the container Freightdock runs in, on the local Docker, when it runs in one (`docker::own`): a console
    /// there is an Administrator's alone ("Access" 20).
    pub own_container: std::sync::OnceLock<String>,
    /// What `/api/setup` asks for while nobody exists ("Sign-in" 14): made at start and printed to the log, or
    /// `FREIGHTDOCK_SETUP_TOKEN`. Unset, setup is refused.
    pub setup_token: std::sync::OnceLock<String>,
    /// The cluster this Freightdock is the UI of, when `FREIGHTDOCK_CLUSTER_API` names its manager ("Cluster" 1).
    pub cluster: std::sync::OnceLock<std::sync::Arc<crate::cluster::Cluster>>,
}

impl AppState {
    pub fn new(
        db: Db,
        secure: bool,
        docker: crate::docker::Docker,
        sealer: crate::secret::Sealer,
        sockets: crate::docker::sockets::Sockets,
        agents: crate::agent::Agents,
        stacks: crate::stacks::Stacks,
    ) -> App {
        Arc::new(AppState {
            db,
            limiter: Limiter::default(),
            secure,
            docker,
            sealer,
            oidc: Default::default(),
            tickets: Default::default(),
            sockets,
            agents,
            stacks,
            templates: Default::default(),
            webhooks: Default::default(),
            mcp: Default::default(),
            notifier: Default::default(),
            backups: Default::default(),
            importing: Default::default(),
            tls_ca: Default::default(),
            own_container: Default::default(),
            setup_token: Default::default(),
            cluster: Default::default(),
        })
    }
}
