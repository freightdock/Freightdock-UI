//! LDAP and Active Directory: a directory checks the password typed in Freightdock's own sign-in form ("LDAP" in
//! DECISIONS.md). Freightdock binds as its service account (or anonymously), finds the one entry the user filter gives for
//! the name typed, reads its groups (the entry's `memberOf`, or a group search; Active Directory's in-chain rule,
//! 1.2.840.113556.1.4.1941, follows nested groups), then binds as that entry with the password: only the directory
//! decides whether it is right. Over LDAPS or StartTLS, with the system's roots or a CA given; plain LDAP only when an
//! administrator allows it, which the activity log keeps. Every value put in a filter is escaped (RFC 4515), so a name
//! typed cannot change what is searched for.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Identity;
use super::providers::{self, Kind, Provider};
use crate::api::Admin;
use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// Connecting, and each operation after, waits no longer than this.
const TIMEOUT: Duration = Duration::from_secs(10);
/// LDAP's invalidCredentials.
const INVALID_CREDENTIALS: u32 = 49;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// A search under `group_base` with `group_filter`.
    #[default]
    Search,
    /// The user entry's own `memberOf`.
    MemberOf,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// `ldaps://host[:636]`, or `ldap://host[:389]` with StartTLS (or, only when allowed, plain).
    pub url: String,
    pub starttls: bool,
    /// Plain LDAP, no TLS: passwords cross the network as they are typed. Refused unless this is on ("LDAP" 2).
    pub allow_plain: bool,
    /// A CA (PEM) to trust for the directory's certificate instead of the system's roots.
    pub ca_pem: String,
    /// The service account Freightdock searches as; empty for an anonymous bind.
    pub bind_dn: String,
    /// Sealed when kept, never sent back.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bind_password: String,
    pub user_base: String,
    /// `{username}` is the name typed, escaped.
    pub user_filter: String,
    /// The attribute a person's Freightdock name is taken from.
    pub username_attribute: String,
    pub email_attribute: String,
    /// What a person is kept by: `entryUUID` (OpenLDAP), `objectGUID` (Active Directory); the DN when it is missing.
    pub id_attribute: String,
    pub group_mode: GroupMode,
    pub group_base: String,
    /// `{dn}` is the user's DN and `{username}` the name typed, both escaped.
    pub group_filter: String,
    pub group_name_attribute: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::openldap()
    }
}

impl Settings {
    pub fn openldap() -> Self {
        Settings {
            url: "ldaps://ldap.example.org".into(),
            starttls: false,
            allow_plain: false,
            ca_pem: String::new(),
            bind_dn: "cn=freightdock,ou=services,dc=example,dc=org".into(),
            bind_password: String::new(),
            user_base: "ou=people,dc=example,dc=org".into(),
            user_filter: "(&(objectClass=inetOrgPerson)(uid={username}))".into(),
            username_attribute: "uid".into(),
            email_attribute: "mail".into(),
            id_attribute: "entryUUID".into(),
            group_mode: GroupMode::Search,
            group_base: "ou=groups,dc=example,dc=org".into(),
            group_filter: "(|(member={dn})(uniqueMember={dn})(memberUid={username}))".into(),
            group_name_attribute: "cn".into(),
        }
    }

    pub fn active_directory() -> Self {
        Settings {
            url: "ldaps://dc.example.com".into(),
            bind_dn: "CN=freightdock,OU=Service Accounts,DC=example,DC=com".into(),
            user_base: "DC=example,DC=com".into(),
            user_filter: "(&(objectCategory=person)(objectClass=user)(sAMAccountName={username}))".into(),
            username_attribute: "sAMAccountName".into(),
            email_attribute: "mail".into(),
            id_attribute: "objectGUID".into(),
            group_mode: GroupMode::Search,
            group_base: "DC=example,DC=com".into(),
            // The in-chain rule: every group the user is in, through nested groups too.
            group_filter: "(&(objectClass=group)(member:1.2.840.113556.1.4.1941:={dn}))".into(),
            group_name_attribute: "cn".into(),
            ..Settings::openldap()
        }
    }
}

/// A value for a filter, escaped as RFC 4515 says: `*`, `(`, `)`, `\` and NUL as `\XX`, and every other byte that is not
/// printable ASCII too, so nothing typed can close the filter, widen it or add to it.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'*' | b'(' | b')' | b'\\' | 0 => out.push_str(&format!("\\{b:02x}")),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:02x}")),
        }
    }
    out
}

/// A filter template with its placeholders filled in, each value escaped.
pub fn fill(template: &str, username: &str, dn: &str) -> String {
    // One pass, so a value that holds a placeholder's text is never filled in again.
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("{username}") {
            out.push_str(&escape(username));
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{dn}") {
            out.push_str(&escape(dn));
            rest = t;
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Whether a filter's parentheses balance and it is one filter: `(...)`.
fn balanced(filter: &str) -> bool {
    let (mut depth, mut prev) = (0i32, ' ');
    for (i, ch) in filter.char_indices() {
        match ch {
            '(' if prev != '\\' => depth += 1,
            ')' if prev != '\\' => {
                depth -= 1;
                if depth < 0 || (depth == 0 && i != filter.len() - 1) {
                    return false;
                }
            }
            _ => {}
        }
        prev = ch;
    }
    depth == 0 && filter.starts_with('(')
}

/// A name that can be looked up: 1 to 256 characters, no control characters, no space at either end.
pub fn name_ok(name: &str) -> bool {
    let n = name.chars().count();
    (1..=256).contains(&n) && name.trim() == name && !name.chars().any(char::is_control)
}

/// No TLS at all: `ldap://` without StartTLS.
pub fn is_plain(s: &Settings) -> bool {
    s.url.starts_with("ldap://") && !s.starttls
}

/// A directory must be whole, and safe unless allowed otherwise, before it is turned on.
pub fn check(s: &Settings) -> Result<(), ApiError> {
    let u = url::Url::parse(&s.url).map_err(|_| ApiError::bad("The directory's address is ldaps://host or ldap://host.").field("url"))?;
    match u.scheme() {
        "ldaps" if s.starttls => return Err(ApiError::bad("LDAPS is TLS already: StartTLS is for ldap:// addresses.").field("starttls")),
        "ldaps" | "ldap" => {}
        _ => return Err(ApiError::bad("The directory's address is ldaps://host or ldap://host.").field("url")),
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(ApiError::bad("The directory's address names no host.").field("url"));
    }
    if is_plain(s) && !s.allow_plain {
        return Err(ApiError::bad("Plain LDAP sends every password as it is typed. Use ldaps://, or StartTLS, or allow plain LDAP on purpose.").field("url"));
    }
    if !s.ca_pem.trim().is_empty() {
        roots(&s.ca_pem).map_err(|e| ApiError::bad(format!("The CA certificate does not read: {e}")).field("ca_pem"))?;
    }
    if s.user_base.trim().is_empty() {
        return Err(ApiError::bad("Name where the people are (the user search base).").field("user_base"));
    }
    if !s.user_filter.contains("{username}") || !balanced(&s.user_filter) {
        return Err(ApiError::bad("The user filter is one filter in parentheses with {username} in it, such as (uid={username}).").field("user_filter"));
    }
    if s.username_attribute.trim().is_empty() {
        return Err(ApiError::bad("Name the attribute a person's name is taken from.").field("username_attribute"));
    }
    if s.group_mode == GroupMode::Search {
        if s.group_base.trim().is_empty() {
            return Err(ApiError::bad("Name where the groups are (the group search base).").field("group_base"));
        }
        if !(s.group_filter.contains("{dn}") || s.group_filter.contains("{username}")) || !balanced(&s.group_filter) {
            return Err(ApiError::bad("The group filter is one filter in parentheses with {dn} or {username} in it, such as (member={dn}).").field("group_filter"));
        }
    }
    Ok(())
}

fn roots(pem: &str) -> Result<rustls::RootCertStore> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let mut store = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        store.add(c.context("not a PEM certificate")?)?;
    }
    if store.is_empty() {
        bail!("no certificate in it");
    }
    Ok(store)
}

/// A connection to the directory, TLS as set.
async fn connect(s: &Settings) -> Result<ldap3::Ldap> {
    let mut settings = LdapConnSettings::new().set_conn_timeout(TIMEOUT).set_starttls(s.starttls && s.url.starts_with("ldap://"));
    if !s.ca_pem.trim().is_empty() {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions()?.with_root_certificates(roots(&s.ca_pem)?).with_no_client_auth();
        settings = settings.set_config(Arc::new(config));
    }
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &s.url).await.with_context(|| format!("connecting to {}", s.url))?;
    ldap3::drive!(conn);
    ldap.with_timeout(TIMEOUT);
    Ok(ldap)
}

/// Binds as Freightdock's service account, or anonymously without one.
async fn service_bind(ldap: &mut ldap3::Ldap, s: &Settings, password: &str) -> Result<()> {
    if s.bind_dn.is_empty() {
        return Ok(());
    }
    if password.is_empty() {
        bail!("the service account has no password set");
    }
    let r = ldap.with_timeout(TIMEOUT).simple_bind(&s.bind_dn, password).await?;
    if r.rc != 0 {
        bail!("the directory refused the service account {} ({}: {})", s.bind_dn, r.rc, r.text);
    }
    Ok(())
}

/// The attribute `name` of an entry, its name matched without regard to case as LDAP's are.
fn attr<'a>(e: &'a SearchEntry, name: &str) -> Option<&'a Vec<String>> {
    e.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

fn bin_attr<'a>(e: &'a SearchEntry, name: &str) -> Option<&'a Vec<Vec<u8>>> {
    e.bin_attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

/// A search's entries, read without letting a malformed answer take the server down.
async fn search(ldap: &mut ldap3::Ldap, base: &str, filter: &str, attrs: Vec<&str>) -> Result<Vec<SearchEntry>> {
    let (entries, _) = ldap.with_timeout(TIMEOUT).search(base, Scope::Subtree, filter, attrs).await?.success()?;
    entries.into_iter().map(|e| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| SearchEntry::construct(e))).map_err(|_| anyhow!("the directory sent an entry Freightdock cannot read"))).collect()
}

/// The first RDN's value of a DN: `cn=devs,ou=groups,...` is `devs`.
pub fn first_rdn_value(dn: &str) -> Option<String> {
    let mut first = String::new();
    let mut esc = false;
    for ch in dn.chars() {
        if esc {
            first.push(ch);
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == ',' || ch == '+' {
            break;
        } else {
            first.push(ch);
        }
    }
    first.split_once('=').map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// What the directory says of a person, before their password is checked.
#[derive(Debug, Default, Serialize)]
pub struct Found {
    pub dn: String,
    pub name: String,
    pub email: Option<String>,
    pub id: String,
    pub groups: Vec<String>,
}

/// The one entry the user filter finds for `name`, and its groups. `None` when there is none, or more than one (the
/// filter does not tell people apart, and guessing would sign in the wrong one).
async fn find(ldap: &mut ldap3::Ldap, s: &Settings, name: &str) -> Result<Option<Found>> {
    let filter = fill(&s.user_filter, name, "");
    let mut want = vec![s.username_attribute.as_str(), s.email_attribute.as_str(), s.id_attribute.as_str()];
    if s.group_mode == GroupMode::MemberOf {
        want.push("memberOf");
    }
    want.retain(|a| !a.is_empty());
    let mut entries = search(ldap, &s.user_base, &filter, want).await?;
    if entries.len() != 1 {
        if entries.len() > 1 {
            tracing::warn!("LDAP: the user filter finds {} entries for {name:?}; signing in none of them", entries.len());
        }
        return Ok(None);
    }
    let e = entries.remove(0);
    let id = match (attr(&e, &s.id_attribute).and_then(|v| v.first()), bin_attr(&e, &s.id_attribute).and_then(|v| v.first())) {
        (Some(v), _) => v.clone(),
        (None, Some(b)) => hex::encode(b),
        _ => e.dn.clone(),
    };
    let display = attr(&e, &s.username_attribute).and_then(|v| v.first()).cloned().unwrap_or_else(|| name.to_string());
    let email = attr(&e, &s.email_attribute).and_then(|v| v.first()).cloned();
    let mut groups = Vec::new();
    let mut add = |dn: &str, name: Option<String>| {
        for g in name.into_iter().chain(std::iter::once(dn.to_string())) {
            if !g.is_empty() && !groups.contains(&g) {
                groups.push(g);
            }
        }
    };
    match s.group_mode {
        GroupMode::MemberOf => {
            for dn in attr(&e, "memberOf").cloned().unwrap_or_default() {
                add(&dn, first_rdn_value(&dn));
            }
        }
        GroupMode::Search => {
            let gf = fill(&s.group_filter, name, &e.dn);
            for g in search(ldap, &s.group_base, &gf, vec![s.group_name_attribute.as_str()]).await? {
                let n = attr(&g, &s.group_name_attribute).and_then(|v| v.first()).cloned().or_else(|| first_rdn_value(&g.dn));
                add(&g.dn, n);
            }
        }
    }
    Ok(Some(Found { dn: e.dn, name: display, email, id, groups }))
}

/// Whether the directory takes `password` for `dn`. An empty password is never tried: LDAP takes a bind with a name and
/// no password as anonymous, and answers it with success ("LDAP" 3).
async fn password_ok(ldap: &mut ldap3::Ldap, dn: &str, password: &str) -> Result<bool> {
    if password.is_empty() {
        return Ok(false);
    }
    let r = ldap.with_timeout(TIMEOUT).simple_bind(dn, password).await?;
    match r.rc {
        0 => Ok(true),
        INVALID_CREDENTIALS => Ok(false),
        rc => Err(anyhow!("the directory answered {rc}: {}", r.text)),
    }
}

/// Who `name` with `password` is in directory `p`: `Ok(None)` for a wrong name or password.
pub async fn authenticate(p: &Provider, bind_password: &str, name: &str, password: &str) -> Result<Option<Identity>> {
    if !name_ok(name) || password.is_empty() || password.len() > 1024 {
        return Ok(None);
    }
    let s = &p.ldap;
    let mut ldap = connect(s).await?;
    service_bind(&mut ldap, s, bind_password).await?;
    let Some(f) = find(&mut ldap, s, name).await? else {
        let _ = ldap.unbind().await;
        return Ok(None);
    };
    let ok = password_ok(&mut ldap, &f.dn, password).await?;
    let _ = ldap.unbind().await;
    Ok(ok.then_some(Identity { sub: f.id, name: f.name, email: f.email, email_verified: true, groups: f.groups, picture: None }))
}

/// The password form's second try, after the local accounts: each directory that is on, in turn. `None` when no
/// directory knows the name and password; a directory that cannot be reached is passed over (and logged), so the local
/// sign-in and the other directories still work.
pub async fn sign_in(app: &App, peer: &crate::api::Peer, name: &str, password: &str) -> Option<Result<crate::auth::User, super::Refused>> {
    let list = providers::load(app).await.ok()?;
    for p in list.into_iter().filter(|p| p.enabled && p.kind == Kind::Ldap) {
        let bind = if p.ldap.bind_password.is_empty() { String::new() } else { app.sealer.open(&p.ldap.bind_password).ok()? };
        match authenticate(&p, &bind, name, password).await {
            Ok(Some(who)) => return Some(super::admit(app, &p, who, peer, "ldap").await),
            Ok(None) => continue,
            Err(e) => tracing::warn!("LDAP {}: {e:#}", p.id),
        }
    }
    None
}

#[derive(Deserialize)]
pub struct TestBody {
    /// The provider as the form has it now, before it is saved; an empty bind password is the one kept, while the
    /// address is the same.
    provider: Provider,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
}

/// `POST /api/settings/sso/{id}/ldap-test`: connect and bind as the service account; with a name, find that person and
/// their groups and say what they would be; with a password too, check it.
pub async fn test(State(app): State<App>, _: Admin, Path(id): Path<String>, Json(t): Json<TestBody>) -> ApiResult<Json<Value>> {
    let mut p = t.provider;
    p.id = id;
    p.kind = Kind::Ldap;
    p.enabled = true;
    check(&p.ldap)?;
    let kept = providers::load(&app).await?.into_iter().find(|o| o.id == p.id);
    let bind = match (p.ldap.bind_password.is_empty(), &kept) {
        (false, _) => p.ldap.bind_password.clone(),
        // The kept password only goes to the address it was given for ("LDAP" 4).
        (true, Some(o)) if o.ldap.url == p.ldap.url && !o.ldap.bind_password.is_empty() => app.sealer.open(&o.ldap.bind_password)?,
        (true, _) => String::new(),
    };
    let fail = |step: &str, e: anyhow::Error| ApiError::new(StatusCode::BAD_GATEWAY, format!("{step}: {e:#}"));
    let mut ldap = connect(&p.ldap).await.map_err(|e| fail("Connecting", e))?;
    service_bind(&mut ldap, &p.ldap, &bind).await.map_err(|e| fail("Binding", e))?;
    let mut out = json!({ "ok": true, "connected": true, "tls": !is_plain(&p.ldap) });
    let name = t.name.trim();
    if !name.is_empty() {
        if !name_ok(name) {
            return Err(ApiError::bad("That is not a name to look up.").field("name"));
        }
        let found = find(&mut ldap, &p.ldap, name).await.map_err(|e| fail("Searching", e))?;
        match found {
            None => out["user"] = Value::Null,
            Some(f) => {
                let role = providers::role_for(&f.groups, &p);
                let teams = if p.teams.enabled { super::teams::wanted(&f.groups, &p.teams) } else { vec![] };
                let password = if t.password.is_empty() { Value::Null } else { json!(password_ok(&mut ldap, &f.dn, &t.password).await.map_err(|e| fail("Checking the password", e))?) };
                out["user"] = json!({ "dn": f.dn, "name": f.name, "email": f.email, "id": f.id, "groups": f.groups, "role": role, "teams": teams, "password_ok": password });
            }
        }
    }
    let _ = ldap.unbind().await;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_typed_cannot_change_the_filter() {
        assert_eq!(escape("ola"), "ola");
        assert_eq!(escape("*"), "\\2a", "no wildcard: a star would find everybody");
        assert_eq!(escape("admin)(uid=*"), "admin\\29\\28uid=\\2a");
        assert_eq!(escape("a\\b"), "a\\5cb");
        assert_eq!(escape("a\0b"), "a\\00b");
        assert_eq!(escape("Åse"), "\\c3\\85se", "every byte past ASCII as \\XX");
        assert_eq!(escape("tab\there"), "tab\\09here");
        for nasty in ["*)(|(objectClass=*", "x)(&", "\\2a", "*)(uid=*))(|(uid=*", ")(cn=*))\0"] {
            let f = fill("(&(objectClass=person)(uid={username}))", nasty, "");
            assert!(balanced(&f), "{f} is still one filter");
            assert!(!f.contains('*'), "{f}");
            let inner = &f["(&(objectClass=person)(uid=".len()..f.len() - 2];
            assert!(!inner.contains(['(', ')', '\0']), "{inner}");
        }
    }

    #[test]
    fn placeholders_are_filled_once_each_value_escaped() {
        assert_eq!(fill("(uid={username})", "ola", ""), "(uid=ola)");
        assert_eq!(fill("(|(member={dn})(memberUid={username}))", "ola", "uid=ola,ou=people,dc=x"), "(|(member=uid=ola,ou=people,dc=x)(memberUid=ola))");
        assert_eq!(fill("(uid={username})", "{dn}", "cn=admin"), "(uid={dn})", "a name that looks like a placeholder is a name");
        assert_eq!(fill("(member={dn})", "", "cn=a\\,b (x),dc=y"), "(member=cn=a\\5c,b \\28x\\29,dc=y)", "a DN's own escapes and parentheses are escaped for the filter");
        assert_eq!(fill("(cn={other})", "ola", ""), "(cn={other})");
        assert_eq!(
            fill(&Settings::active_directory().group_filter, "ola", "CN=Ola,OU=People,DC=example,DC=com"),
            "(&(objectClass=group)(member:1.2.840.113556.1.4.1941:=CN=Ola,OU=People,DC=example,DC=com))",
            "Active Directory's in-chain rule follows nested groups"
        );
    }

    #[test]
    fn a_filter_is_one_balanced_filter() {
        assert!(balanced("(uid={username})"));
        assert!(balanced("(&(a=b)(c=d))"));
        assert!(balanced("(cn=a\\29b)"), "an escaped parenthesis is not one");
        assert!(!balanced("(uid={username}"));
        assert!(!balanced("(a=b))(c=d"));
        assert!(!balanced("(a=b)(c=d)"), "two filters side by side");
        assert!(!balanced("uid={username}"));
    }

    #[test]
    fn names_that_cannot_be_looked_up_are_not() {
        assert!(name_ok("ola"));
        assert!(name_ok("Ola Nordmann"));
        assert!(!name_ok(""));
        assert!(!name_ok(" ola"));
        assert!(!name_ok("ola\0"));
        assert!(!name_ok("ola\n"));
        assert!(!name_ok(&"x".repeat(257)));
    }

    #[test]
    fn a_dn_s_first_value_is_the_group_s_name() {
        assert_eq!(first_rdn_value("cn=devs,ou=groups,dc=example,dc=org").as_deref(), Some("devs"));
        assert_eq!(first_rdn_value("CN=Domain Admins,CN=Users,DC=x").as_deref(), Some("Domain Admins"));
        assert_eq!(first_rdn_value("cn=a\\,b,dc=x").as_deref(), Some("a,b"));
        assert_eq!(first_rdn_value("garbage"), None);
    }

    #[test]
    fn plain_ldap_is_refused_unless_allowed_and_settings_must_be_whole() {
        let good = Settings { url: "ldaps://ldap.example.org".into(), ..Settings::openldap() };
        assert!(check(&good).is_ok());
        assert!(check(&Settings { url: "ldap://ldap.example.org".into(), starttls: true, ..good.clone() }).is_ok());
        let plain = Settings { url: "ldap://ldap.example.org".into(), ..good.clone() };
        assert!(is_plain(&plain));
        assert_eq!(check(&plain).unwrap_err().field, Some("url"), "plain LDAP is refused");
        assert!(check(&Settings { allow_plain: true, ..plain }).is_ok(), "unless allowed on purpose");
        assert_eq!(check(&Settings { starttls: true, ..good.clone() }).unwrap_err().field, Some("starttls"));
        assert_eq!(check(&Settings { url: "https://ldap.example.org".into(), ..good.clone() }).unwrap_err().field, Some("url"));
        assert_eq!(check(&Settings { user_filter: "(uid=ola)".into(), ..good.clone() }).unwrap_err().field, Some("user_filter"));
        assert_eq!(check(&Settings { user_filter: "(uid={username}".into(), ..good.clone() }).unwrap_err().field, Some("user_filter"));
        assert_eq!(check(&Settings { group_filter: "(objectClass=group)".into(), ..good.clone() }).unwrap_err().field, Some("group_filter"));
        assert!(check(&Settings { group_mode: GroupMode::MemberOf, group_filter: String::new(), group_base: String::new(), ..good.clone() }).is_ok());
        assert_eq!(check(&Settings { ca_pem: "not a certificate".into(), ..good.clone() }).unwrap_err().field, Some("ca_pem"));
        assert!(check(&Settings::active_directory()).is_ok());
    }

    #[tokio::test]
    async fn an_empty_password_is_never_sent_to_the_directory() {
        // Nothing listens there: had it tried, it would have failed to connect rather than said no.
        let p = Provider { kind: Kind::Ldap, ldap: Settings { url: "ldaps://127.0.0.1:1".into(), ..Settings::openldap() }, ..Default::default() };
        assert!(authenticate(&p, "", "ola", "").await.unwrap().is_none());
        assert!(authenticate(&p, "", "*", "x").await.is_err(), "a name is looked up, escaped");
        assert!(authenticate(&p, "", "ola\n", "x").await.unwrap().is_none(), "a name that cannot be one is not looked up");
    }

    #[tokio::test]
    async fn a_directory_that_cannot_be_reached_leaves_the_local_sign_in_as_it_was() {
        use crate::api::tests::{app, call, json_body, post, signed_in};
        let app = app();
        signed_in(&app).await;
        let dir = Provider { id: "dir".into(), kind: Kind::Ldap, enabled: true, ldap: Settings { url: "ldaps://127.0.0.1:1".into(), ..Settings::openldap() }, ..Default::default() };
        crate::oidc::tests::keep(&app, dir).await;
        let login = |p: &str| post("/api/login").body(json_body(json!({ "name": "admin", "password": p }))).unwrap();
        let (s, h, _) = call(&app, login("a long enough password")).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.contains_key(axum::http::header::SET_COOKIE));
        let (s, _, b) = call(&app, login("not the password")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{b}");
        let (s, _, b) = call(&app, post("/api/login").body(json_body(json!({ "name": "somebody", "password": "anything at all" }))).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{b}");
        let (_, _, st) = call(&app, axum::http::Request::get("/api/status").body(axum::body::Body::empty()).unwrap()).await;
        assert_eq!(st["sso"]["ldap"], true);
    }
}
