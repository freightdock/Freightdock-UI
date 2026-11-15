//! The ways in besides a local password: sign-in providers (OpenID Connect, GitHub's OAuth, LDAP and Active Directory),
//! each one of a list in Settings; the teams a provider's groups put a person in; and TOTP, the second factor of a
//! sign-in with a password ("SSO", "Two-factor" and "LDAP" in DECISIONS.md). The OpenID Connect flow itself is
//! `oidc.rs`, which GitHub's shares.

pub mod confirm;
pub mod github;
pub mod ldap;
pub mod providers;
pub mod teams;
pub mod totp;

use serde_json::Value;

use crate::api::Peer;
use crate::auth::User;
use crate::db::rusqlite::params;
use crate::state::App;
use providers::Provider;

/// Who a provider says somebody is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    /// Stable at the provider: an OpenID subject, a GitHub user id, an LDAP entry's UUID.
    pub sub: String,
    pub name: String,
    pub email: Option<String>,
    /// The provider vouches for the e-mail address (only then does it count for `allowed_domains`).
    pub email_verified: bool,
    pub groups: Vec<String>,
    /// A picture's address, when the provider gives one ("Users" 1).
    pub picture: Option<String>,
}

/// A sign-in turned away: what the person is told, and who they were, when that is known.
#[derive(Debug)]
pub struct Refused(pub String, pub Option<String>);

impl From<anyhow::Error> for Refused {
    fn from(e: anyhow::Error) -> Self {
        tracing::warn!("sign-in through a provider: {e:#}");
        Refused("The sign-in could not be finished. The server log says why.".into(), None)
    }
}

/// An auth log entry about a provider's sign-in.
pub fn event<'a>(event: &'static str, method: &'static str, ok: bool, user: Option<&'a User>, name: Option<String>, peer: &Peer, detail: Option<String>) -> crate::authlog::Event<'a> {
    crate::authlog::Event { event, method, ok, user, name, ip: peer.ip.clone(), user_agent: peer.user_agent.clone(), detail }
}

/// Lets in the person `who` the provider `p` vouched for, or says why not: the groups decide whether and as what
/// ("SSO" 2, 12), an e-mail domain when the provider names some ("SSO" 10), then their Freightdock account is found, linked
/// or made ("SSO" 3, 6) and their teams set from the groups ("SSO" 11). Somebody turned away by the groups loses the
/// sessions they had.
pub async fn admit(app: &App, p: &Provider, who: Identity, peer: &Peer, method: &'static str) -> Result<User, Refused> {
    let Identity { sub, name, email, email_verified, groups, picture } = who;
    if !p.allowed_domains.is_empty() && !providers::domain_allowed(email.as_deref(), email_verified, &p.allowed_domains) {
        return Err(Refused(format!("{name} has no verified e-mail address at {}.", p.allowed_domains.join(" or ")), Some(name)));
    }
    let Some(role) = providers::role_for(&groups, p) else {
        let (iss, s) = (p.identity_issuer(), sub.clone());
        app.db
            .call(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id IN (SELECT id FROM users WHERE oidc_issuer = ?1 AND oidc_sub = ?2)", params![iss, s])?))
            .await?;
        let ask = p.user_groups.first().or(p.admin_groups.first()).map_or("one", String::as_str);
        return Err(Refused(format!("{name} is not in a group that may use Freightdock. Ask an administrator to add you to {ask}."), Some(name)));
    };
    let set_role = p.sets_roles();
    let (issuer, n, link) = (p.identity_issuer(), name.clone(), p.link_by_name);
    let user = match app.db.call(move |c| crate::oidc::sso_user(c, &issuer, &sub, &n, email.as_deref(), role, set_role, link)).await? {
        Ok(u) => u,
        Err(crate::oidc::NotIn::Disabled(n)) => return Err(Refused(format!("The account {n} is disabled. Ask an administrator."), Some(name))),
        Err(crate::oidc::NotIn::NameTaken(n)) => return Err(Refused(format!("The account {n} is linked to somebody else at a sign-in provider. Ask an administrator."), Some(name))),
        Err(crate::oidc::NotIn::BadName(n)) => {
            return Err(Refused(format!("The sign-in provider names you {n:?}, which is not a name Freightdock can use: 1 to 64 characters, no control characters, no spaces at the ends. Ask an administrator."), Some(n)));
        }
    };
    if let Err(e) = crate::avatars::remember(app, user.id, picture).await {
        tracing::warn!("{}'s picture could not be kept: {e:#}", user.name);
    }
    if p.teams.enabled {
        let wanted = teams::wanted(&groups, &p.teams);
        let (uid, pid, make) = (user.id, p.id.clone(), p.teams.create_missing);
        let change = app.db.call(move |c| teams::apply(c, uid, &pid, &wanted, make)).await?;
        if !change.is_empty() {
            crate::authlog::record(&app.db, event("teams.sync", method, true, Some(&user), None, peer, Some(change.to_string()))).await;
        }
    }
    Ok(user)
}

/// What the sign-in page shows: a button for each OAuth provider that is on, and whether a directory takes the
/// password form too.
pub async fn public(app: &App) -> Value {
    let list = providers::load(app).await.unwrap_or_default();
    let buttons: Vec<Value> = list
        .iter()
        .filter(|p| p.enabled && p.kind != providers::Kind::Ldap)
        .map(|p| serde_json::json!({ "id": p.id, "label": p.label, "kind": p.kind, "preset": p.preset }))
        .collect();
    let ldap = list.iter().any(|p| p.enabled && p.kind == providers::Kind::Ldap);
    serde_json::json!({ "providers": buttons, "ldap": ldap })
}
