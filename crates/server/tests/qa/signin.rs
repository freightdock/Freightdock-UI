//! Sign-in beyond a local password, end to end against the real binary ("SSO", "LDAP" and "Two-factor" in
//! DECISIONS.md): a real OpenLDAP (qa/run.sh starts it beside the dind, with a CA made for the run and the people of
//! qa/ldap/seed.ldif) tested from Settings, signing people in by their directory password as their groups say and into
//! the teams their groups map to; and TOTP, enrolled from Account with codes worked out here as an authenticator app
//! would, then asked for at every sign-in with a password, a code never good twice.

use anyhow::{Context, Result, ensure};
use hmac::{Hmac, Mac};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::{Qa, step};

/// RFC 6238 as an authenticator app has it: HMAC-SHA1, six digits, 30-second steps.
fn totp(secret_b32: &str, step: u64) -> String {
    let mut key = Vec::new();
    let (mut buf, mut bits) = (0u32, 0);
    for c in secret_b32.chars().filter(|c| c.is_ascii_alphanumeric()) {
        let v = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567".find(c.to_ascii_uppercase()).expect("base32") as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            key.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    let mut mac = <Hmac<sha1::Sha1> as hmac::digest::KeyInit>::new_from_slice(&key).unwrap();
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[19] & 0x0f) as usize;
    let n = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]) % 1_000_000;
    format!("{n:06}")
}

fn now_step() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() / 30
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name}: qa/run.sh sets it"))
}

/// A sign-in through the password form, as the sign-in page sends it: its status, the session cookie if one came, and
/// the answer.
async fn login(qa: &Qa, name: &str, password: &str) -> Result<(StatusCode, Option<String>, Value)> {
    let r = qa.http.post(qa.url("/api/login")).header("origin", &qa.base).json(&json!({ "name": name, "password": password })).send().await?;
    let s = r.status();
    let cookie = r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|v| v.starts_with("freightdock_session=")).map(|v| v.split(';').next().unwrap_or_default().to_string());
    let body = r.text().await?;
    Ok((s, cookie, serde_json::from_str(&body).unwrap_or(Value::String(body))))
}

/// The code step of a sign-in.
async fn code_step(qa: &Qa, path: &str, body: Value) -> Result<(StatusCode, Option<String>, Value)> {
    let r = qa.http.post(qa.url(path)).header("origin", &qa.base).json(&body).send().await?;
    let s = r.status();
    let cookie = r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|v| v.starts_with("freightdock_session=")).map(|v| v.split(';').next().unwrap_or_default().to_string());
    let body = r.text().await?;
    Ok((s, cookie, serde_json::from_str(&body).unwrap_or(Value::String(body))))
}

/// Who a session cookie signs in as.
async fn me(qa: &Qa, cookie: &str) -> Result<Value> {
    let r = qa.http.get(qa.url("/api/me")).header("cookie", cookie).send().await?;
    ensure!(r.status() == StatusCode::OK, "/api/me: {}", r.status());
    Ok(r.json::<Value>().await?["user"].clone())
}

fn directory(url: &str, starttls: bool, ca: &str) -> Value {
    json!({
        "kind": "ldap", "preset": "ldap", "enabled": true, "label": "QA directory",
        "admin_groups": ["freightdock-admins"], "user_groups": ["freightdock-users"],
        "teams": { "enabled": true, "create_missing": true, "rules": [{ "group": "devs", "team": "QA developers" }, { "group": "/^freightdock-(.+)$/", "team": "QA $1" }] },
        "ldap": {
            "url": url, "starttls": starttls, "ca_pem": ca,
            "bind_dn": "cn=freightdock,dc=example,dc=org", "bind_password": "qa-ldap-readonly",
            "user_base": "ou=people,dc=example,dc=org", "user_filter": "(&(objectClass=inetOrgPerson)(uid={username}))",
            "username_attribute": "uid", "email_attribute": "mail", "id_attribute": "entryUUID",
            "group_mode": "search", "group_base": "ou=groups,dc=example,dc=org",
            "group_filter": "(|(member={dn})(uniqueMember={dn}))", "group_name_attribute": "cn"
        }
    })
}

pub async fn steps(qa: &Qa) {
    let (ldaps, ldap, ca) = (env("FREIGHTDOCK_QA_LDAPS"), env("FREIGHTDOCK_QA_LDAP"), std::fs::read_to_string(env("FREIGHTDOCK_QA_LDAP_CA")).expect("the LDAP CA"));

    step("a directory whose certificate Freightdock cannot trust is refused when it is tested", async {
        let (s, b) = qa.post(&qa.url("/api/settings/sso/qa-dir/ldap-test"), json!({ "provider": directory(&ldaps, false, "") })).await?;
        ensure!(s == StatusCode::BAD_GATEWAY, "without the CA: {s}: {b}");
        ensure!(b["error"].as_str().is_some_and(|e| e.contains("certificate") || e.contains("UnknownIssuer")), "the reason: {b}");
        Ok(())
    })
    .await;

    step("plain LDAP is refused unless it is allowed on purpose", async {
        let mut p = directory(&ldap, false, &ca);
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/sso/qa-dir"), Some(p.clone())).await?;
        ensure!(s == StatusCode::BAD_REQUEST && b["field"] == json!("url"), "plain: {s}: {b}");
        p["ldap"]["allow_plain"] = json!(true);
        let (s, b) = qa.post(&qa.url("/api/settings/sso/qa-dir/ldap-test"), json!({ "provider": p })).await?;
        ensure!(s == StatusCode::OK && b["tls"] == json!(false), "allowed, it connects without TLS: {s}: {b}");
        Ok(())
    })
    .await;

    step("testing the directory over LDAPS and StartTLS finds a person, their groups and what they would be", async {
        for (url, starttls) in [(&ldaps, false), (&ldap, true)] {
            let (s, b) = qa.post(&qa.url("/api/settings/sso/qa-dir/ldap-test"), json!({ "provider": directory(url, starttls, &ca), "name": "kari", "password": "kari-directory-password" })).await?;
            ensure!(s == StatusCode::OK && b["tls"] == json!(true), "{url}: {s}: {b}");
            let u = &b["user"];
            ensure!(u["dn"] == json!("uid=kari,ou=people,dc=example,dc=org") && u["role"] == json!("user") && u["password_ok"] == json!(true), "{url}: {u}");
            ensure!(u["groups"].as_array().is_some_and(|g| g.contains(&json!("devs")) && g.contains(&json!("freightdock-users"))), "groups: {u}");
            ensure!(u["teams"] == json!(["QA developers", "QA users"]), "teams: {u}");
        }
        let mut member_of = directory(&ldaps, false, &ca);
        member_of["ldap"]["group_mode"] = json!("member_of");
        let (s, b) = qa.post(&qa.url("/api/settings/sso/qa-dir/ldap-test"), json!({ "provider": member_of, "name": "ola" })).await?;
        ensure!(s == StatusCode::OK && b["user"]["role"] == json!("admin") && b["user"]["password_ok"].is_null(), "memberOf: {s}: {b}");
        let (s, b) = qa.post(&qa.url("/api/settings/sso/qa-dir/ldap-test"), json!({ "provider": directory(&ldaps, false, &ca), "name": "*" })).await?;
        ensure!(s == StatusCode::OK && b["user"].is_null(), "a star is a name, not a wildcard: {s}: {b}");
        Ok(())
    })
    .await;

    step("a saved directory keeps its bind password sealed and never sends it back", async {
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/sso/qa-dir"), Some(directory(&ldaps, false, &ca))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "saving: {s}: {b}");
        let list = qa.get_json(&qa.url("/api/settings/sso")).await?;
        ensure!(!list.to_string().contains("qa-ldap-readonly"), "the bind password came back: {list}");
        ensure!(list["providers"][0]["ldap"]["has_bind_password"] == json!(true), "{list}");
        let st: Value = qa.http.get(qa.url("/api/status")).send().await?.json().await?;
        ensure!(st["sso"]["ldap"] == json!(true) && st["sso"]["providers"] == json!([]), "the sign-in page is told: {st}");
        Ok(())
    })
    .await;

    step("a directory user signs in with their directory password as their groups say, and into their teams", async {
        let (s, cookie, b) = login(qa, "ola", "ola-directory-password").await?;
        ensure!(s == StatusCode::OK, "ola: {s}: {b}");
        let ola = me(qa, &cookie.context("no session for ola")?).await?;
        ensure!(ola["role"] == json!("admin"), "ola is an administrator by group: {ola}");
        let (s, cookie, b) = login(qa, "kari", "kari-directory-password").await?;
        ensure!(s == StatusCode::OK, "kari: {s}: {b}");
        ensure!(me(qa, &cookie.context("no session for kari")?).await?["role"] == json!("user"), "kari is a user");
        let users = qa.get_json(&qa.url("/api/users")).await?;
        let kari = users.as_array().and_then(|l| l.iter().find(|u| u["name"] == json!("kari"))).with_context(|| format!("no kari: {users}"))?;
        ensure!(kari["kind"] == json!("ldap") && kari["email"] == json!("kari@example.org"), "{kari}");
        let teams: Vec<&str> = kari["teams"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str()).collect();
        ensure!(teams.contains(&"QA developers") && teams.contains(&"QA users"), "kari's teams: {kari}");
        Ok(())
    })
    .await;

    step("a wrong directory password, and somebody in no group of Freightdock's, are turned away", async {
        let (s, c, _) = login(qa, "kari", "not her password").await?;
        ensure!(s == StatusCode::UNAUTHORIZED && c.is_none(), "a wrong password: {s}");
        let (s, c, b) = login(qa, "nils", "nils-directory-password").await?;
        ensure!(s == StatusCode::FORBIDDEN && c.is_none(), "nils: {s}: {b}");
        ensure!(b["error"].as_str().is_some_and(|e| e.contains("not in a group")), "the reason: {b}");
        let (s, _, _) = login(qa, "*", "ola-directory-password").await?;
        ensure!(s == StatusCode::UNAUTHORIZED, "a star signs nobody in: {s}");
        let users = qa.get_json(&qa.url("/api/users")).await?;
        ensure!(!users.to_string().contains("\"nils\""), "nobody is made for nils: {users}");
        let log = qa.get_json(&qa.url("/api/auth-log?method=ldap")).await?;
        let events: Vec<&str> = log["entries"].as_array().into_iter().flatten().filter_map(|e| e["event"].as_str()).collect();
        ensure!(events.contains(&"login") && events.contains(&"login.failed") && events.contains(&"teams.sync"), "the auth log: {log}");
        Ok(())
    })
    .await;

    step("the local sign-in still works beside the directory", async {
        let (s, c, b) = login(qa, "qa-admin", "correct horse battery").await?;
        ensure!(s == StatusCode::OK && c.is_some(), "qa-admin: {s}: {b}");
        Ok(())
    })
    .await;

    let (person, secret) = step("TOTP is enrolled from Account with a code from the secret it gives", async {
        let (s, b) = qa.post(&qa.url("/api/users"), json!({ "name": "qa-2fa", "password": "a long qa-2fa password" })).await?;
        ensure!(s == StatusCode::CREATED, "making qa-2fa: {s}: {b}");
        let (s, cookie, _) = login(qa, "qa-2fa", "a long qa-2fa password").await?;
        ensure!(s == StatusCode::OK, "qa-2fa signs in: {s}");
        let mut p = Qa { http: qa.http.clone(), base: qa.base.clone(), cookie: cookie.context("no cookie")?, csrf: String::new(), env: qa.env };
        p.csrf = p.get_json(&p.url("/api/me")).await?["csrf"].as_str().context("csrf")?.to_string();
        let (s, made) = p.post(&p.url("/api/me/totp"), json!({})).await?;
        ensure!(s == StatusCode::OK, "enrol: {s}: {made}");
        let secret = made["secret"].as_str().context("no secret")?.to_string();
        ensure!(made["uri"].as_str().is_some_and(|u| u.starts_with("otpauth://totp/Freightdock%3Aqa-2fa?secret=")), "{made}");
        let (s, b) = p.post(&p.url("/api/me/totp/confirm"), json!({ "code": totp(&secret, now_step()) })).await?;
        ensure!(s == StatusCode::OK && b["recovery_codes"].as_array().is_some_and(|c| c.len() == 10), "confirm: {s}: {b}");
        let st = p.get_json(&p.url("/api/me/totp")).await?;
        ensure!(st["enabled"] == json!(true), "{st}");
        Ok((p, secret))
    })
    .await;

    step("then the password alone is not enough, the code signs in, and the same code is never good twice", async {
        let (s, c, b) = login(qa, "qa-2fa", "a long qa-2fa password").await?;
        ensure!(s == StatusCode::OK && c.is_none() && b["second_factor"] == json!("totp"), "the password: {s}: {b}");
        let ticket = b["ticket"].as_str().context("no ticket")?.to_string();
        let (s, c, _) = code_step(qa, "/api/login/totp", json!({ "ticket": ticket, "code": "000000" })).await?;
        ensure!(s == StatusCode::UNAUTHORIZED && c.is_none(), "a wrong code: {s}");
        // The step the enrolment took is spent; the next one is good, and only once.
        let next = totp(&secret, now_step() + 1);
        let (s, c, b) = code_step(qa, "/api/login/totp", json!({ "ticket": ticket, "code": next })).await?;
        ensure!(s == StatusCode::OK, "the code: {s}: {b}");
        ensure!(me(qa, &c.context("no session after the code")?).await?["name"] == json!("qa-2fa"), "signed in as qa-2fa");
        let (_, _, b) = login(qa, "qa-2fa", "a long qa-2fa password").await?;
        let (s, c, _) = code_step(qa, "/api/login/totp", json!({ "ticket": b["ticket"], "code": next })).await?;
        ensure!(s == StatusCode::UNAUTHORIZED && c.is_none(), "the same code again: {s}");
        let log = qa.get_json(&qa.url("/api/auth-log?user=qa-2fa")).await?;
        let events: Vec<&str> = log["entries"].as_array().into_iter().flatten().filter_map(|e| e["event"].as_str()).collect();
        ensure!(events.contains(&"login.totp.failed") && events.contains(&"totp.enrol"), "the auth log: {events:?}");
        Ok(())
    })
    .await;

    step("with two-factor required, somebody without it sets it up at sign-in and gets their recovery codes", async {
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/two-factor"), Some(json!({ "required": true }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "required: {s}: {b}");
        let (s, b) = qa.post(&qa.url("/api/users"), json!({ "name": "qa-2fa-new", "password": "a long qa-2fa-new password" })).await?;
        ensure!(s == StatusCode::CREATED, "{s}: {b}");
        let (s, c, b) = login(qa, "qa-2fa-new", "a long qa-2fa-new password").await?;
        ensure!(s == StatusCode::OK && c.is_none() && b["second_factor"] == json!("enrol"), "the password: {s}: {b}");
        let ticket = b["ticket"].clone();
        let (s, _, made) = code_step(qa, "/api/login/totp/enrol", json!({ "ticket": ticket })).await?;
        ensure!(s == StatusCode::OK, "the seed: {s}: {made}");
        let code = totp(made["secret"].as_str().context("no secret")?, now_step());
        let (s, c, b) = code_step(qa, "/api/login/totp", json!({ "ticket": ticket, "code": code })).await?;
        ensure!(s == StatusCode::OK && c.is_some() && b["recovery_codes"].as_array().is_some_and(|r| r.len() == 10), "enrolled and signed in: {s}: {b}");
        // A recovery code then signs in once, in place of a code.
        let rc = b["recovery_codes"][0].as_str().context("a recovery code")?.to_string();
        for (want, what) in [(StatusCode::OK, "first"), (StatusCode::UNAUTHORIZED, "second")] {
            let (_, _, b) = login(qa, "qa-2fa-new", "a long qa-2fa-new password").await?;
            let (s, _, _) = code_step(qa, "/api/login/totp", json!({ "ticket": b["ticket"], "code": rc })).await?;
            ensure!(s == want, "a recovery code the {what} time: {s}");
        }
        let (s, b) = qa.send(Method::PUT, &qa.url("/api/settings/two-factor"), Some(json!({ "required": false }))).await?;
        ensure!(s == StatusCode::NO_CONTENT, "optional again: {s}: {b}");
        Ok(())
    })
    .await;

    step("an administrator resets a lost second factor, and the password alone signs in again", async {
        let users = qa.get_json(&qa.url("/api/users")).await?;
        let id = users.as_array().and_then(|l| l.iter().find(|u| u["name"] == json!("qa-2fa"))).and_then(|u| u["id"].as_i64()).context("no qa-2fa")?;
        let (s, b) = qa.send(Method::DELETE, &qa.url(&format!("/api/users/{id}/totp")), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "reset: {s}: {b}");
        let (s, c, b) = login(qa, "qa-2fa", "a long qa-2fa password").await?;
        ensure!(s == StatusCode::OK && c.is_some(), "after the reset: {s}: {b}");
        let st = person.get_json(&person.url("/api/me/totp")).await?;
        ensure!(st["enabled"] == json!(false), "{st}");
        for name in ["qa-2fa", "qa-2fa-new"] {
            let id = users.as_array().and_then(|l| l.iter().find(|u| u["name"] == json!(name))).and_then(|u| u["id"].as_i64()).context("user")?;
            qa.send(Method::DELETE, &qa.url(&format!("/api/users/{id}")), None).await?;
        }
        let (s, _) = qa.send(Method::DELETE, &qa.url("/api/settings/sso/qa-dir"), None).await?;
        ensure!(s == StatusCode::NO_CONTENT, "the directory removed: {s}");
        Ok(())
    })
    .await;
}
