//! What every request and every background task shares: the database, the sign-in limiter, how the server is reached,
//! and the cluster whose UI this is.

use std::sync::Arc;

use crate::auth::Limiter;
use crate::db::Db;

pub type App = Arc<AppState>;

pub struct AppState {
    pub db: Db,
    pub limiter: Limiter,
    /// The server answers over HTTPS, so its cookies are `Secure`.
    pub secure: bool,
    pub sealer: crate::secret::Sealer,
    pub oidc: crate::oidc::Oidc,
    /// Sign-ins whose password was right and whose second factor is to come.
    pub tickets: crate::signin::totp::Tickets,
    /// The CA that signed the web server's certificate, when Freightdock made it (`GET /api/tls/ca.pem`).
    pub tls_ca: std::sync::OnceLock<String>,
    /// What `/api/setup` asks for while nobody exists ("Sign-in" 14): made at start and printed to the log, or
    /// `FREIGHTDOCK_SETUP_TOKEN`. Unset, setup is refused.
    pub setup_token: std::sync::OnceLock<String>,
    /// The cluster this Freightdock is the UI of ("Cluster" 1).
    pub cluster: std::sync::OnceLock<std::sync::Arc<crate::cluster::Cluster>>,
    /// The data folder, where the setup token waits until setup is done.
    pub data_dir: std::sync::OnceLock<std::path::PathBuf>,
    /// The MCP sessions `initialize` began ("MCP" 1).
    pub mcp: crate::mcp::Sessions,
}

impl AppState {
    pub fn new(db: Db, secure: bool, sealer: crate::secret::Sealer) -> App {
        Arc::new(AppState {
            db,
            limiter: Limiter::default(),
            secure,
            sealer,
            oidc: Default::default(),
            tickets: Default::default(),
            tls_ca: Default::default(),
            setup_token: Default::default(),
            cluster: Default::default(),
            data_dir: Default::default(),
            mcp: Default::default(),
        })
    }
}
