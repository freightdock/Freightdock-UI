//! The guard every API route passes through (`api::router` puts it on as a route layer), and the rule each route is
//! held to: [`RULES`], one line a route, by method and the path as the router writes it. A route with no line is
//! refused, so a route added without one cannot be reached until it has one, and a test reads every `.route(` in the
//! source and fails for any without a line ("Access" 5).
//!
//! The guard signs the person in (`Authed`), holds them to the rule, and leaves what it found on the request for the
//! handler: the `Authed` (so it is looked up once) and, for an environment's route, the [`Access`].

use axum::extract::{FromRequestParts, MatchedPath, RawPathParams, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::ownership::{self, Owners};
use super::{Access, Action, Allow};
use crate::api::Authed;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// What a route needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Nobody signed in: status, setup, sign-in, SSO, a webhook's call (its token is the credential), `/mcp` (which
    /// signs in by token itself and calls back through this router), health.
    Public,
    /// Anybody signed in; the handler says what of it they see (environments they have a role on).
    Session,
    /// An Administrator: Freightdock's own settings, users and teams, environments, registries, the logs.
    Admin,
    /// An environment's route (`{env}`): the person's role there must allow the action, on the resource named.
    Env(Act, Res),
}

/// The action of an environment's route: fixed, or read from its `{action}` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Is(Action),
    /// `containers/{id}/{action}`: start, stop, restart, kill, pause, unpause.
    Container,
    /// `stacks/{id}/{action}`: start and stop operate; deploy, pull and remove manage.
    Stack,
}

/// The container or stack a route is about, which a limited person must be able to see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    None,
    /// `{id}`, a container's id or name.
    Container,
    /// `{id}`, a Freightdock stack's id.
    Stack,
    /// `{name}`, a Compose project Freightdock did not start: the administrators', like a container with no record.
    External,
    /// `{name}`, a volume.
    Volume,
    /// `{id}`, a network's id or name.
    Network,
    /// `{id}`, an image's id or a reference to it.
    Image,
    /// `{id}` or `{name}`, a swarm service or a swarm stack: no owner record, and only an environment administrator
    /// makes one, so the administrators', as an external project is ("Swarm" 7).
    Swarm,
}

use Act::Is;
use Action::*;
use Res::{Container as C, External as X, Image as I, Network as W, None as N, Stack as S, Swarm as Q, Volume as V};
use Rule::{Admin, Env, Public, Session};

/// Every route there is, and its rule.
pub const RULES: &[(&str, &str, Rule)] = &[
    // Freightdock's own
    ("GET", "/api/health", Public),
    ("GET", "/api/status", Public),
    ("POST", "/api/setup", Public),
    ("POST", "/api/login", Public),
    ("POST", "/api/logout", Session),
    ("GET", "/api/me", Session),
    ("GET", "/api/users/{id}/avatar", Session),
    ("PUT", "/api/me/password", Session),
    ("POST", "/api/me/sso-link", Session),
    ("GET", "/api/audit", Admin),
    ("GET", "/api/auth-log", Admin),
    ("GET", "/api/changelog", Session),
    ("GET", "/api/auth/oidc/login", Public),
    ("GET", "/api/auth/oidc/callback", Public),
    ("GET", "/api/auth/sso/{id}/login", Public),
    ("GET", "/api/auth/sso/{id}/callback", Public),
    // The code step of a sign-in: its ticket is what the right password earned ("Two-factor" 2).
    ("POST", "/api/login/totp", Public),
    ("POST", "/api/login/totp/enrol", Public),
    ("GET", "/api/me/totp", Session),
    ("POST", "/api/me/totp", Session),
    ("POST", "/api/me/totp/confirm", Session),
    ("POST", "/api/me/totp/off", Session),
    ("POST", "/api/me/totp/recovery", Session),
    ("DELETE", "/api/users/{id}/totp", Admin),
    ("GET", "/api/settings/two-factor", Admin),
    ("PUT", "/api/settings/two-factor", Admin),
    ("GET", "/api/settings/sso", Admin),
    ("POST", "/api/settings/sso/test", Admin),
    ("PUT", "/api/settings/sso/{id}", Admin),
    ("DELETE", "/api/settings/sso/{id}", Admin),
    ("POST", "/api/settings/sso/{id}/ldap-test", Admin),
    ("GET", "/api/settings/appearance", Admin),
    ("PUT", "/api/settings/appearance", Admin),
    ("GET", "/api/settings/templates", Admin),
    ("PUT", "/api/settings/templates", Admin),
    ("GET", "/api/templates", Session),
    ("POST", "/api/templates/custom", Admin),
    ("GET", "/api/templates/custom/{id}", Session),
    ("PUT", "/api/templates/custom/{id}", Admin),
    ("DELETE", "/api/templates/custom/{id}", Admin),
    ("POST", "/api/webhooks/{token}", Public),
    ("GET", "/api/users", Admin),
    ("POST", "/api/users", Admin),
    ("PUT", "/api/users/{id}", Admin),
    ("DELETE", "/api/users/{id}", Admin),
    ("POST", "/api/users/{id}/password", Admin),
    ("GET", "/api/teams", Session),
    ("POST", "/api/teams", Admin),
    ("PUT", "/api/teams/{id}", Admin),
    ("DELETE", "/api/teams/{id}", Admin),
    ("GET", "/api/environments", Session),
    ("POST", "/api/environments", Admin),
    ("GET", "/api/environments/{id}", Session),
    ("PUT", "/api/environments/{id}", Admin),
    ("DELETE", "/api/environments/{id}", Admin),
    ("POST", "/api/environments/{id}/join", Admin),
    ("GET", "/api/environments/{id}/access", Admin),
    ("PUT", "/api/environments/{id}/access", Admin),
    ("GET", "/api/environment-groups", Session),
    ("POST", "/api/environment-groups", Admin),
    ("PUT", "/api/environment-groups/{id}", Admin),
    ("DELETE", "/api/environment-groups/{id}", Admin),
    ("GET", "/api/environment-groups/{id}/access", Admin),
    ("PUT", "/api/environment-groups/{id}/access", Admin),
    ("GET", "/api/environments/{id}/security", Admin),
    ("PUT", "/api/environments/{id}/security", Admin),
    ("GET", "/api/environment-groups/{id}/security", Admin),
    ("PUT", "/api/environment-groups/{id}/security", Admin),
    ("GET", "/api/tags", Session),
    ("POST", "/api/tags", Admin),
    ("PUT", "/api/tags/{id}", Admin),
    ("DELETE", "/api/tags/{id}", Admin),
    ("GET", "/api/registries", Session),
    ("POST", "/api/registries", Admin),
    ("GET", "/api/registries/{id}", Session),
    ("PUT", "/api/registries/{id}", Admin),
    ("DELETE", "/api/registries/{id}", Admin),
    ("GET", "/api/registries/{id}/repositories", Session),
    ("GET", "/api/registries/{id}/tags", Session),
    ("GET", "/api/tokens", Session),
    ("POST", "/api/tokens", Session),
    ("DELETE", "/api/tokens/{id}", Session),
    // The cluster: reading it is anybody's who is signed in, changing it an Administrator's ("Cluster" 2)
    ("GET", "/api/cluster", Session),
    ("GET", "/api/cluster/nodes", Session),
    ("DELETE", "/api/cluster/nodes/{id}", Admin),
    ("POST", "/api/cluster/nodes/{id}/commands", Admin),
    ("PUT", "/api/cluster/nodes/{id}/labels", Admin),
    ("POST", "/api/cluster/nodes/{id}/authorize", Admin),
    ("GET", "/api/cluster/commands/{id}", Session),
    ("GET", "/api/cluster/events", Session),
    ("GET", "/api/cluster/recoveries", Session),
    ("POST", "/api/cluster/join-tokens", Admin),
    ("GET", "/api/cluster/stacks", Session),
    ("POST", "/api/cluster/stacks", Admin),
    ("GET", "/api/cluster/stacks/{name}", Session),
    ("DELETE", "/api/cluster/stacks/{name}", Admin),
    ("POST", "/api/cluster/stacks/{name}/{action}", Admin),
    ("GET", "/api/settings/history", Admin),
    ("PUT", "/api/settings/history", Admin),
    ("GET", "/api/settings/updates", Admin),
    ("PUT", "/api/settings/updates", Admin),
    ("POST", "/api/updates/check", Admin),
    ("GET", "/api/settings/backups", Admin),
    ("PUT", "/api/settings/backups", Admin),
    ("GET", "/api/backups", Admin),
    ("POST", "/api/backups/run", Admin),
    ("POST", "/api/backups/restore", Admin),
    ("POST", "/api/backups/upload", Admin),
    ("GET", "/api/backups/{name}/download", Admin),
    ("GET", "/api/notifications", Admin),
    ("POST", "/api/notifications", Admin),
    ("PUT", "/api/notifications/{id}", Admin),
    ("DELETE", "/api/notifications/{id}", Admin),
    ("POST", "/api/notifications/{id}/test", Admin),
    ("GET", "/api/portainer", Admin),
    ("PUT", "/api/portainer", Admin),
    ("DELETE", "/api/portainer", Admin),
    ("POST", "/api/portainer/scan", Admin),
    ("POST", "/api/portainer/import", Admin),
    ("POST", "/mcp", Public),
    ("GET", "/mcp", Public),
    ("DELETE", "/mcp", Public),
    ("GET", "/api/settings/mcp", Admin),
    ("PUT", "/api/settings/mcp", Admin),
    ("DELETE", "/api/settings/mcp/tokens/{id}", Admin),
    ("GET", "/api/mcp/status", Session),
    // The CA that signed the web server's certificate: a certificate, public, fetched before anybody trusts it.
    ("GET", "/api/tls/ca.pem", Public),
    // One environment's Docker
    ("GET", "/api/envs/{env}/info", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/df", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/summary", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/prune", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/events/ws", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/stats", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/containers", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/containers", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/containers/prune", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/containers/{id}", Env(Is(Read), C)),
    ("DELETE", "/api/envs/{env}/containers/{id}", Env(Is(Manage), C)),
    ("GET", "/api/envs/{env}/containers/{id}/config", Env(Is(Read), C)),
    ("GET", "/api/envs/{env}/containers/{id}/top", Env(Is(Read), C)),
    ("POST", "/api/envs/{env}/containers/{id}/rename", Env(Is(Manage), C)),
    ("POST", "/api/envs/{env}/containers/{id}/recreate", Env(Is(Manage), C)),
    ("PUT", "/api/envs/{env}/containers/{id}/access", Env(Is(Manage), C)),
    ("GET", "/api/envs/{env}/containers/{id}/history", Env(Is(Read), C)),
    ("POST", "/api/envs/{env}/containers/{id}/update", Env(Is(Manage), C)),
    ("PUT", "/api/envs/{env}/containers/{id}/auto-update", Env(Is(Manage), C)),
    ("GET", "/api/envs/{env}/updates", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/containers/{id}/logs", Env(Is(Read), C)),
    ("GET", "/api/envs/{env}/containers/{id}/logs/ws", Env(Is(Read), C)),
    ("GET", "/api/envs/{env}/containers/{id}/stats", Env(Is(Read), C)),
    ("GET", "/api/envs/{env}/containers/{id}/stats/ws", Env(Is(Read), C)),
    ("POST", "/api/envs/{env}/containers/{id}/exec", Env(Is(Console), C)),
    ("GET", "/api/envs/{env}/containers/{id}/exec/ws", Env(Is(Console), C)),
    ("GET", "/api/envs/{env}/containers/{id}/attach/ws", Env(Is(Console), C)),
    ("POST", "/api/envs/{env}/containers/{id}/{action}", Env(Act::Container, C)),
    ("POST", "/api/envs/{env}/containers/{id}/webhook", Env(Is(Manage), C)),
    ("DELETE", "/api/envs/{env}/containers/{id}/webhook", Env(Is(Manage), C)),
    ("GET", "/api/envs/{env}/images", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/images/prune", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/images/pull", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/images/push", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/images/build", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/images/load", Env(Is(Create), N)),
    ("GET", "/api/envs/{env}/images/{id}", Env(Is(Read), I)),
    ("PUT", "/api/envs/{env}/images/{id}/access", Env(Is(Manage), I)),
    ("DELETE", "/api/envs/{env}/images/{id}", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/images/{id}/tag", Env(Is(Administer), N)),
    // An export is every file of every layer (code, what a build copied in, credentials baked in): an environment
    // administrator's, as a volume's files are ("Access" 19).
    ("GET", "/api/envs/{env}/images/{id}/export", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/volumes", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/volumes", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/volumes/prune", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/volumes/{name}", Env(Is(Read), V)),
    ("PUT", "/api/envs/{env}/volumes/{name}/access", Env(Is(Manage), V)),
    ("DELETE", "/api/envs/{env}/volumes/{name}", Env(Is(Administer), N)),
    // A volume's files are any container's data (a database's, its secrets): an environment administrator's, reading
    // too ("Access" 10).
    ("GET", "/api/envs/{env}/volumes/{name}/files", Env(Is(Administer), N)),
    ("DELETE", "/api/envs/{env}/volumes/{name}/files", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/volumes/{name}/files/download", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/volumes/{name}/files/upload", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/volumes/{name}/files/mkdir", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/networks", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/networks", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/networks/prune", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/networks/{id}", Env(Is(Read), W)),
    ("PUT", "/api/envs/{env}/networks/{id}/access", Env(Is(Manage), W)),
    ("DELETE", "/api/envs/{env}/networks/{id}", Env(Is(Administer), N)),
    // The network must be one they see; the container in the body is checked by the handler (Manage on it).
    ("POST", "/api/envs/{env}/networks/{id}/connect", Env(Is(Manage), W)),
    ("POST", "/api/envs/{env}/networks/{id}/disconnect", Env(Is(Manage), W)),
    ("POST", "/api/envs/{env}/templates/deploy", Env(Is(Create), N)),
    ("GET", "/api/envs/{env}/stacks", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/stacks", Env(Is(Create), N)),
    ("POST", "/api/envs/{env}/stacks/adopt", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/stacks/git", Env(Is(Create), N)),
    ("GET", "/api/envs/{env}/webhooks", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/stacks/{id}/webhook", Env(Is(Manage), S)),
    ("DELETE", "/api/envs/{env}/stacks/{id}/webhook", Env(Is(Manage), S)),
    ("PUT", "/api/envs/{env}/stacks/{id}/git", Env(Is(Manage), S)),
    ("PUT", "/api/envs/{env}/stacks/{id}/access", Env(Is(Manage), S)),
    ("GET", "/api/envs/{env}/stacks/external/{name}", Env(Is(Read), X)),
    ("GET", "/api/envs/{env}/stacks/{id}", Env(Is(Read), S)),
    ("GET", "/api/envs/{env}/stacks/{id}/environment", Env(Is(Read), S)),
    ("PUT", "/api/envs/{env}/stacks/{id}", Env(Is(Manage), S)),
    ("POST", "/api/envs/{env}/stacks/{id}/{action}", Env(Act::Stack, S)),
    // Swarm: anybody with a role reads the swarm and its nodes, secrets and configs; its services, tasks and stacks are
    // the administrators' for a limited role (the lists leave them out, `Q` hides one); changes reach the whole swarm,
    // so they are an environment administrator's ("Swarm" 7).
    ("GET", "/api/envs/{env}/swarm", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/swarm/nodes", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/swarm/nodes/{id}", Env(Is(Read), N)),
    ("PUT", "/api/envs/{env}/swarm/nodes/{id}", Env(Is(Administer), N)),
    ("DELETE", "/api/envs/{env}/swarm/nodes/{id}", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/services", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/swarm/services", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/services/{id}", Env(Is(Read), Q)),
    ("PUT", "/api/envs/{env}/swarm/services/{id}", Env(Is(Administer), N)),
    ("DELETE", "/api/envs/{env}/swarm/services/{id}", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/swarm/services/{id}/scale", Env(Is(Administer), N)),
    ("POST", "/api/envs/{env}/swarm/services/{id}/rollback", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/services/{id}/logs/ws", Env(Is(Read), Q)),
    ("GET", "/api/envs/{env}/swarm/tasks", Env(Is(Read), N)),
    ("GET", "/api/envs/{env}/swarm/secrets", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/swarm/secrets", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/secrets/{id}", Env(Is(Read), N)),
    ("DELETE", "/api/envs/{env}/swarm/secrets/{id}", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/configs", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/swarm/configs", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/configs/{id}", Env(Is(Read), N)),
    ("DELETE", "/api/envs/{env}/swarm/configs/{id}", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/stacks", Env(Is(Read), N)),
    ("POST", "/api/envs/{env}/swarm/stacks", Env(Is(Administer), N)),
    ("GET", "/api/envs/{env}/swarm/stacks/{name}", Env(Is(Read), Q)),
    ("DELETE", "/api/envs/{env}/swarm/stacks/{name}", Env(Is(Administer), N)),
];

/// The rule for a request, by its method and the path the router matched. `HEAD` is a `GET`.
pub fn rule(method: &Method, path: &str) -> Option<Rule> {
    let m = if method == Method::HEAD { "GET" } else { method.as_str() };
    RULES.iter().find(|(rm, rp, _)| *rm == m && *rp == path).map(|(_, _, r)| *r)
}

impl Act {
    /// The action, given the route's `{action}` word; `None` for a word the route does not know.
    pub fn action(self, word: Option<&str>) -> Option<Action> {
        match self {
            Act::Is(a) => Some(a),
            Act::Container => matches!(word?, "start" | "stop" | "restart" | "kill" | "pause" | "unpause").then_some(Operate),
            Act::Stack => match word? {
                "start" | "stop" => Some(Operate),
                "deploy" | "pull" | "remove" => Some(Manage),
                _ => None,
            },
        }
    }
}

/// The guard: see the module's comment.
pub async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    match check(&app, &mut parts).await {
        Ok(()) => next.run(Request::from_parts(parts, body)).await,
        Err(e) => e.into_response(),
    }
}

async fn check(app: &App, parts: &mut axum::http::request::Parts) -> ApiResult<()> {
    let path = parts.extensions.get::<MatchedPath>().map(|m| m.as_str().to_string()).unwrap_or_default();
    let Some(rule) = rule(&parts.method, &path) else {
        tracing::error!("{} {path} has no rule in access::routes::RULES, so it is refused", parts.method);
        return Err(ApiError::new(StatusCode::FORBIDDEN, "This route has no permission rule yet."));
    };
    if rule == Public {
        return Ok(());
    }
    let a = Authed::from_request_parts(parts, app).await?;
    parts.extensions.insert(a.clone());
    match rule {
        Public | Session => Ok(()),
        Admin => {
            if a.session.user.is_admin() {
                Ok(())
            } else {
                Err(ApiError::forbidden())
            }
        }
        Env(act, res) => {
            let params = RawPathParams::from_request_parts(parts, app).await.map_err(|_| ApiError::bad("That address is not one Freightdock knows."))?;
            let param = |k: &str| params.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
            let env: i64 = param("env").and_then(|e| e.parse().ok()).ok_or_else(|| ApiError::not_found("environment"))?;
            // Somebody with no role on an environment is told it is not there, as the list leaves it out.
            let access = super::load(&app.db, &a.session.user, env).await?.ok_or_else(|| ApiError::not_found("environment"))?;
            let action = act.action(param("action").as_deref()).ok_or_else(|| ApiError::not_found("action"))?;
            if !action.reads() && a.session.user.token.as_ref().is_some_and(|t| t.read_only) {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "This API token may only read."));
            }
            if action == Console && access.allows(action) != Allow::No && super::security::consoles_closed(&app.db, &access).await? {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "Consoles are turned off for non-administrators on this host.").field("block_consoles"));
            }
            match access.allows(action) {
                Allow::No => return Err(refused(&access, action)),
                Allow::All => {}
                Allow::Visible => {
                    let id = param("id").or_else(|| param("name")).unwrap_or_default();
                    if !visible(app, &access, res, &id).await? {
                        return Err(match res {
                            Res::Stack | Res::External => ApiError::not_found("stack"),
                            Res::Volume => ApiError::not_found("volume"),
                            Res::Network => ApiError::not_found("network"),
                            Res::Image => ApiError::not_found("image"),
                            Res::Swarm if path.contains("/swarm/stacks/") => ApiError::not_found("stack"),
                            Res::Swarm => ApiError::not_found("service"),
                            _ => ApiError::not_found("container"),
                        });
                    }
                }
            }
            // A console in the container Freightdock runs in is every environment's: an Administrator's alone ("Access" 20).
            if action == Console && !a.session.user.is_admin() && crate::docker::own::is_own(app, env, &param("id").unwrap_or_default()).await? {
                return Err(crate::docker::own::refused());
            }
            parts.extensions.insert(access);
            Ok(())
        }
    }
}

/// The refusal, in words that say what the role lacks.
pub fn refused(access: &Access, action: Action) -> ApiError {
    let role = match access.level {
        super::Level::Admin => "An administrator",
        super::Level::Role(r) => match r {
            super::Role::EnvAdmin => "An environment administrator",
            super::Role::Operator => "An operator",
            super::Role::Helpdesk => "Helpdesk",
            super::Role::Standard => "A standard user",
            super::Role::ReadOnly => "A read-only user",
        },
    };
    let what = match action {
        Read => "look at that",
        Operate => "start or stop things",
        Console => "open a console",
        Create => "make things",
        Manage => "change or remove that",
        Administer => "do that",
    };
    ApiError::new(StatusCode::FORBIDDEN, format!("{role} may not {what} here."))
}

/// Whether a limited person can see the resource a route names.
async fn visible(app: &App, access: &Access, res: Res, id: &str) -> ApiResult<bool> {
    match res {
        Res::None => Ok(true),
        Res::External | Res::Swarm => Ok(false),
        Res::Stack => {
            let Ok(id) = id.parse::<i64>() else { return Ok(false) };
            let owners = Owners::load(&app.db, access.env).await?;
            Ok(access.sees(owners.stack(id)))
        }
        Res::Container => {
            if !crate::docker::valid_ref(id) {
                return Ok(false);
            }
            Ok(container_owner(app, access.env, id).await?.is_some_and(|o| access.sees(o.as_ref())))
        }
        Res::Volume | Res::Network | Res::Image => {
            if !crate::docker::valid_ref(id) && !(res == Res::Image && crate::docker::images::valid_image(id)) {
                return Ok(false);
            }
            let d = crate::docker::endpoint(app, access.env).await?;
            let c = d.client();
            let found = match res {
                Res::Volume => c.inspect_volume(id).await.map(|v| serde_json::to_value(v).unwrap_or_default()),
                Res::Network => c.inspect_network(id, None::<bollard::query_parameters::InspectNetworkOptions>).await.map(|v| serde_json::to_value(v).unwrap_or_default()),
                _ => c.inspect_image(id).await.map(|v| serde_json::to_value(v).unwrap_or_default()),
            };
            let v = match found {
                Ok(v) => v,
                Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => return Ok(false),
                Err(e) => return Err(crate::docker::api_error(e)),
            };
            let (owners, seen) = sight(app, access).await?;
            let project = ownership::project_of(&v);
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            Ok(match res {
                Res::Volume => seen.volume(access, &owners, &s("Name"), project.as_deref()),
                Res::Network => seen.network(access, &owners, &s("Name"), &s("Id"), project.as_deref()),
                _ => seen.image(access, &owners, &s("Id")),
            })
        }
    }
}

/// An environment's ownership records, and what a person sees through the containers they can see: for the lists and
/// checks of volumes, networks and images ("Access" 18).
pub async fn sight(app: &App, access: &Access) -> ApiResult<(Owners, ownership::Seen)> {
    let owners = Owners::load(&app.db, access.env).await?;
    if !access.limited() {
        return Ok((owners, ownership::Seen::default()));
    }
    let d = crate::docker::endpoint(app, access.env).await?;
    let list = d.client().list_containers(Some(bollard::query_parameters::ListContainersOptions { all: true, ..Default::default() })).await.map_err(crate::docker::api_error)?;
    let list: Vec<serde_json::Value> = list.into_iter().map(|c| serde_json::to_value(c).unwrap_or_default()).collect();
    let seen = ownership::Seen::of(access, &owners, &list);
    Ok((owners, seen))
}

/// Holds a person to `action` on container `id`, as the guard does for a route that names it: for a container a route
/// takes in its body (a network's connect), or one an edit replaces.
pub async fn may_container(app: &App, access: &Access, action: Action, id: &str) -> ApiResult<()> {
    if access.allows(action) == Allow::All {
        return Ok(());
    }
    let o = container_owner(app, access.env, id).await?.ok_or_else(|| ApiError::not_found("container"))?;
    if access.may(action, o.as_ref()) {
        Ok(())
    } else if access.sees(o.as_ref()) {
        Err(refused(access, action))
    } else {
        Err(ApiError::not_found("container"))
    }
}

/// A container's ownership by its id or name, asked of Docker: `None` when Docker has no such container, `Some(None)`
/// when it is the administrators'.
pub async fn container_owner(app: &App, env: i64, id: &str) -> ApiResult<Option<Option<ownership::Owner>>> {
    let d = crate::docker::endpoint(app, env).await?;
    let c = match d.client().inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>).await {
        Ok(c) => serde_json::to_value(c).unwrap_or_default(),
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => return Ok(None),
        Err(e) => return Err(crate::docker::api_error(e)),
    };
    let (name, project) = ownership::container_keys(&c);
    let owners = Owners::load(&app.db, env).await?;
    Ok(Some(owners.container(&name, project.as_deref()).cloned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.route(` in the server's source, as (method, path): read from the files, so that a route added anywhere
    /// is seen here without anybody listing it. Test-only routers (fake providers and registries) are left out by
    /// being inside `mod tests` or under a path that is not Freightdock's.
    fn routes_in_source() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        let mut dirs = vec![root];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        let mut out = vec![];
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            // Only the code before the tests.
            let src = src.split("#[cfg(test)]").next().unwrap_or_default();
            let src: String = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
            let src = src.as_str();
            // A file's `e` helper: `let e = |p: &str| format!("/api/envs/{{env}}{p}");`
            let prefix = src.find("let e = |p: &str| format!(\"").map(|i| {
                let rest = &src[i + "let e = |p: &str| format!(\"".len()..];
                rest[..rest.find("{p}").unwrap()].replace("{{", "{").replace("}}", "}")
            });
            let mut rest = src;
            while let Some(i) = rest.find(".route(") {
                rest = &rest[i + ".route(".len()..];
                // The call's text, to its closing parenthesis.
                let (mut depth, mut end) = (1, 0);
                for (j, ch) in rest.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let call = &rest[..end];
                let path = if let Some(p) = call.strip_prefix("&e(\"") {
                    format!("{}{}", prefix.clone().unwrap_or_else(|| panic!("{} uses e() without one", f.display())), &p[..p.find('"').unwrap()])
                } else if let Some(p) = call.strip_prefix('"') {
                    p[..p.find('"').unwrap()].to_string()
                } else if call.starts_with("PATH_") {
                    continue; // the agent endpoint's own listener, not the API
                } else {
                    panic!("{}: a route whose path this test cannot read: {call}", f.display());
                };
                let handlers = &call[call.find(',').unwrap()..];
                let mut found = false;
                for m in ["get", "post", "put", "delete", "patch"] {
                    let mut h = handlers;
                    while let Some(k) = h.find(&format!("{m}(")) {
                        let before = h[..k].chars().last().unwrap_or(' ');
                        if !(before.is_alphanumeric() || before == '_') || h[..k].ends_with("routing::") {
                            out.push((m.to_uppercase(), path.clone()));
                            found = true;
                            break;
                        }
                        h = &h[k + 1..];
                    }
                }
                assert!(found, "{}: no method in {call}", f.display());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn every_route_in_the_source_has_a_rule_and_every_rule_a_route() {
        let routes = routes_in_source();
        assert!(routes.len() > 100, "the routes are found: {}", routes.len());
        let missing: Vec<_> = routes.iter().filter(|(m, p)| rule(&m.parse().unwrap(), p).is_none()).collect();
        assert!(missing.is_empty(), "routes without a rule in access::routes::RULES: {missing:#?}");
        let stale: Vec<_> = RULES.iter().filter(|(m, p, _)| !routes.iter().any(|(rm, rp)| rm == m && rp == p)).map(|(m, p, _)| (m, p)).collect();
        assert!(stale.is_empty(), "rules for routes that are not there: {stale:#?}");
    }

    #[test]
    fn every_environment_route_names_its_environment_and_its_resource() {
        for (m, p, r) in RULES {
            if p.starts_with("/api/envs/") {
                let Env(act, res) = r else { panic!("{m} {p} is an environment's route without an environment's rule") };
                assert!(p.starts_with("/api/envs/{env}/") || *p == "/api/envs/{env}/swarm", "{p}");
                match res {
                    Res::Container | Res::Stack | Res::Network | Res::Image => assert!(p.contains("/{id}"), "{m} {p} names a resource but has no {{id}}"),
                    Res::External => assert!(p.ends_with("/{name}")),
                    Res::Swarm => assert!(p.contains("/swarm/") && (p.contains("/{id}") || p.ends_with("/{name}")), "{m} {p}"),
                    Res::Volume => assert!(p.contains("/volumes/{name}"), "{m} {p}"),
                    Res::None => {}
                }
                if p.ends_with("/{action}") {
                    assert_ne!(*act, Act::Is(Read), "{p}: an action word is never a read");
                }
            } else {
                assert!(!matches!(r, Env(..)), "{m} {p}");
            }
            if *m != "GET" && *r != Public {
                // A change is never a read, so a read-only token never makes one through an environment's route.
                if let Env(Act::Is(a), _) = r {
                    assert_ne!(*a, Read, "{m} {p} changes something but is ruled a read");
                }
            }
        }
    }

    #[test]
    fn consoles_are_never_reads_even_though_they_open_with_a_get() {
        for p in ["/api/envs/{env}/containers/{id}/exec/ws", "/api/envs/{env}/containers/{id}/attach/ws"] {
            assert_eq!(rule(&Method::GET, p), Some(Env(Is(Console), Res::Container)));
        }
    }

    #[test]
    fn action_words_are_read_as_the_table_has_them() {
        assert_eq!(Act::Container.action(Some("stop")), Some(Operate));
        assert_eq!(Act::Container.action(Some("rm")), None);
        assert_eq!(Act::Stack.action(Some("start")), Some(Operate));
        assert_eq!(Act::Stack.action(Some("deploy")), Some(Manage));
        assert_eq!(Act::Stack.action(Some("remove")), Some(Manage));
        assert_eq!(Act::Stack.action(None), None);
        assert_eq!(rule(&Method::HEAD, "/api/status"), Some(Public));
        assert_eq!(rule(&Method::GET, "/api/nothing"), None);
    }
}
