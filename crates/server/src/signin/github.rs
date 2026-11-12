//! GitHub, which is OAuth 2.0 but not OpenID Connect: no ID token, so who somebody is comes from its API with the access
//! token the code is swapped for. `/user` gives the id (the subject: a login can be renamed and taken by somebody
//! else) and the login (the name), `/user/emails` the verified primary address, and `/user/orgs` and `/user/teams` the
//! groups: each organization's login, and each team as `org/team` ("SSO" 10). The flow before that is `oidc.rs`'s
//! (state and PKCE; GitHub takes S256).

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::Identity;
use super::providers::Provider;

#[derive(Deserialize)]
struct Token {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct GhUser {
    id: i64,
    login: String,
}

#[derive(Deserialize)]
struct GhEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Deserialize)]
struct GhOrg {
    login: String,
}

#[derive(Deserialize)]
struct GhTeam {
    slug: String,
    organization: GhOrg,
}

/// At most this many pages of 100 orgs or teams are read.
const PAGES: usize = 5;

async fn api<T: serde::de::DeserializeOwned>(p: &Provider, token: &str, path: &str) -> Result<T> {
    let url = format!("{}{path}", p.api_url.trim_end_matches('/'));
    let r = crate::oidc::http()
        .get(&url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .with_context(|| format!("asking {url}"))?;
    if !r.status().is_success() {
        bail!("{url} answered {}", r.status());
    }
    r.json().await.with_context(|| format!("reading {url}"))
}

async fn pages<T: serde::de::DeserializeOwned>(p: &Provider, token: &str, path: &str) -> Result<Vec<T>> {
    let mut all = Vec::new();
    for page in 1..=PAGES {
        let batch: Vec<T> = api(p, token, &format!("{path}?per_page=100&page={page}")).await?;
        let n = batch.len();
        all.extend(batch);
        if n < 100 {
            break;
        }
    }
    Ok(all)
}

/// The code swapped for an access token, and who it belongs to.
pub async fn identify(p: &Provider, secret: &str, code: &str, verifier: &str) -> Result<Identity> {
    let url = format!("{}/login/oauth/access_token", p.issuer.trim_end_matches('/'));
    let form = [("client_id", p.client_id.as_str()), ("client_secret", secret), ("code", code), ("redirect_uri", p.redirect_uri.as_str()), ("code_verifier", verifier)];
    let r = crate::oidc::http().post(&url).header("Accept", "application/json").form(&form).send().await.context("asking GitHub for the token")?;
    let status = r.status();
    // GitHub answers a refused code with 200 and an `error`.
    let t: Token = r.json().await.with_context(|| format!("reading GitHub's token answer ({status})"))?;
    let token = match (t.access_token, t.error) {
        (Some(at), None) => at,
        (_, e) => return Err(anyhow!("GitHub refused the code: {}", t.error_description.or(e).unwrap_or_else(|| status.to_string()))),
    };
    let user: GhUser = api(p, &token, "/user").await?;
    let emails: Vec<GhEmail> = api(p, &token, "/user/emails").await.unwrap_or_default();
    let primary = emails.iter().find(|e| e.primary && e.verified).or_else(|| emails.iter().find(|e| e.verified));
    let mut groups: Vec<String> = pages::<GhOrg>(p, &token, "/user/orgs").await?.into_iter().map(|o| o.login).collect();
    for t in pages::<GhTeam>(p, &token, "/user/teams").await? {
        if !groups.contains(&t.organization.login) {
            groups.push(t.organization.login.clone());
        }
        groups.push(format!("{}/{}", t.organization.login, t.slug));
    }
    Ok(Identity { sub: user.id.to_string(), name: user.login, email: primary.map(|e| e.email.clone()), email_verified: primary.is_some(), groups, picture: None })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::providers::{Kind, Provider};
    use crate::api::tests::{app, call};
    use axum::Json;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use serde_json::json;
    use std::collections::HashMap;

    /// GitHub of our own on a local port: the token endpoint (which wants the verifier, the secret and the code), and
    /// `/user`, `/user/emails`, `/user/orgs`, `/user/teams` for the token it gave.
    pub(crate) async fn mock_github() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let authed = |h: &axum::http::HeaderMap| h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some("Bearer gho_token");
        let app = axum::Router::new()
            .route("/login/oauth/access_token", axum::routing::post(|body: String| async move {
                let f: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                if f.get("code").map(String::as_str) != Some("good-code") || f.get("client_secret").map(String::as_str) != Some("gh-secret") || f.get("code_verifier").is_none_or(|v| v.len() < 43) {
                    return Json(json!({ "error": "bad_verification_code", "error_description": "The code passed is incorrect or expired." }));
                }
                Json(json!({ "access_token": "gho_token", "token_type": "bearer", "scope": "read:org,read:user,user:email" }))
            }))
            .route("/api/user", axum::routing::get(move |h: axum::http::HeaderMap| async move {
                if !authed(&h) { return (StatusCode::UNAUTHORIZED, Json(json!({}))) }
                (StatusCode::OK, Json(json!({ "id": 583231, "login": "octo-ola", "email": null })))
            }))
            .route("/api/user/emails", axum::routing::get(|| async { Json(json!([{ "email": "old@example.org", "primary": false, "verified": true }, { "email": "ola@example.org", "primary": true, "verified": true }])) }))
            .route("/api/user/orgs", axum::routing::get(|| async { Json(json!([{ "login": "freightdock-org" }])) }))
            .route("/api/user/teams", axum::routing::get(|| async { Json(json!([{ "slug": "admins", "organization": { "login": "freightdock-org" } }, { "slug": "ops", "organization": { "login": "other-org" } }])) }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn github(app: &crate::state::App, base: &str) -> Provider {
        Provider {
            id: "github".into(),
            kind: Kind::Github,
            preset: "github".into(),
            enabled: true,
            label: "GitHub".into(),
            issuer: base.into(),
            api_url: format!("{base}/api"),
            client_id: "Iv1.test".into(),
            client_secret: app.sealer.seal("gh-secret").unwrap(),
            redirect_uri: "https://ship.test/api/auth/sso/github/callback".into(),
            scopes: "read:user user:email read:org".into(),
            admin_groups: vec!["freightdock-org/admins".into()],
            user_groups: vec!["freightdock-org".into()],
            ..Default::default()
        }
    }

    /// The sign-in started: the query sent to GitHub, and the cookie the browser keeps it by.
    async fn start(app: &crate::state::App) -> (HashMap<String, String>, String) {
        let (to, bound) = crate::oidc::tests::start(app, "/api/auth/sso/github/login?next=/stacks").await;
        let to = url::Url::parse(&to).unwrap();
        assert!(to.path().ends_with("/login/oauth/authorize"), "{to}");
        (to.query_pairs().into_owned().collect(), bound)
    }

    /// Back from GitHub with `code`, in the browser that started the sign-in.
    fn back(code: &str, (q, bound): &(HashMap<String, String>, String)) -> Request<Body> {
        Request::get(format!("/api/auth/sso/github/callback?code={code}&state={}", q["state"])).header(header::COOKIE, bound).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn signing_in_with_github_reads_the_user_its_address_and_its_orgs_and_teams() {
        let app = app();
        let base = mock_github().await;
        crate::oidc::tests::keep(&app, github(&app, &base)).await;
        let started = start(&app).await;
        let q = &started.0;
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["scope"], "read:user user:email read:org");
        assert!(!q.contains_key("nonce"), "no ID token, so no nonce");
        let (s, h, _) = call(&app, back("good-code", &started)).await;
        assert!(s.is_redirection());
        assert_eq!(h[header::LOCATION], "/stacks");
        let cookie = crate::oidc::tests::session_set(&h).unwrap();
        let (_, _, me) = call(&app, Request::get("/api/me").header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(me["user"]["name"], "octo-ola");
        assert_eq!(me["user"]["role"], "admin", "a member of freightdock-org/admins");
        let (iss, sub, email): (String, String, String) = app.db.call(|c| Ok(c.query_row("SELECT oidc_issuer, oidc_sub, email FROM users", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
        assert_eq!((iss.as_str(), sub.as_str(), email.as_str()), (base.as_str(), "583231", "ola@example.org"), "found again by GitHub's id, not the login");
    }

    #[tokio::test]
    async fn a_github_user_in_none_of_the_orgs_named_and_a_code_github_refuses_are_turned_away() {
        let app = app();
        let base = mock_github().await;
        crate::oidc::tests::keep(&app, Provider { admin_groups: vec![], user_groups: vec!["somebody-elses-org".into()], ..github(&app, &base) }).await;
        let (_, h, _) = call(&app, back("good-code", &start(&app).await)).await;
        assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login?sso_error="), "{h:?}");
        assert!(crate::oidc::tests::session_set(&h).is_none());
        let (_, h, _) = call(&app, back("stolen", &start(&app).await)).await;
        assert!(h[header::LOCATION].to_str().unwrap().starts_with("/login?sso_error="));
        let failed: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM auth_log WHERE event = 'login.sso.failed'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(failed, 2);
        let users: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(users, 0);
    }

    #[tokio::test]
    async fn github_s_teams_map_to_freightdock_teams() {
        use super::super::teams::{TeamRule, TeamSync};
        let app = app();
        let base = mock_github().await;
        let teams = TeamSync { enabled: true, create_missing: true, rules: vec![TeamRule { group: "/^freightdock-org/(.+)$/".into(), team: "GitHub $1".into() }] };
        crate::oidc::tests::keep(&app, Provider { teams, ..github(&app, &base) }).await;
        call(&app, back("good-code", &start(&app).await)).await;
        let teams: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(teams, ["GitHub admins"], "only the org the rule names");
    }
}
