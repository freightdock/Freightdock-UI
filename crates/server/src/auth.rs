//! Who is asking. Passwords are Argon2id hashes; a session is a random token the browser holds in an HttpOnly cookie
//! and the database holds only as a SHA-256 hash, so a copy of the database signs nobody in. Every session carries a CSRF
//! token of its own that a state-changing request repeats in `X-CSRF-Token` ("Sign-in" in DECISIONS.md).

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::db::{Db, now, rusqlite};
use rusqlite::{OptionalExtension, params};

pub const COOKIE: &str = "freightdock_session";
/// A session not used for this long is over.
pub const IDLE_S: i64 = 7 * 24 * 3600;
/// And none lasts longer than this, used or not.
pub const LIFETIME_S: i64 = 30 * 24 * 3600;
pub const MIN_PASSWORD: usize = 12;

pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A hash to verify against when the name is unknown, so a wrong name takes as long as a wrong password.
pub fn decoy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| hash_password("freightdock decoy password").unwrap_or_default())
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Constant-time equality for tokens.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A first run's setup token: 128 random bits in hex, short enough to copy out of a log ("Sign-in" 14).
pub fn setup_token() -> String {
    random_token()[..32].to_string()
}

/// Where a first run's setup token waits in the data folder until the first administrator is made ("Sign-in" 16).
pub const SETUP_TOKEN_FILE: &str = "setup-token";

/// The setup token kept in `dir`, if one waits there and is long enough to trust.
pub fn read_setup_token(dir: &std::path::Path) -> Option<String> {
    let t = std::fs::read_to_string(dir.join(SETUP_TOKEN_FILE)).ok()?;
    let t = t.trim();
    (t.chars().count() >= 16).then(|| t.to_string())
}

/// The setup token kept in `dir`, made and written there (readable by its owner alone) when none is: the same token
/// across restarts until setup is done, so the one in an older log line or a terminal still works.
pub fn kept_setup_token(dir: &std::path::Path) -> Result<String> {
    if let Some(t) = read_setup_token(dir) {
        return Ok(t);
    }
    let t = setup_token();
    let path = dir.join(SETUP_TOKEN_FILE);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    use std::io::Write;
    let mut f = o.open(&path).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    writeln!(f, "{t}")?;
    Ok(t)
}

/// Removes the kept setup token, once there is an administrator and it opens nothing any more.
pub fn forget_setup_token(dir: &std::path::Path) {
    match std::fs::remove_file(dir.join(SETUP_TOKEN_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => tracing::warn!("could not remove the used setup token: {e}"),
        _ => {}
    }
}

/// The longest a user name may be, in characters.
pub const MAX_NAME: usize = 64;

/// A name a person signs in with: 1 to 64 characters, no control characters, no space at either end.
pub fn check_name(name: &str) -> Result<()> {
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME || name.trim() != name || name.chars().any(char::is_control) {
        bail!("A user name is 1 to 64 characters, with no spaces at the ends.");
    }
    Ok(())
}

pub fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD {
        bail!("A password is at least {MIN_PASSWORD} characters.");
    }
    if password.len() > 1024 {
        bail!("A password is at most 1024 bytes.");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub role: String,
    /// The API token this request came with, when it came with one rather than a session.
    #[serde(skip)]
    pub token: Option<crate::tokens::TokenUse>,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub user: User,
    pub csrf: String,
    pub token_hash: String,
    /// When the session was started or its person last gave their password again; for an API token, 0.
    pub confirmed_at: i64,
}

/// Starts a session for `user_id`; answers the token for the cookie.
pub async fn start(db: &Db, user_id: i64, ip: Option<String>, ua: Option<String>) -> Result<String> {
    let token = random_token();
    let hash = token_hash(&token);
    let csrf = random_token();
    db.call(move |c| {
        let t = now();
        c.execute(
            "INSERT INTO sessions (token_hash, user_id, csrf, created_at, expires_at, last_seen_at, ip, user_agent, confirmed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6, ?7, ?4)",
            params![hash, user_id, csrf, t, t + LIFETIME_S, ip, ua.map(|u| u.chars().take(256).collect::<String>())],
        )?;
        c.execute("UPDATE users SET last_login_at = ?1 WHERE id = ?2", params![t, user_id])?;
        Ok(())
    })
    .await?;
    Ok(token)
}

/// The session a cookie names, if it is still good. Using it keeps it alive (no more often than once a minute).
pub async fn lookup(db: &Db, token: &str) -> Result<Option<Session>> {
    let hash = token_hash(token);
    db.call(move |c| {
        let t = now();
        let row = c
            .query_row(
                "SELECT u.id, u.name, u.role, s.csrf, s.last_seen_at, s.confirmed_at FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND s.last_seen_at > ?3 AND u.disabled = 0",
                params![hash, t, t - IDLE_S],
                |r| Ok((User { id: r.get(0)?, name: r.get(1)?, role: r.get(2)?, token: None }, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?)),
            )
            .optional()?;
        let Some((user, csrf, seen, confirmed_at)) = row else { return Ok(None) };
        if t - seen > 60 {
            c.execute("UPDATE sessions SET last_seen_at = ?1 WHERE token_hash = ?2", params![t, hash])?;
        }
        Ok(Some(Session { user, csrf, token_hash: hash, confirmed_at }))
    })
    .await
}

/// How long a session's sign-in, or its password given again, counts for what needs it to be recent ("Sign-in" 18).
pub const CONFIRM_S: i64 = 10 * 60;

/// Notes that the session's person just gave their password (and code) again.
pub async fn confirmed(db: &Db, token_hash: String) -> Result<()> {
    db.call(move |c| Ok(c.execute("UPDATE sessions SET confirmed_at = ?1 WHERE token_hash = ?2", params![now(), token_hash]).map(|_| ())?)).await
}

pub async fn end(db: &Db, token_hash: String) -> Result<()> {
    db.call(move |c| Ok(c.execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash]).map(|_| ())?)).await
}

/// Checks a name and a password. `None` for any wrong answer, said the same way whichever part was wrong.
pub async fn check(db: &Db, name: String, password: String) -> Result<Option<User>> {
    db.call(move |c| {
        let row = c
            .query_row("SELECT id, name, role, password_hash FROM users WHERE name = ?1 AND disabled = 0", [&name], |r| {
                Ok((User { id: r.get(0)?, name: r.get(1)?, role: r.get(2)?, token: None }, r.get::<_, String>(3)?))
            })
            .optional()?;
        Ok(match row {
            Some((user, hash)) if verify_password(&password, &hash) => Some(user),
            Some(_) => None,
            None => {
                verify_password(&password, decoy_hash());
                None
            }
        })
    })
    .await
}

/// Password and code tries, by address and by name: `LIMIT` within `WINDOW_S` and that address, or that name, waits
/// out the rest of the window. A try is counted as it starts, under one lock with the check that it may be made, so
/// tries sent at once cannot all pass before any is counted; a right one is given back (`release`). Kept in memory: a
/// restart forgives, which is fine for a limit counted in minutes.
#[derive(Default)]
pub struct Limiter {
    misses: Mutex<HashMap<String, Vec<i64>>>,
}

impl Limiter {
    pub const LIMIT: usize = 5;
    pub const WINDOW_S: i64 = 15 * 60;

    /// The name is cut to the longest a name may be (`check_name`): no try can make a key of any size.
    fn keys(ip: &str, name: &str) -> [String; 2] {
        let name: String = name.chars().take(MAX_NAME).collect();
        [format!("ip:{ip}"), format!("name:{}", name.to_lowercase())]
    }

    /// Takes a try from `ip` as `name` at `at`, counted as a miss until it is released; or the seconds until one may
    /// be made, when `LIMIT` are counted within the window already.
    pub fn attempt(&self, ip: &str, name: &str, at: i64) -> Result<(), i64> {
        let mut m = self.misses.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, v| v.last().is_some_and(|t| at - t < Self::WINDOW_S));
        }
        let keys = Self::keys(ip, name);
        let mut wait = 0;
        for k in &keys {
            if let Some(v) = m.get_mut(k) {
                v.retain(|t| at - t < Self::WINDOW_S);
                if v.len() >= Self::LIMIT {
                    wait = wait.max(v[0] + Self::WINDOW_S - at);
                }
            }
        }
        if wait > 0 {
            return Err(wait);
        }
        for k in keys {
            m.entry(k).or_default().push(at);
        }
        Ok(())
    }

    /// Gives back the try taken at `at`: it was right, so it is no miss.
    pub fn release(&self, ip: &str, name: &str, at: i64) {
        let mut m = self.misses.lock().unwrap_or_else(|e| e.into_inner());
        for k in Self::keys(ip, name) {
            if let Some(v) = m.get_mut(&k)
                && let Some(i) = v.iter().position(|t| *t == at)
            {
                v.remove(i);
            }
        }
    }

    pub fn forgive(&self, ip: &str, name: &str) {
        let mut m = self.misses.lock().unwrap_or_else(|e| e.into_inner());
        for k in Self::keys(ip, name) {
            m.remove(&k);
        }
    }
}

/// The 429 for a try `attempt` refused.
pub fn too_many(wait: i64) -> crate::error::ApiError {
    let mut e = crate::error::ApiError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, format!("Too many tries. Try again in {} minutes.", (wait + 59) / 60));
    e.retry_after = Some(wait);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_token_is_kept_in_the_data_folder_until_setup_is_done() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let t = kept_setup_token(dir.path()).unwrap();
        assert_eq!(t.len(), 32);
        assert_eq!(kept_setup_token(dir.path()).unwrap(), t, "a restart keeps the same token");
        assert_eq!(read_setup_token(dir.path()).as_deref(), Some(t.as_str()));
        let file = dir.path().join(SETUP_TOKEN_FILE);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        forget_setup_token(dir.path());
        assert!(!file.exists());
        assert_eq!(read_setup_token(dir.path()), None);
        forget_setup_token(dir.path());
    }

    #[test]
    fn a_kept_setup_token_too_short_to_trust_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SETUP_TOKEN_FILE), "short\n").unwrap();
        let t = kept_setup_token(dir.path()).unwrap();
        assert_eq!(t.len(), 32);
        assert_eq!(read_setup_token(dir.path()).as_deref(), Some(t.as_str()));
    }

    #[test]
    fn a_password_verifies_against_its_own_hash_only() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batterz", &h));
        assert!(!verify_password("anything", "not a hash"));
    }

    #[test]
    fn the_database_keeps_a_hash_of_the_token_never_the_token() {
        let t = random_token();
        assert_eq!(t.len(), 64);
        assert_ne!(token_hash(&t), t);
        assert_eq!(token_hash(&t), token_hash(&t));
    }

    #[test]
    fn names_and_passwords_are_held_to_their_rules() {
        assert!(check_name("admin").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name(" admin").is_err());
        assert!(check_name("a\u{0}b").is_err());
        assert!(check_name(&"x".repeat(65)).is_err());
        assert!(check_password("short").is_err());
        assert!(check_password("twelve chars").is_ok());
    }

    #[test]
    fn five_misses_lock_the_address_and_the_name_for_the_window() {
        let l = Limiter::default();
        for i in 0..5 {
            assert_eq!(l.attempt("1.2.3.4", "admin", 100 + i), Ok(()));
        }
        assert_eq!(l.attempt("1.2.3.4", "someone", 105), Err(Limiter::WINDOW_S - 5));
        assert!(l.attempt("9.9.9.9", "ADMIN", 105).is_err(), "the name is locked from anywhere, whatever its case");
        assert_eq!(l.attempt("9.9.9.9", "other", 105), Ok(()));
        assert_eq!(l.attempt("1.2.3.4", "admin", 100 + Limiter::WINDOW_S), Ok(()), "the window passes");
    }

    #[test]
    fn a_right_try_is_given_back_and_a_long_name_makes_no_long_key() {
        let l = Limiter::default();
        for _ in 0..10 {
            l.attempt("1.2.3.4", "admin", 100).unwrap();
            l.release("1.2.3.4", "admin", 100);
        }
        assert_eq!(l.attempt("1.2.3.4", "admin", 100), Ok(()), "right passwords are no misses");
        l.attempt("5.6.7.8", &"x".repeat(1_000_000), 100).unwrap();
        let m = l.misses.lock().unwrap();
        assert!(m.keys().all(|k| k.chars().count() <= MAX_NAME + 5), "keys are bounded");
    }

    #[tokio::test]
    async fn a_session_signs_in_until_it_is_ended() {
        let db = Db::open_in_memory().unwrap();
        let hash = hash_password("a long password").unwrap();
        db.call(move |c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'Ola', ?1, 'admin', 0)", [hash])?)).await.unwrap();
        assert!(check(&db, "ola".into(), "a long password".into()).await.unwrap().is_some(), "names are matched without case");
        assert!(check(&db, "Ola".into(), "wrong password".into()).await.unwrap().is_none());
        assert!(check(&db, "nobody".into(), "a long password".into()).await.unwrap().is_none());

        let token = start(&db, 1, None, None).await.unwrap();
        let s = lookup(&db, &token).await.unwrap().expect("a fresh session is good");
        assert_eq!(s.user.name, "Ola");
        assert!(s.user.is_admin());
        assert!(lookup(&db, "not a token").await.unwrap().is_none());
        end(&db, s.token_hash).await.unwrap();
        assert!(lookup(&db, &token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_idle_session_is_over() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (1, 'a', 'x', 'user', 0)", [])?)).await.unwrap();
        let token = start(&db, 1, None, None).await.unwrap();
        let h = token_hash(&token);
        db.call(move |c| Ok(c.execute("UPDATE sessions SET last_seen_at = ?1 WHERE token_hash = ?2", params![now() - IDLE_S - 1, h])?)).await.unwrap();
        assert!(lookup(&db, &token).await.unwrap().is_none());
    }
}
