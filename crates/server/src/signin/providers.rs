//! The sign-in providers: a list in `settings` (key `sso`), each OpenID Connect (Authentik, Keycloak, Google, Microsoft
//! Entra ID...), GitHub's OAuth, or an LDAP directory (OpenLDAP, Active Directory). Secrets in it (a client secret, a
//! bind password) are sealed and never sent back ("Secrets" 1). The settings routes are here; the flows are `oidc.rs`,
//! `github.rs` and `ldap.rs`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::teams::TeamSync;
use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub const KEY: &str = "sso";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Oidc,
    Github,
    Ldap,
}

/// One provider as kept and as the settings page edits it. What a kind does not use stays at its default.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Provider {
    /// In its addresses (`/api/auth/sso/{id}/...`): 1 to 32 of a-z, 0-9 and `-`. The provider kept before the list
    /// was there is `oidc`.
    pub id: String,
    pub kind: Kind,
    /// The preset it was made from (`google`, `microsoft`, `github`, `ldap`, `ad`), or none.
    pub preset: String,
    pub enabled: bool,
    /// The button's words: "Sign in with {label}".
    pub label: String,
    /// OpenID Connect: the issuer, whose `/.well-known/openid-configuration` says the rest. GitHub: its web address
    /// (`https://github.com`, or a GitHub Enterprise server's).
    pub issuer: String,
    pub client_id: String,
    /// Sealed (secret.rs) when kept; plain only on its way in from the settings page.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    /// Where the provider sends people back to: `https://<freightdock>/api/auth/sso/<id>/callback` (or, for the provider
    /// kept from before the list, `/api/auth/oidc/callback`), set exactly so at the provider.
    pub redirect_uri: String,
    pub scopes: String,
    /// The claim a person's Freightdock name comes from.
    pub username_claim: String,
    pub groups_claim: String,
    /// GitHub's API (`https://api.github.com`, or GitHub Enterprise's `https://<host>/api/v3`).
    pub api_url: String,
    /// Members of any of these are administrators, and then the role is set at every sign-in ("SSO" 2, 12).
    pub admin_groups: Vec<String>,
    /// Members of any of these may come in. With both lists empty, everybody the provider lets through may.
    pub user_groups: Vec<String>,
    /// When any: only a person whose verified e-mail address is at one of these domains comes in ("SSO" 10).
    pub allowed_domains: Vec<String>,
    /// A person's first sign-in lands in the local account of the same name, when it is linked to nobody yet ("SSO" 6).
    pub link_by_name: bool,
    pub teams: TeamSync,
    pub ldap: super::ldap::Settings,
}

impl Default for Provider {
    fn default() -> Self {
        Provider {
            id: String::new(),
            kind: Kind::Oidc,
            preset: String::new(),
            enabled: false,
            label: "SSO".into(),
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            scopes: "openid email profile".into(),
            username_claim: "preferred_username".into(),
            groups_claim: "groups".into(),
            api_url: String::new(),
            admin_groups: vec!["freightdock-write".into()],
            user_groups: vec!["freightdock-read".into()],
            allowed_domains: vec![],
            link_by_name: true,
            teams: TeamSync::default(),
            ldap: Default::default(),
        }
    }
}

impl Provider {
    /// What its people are kept by in `users.oidc_issuer`: the OpenID issuer as before, GitHub's web address, and
    /// `ldap:<id>` for a directory (whose address may change while its people stay the same).
    pub fn identity_issuer(&self) -> String {
        match self.kind {
            Kind::Oidc | Kind::Github => self.issuer.clone(),
            Kind::Ldap => format!("ldap:{}", self.id),
        }
    }

    /// The groups say who is an administrator only when an administrator group is named; otherwise a sign-in leaves
    /// the role as it is, and somebody new is a user ("SSO" 12).
    pub fn sets_roles(&self) -> bool {
        !self.admin_groups.is_empty()
    }

    /// The callback path this provider's people come back on.
    pub fn callback_path(&self) -> Option<String> {
        url::Url::parse(&self.redirect_uri).ok().map(|u| u.path().to_string())
    }

    /// A provider anybody can make an account at: Google, GitHub.
    fn open_to_all(&self) -> bool {
        self.kind == Kind::Github || matches!(self.preset.as_str(), "google" | "github") || self.issuer.trim_end_matches('/') == "https://accounts.google.com"
    }
}

/// What a person may be, by the groups the provider says they are in. `None`: not let in.
pub fn role_for(groups: &[String], p: &Provider) -> Option<&'static str> {
    let has = |list: &[String]| list.iter().any(|g| groups.iter().any(|x| x == g));
    if has(&p.admin_groups) {
        Some("admin")
    } else if has(&p.user_groups) || (p.admin_groups.is_empty() && p.user_groups.is_empty()) {
        Some("user")
    } else {
        None
    }
}

/// Whether a person's e-mail is at one of `domains`, and the provider vouches for it.
pub fn domain_allowed(email: Option<&str>, verified: bool, domains: &[String]) -> bool {
    let Some((_, domain)) = email.and_then(|e| e.rsplit_once('@')) else { return false };
    verified && domains.iter().any(|d| d.trim_start_matches('@').eq_ignore_ascii_case(domain))
}

/// The presets the settings page offers, filled in as far as a provider of that kind goes.
pub fn presets() -> Vec<Provider> {
    let none = || Vec::<String>::new();
    vec![
        Provider { preset: String::new(), label: "SSO".into(), ..Default::default() },
        Provider {
            preset: "google".into(),
            label: "Google".into(),
            issuer: "https://accounts.google.com".into(),
            scopes: "openid email profile".into(),
            username_claim: "email".into(),
            admin_groups: none(),
            user_groups: none(),
            ..Default::default()
        },
        Provider {
            preset: "microsoft".into(),
            label: "Microsoft".into(),
            issuer: "https://login.microsoftonline.com/<tenant-id>/v2.0".into(),
            scopes: "openid email profile".into(),
            username_claim: "preferred_username".into(),
            groups_claim: "groups".into(),
            admin_groups: none(),
            user_groups: none(),
            ..Default::default()
        },
        Provider {
            kind: Kind::Github,
            preset: "github".into(),
            label: "GitHub".into(),
            issuer: "https://github.com".into(),
            api_url: "https://api.github.com".into(),
            scopes: "read:user user:email read:org".into(),
            username_claim: String::new(),
            groups_claim: String::new(),
            admin_groups: none(),
            user_groups: none(),
            ..Default::default()
        },
        Provider { kind: Kind::Ldap, preset: "ldap".into(), label: "LDAP".into(), scopes: String::new(), username_claim: String::new(), groups_claim: String::new(), admin_groups: none(), user_groups: none(), ldap: super::ldap::Settings::openldap(), ..Default::default() },
        Provider { kind: Kind::Ldap, preset: "ad".into(), label: "Active Directory".into(), scopes: String::new(), username_claim: String::new(), groups_claim: String::new(), admin_groups: none(), user_groups: none(), ldap: super::ldap::Settings::active_directory(), ..Default::default() },
    ]
}

#[derive(Serialize, Deserialize, Default)]
struct Kept {
    #[serde(default)]
    providers: Vec<Provider>,
}

/// Every provider as kept, its secrets still sealed.
pub async fn load(app: &App) -> anyhow::Result<Vec<Provider>> {
    Ok(app.db.setting::<Kept>(KEY).await?.unwrap_or_default().providers)
}

async fn keep(app: &App, list: Vec<Provider>) -> anyhow::Result<()> {
    app.db.put_setting(KEY, &Kept { providers: list }).await
}

/// The provider `id`, when it is on.
pub async fn enabled(app: &App, id: &str) -> Option<Provider> {
    load(app).await.ok()?.into_iter().find(|p| p.id == id && p.enabled)
}

fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An https address, or http to this machine: what a provider's addresses may be.
pub(crate) fn https_or_local(u: &str) -> bool {
    url::Url::parse(u).is_ok_and(|u| u.scheme() == "https" || (u.scheme() == "http" && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
}

/// A provider must be whole before it is turned on.
pub fn check(p: &Provider) -> Result<(), ApiError> {
    if !valid_id(&p.id) {
        return Err(ApiError::bad("An id is 1 to 32 of a to z, 0 to 9 and -.").field("id"));
    }
    super::teams::check(&p.teams)?;
    if !p.enabled {
        return Ok(());
    }
    if p.kind == Kind::Ldap {
        return super::ldap::check(&p.ldap);
    }
    if !https_or_local(&p.issuer) || p.issuer.contains(['<', '>']) {
        return Err(ApiError::bad(match p.kind {
            Kind::Github => "GitHub's address is an https address: https://github.com, or your GitHub Enterprise server.",
            _ => "The issuer is an https address, such as https://auth.example.com/application/o/freightdock/.",
        })
        .field("issuer"));
    }
    if p.kind == Kind::Github && !https_or_local(&p.api_url) {
        return Err(ApiError::bad("GitHub's API is an https address: https://api.github.com, or https://<server>/api/v3.").field("api_url"));
    }
    if p.client_id.trim().is_empty() {
        return Err(ApiError::bad("Give the client ID the provider made.").field("client_id"));
    }
    let ours = format!("/api/auth/sso/{}/callback", p.id);
    let path_ok = p.callback_path().is_some_and(|path| path == ours || path == "/api/auth/oidc/callback");
    if !https_or_local(&p.redirect_uri) || !path_ok {
        return Err(ApiError::bad(format!("The redirect address is Freightdock's own, ending {ours}.")).field("redirect_uri"));
    }
    if p.kind == Kind::Oidc && !p.scopes.split_whitespace().any(|s| s == "openid") {
        return Err(ApiError::bad("The scopes must include openid.").field("scopes"));
    }
    if p.kind == Kind::Oidc && (p.username_claim.trim().is_empty() || p.groups_claim.trim().is_empty()) {
        return Err(ApiError::bad("Name the claims to read the name and the groups from.").field("username_claim"));
    }
    if p.open_to_all() && p.admin_groups.is_empty() && p.user_groups.is_empty() && p.allowed_domains.is_empty() {
        return Err(ApiError::bad(match p.kind {
            Kind::Github => "Anybody can make a GitHub account: name the organizations or teams (org/team) that may come in, or the e-mail domains.",
            _ => "Anybody can make an account there: name the e-mail domains, or the groups, that may come in.",
        })
        .field("user_groups"));
    }
    Ok(())
}

fn clean(v: Vec<String>) -> Vec<String> {
    v.into_iter().map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect()
}

/// The provider as the settings page sees it: no secrets, only whether there are any.
fn shown(p: &Provider) -> Value {
    let (has_secret, has_bind) = (!p.client_secret.is_empty(), !p.ldap.bind_password.is_empty());
    let mut q = p.clone();
    q.client_secret.clear();
    q.ldap.bind_password.clear();
    let mut v = json!(q);
    v["has_secret"] = json!(has_secret);
    v["ldap"]["has_bind_password"] = json!(has_bind);
    v
}

/// `GET /api/settings/sso`: every provider (no secrets), and the presets to start a new one from.
pub async fn get_settings(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let list = load(&app).await?;
    Ok(Json(json!({ "providers": list.iter().map(shown).collect::<Vec<_>>(), "presets": presets().iter().map(shown).collect::<Vec<_>>() })))
}

/// `PUT /api/settings/sso/{id}`: a provider made or changed. An empty secret keeps the one there is.
pub async fn put_provider(State(app): State<App>, admin: Admin, Path(id): Path<String>, Json(mut p): Json<Provider>) -> ApiResult<StatusCode> {
    p.id = id;
    p.admin_groups = clean(p.admin_groups);
    p.user_groups = clean(p.user_groups);
    p.allowed_domains = clean(p.allowed_domains).into_iter().map(|d| d.trim_start_matches('@').to_ascii_lowercase()).collect();
    p.teams.rules.retain(|r| !r.group.trim().is_empty() || !r.team.trim().is_empty());
    for s in [&mut p.issuer, &mut p.client_id, &mut p.redirect_uri, &mut p.api_url] {
        *s = s.trim().to_string();
    }
    p.ldap.url = p.ldap.url.trim().to_string();
    if p.label.trim().is_empty() {
        p.label = "SSO".into();
    }
    check(&p)?;
    let mut list = load(&app).await?;
    let old = list.iter().find(|o| o.id == p.id).cloned();
    if old.as_ref().is_some_and(|o| o.kind != p.kind) {
        return Err(ApiError::conflict("A provider keeps its kind. Remove it and add another.").field("id"));
    }
    p.client_secret = match (p.client_secret.is_empty(), &old) {
        (true, Some(o)) => o.client_secret.clone(),
        (true, None) => String::new(),
        (false, _) => app.sealer.seal(&p.client_secret)?,
    };
    // A kept bind password only goes to the server it was given for: changing the address asks for it again, so it
    // cannot be sent somewhere else to be read ("LDAP" 4).
    p.ldap.bind_password = match (p.ldap.bind_password.is_empty(), &old) {
        (true, Some(o)) if o.ldap.url == p.ldap.url => o.ldap.bind_password.clone(),
        (true, Some(o)) if !o.ldap.bind_password.is_empty() && !p.ldap.bind_dn.is_empty() && p.enabled => {
            return Err(ApiError::bad("The server's address changed: type the bind password again.").field("bind_password"));
        }
        (true, _) => String::new(),
        (false, _) => app.sealer.seal(&p.ldap.bind_password)?,
    };
    let plain = p.kind == Kind::Ldap && p.enabled && super::ldap::is_plain(&p.ldap);
    let detail = format!("{} {}{}", p.id, if p.enabled { "on" } else { "off" }, if plain { ", plain LDAP allowed" } else { "" });
    let target = Some(p.label.clone());
    match list.iter_mut().find(|o| o.id == p.id) {
        Some(o) => *o = p,
        None => list.push(p),
    }
    keep(&app, list).await?;
    *app.oidc.discovery.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.sso", target, detail: Some(detail) }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/settings/sso/{id}`: the provider gone. Its people keep their accounts (and their link, should it come
/// back); with no password, they are in through another way or not at all.
pub async fn remove_provider(State(app): State<App>, admin: Admin, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let mut list = load(&app).await?;
    let Some(i) = list.iter().position(|p| p.id == id) else { return Err(ApiError::not_found("provider")) };
    let gone = list.remove(i);
    keep(&app, list).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.sso.remove", target: Some(gone.label), detail: Some(gone.id) }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct TestBody {
    issuer: String,
}

/// `POST /api/settings/sso/test`: whether an issuer answers as an OpenID provider, and what it says it can do.
pub async fn test(_: Admin, Json(t): Json<TestBody>) -> ApiResult<Json<Value>> {
    let issuer = t.issuer.trim().to_string();
    match crate::oidc::Oidc::default().discover(&issuer).await {
        Ok(d) => Ok(Json(json!({ "ok": true, "issuer": d.issuer, "authorization_endpoint": d.authorization_endpoint, "end_session_endpoint": d.end_session_endpoint }))),
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}")).field("issuer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, json_body, post, signed_in};
    use axum::body::Body;
    use axum::http::{Request, header};

    fn g(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_groups_say_who_comes_in_and_as_what() {
        let p = Provider::default();
        assert_eq!(role_for(&g(&["freightdock-write", "freightdock-read"]), &p), Some("admin"));
        assert_eq!(role_for(&g(&["freightdock-read"]), &p), Some("user"));
        assert_eq!(role_for(&g(&["portainer-write"]), &p), None, "another app's group lets nobody in");
        assert_eq!(role_for(&[], &p), None);
        let open = Provider { admin_groups: vec![], user_groups: vec![], ..p.clone() };
        assert_eq!(role_for(&[], &open), Some("user"), "no groups named: everybody the provider lets through");
        assert!(p.sets_roles());
        assert!(!open.sets_roles(), "with no administrator group, the provider cannot say who is one");
    }

    #[test]
    fn only_a_verified_address_at_a_named_domain_counts() {
        let d = g(&["example.org"]);
        assert!(domain_allowed(Some("ola@Example.ORG"), true, &d));
        assert!(!domain_allowed(Some("ola@example.org"), false, &d), "not verified");
        assert!(!domain_allowed(Some("ola@example.org.evil.test"), true, &d));
        assert!(!domain_allowed(Some("ola@evil.test@example.org"), true, &g(&["evil.test"])), "the last @ is the domain");
        assert!(!domain_allowed(None, true, &d));
    }

    #[test]
    fn a_provider_must_be_whole_before_it_is_turned_on() {
        let good = Provider { id: "authentik".into(), enabled: true, issuer: "https://auth.example.com/application/o/freightdock/".into(), client_id: "id".into(), redirect_uri: "https://ship.example.com/api/auth/sso/authentik/callback".into(), ..Default::default() };
        assert!(check(&good).is_ok());
        assert!(check(&Provider { redirect_uri: "https://ship.example.com/api/auth/oidc/callback".into(), ..good.clone() }).is_ok(), "the old callback still works");
        assert_eq!(check(&Provider { redirect_uri: "https://ship.example.com/api/auth/sso/other/callback".into(), ..good.clone() }).unwrap_err().field, Some("redirect_uri"));
        assert_eq!(check(&Provider { issuer: "http://auth.example.com/".into(), ..good.clone() }).unwrap_err().field, Some("issuer"));
        assert_eq!(check(&Provider { scopes: "email profile".into(), ..good.clone() }).unwrap_err().field, Some("scopes"));
        assert_eq!(check(&Provider { id: "Not ok".into(), ..good.clone() }).unwrap_err().field, Some("id"));
        assert!(check(&Provider { enabled: false, issuer: String::new(), ..good.clone() }).is_ok(), "off may be half filled in");
    }

    #[test]
    fn the_presets_fill_in_what_each_provider_needs_and_open_ones_say_who_comes_in() {
        let p = |name: &str| presets().into_iter().find(|p| p.preset == name).unwrap();
        let google = p("google");
        assert_eq!((google.kind, google.issuer.as_str(), google.username_claim.as_str()), (Kind::Oidc, "https://accounts.google.com", "email"));
        let ms = p("microsoft");
        assert!(ms.issuer.starts_with("https://login.microsoftonline.com/") && ms.issuer.ends_with("/v2.0"));
        let gh = p("github");
        assert_eq!((gh.kind, gh.api_url.as_str()), (Kind::Github, "https://api.github.com"));
        assert!(gh.scopes.contains("read:org"), "orgs and teams are asked for");
        assert_eq!(p("ad").ldap.id_attribute, "objectGUID");

        let fill = |p: Provider| Provider { id: "x".into(), enabled: true, client_id: "c".into(), redirect_uri: "https://ship.test/api/auth/sso/x/callback".into(), ..p };
        assert_eq!(check(&fill(google.clone())).unwrap_err().field, Some("user_groups"), "anybody can have a Google account");
        assert!(check(&fill(Provider { allowed_domains: g(&["example.org"]), ..google })).is_ok());
        assert_eq!(check(&fill(gh.clone())).unwrap_err().field, Some("user_groups"), "anybody can have a GitHub account");
        assert!(check(&fill(Provider { user_groups: g(&["my-org"]), ..gh })).is_ok());
        assert_eq!(check(&fill(ms.clone())).unwrap_err().field, Some("issuer"), "the tenant must be filled in");
        assert!(check(&fill(Provider { issuer: "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0".into(), ..ms })).is_ok(), "a tenant lets in only its own");
    }

    #[tokio::test]
    async fn the_one_oidc_setting_kept_before_becomes_the_first_provider_with_its_secret_and_callback() {
        // A database at migration 17 with the setting as 1.0.0 kept it, then the rest of the migrations.
        let db = crate::db::Db::open_in_memory().unwrap();
        let old = r#"{"enabled":true,"label":"Authentik","issuer":"https://auth.example.org/application/o/freightdock/","client_id":"cid","client_secret":"sealed1:abc","redirect_uri":"https://localhost:7443/api/auth/oidc/callback","scopes":"openid email profile","username_claim":"preferred_username","groups_claim":"groups","admin_groups":["freightdock-write"],"user_groups":["freightdock-read"],"link_by_name":true}"#;
        let list = db
            .call(move |c| {
                c.execute("DELETE FROM settings", [])?;
                c.execute("INSERT INTO settings (key, value) VALUES ('oidc', ?1)", [old])?;
                // Migration 18 again, as a database that had the old setting would run it.
                let m = crate::db::MIGRATIONS[17];
                assert!(m.contains("'sso'"), "migration 18 is the one that moves the setting");
                let sql = m.split_once("INSERT INTO settings").map(|(_, rest)| format!("INSERT INTO settings{rest}")).unwrap();
                c.execute_batch(&sql)?;
                let old_left: i64 = c.query_row("SELECT COUNT(*) FROM settings WHERE key = 'oidc'", [], |r| r.get(0))?;
                assert_eq!(old_left, 0);
                let v: String = c.query_row("SELECT value FROM settings WHERE key = 'sso'", [], |r| r.get(0))?;
                Ok(serde_json::from_str::<Kept>(&v)?.providers)
            })
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        let p = &list[0];
        assert_eq!((p.id.as_str(), p.kind, p.label.as_str()), ("oidc", Kind::Oidc, "Authentik"));
        assert_eq!(p.client_secret, "sealed1:abc", "the sealed secret is carried over as it is");
        assert_eq!(p.callback_path().as_deref(), Some("/api/auth/oidc/callback"));
        assert!(p.enabled && p.link_by_name && p.sets_roles());
        assert!(!p.teams.enabled);
        assert!(check(p).is_ok(), "and it passes as it is");
    }

    #[tokio::test]
    async fn providers_are_added_changed_and_removed_and_their_secrets_never_come_back() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let put = |id: &str, body: Value| post(&format!("/api/settings/sso/{id}")).method("PUT").header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(body)).unwrap();
        let gh = json!({ "kind": "github", "preset": "github", "enabled": true, "label": "GitHub", "issuer": "https://github.com", "api_url": "https://api.github.com", "client_id": "Iv1.x", "client_secret": "gh-secret", "redirect_uri": "https://freightdock.test/api/auth/sso/github/callback", "user_groups": ["my-org"] });
        let (s, _, b) = call(&app, put("github", gh.clone())).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let (s, _, b) = call(&app, put("github", json!({ "kind": "oidc" }))).await;
        assert_eq!(s, StatusCode::CONFLICT, "a provider keeps its kind: {b}");
        let mut again = gh.clone();
        again["client_secret"] = json!("");
        again["label"] = json!("GitHub (work)");
        call(&app, put("github", again)).await;
        let (_, _, list) = call(&app, Request::get("/api/settings/sso").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(list["providers"][0]["label"], "GitHub (work)");
        assert_eq!(list["providers"][0]["has_secret"], true, "an empty secret keeps the one there was");
        assert!(!list.to_string().contains("gh-secret") && !list.to_string().contains("sealed1:"), "{list}");
        assert!(list["presets"].as_array().unwrap().len() >= 6);
        let kept = load(&app).await.unwrap();
        assert_eq!(app.sealer.open(&kept[0].client_secret).unwrap(), "gh-secret");
        let (_, _, st) = call(&app, Request::get("/api/status").body(Body::empty()).unwrap()).await;
        assert_eq!(st["sso"]["providers"][0]["id"], "github");
        assert_eq!(st["sso"]["ldap"], false);
        let (s, _, _) = call(&app, post("/api/settings/sso/github").method("DELETE").header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(load(&app).await.unwrap().is_empty());
        let actions: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT action FROM audit WHERE action LIKE 'settings.sso%'")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(actions, ["settings.sso", "settings.sso", "settings.sso.remove"]);
    }
}
