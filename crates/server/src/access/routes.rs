//! The guard every API route passes through (`api::router` puts it on as a route layer), and the rule each route is
//! held to: [`RULES`], one line a route, by method and the path as the router writes it. A route with no line is
//! refused, so a route added without one cannot be reached until it has one, and a test reads every `.route(` in the
//! source and fails for any without a line ("Access" 5).
//!
//! The guard signs the person in (`Authed`), holds them to the rule, and leaves the `Authed` on the request for the
//! handler, so it is looked up once.

use axum::extract::{FromRequestParts, MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::Authed;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// What a route needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Nobody signed in: status, setup, sign-in, SSO, health, the TLS CA.
    Public,
    /// Anybody signed in.
    Session,
    /// An Administrator: Freightdock's own settings, users and teams, the logs, changes to the cluster.
    Admin,
}

use Rule::{Admin, Public, Session};

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
    ("GET", "/api/users", Admin),
    ("POST", "/api/users", Admin),
    ("PUT", "/api/users/{id}", Admin),
    ("DELETE", "/api/users/{id}", Admin),
    ("POST", "/api/users/{id}/password", Admin),
    ("GET", "/api/teams", Session),
    ("POST", "/api/teams", Admin),
    ("PUT", "/api/teams/{id}", Admin),
    ("DELETE", "/api/teams/{id}", Admin),
    ("GET", "/api/tokens", Session),
    ("POST", "/api/tokens", Session),
    ("DELETE", "/api/tokens/{id}", Session),
    // The cluster: reading it is anybody's who is signed in, changing it an Administrator's ("Cluster" 2)
    ("GET", "/api/cluster", Session),
    ("GET", "/api/cluster/nodes", Session),
    ("GET", "/api/cluster/nodes/{id}", Session),
    ("DELETE", "/api/cluster/nodes/{id}", Admin),
    ("PUT", "/api/cluster/nodes/{id}/name", Admin),
    ("PUT", "/api/cluster/managers/{id}/name", Admin),
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
    ("GET", "/api/cluster/floating", Session),
    ("GET", "/api/cluster/traffic", Session),
    ("POST", "/api/cluster/floating", Admin),
    ("DELETE", "/api/cluster/floating/{address}", Admin),
    // The CA that signed the web server's certificate: a certificate, public, fetched before anybody trusts it.
    ("GET", "/api/tls/ca.pem", Public),
];

/// The rule for a request, by its method and the path the router matched. `HEAD` is a `GET`.
pub fn rule(method: &Method, path: &str) -> Option<Rule> {
    let m = if method == Method::HEAD { "GET" } else { method.as_str() };
    RULES.iter().find(|(rm, rp, _)| *rm == m && *rp == path).map(|(_, _, r)| *r)
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
        Admin if a.session.user.is_admin() => Ok(()),
        Admin => Err(ApiError::forbidden()),
    }
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
        assert!(routes.len() > 50, "the routes are found: {}", routes.len());
        let missing: Vec<_> = routes.iter().filter(|(m, p)| rule(&m.parse().unwrap(), p).is_none()).collect();
        assert!(missing.is_empty(), "routes without a rule in access::routes::RULES: {missing:#?}");
        let stale: Vec<_> = RULES.iter().filter(|(m, p, _)| !routes.iter().any(|(rm, rp)| rm == m && rp == p)).map(|(m, p, _)| (m, p)).collect();
        assert!(stale.is_empty(), "rules for routes that are not there: {stale:#?}");
    }
}
