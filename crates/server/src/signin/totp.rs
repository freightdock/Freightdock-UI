//! TOTP two-factor for a sign-in with a password ("Two-factor" in DECISIONS.md): RFC 6238 with HMAC-SHA1, six digits,
//! 30-second steps, a code good in the step before and after its own, and never twice (the last step taken is kept).
//! The seed is sealed (secret.rs) and never sent back once it is confirmed; the ten recovery codes are shown once and
//! kept as SHA-256 hashes, each good once. A password that is right but wants a code answers a ticket, not a session;
//! the code step is held to the sign-in limiter like the password.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;

use crate::api::{Admin, Authed, Peer};
use crate::audit::{self, Entry};
use crate::auth::User;
use crate::db::{now, rusqlite::{self, OptionalExtension, params}};
use crate::error::{ApiError, ApiResult};
use crate::state::App;

pub const STEP: i64 = 30;
pub const DIGITS: u32 = 6;
pub const ISSUER: &str = "Freightdock";
/// A ticket (the password was right, the code is to come) is good this long.
const TICKET_FOR: Duration = Duration::from_secs(5 * 60);
/// And for this many wrong codes; the limiter counts them too.
const TICKET_TRIES: u32 = 5;
/// The most sign-ins one account may have waiting for a code at once: a new one past it takes the oldest's place.
const TICKETS_EACH: usize = 5;
const RECOVERY_CODES: usize = 10;
pub const POLICY_KEY: &str = "two_factor";
/// The accounts that sign in with a password typed into Freightdock: local ones, and a directory's ("Two-factor" 1).
const PASSWORD_SIGNIN: &str = "(password_hash <> '!' OR oidc_issuer LIKE 'ldap:%')";

// ---------------------------------------------------------------- the algorithm

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32, without padding (as authenticator apps take it).
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn unbase32(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.chars().filter(|c| !matches!(c, ' ' | '-' | '=')) {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// RFC 4226's HOTP with the MAC `M`, as RFC 6238 uses it.
pub fn hotp<M: Mac + hmac::digest::KeyInit>(key: &[u8], counter: u64, digits: u32) -> String {
    let mut mac = <M as hmac::digest::KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]);
    format!("{:0width$}", bin % 10u32.pow(digits), width = digits as usize)
}

/// The code for step `step` (Unix time / 30), as an authenticator app shows it.
pub fn code_at(key: &[u8], step: i64) -> String {
    hotp::<Hmac<sha1::Sha1>>(key, step as u64, DIGITS)
}

/// The step `code` is good for, at time `at`: its own, the one before or the one after, and only one after
/// `last` (no code is good twice). Compared in constant time.
pub fn check(key: &[u8], code: &str, at: i64, last: Option<i64>) -> Option<i64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = at.div_euclid(STEP);
    let mut hit = None;
    for s in [now - 1, now, now + 1] {
        if crate::auth::same(&code_at(key, s), &code) && last.is_none_or(|l| s > l) && hit.is_none() {
            hit = Some(s);
        }
    }
    hit
}

pub fn new_secret() -> Vec<u8> {
    use rand::RngCore;
    let mut b = vec![0u8; 20];
    rand::rng().fill_bytes(&mut b);
    b
}

/// The address an authenticator app reads from the QR code.
pub fn uri(name: &str, secret_b32: &str) -> String {
    let label: String = url::form_urlencoded::byte_serialize(format!("{ISSUER}:{name}").as_bytes()).collect::<String>().replace('+', "%20");
    format!("otpauth://totp/{label}?secret={secret_b32}&issuer={ISSUER}&algorithm=SHA1&digits={DIGITS}&period={STEP}")
}

/// Ten codes of 80 random bits each, as `xxxx-xxxx-xxxx-xxxx` in base32: beyond guessing, so a plain SHA-256 keeps them.
pub fn recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            use rand::RngCore;
            let mut b = [0u8; 10];
            rand::rng().fill_bytes(&mut b);
            let s = base32(&b).to_lowercase();
            format!("{}-{}-{}-{}", &s[0..4], &s[4..8], &s[8..12], &s[12..16])
        })
        .collect()
}

/// A recovery code as kept: without dashes, spaces or case, hashed.
pub fn recovery_hash(code: &str) -> String {
    let norm: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
    hex::encode(sha2::Sha256::digest(norm.as_bytes()))
}

fn looks_like_recovery(code: &str) -> bool {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).count() == 16
}

// ---------------------------------------------------------------- what is kept

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(default)]
pub struct Policy {
    /// Every sign-in with a password must have a second factor; somebody without one enrols at their next sign-in.
    pub required: bool,
}

pub async fn policy(app: &App) -> Policy {
    app.db.setting(POLICY_KEY).await.ok().flatten().unwrap_or_default()
}

/// The user's confirmed seed (sealed) and the last step a code was taken for.
fn seed(c: &rusqlite::Connection, uid: i64) -> rusqlite::Result<Option<(String, Option<i64>)>> {
    Ok(c.query_row("SELECT totp_secret, totp_last_step FROM users WHERE id = ?1", [uid], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?))).optional()?.and_then(|(s, l)| s.map(|s| (s, l))))
}

pub async fn enabled(app: &App, uid: i64) -> Result<bool> {
    app.db.call(move |c| Ok(seed(c, uid)?.is_some())).await
}

/// Takes `step` as the last one used, unless another sign-in took it (or a later one) first: of two at once with the
/// same code, one wins.
fn take_step(c: &rusqlite::Connection, uid: i64, step: i64) -> rusqlite::Result<bool> {
    Ok(c.execute("UPDATE users SET totp_last_step = ?1 WHERE id = ?2 AND (totp_last_step IS NULL OR totp_last_step < ?1)", params![step, uid])? == 1)
}

/// How a code checked out.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Used {
    Code,
    Recovery,
}

/// Checks a TOTP code (or a recovery code, used up) against user `uid`'s confirmed seed.
pub async fn verify(app: &App, uid: i64, code: &str) -> Result<Option<Used>> {
    let sealed = app.db.call(move |c| Ok(seed(c, uid)?)).await?;
    let Some((sealed, last)) = sealed else { return Ok(None) };
    let key = unbase32(&app.sealer.open(&sealed)?).unwrap_or_default();
    if let Some(step) = check(&key, code, now(), last) {
        return Ok(app.db.call(move |c| Ok(take_step(c, uid, step)?)).await?.then_some(Used::Code));
    }
    if looks_like_recovery(code) {
        let h = recovery_hash(code);
        let gone = app.db.call(move |c| Ok(c.execute("DELETE FROM recovery_codes WHERE user_id = ?1 AND code_hash = ?2", params![uid, h])?)).await?;
        return Ok((gone == 1).then_some(Used::Recovery));
    }
    Ok(None)
}

/// Keeps `secret` as the user's confirmed seed with `step` taken, and new recovery codes in place of any before; answers
/// the codes, to be shown once.
async fn confirm_seed(app: &App, uid: i64, secret_b32: &str, step: i64) -> Result<Vec<String>> {
    let sealed = app.sealer.seal(secret_b32)?;
    let codes = recovery_codes();
    let hashes: Vec<String> = codes.iter().map(|c| recovery_hash(c)).collect();
    app.db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE users SET totp_secret = ?1, totp_pending = NULL, totp_last_step = ?2 WHERE id = ?3", params![sealed, step, uid])?;
            tx.execute("DELETE FROM recovery_codes WHERE user_id = ?1", [uid])?;
            for h in hashes {
                tx.execute("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?1, ?2)", params![uid, h])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(codes)
}

async fn clear(app: &App, uid: i64) -> Result<()> {
    app.db
        .call(move |c| {
            c.execute("UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_last_step = NULL WHERE id = ?1", [uid])?;
            c.execute("DELETE FROM recovery_codes WHERE user_id = ?1", [uid])?;
            Ok(())
        })
        .await
}

// ---------------------------------------------------------------- the sign-in's second step

struct Ticket {
    user: User,
    /// How the password was checked: `password` (a local account) or `ldap`.
    method: &'static str,
    /// Enrolling at sign-in (2FA required, none yet): the seed offered, once asked for.
    enrol: Option<String>,
    must_enrol: bool,
    at: Instant,
    tries: u32,
}

/// The sign-ins whose password was right and whose code is to come, by ticket.
#[derive(Default)]
pub struct Tickets(Mutex<HashMap<String, Ticket>>);

impl Tickets {
    /// A ticket for `t`. An account has at most `TICKETS_EACH` waiting, its oldest giving way, so one account's right
    /// password asked again and again never fills the store for everybody else; and when the store is full all the
    /// same, the oldest of all gives way rather than the new one.
    fn issue(&self, t: Ticket) -> String {
        let id = crate::auth::random_token();
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, v| v.at.elapsed() < TICKET_FOR);
        let oldest = |m: &HashMap<String, Ticket>, user: Option<i64>| m.iter().filter(|(_, v)| user.is_none_or(|u| v.user.id == u)).min_by_key(|(_, v)| v.at).map(|(k, _)| k.clone());
        while m.values().filter(|v| v.user.id == t.user.id).count() >= TICKETS_EACH {
            let Some(k) = oldest(&m, Some(t.user.id)) else { break };
            m.remove(&k);
        }
        if m.len() >= 10_000
            && let Some(k) = oldest(&m, None)
        {
            m.remove(&k);
        }
        m.insert(id.clone(), t);
        id
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut Ticket) -> T) -> Option<T> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, v| v.at.elapsed() < TICKET_FOR);
        m.get_mut(id).map(f)
    }

    fn drop_ticket(&self, id: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }
}

/// After a right password: a session at once when the account has no second factor and none is required; else a
/// ticket for the code step (`second_factor`: `totp`, or `enrol` when one is required and there is none yet).
pub async fn after_password(app: &App, user: User, method: &'static str, peer: Peer) -> ApiResult<Response> {
    let has = enabled(app, user.id).await?;
    if !has && !policy(app).await.required {
        return crate::api::signed_in(app, &user, method, &peer, None).await;
    }
    let ticket = app.tickets.issue(Ticket { user, method, enrol: None, must_enrol: !has, at: Instant::now(), tries: 0 });
    let kind = if has { "totp" } else { "enrol" };
    Ok(axum::response::IntoResponse::into_response(Json(json!({ "second_factor": kind, "ticket": ticket }))))
}

#[derive(Deserialize)]
pub struct TicketBody {
    ticket: String,
}

fn gone() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "That sign-in took too long. Sign in again.")
}

/// `POST /api/login/totp/enrol`: the seed for an account that must enrol before it signs in, as the address for the QR
/// code and as text.
pub async fn login_enrol(State(app): State<App>, peer: Peer, Json(b): Json<TicketBody>) -> ApiResult<Json<Value>> {
    if !peer.same_origin {
        return Err(ApiError::forbidden());
    }
    let secret = base32(&new_secret());
    let name = app
        .tickets
        .with(&b.ticket, |t| {
            if !t.must_enrol {
                return None;
            }
            let s = t.enrol.get_or_insert_with(|| secret.clone()).clone();
            Some((t.user.name.clone(), s))
        })
        .ok_or_else(gone)?
        .ok_or_else(|| ApiError::conflict("This account has a second factor already."))?;
    Ok(Json(json!({ "secret": name.1, "uri": uri(&name.0, &name.1) })))
}

#[derive(Deserialize)]
pub struct CodeBody {
    ticket: String,
    code: String,
}

/// `POST /api/login/totp`: the code (or a recovery code) for a ticket; the session when it is right. Enrolling, the
/// first code confirms the seed and the answer has the recovery codes, shown this once.
pub async fn login_code(State(app): State<App>, peer: Peer, Json(b): Json<CodeBody>) -> ApiResult<Response> {
    if !peer.same_origin {
        return Err(ApiError::forbidden());
    }
    let (user, method, enrol, must_enrol) = app.tickets.with(&b.ticket, |t| (t.user.clone(), t.method, t.enrol.clone(), t.must_enrol)).ok_or_else(gone)?;
    let ip = peer.ip.clone().unwrap_or_default();
    // Counted before the code is checked, as a password is ("Sign-in" 5).
    let at = now();
    app.limiter.attempt(&ip, &user.name, at).map_err(crate::auth::too_many)?;
    let outcome = if must_enrol {
        let Some(secret) = enrol else { return Err(ApiError::conflict("Ask for the code to scan first.")) };
        let key = unbase32(&secret).unwrap_or_default();
        match check(&key, &b.code, now(), None) {
            Some(step) => Some((Used::Code, Some(confirm_seed(&app, user.id, &secret, step).await?))),
            None => None,
        }
    } else {
        verify(&app, user.id, &b.code).await?.map(|u| (u, None))
    };
    let Some((used, codes)) = outcome else {
        let out = app.tickets.with(&b.ticket, |t| {
            t.tries += 1;
            t.tries >= TICKET_TRIES
        });
        if out == Some(true) {
            app.tickets.drop_ticket(&b.ticket);
        }
        crate::authlog::record(&app.db, crate::authlog::Event { event: "login.totp.failed", method: "totp", ok: false, user: Some(&user), name: None, ip: peer.ip.clone(), user_agent: peer.user_agent.clone(), detail: Some("wrong code".into()) }).await;
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "That code is not right. Codes change every 30 seconds.").field("code"));
    };
    app.tickets.drop_ticket(&b.ticket);
    let detail = match (used, codes.is_some()) {
        (_, true) => "two-factor enrolled at sign-in",
        (Used::Code, _) => "with a TOTP code",
        (Used::Recovery, _) => "with a recovery code",
    };
    if codes.is_some() {
        crate::authlog::record(&app.db, crate::authlog::Event { event: "totp.enrol", method: "totp", ok: true, user: Some(&user), name: None, ip: peer.ip.clone(), user_agent: peer.user_agent.clone(), detail: Some("at sign-in, as required".into()) }).await;
    }
    let mut r = crate::api::signed_in(&app, &user, method, &peer, Some(detail.into())).await?;
    if let Some(codes) = codes {
        let body = serde_json::to_vec(&json!({ "user": user, "recovery_codes": codes })).map_err(anyhow::Error::from)?;
        *r.body_mut() = axum::body::Body::from(body);
    }
    Ok(r)
}

// ---------------------------------------------------------------- Account

fn own(a: &Authed) -> ApiResult<&User> {
    if a.session.user.token.is_some() {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "Two-factor is set up in the Account page, not with a token."));
    }
    Ok(&a.session.user)
}

/// `GET /api/me/totp`: whether this account has a second factor, whether one is required, and how many recovery codes
/// are left.
pub async fn status(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let uid = a.session.user.id;
    let (on, left, password) = app
        .db
        .call(move |c| {
            let on = seed(c, uid)?.is_some();
            let left: i64 = c.query_row("SELECT COUNT(*) FROM recovery_codes WHERE user_id = ?1", [uid], |r| r.get(0))?;
            let password: bool = c.query_row(&format!("SELECT {PASSWORD_SIGNIN} FROM users WHERE id = ?1"), [uid], |r| r.get(0))?;
            Ok((on, left, password))
        })
        .await?;
    Ok(Json(json!({ "enabled": on, "required": policy(&app).await.required, "recovery_left": left, "password_signin": password })))
}

/// `POST /api/me/totp`: a new seed to enrol, kept (sealed) until a code from it confirms it.
pub async fn start(State(app): State<App>, a: Authed) -> ApiResult<Json<Value>> {
    let u = own(&a)?;
    if enabled(&app, u.id).await? {
        return Err(ApiError::conflict("Two-factor is on already. Turn it off first to enrol another device."));
    }
    let secret = base32(&new_secret());
    let (sealed, uid) = (app.sealer.seal(&secret)?, u.id);
    app.db.call(move |c| Ok(c.execute("UPDATE users SET totp_pending = ?1 WHERE id = ?2", params![sealed, uid])?)).await?;
    Ok(Json(json!({ "secret": secret, "uri": uri(&u.name, &secret) })))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
}

/// `POST /api/me/totp/confirm`: a code from the seed being enrolled turns two-factor on; the answer has the recovery
/// codes, shown this once.
pub async fn confirm(State(app): State<App>, a: Authed, Json(b): Json<Code>) -> ApiResult<Json<Value>> {
    let u = own(&a)?.clone();
    let uid = u.id;
    let pending: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT totp_pending FROM users WHERE id = ?1", [uid], |r| r.get(0))?)).await?;
    let Some(pending) = pending else { return Err(ApiError::conflict("Start again: there is no code being set up.")) };
    let secret = app.sealer.open(&pending)?;
    let Some(step) = check(&unbase32(&secret).unwrap_or_default(), &b.code, now(), None) else {
        return Err(ApiError::bad("That code is not right. Check the time on the device, and type the code it shows now.").field("code"));
    };
    let codes = confirm_seed(&app, uid, &secret, step).await?;
    crate::authlog::record(&app.db, crate::authlog::Event { event: "totp.enrol", method: "totp", ok: true, user: Some(&u), name: None, ip: a.ip.clone(), user_agent: None, detail: None }).await;
    audit::record(&app.db, Some(&u), a.ip.clone(), Entry { action: "account.totp-on", target: Some(u.name.clone()), detail: None }).await;
    Ok(Json(json!({ "recovery_codes": codes })))
}

/// `POST /api/me/totp/off`: with a code (or a recovery code), two-factor off; refused while it is required of
/// everybody signing in with a password.
pub async fn off(State(app): State<App>, a: Authed, Json(b): Json<Code>) -> ApiResult<StatusCode> {
    let u = own(&a)?.clone();
    if policy(&app).await.required {
        return Err(ApiError::conflict("Two-factor is required here. An administrator can reset it for you."));
    }
    if verify(&app, u.id, &b.code).await?.is_none() {
        return Err(ApiError::bad("That code is not right.").field("code"));
    }
    clear(&app, u.id).await?;
    crate::authlog::record(&app.db, crate::authlog::Event { event: "totp.off", method: "totp", ok: true, user: Some(&u), name: None, ip: a.ip.clone(), user_agent: None, detail: None }).await;
    audit::record(&app.db, Some(&u), a.ip.clone(), Entry { action: "account.totp-off", target: Some(u.name.clone()), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/me/totp/recovery`: with a code, ten new recovery codes in place of the old ones, shown this once.
pub async fn new_recovery(State(app): State<App>, a: Authed, Json(b): Json<Code>) -> ApiResult<Json<Value>> {
    let u = own(&a)?.clone();
    // Not with a recovery code: checking it would use it up.
    if looks_like_recovery(&b.code) || verify(&app, u.id, &b.code).await? != Some(Used::Code) {
        return Err(ApiError::bad("Type the code the device shows now.").field("code"));
    }
    let codes = recovery_codes();
    let hashes: Vec<String> = codes.iter().map(|c| recovery_hash(c)).collect();
    let uid = u.id;
    app.db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM recovery_codes WHERE user_id = ?1", [uid])?;
            for h in hashes {
                tx.execute("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?1, ?2)", params![uid, h])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    audit::record(&app.db, Some(&u), a.ip.clone(), Entry { action: "account.totp-recovery", target: Some(u.name.clone()), detail: None }).await;
    Ok(Json(json!({ "recovery_codes": codes })))
}

// ---------------------------------------------------------------- an administrator's

/// `DELETE /api/users/{id}/totp`: a person's second factor taken away (a lost phone); they sign in with the password
/// alone, or enrol again at their next sign-in when it is required.
pub async fn reset(State(app): State<App>, admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let name: Option<String> = app.db.call(move |c| Ok(c.query_row("SELECT name FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?)).await?;
    let name = name.ok_or_else(|| ApiError::not_found("user"))?;
    clear(&app, id).await?;
    let a = &admin.0;
    let who = User { id, name: name.clone(), role: String::new(), token: None };
    crate::authlog::record(&app.db, crate::authlog::Event { event: "totp.reset", method: "totp", ok: true, user: Some(&who), name: None, ip: a.ip.clone(), user_agent: None, detail: Some(format!("by {}", a.session.user.name)) }).await;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "user.totp-reset", target: Some(name), detail: None }).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/settings/two-factor`.
pub async fn get_policy(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    let p = policy(&app).await;
    let without: i64 = app.db.call(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM users WHERE {PASSWORD_SIGNIN} AND totp_secret IS NULL AND disabled = 0"), [], |r| r.get(0))?)).await?;
    Ok(Json(json!({ "required": p.required, "local_without": without })))
}

/// `PUT /api/settings/two-factor`: `{required}`.
pub async fn put_policy(State(app): State<App>, admin: Admin, Json(p): Json<Policy>) -> ApiResult<StatusCode> {
    app.db.put_setting(POLICY_KEY, &p).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.two-factor", target: None, detail: Some(if p.required { "required".into() } else { "optional".into() }) }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::api::tests::{app, call, json_body, post, signed_in};
    use axum::body::Body;
    use axum::http::{Request, header};

    #[test]
    fn one_account_s_sign_ins_never_crowd_out_another_s() {
        let t = Tickets::default();
        let ticket = |id: i64| Ticket { user: User { id, name: format!("u{id}"), role: "user".into(), token: None }, method: "password", enrol: None, must_enrol: false, at: Instant::now(), tries: 0 };
        let first = t.issue(ticket(1));
        for _ in 0..10_000 {
            t.issue(ticket(1));
        }
        let theirs = t.issue(ticket(2));
        assert_eq!(t.with(&theirs, |x| x.user.id), Some(2), "somebody else's right password still gets its code step");
        assert!(t.with(&first, |_| ()).is_none(), "one account keeps only its newest few");
        assert!(t.0.lock().unwrap().len() <= TICKETS_EACH + 1);
    }

    #[test]
    fn rfc_6238_s_test_vectors() {
        // Appendix B: the seeds are "12345678901234567890" repeated to the hash's length, 8 digits, 30 s steps.
        let k1 = b"12345678901234567890";
        let k256 = b"12345678901234567890123456789012";
        let k512 = b"1234567890123456789012345678901234567890123456789012345678901234";
        let vectors: [(i64, &str, &str, &str); 6] = [
            (59, "94287082", "46119246", "90693936"),
            (1111111109, "07081804", "68084774", "25091201"),
            (1111111111, "14050471", "67062674", "99943326"),
            (1234567890, "89005924", "91819424", "93441116"),
            (2000000000, "69279037", "90698825", "38618901"),
            (20000000000, "65353130", "77737706", "47863826"),
        ];
        for (t, sha1, sha256, sha512) in vectors {
            let step = (t / STEP) as u64;
            assert_eq!(hotp::<Hmac<sha1::Sha1>>(k1, step, 8), sha1, "SHA1 at {t}");
            assert_eq!(hotp::<Hmac<sha2::Sha256>>(k256, step, 8), sha256, "SHA256 at {t}");
            assert_eq!(hotp::<Hmac<sha2::Sha512>>(k512, step, 8), sha512, "SHA512 at {t}");
        }
        assert_eq!(code_at(k1, 59 / STEP), "287082", "six digits are the last six of eight");
    }

    #[test]
    fn rfc_4226_s_hotp_vectors() {
        let want = ["755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583", "399871", "520489"];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(hotp::<Hmac<sha1::Sha1>>(b"12345678901234567890", i as u64, 6), *w);
        }
    }

    #[test]
    fn base32_is_rfc_4648_s() {
        for (plain, enc) in [("", ""), ("f", "MY"), ("fo", "MZXQ"), ("foo", "MZXW6"), ("foob", "MZXW6YQ"), ("fooba", "MZXW6YTB"), ("foobar", "MZXW6YTBOI")] {
            assert_eq!(base32(plain.as_bytes()), enc);
            assert_eq!(unbase32(enc).unwrap(), plain.as_bytes());
        }
        assert_eq!(unbase32("mzxw 6ytb-oi======").unwrap(), b"foobar", "spaces, dashes, padding and case are let be");
        assert!(unbase32("MZ1").is_none(), "1 is not base32");
        let s = new_secret();
        assert_eq!(unbase32(&base32(&s)).unwrap(), s);
        assert_eq!(base32(&s).len(), 32, "160 bits");
    }

    #[test]
    fn a_code_is_good_a_step_either_side_and_never_twice() {
        let k = b"12345678901234567890";
        let t = 1_111_111_111;
        let now = t / STEP;
        assert_eq!(check(k, &code_at(k, now), t, None), Some(now));
        assert_eq!(check(k, &code_at(k, now - 1), t, None), Some(now - 1), "the step before: a code typed as it changed");
        assert_eq!(check(k, &code_at(k, now + 1), t, None), Some(now + 1), "the step after: a clock a little ahead");
        assert_eq!(check(k, &code_at(k, now - 2), t, None), None);
        assert_eq!(check(k, &code_at(k, now + 2), t, None), None);
        assert_eq!(check(k, &code_at(k, now), t, Some(now)), None, "a code once taken is not good again");
        assert_eq!(check(k, &code_at(k, now - 1), t, Some(now)), None, "nor an older one");
        assert_eq!(check(k, &code_at(k, now + 1), t, Some(now)), Some(now + 1));
        assert_eq!(check(k, &format!(" {} ", code_at(k, now)), t, None), Some(now), "spaces around it are let be");
        assert_eq!(check(k, "12345", t, None), None);
        assert_eq!(check(k, "abcdef", t, None), None);
    }

    #[test]
    fn the_address_for_the_qr_code_names_freightdock_and_the_account() {
        assert_eq!(uri("ola nordmann", "JBSWY3DPEHPK3PXP"), "otpauth://totp/Freightdock%3Aola%20nordmann?secret=JBSWY3DPEHPK3PXP&issuer=Freightdock&algorithm=SHA1&digits=6&period=30");
    }

    #[test]
    fn recovery_codes_are_long_random_and_kept_as_hashes() {
        let c = recovery_codes();
        assert_eq!(c.len(), 10);
        assert!(c.iter().all(|x| x.len() == 19 && x.matches('-').count() == 3), "{c:?}");
        assert_ne!(c[0], c[1]);
        assert_eq!(recovery_hash(&c[0]), recovery_hash(&c[0].to_uppercase().replace('-', " ")), "typed with spaces, or in capitals");
        assert_ne!(recovery_hash(&c[0]), c[0]);
        assert!(looks_like_recovery(&c[0]) && !looks_like_recovery("123456"));
    }

    /// The administrator's cookie and CSRF token, and a code for them right now once two-factor is on.
    pub(crate) async fn enrolled(app: &App) -> (String, String, Vec<u8>, Vec<String>) {
        let (cookie, csrf) = signed_in(app).await;
        let send = |m: &str, path: &str, body: Value| post(path).method(m).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(app, send("POST", "/api/me/totp", json!({}))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(b["uri"].as_str().unwrap().starts_with("otpauth://totp/Freightdock%3Aadmin?secret="));
        let key = unbase32(b["secret"].as_str().unwrap()).unwrap();
        let (s, _, b) = call(app, send("POST", "/api/me/totp/confirm", json!({ "code": "000000" }))).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::BAD_REQUEST, Some("code")), "a wrong code confirms nothing");
        let (s, _, b) = call(app, send("POST", "/api/me/totp/confirm", json!({ "code": code_at(&key, now() / STEP) }))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let codes: Vec<String> = b["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        (cookie, csrf, key, codes)
    }

    fn login(name: &str, password: &str) -> Request<Body> {
        post("/api/login").body(json_body(json!({ "name": name, "password": password }))).unwrap()
    }

    fn code_step(ticket: &str, code: &str) -> Request<Body> {
        post("/api/login/totp").body(json_body(json!({ "ticket": ticket, "code": code }))).unwrap()
    }

    #[tokio::test]
    async fn once_enrolled_a_password_alone_signs_nobody_in() {
        let app = app();
        let (cookie, _, key, codes) = enrolled(&app).await;
        let (_, _, st) = call(&app, Request::get("/api/me/totp").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!((st["enabled"].as_bool(), st["recovery_left"].as_i64()), (Some(true), Some(10)));
        let kept: String = app.db.call(|c| Ok(c.query_row("SELECT totp_secret FROM users", [], |r| r.get(0))?)).await.unwrap();
        assert!(kept.starts_with("sealed1:"), "the seed is sealed");
        let hashes: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM recovery_codes", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(hashes, 10);

        let (s, h, b) = call(&app, login("admin", "a long enough password")).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!h.contains_key(header::SET_COOKIE), "no session yet");
        assert_eq!(b["second_factor"], "totp");
        let ticket = b["ticket"].as_str().unwrap().to_string();
        let (s, _, b) = call(&app, code_step(&ticket, "000000")).await;
        assert_eq!((s, b["field"].as_str()), (StatusCode::UNAUTHORIZED, Some("code")));
        // The code the enrolment took is spent; the next step's is good.
        let (s, h, _) = call(&app, code_step(&ticket, &code_at(&key, now() / STEP))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "the code taken to enrol is not good again");
        assert!(!h.contains_key(header::SET_COOKIE));
        let (s, h, _) = call(&app, code_step(&ticket, &code_at(&key, now() / STEP + 1))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.contains_key(header::SET_COOKIE), "signed in");
        let (s, _, _) = call(&app, code_step(&ticket, &code_at(&key, now() / STEP + 1))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a ticket is good for one sign-in");

        let (_, _, b) = call(&app, login("admin", "a long enough password")).await;
        let (s, h, _) = call(&app, code_step(b["ticket"].as_str().unwrap(), &codes[0].to_uppercase())).await;
        assert_eq!(s, StatusCode::OK, "a recovery code signs in");
        assert!(h.contains_key(header::SET_COOKIE));
        let (_, _, b) = call(&app, login("admin", "a long enough password")).await;
        let (s, _, _) = call(&app, code_step(b["ticket"].as_str().unwrap(), &codes[0])).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "once");
        let events: Vec<String> = app.db.call(|c| Ok(c.prepare("SELECT event || ':' || IFNULL(detail, '') FROM auth_log ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(events.contains(&"login:with a TOTP code".to_string()) && events.contains(&"login:with a recovery code".to_string()) && events.contains(&"totp.enrol:".to_string()), "{events:?}");
        assert_eq!(events.iter().filter(|e| e.starts_with("login.totp.failed")).count(), 3);
    }

    #[tokio::test]
    async fn wrong_codes_wait_out_the_sign_in_limiter_and_a_right_password_does_not_forgive_them() {
        let app = app();
        let (_, _, key, _) = enrolled(&app).await;
        for _ in 0..crate::auth::Limiter::LIMIT {
            let (_, _, b) = call(&app, login("admin", "a long enough password")).await;
            let (s, _, _) = call(&app, code_step(b["ticket"].as_str().unwrap(), "000000")).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED);
        }
        let (s, _, _) = call(&app, login("admin", "a long enough password")).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "the name waits, the password step too");
        // A ticket from before is held to the limiter as well.
        app.limiter.forgive("", "admin");
        let (_, _, b) = call(&app, login("admin", "a long enough password")).await;
        let t = b["ticket"].as_str().unwrap().to_string();
        for _ in 0..crate::auth::Limiter::LIMIT {
            call(&app, code_step(&t, "000000")).await;
        }
        let (s, _, _) = call(&app, code_step(&t, &code_at(&key, now() / STEP + 1))).await;
        assert!(s == StatusCode::TOO_MANY_REQUESTS || s == StatusCode::UNAUTHORIZED, "even the right code waits: {s}");
    }

    #[tokio::test]
    async fn when_two_factor_is_required_an_account_without_it_enrols_at_sign_in() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let send = |m: &str, path: &str, body: Value| post(path).method(m).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(body)).unwrap();
        let (s, _, _) = call(&app, send("PUT", "/api/settings/two-factor", json!({ "required": true }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, p) = call(&app, Request::get("/api/settings/two-factor").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!((p["required"].as_bool(), p["local_without"].as_i64()), (Some(true), Some(1)));

        let (_, h, b) = call(&app, login("admin", "a long enough password")).await;
        assert!(!h.contains_key(header::SET_COOKIE));
        assert_eq!(b["second_factor"], "enrol");
        let t = b["ticket"].as_str().unwrap().to_string();
        let (s, _, _) = call(&app, code_step(&t, "123456")).await;
        assert_eq!(s, StatusCode::CONFLICT, "the seed is asked for first");
        let (_, _, e) = call(&app, post("/api/login/totp/enrol").body(json_body(json!({ "ticket": t }))).unwrap()).await;
        let (_, _, e2) = call(&app, post("/api/login/totp/enrol").body(json_body(json!({ "ticket": t }))).unwrap()).await;
        assert_eq!(e["secret"], e2["secret"], "the same seed if asked twice");
        let key = unbase32(e["secret"].as_str().unwrap()).unwrap();
        let (s, h, b) = call(&app, code_step(&t, &code_at(&key, now() / STEP))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(h.contains_key(header::SET_COOKIE));
        assert_eq!(b["recovery_codes"].as_array().unwrap().len(), 10, "shown this once");
        assert_eq!(b["user"]["name"], "admin");
        let on: bool = app.db.call(|c| Ok(c.query_row("SELECT totp_secret IS NOT NULL FROM users", [], |r| r.get(0))?)).await.unwrap();
        assert!(on);
        let (s, _, _) = call(&app, send("POST", "/api/me/totp/off", json!({ "code": code_at(&key, now() / STEP + 1) }))).await;
        assert_eq!(s, StatusCode::CONFLICT, "not while it is required");
    }

    #[tokio::test]
    async fn an_administrator_resets_a_lost_second_factor_and_a_person_turns_their_own_off() {
        let app = app();
        let (cookie, csrf, key, old_codes) = enrolled(&app).await;
        let send = |m: &str, path: &str, body: Value| post(path).method(m).header(header::ORIGIN, "https://freightdock.test").header(header::COOKIE, &cookie).header("x-csrf-token", &csrf).body(json_body(body)).unwrap();
        let (s, _, b) = call(&app, send("POST", "/api/users", json!({ "name": "kari", "password": "kari's long password", "role": "user" }))).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let kari = b["id"].as_i64().unwrap();
        app.db.call(move |c| Ok(c.execute("UPDATE users SET totp_secret = 'sealed1:x' WHERE id = ?1", [kari])?)).await.unwrap();
        let (_, _, list) = call(&app, Request::get("/api/users").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert!(list.as_array().unwrap().iter().all(|u| u["totp"] == true), "{list}");
        let (s, _, _) = call(&app, send("DELETE", &format!("/api/users/{kari}/totp"), json!({}))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, h, _) = call(&app, login("kari", "kari's long password")).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.contains_key(header::SET_COOKIE), "the password alone, as before two-factor");
        let reset: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM auth_log WHERE event = 'totp.reset' AND user_name = 'kari'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(reset, 1);

        let (s, _, _) = call(&app, send("POST", "/api/me/totp/off", json!({ "code": "000000" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "a wrong code turns nothing off");
        let (s, _, _) = call(&app, send("POST", "/api/me/totp/recovery", json!({ "code": old_codes[0] }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "new recovery codes want a code from the device");
        let (s, _, b) = call(&app, send("POST", "/api/me/totp/recovery", json!({ "code": code_at(&key, now() / STEP + 1) }))).await;
        assert_eq!(s, StatusCode::OK);
        let fresh: Vec<String> = b["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        assert_eq!(fresh.len(), 10);
        let (s, _, _) = call(&app, send("POST", "/api/me/totp/off", json!({ "code": old_codes[0] }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "the old recovery codes are gone, and that one was not used up by asking");
        let (s, _, _) = call(&app, send("POST", "/api/me/totp/off", json!({ "code": fresh[3] }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "a recovery code turns it off");
        let left: i64 = app.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM recovery_codes", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(left, 0, "its recovery codes go with it");
    }
}
