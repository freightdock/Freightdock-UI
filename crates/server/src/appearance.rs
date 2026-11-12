//! Appearance: the FinUI preset this Freightdock wears, one for the whole instance ("UI" 8). A preset is FinUI's code, one
//! base-36 digit per axis of `create/presets.json` (FinUI create makes them); its overlay is FinUI's own files of tokens,
//! `p/<axis>/<option>.css`, joined in axis order right after `tokens.css` in `/assets/finui.css` (web.rs), so the page is
//! drawn in it from the first paint. Freightdock only takes the code: presets are made at FinUI create ("UI" 15).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Admin;
use crate::audit::{self, Entry};
use crate::db::{Db, rusqlite::{OptionalExtension, params}};
use crate::error::{ApiError, ApiResult};
use crate::state::App;
use crate::web::WebAssets;

const SETTINGS_KEY: &str = "appearance";

/// What Freightdock reads of FinUI's `presets.json`: each axis' options by key.
pub struct Presets {
    axes: Vec<Axis>,
}

struct Axis {
    options: Vec<String>,
}

fn parse() -> anyhow::Result<Presets> {
    let f = WebAssets::get("assets/finui/create/presets.json").ok_or_else(|| anyhow::anyhow!("FinUI's presets.json is missing"))?;
    let v: Value = serde_json::from_slice(&f.data)?;
    let mut axes = Vec::new();
    for a in v["axes"].as_array().ok_or_else(|| anyhow::anyhow!("presets.json has no axes"))? {
        let options = a["options"].as_array().map(|o| o.iter().map(|o| o["key"].as_str().unwrap_or_default().to_string()).collect()).unwrap_or_default();
        axes.push(Axis { options });
    }
    Ok(Presets { axes })
}

/// FinUI's presets, read once (a debug build reads them again each time, as it does the rest of `web/`).
pub fn presets() -> std::sync::Arc<Presets> {
    use std::sync::{Arc, OnceLock};
    let make = || Arc::new(parse().unwrap_or_else(|e| {
        tracing::error!("FinUI's presets could not be read: {e:#}");
        Presets { axes: Vec::new() }
    }));
    if cfg!(debug_assertions) {
        return make();
    }
    static ONCE: OnceLock<Arc<Presets>> = OnceLock::new();
    ONCE.get_or_init(make).clone()
}

impl Presets {
    /// The option of every axis a code names (FinUI's `decode`), or None when it names one that does not exist. A code
    /// shorter than the axes leaves the rest at their defaults: it was made before they were added.
    pub fn decode(&self, code: &str) -> Option<Vec<usize>> {
        if code.len() > self.axes.len() || !code.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()) {
            return None;
        }
        let mut choice = vec![0; self.axes.len()];
        for (i, c) in code.chars().enumerate() {
            let o = c.to_digit(36)? as usize;
            if o >= self.axes[i].options.len() {
                return None;
            }
            choice[i] = o;
        }
        Some(choice)
    }

    /// A choice as its shortest code: the defaults at the end say nothing, so FinUI as it ships is "".
    fn encode(choice: &[usize]) -> String {
        let s: String = choice.iter().map(|&o| char::from_digit(o as u32, 36).unwrap_or('0')).collect();
        s.trim_end_matches('0').to_string()
    }

    /// The code as it is kept: spaces and capitals forgiven, trailing defaults dropped. None when it is no preset.
    pub fn normalize(&self, code: &str) -> Option<String> {
        self.decode(&code.trim().to_ascii_lowercase()).map(|c| Self::encode(&c))
    }

    /// The overlay a code makes: FinUI's file for each axis not at its default, in axis order, after a line naming the
    /// code, as FinUI's install.sh writes it. "" for FinUI as it ships, or for a code that is no preset.
    pub fn overlay(&self, code: &str) -> anyhow::Result<String> {
        let Some(choice) = self.decode(code) else { return Ok(String::new()) };
        if choice.iter().all(|&o| o == 0) {
            return Ok(String::new());
        }
        let mut out = format!("/* FinUI preset {code}, made at FinUI create. */\n");
        for (a, &o) in choice.iter().enumerate().filter(|(_, o)| **o != 0) {
            let path = format!("assets/finui/p/{a}/{o}.css");
            let f = WebAssets::get(&path).ok_or_else(|| anyhow::anyhow!("{path} is missing from FinUI's copy"))?;
            out.push_str(std::str::from_utf8(&f.data)?.trim_end());
            out.push('\n');
        }
        Ok(out)
    }
}

/// The preset kept for this Freightdock: "" (FinUI as it ships) until an administrator sets one.
pub async fn current(db: &Db) -> String {
    let raw: anyhow::Result<Option<String>> = db.call(|c| Ok(c.query_row("SELECT value FROM settings WHERE key = ?1", [SETTINGS_KEY], |r| r.get(0)).optional()?)).await;
    match raw {
        Ok(Some(raw)) => serde_json::from_str::<Value>(&raw).ok().and_then(|v| v["preset"].as_str().map(str::to_string)).unwrap_or_default(),
        Ok(None) => String::new(),
        Err(e) => {
            tracing::error!("the appearance could not be read: {e:#}");
            String::new()
        }
    }
}

/// `GET /api/settings/appearance`: the preset kept, as its code.
pub async fn get_settings(State(app): State<App>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "preset": current(&app.db).await })))
}

#[derive(Deserialize)]
pub struct Body {
    preset: String,
}

/// `PUT /api/settings/appearance`: a new preset for everybody. The next `/assets/finui.css` is drawn in it.
pub async fn put_settings(State(app): State<App>, admin: Admin, Json(b): Json<Body>) -> ApiResult<StatusCode> {
    let p = presets();
    let code = p.normalize(&b.preset).ok_or_else(|| ApiError::bad("That is no FinUI preset. Make one at FinUI create and paste its code.").field("preset"))?;
    let raw = json!({ "preset": code }).to_string();
    app.db.call(move |c| Ok(c.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![SETTINGS_KEY, raw])?)).await?;
    let a = &admin.0;
    audit::record(&app.db, Some(&a.session.user), a.ip.clone(), Entry { action: "settings.appearance", target: Some(format!("preset {code}")), detail: Some(code) }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{app, call, json_body, signed_in};
    use axum::body::Body as HttpBody;
    use axum::http::{Request, header};

    #[test]
    fn a_code_is_one_digit_per_axis_and_names_options_that_exist() {
        let p = presets();
                assert_eq!(p.normalize(""), Some(String::new()), "nothing is FinUI as it ships");
        assert_eq!(p.normalize("0000"), Some(String::new()), "defaults at the end say nothing");
        assert_eq!(p.normalize(" 01 "), Some("01".into()));
        assert_eq!(p.normalize("1A"), Some("1a".into()), "capitals are forgiven");
        assert_eq!(p.normalize("z"), None, "there are not 36 bases");
        assert_eq!(p.normalize("0-1"), None);
        assert_eq!(p.normalize(&"1".repeat(p.axes.len() + 1)), None, "longer than the axes");
    }

    #[test]
    fn every_option_but_the_first_has_its_file_of_tokens() {
        let p = presets();
        let mut want = 0;
        for (a, axis) in p.axes.iter().enumerate() {
            assert!(WebAssets::get(&format!("assets/finui/p/{a}/0.css")).is_none(), "the first option is tokens.css itself");
            for o in 1..axis.options.len() {
                want += 1;
                assert!(WebAssets::get(&format!("assets/finui/p/{a}/{o}.css")).is_some(), "p/{a}/{o}.css");
            }
        }
        let have = WebAssets::iter().filter(|f| f.starts_with("assets/finui/p/")).count();
        assert_eq!(have, want, "no file for an option FinUI does not have");
    }

    #[test]
    fn the_overlay_is_each_chosen_axis_in_order() {
        let p = presets();
        assert_eq!(p.overlay("").unwrap(), "");
        let css = p.overlay("1001").unwrap();
        assert!(css.starts_with("/* FinUI preset 1001"));
        let base = css.find("/* Base colour: Neutral */").expect("the base");
        let radius = css.find("/* Radius: None */").expect("the radius");
        assert!(base < radius, "in axis order");
        assert_eq!(css.matches(":root {").count(), 2, "two axes chosen, two blocks");
        assert_eq!(p.overlay("nope").unwrap(), "", "a code that is no preset changes nothing");
    }

    #[tokio::test]
    async fn an_administrator_picks_the_preset_and_it_is_logged() {
        let app = app();
        let (cookie, csrf) = signed_in(&app).await;
        let get = || Request::get("/api/settings/appearance").header(header::COOKIE, &cookie).body(HttpBody::empty()).unwrap();
        let (s, _, v) = call(&app, get()).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v, json!({ "preset": "" }), "only the code: presets are made at FinUI create (\"UI\" 14)");
        let code = "1a02".to_string();

        let put = |body: Value| {
            Request::put("/api/settings/appearance")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "freightdock.test")
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", &csrf)
                .body(json_body(body))
                .unwrap()
        };
        let (s, _, e) = call(&app, put(json!({ "preset": "zz" }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(e["field"], "preset");
        let (s, _, _) = call(&app, put(json!({ "preset": format!(" {} ", code.to_uppercase()) }))).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (_, _, v) = call(&app, get()).await;
        assert_eq!(v["preset"], code, "kept as FinUI spells it");
        assert_eq!(current(&app.db).await, code);

        let (target, detail): (String, String) = app.db.call(|c| Ok(c.query_row("SELECT target, detail FROM audit WHERE action = 'settings.appearance'", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!((target.as_str(), detail.as_str()), ("preset 1a02", code.as_str()));
    }

    #[tokio::test]
    async fn only_an_administrator_with_the_token_changes_it() {
        let app = app();
        let (cookie, _) = signed_in(&app).await;
        let (s, _, _) = call(&app, Request::get("/api/settings/appearance").body(HttpBody::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let req = Request::put("/api/settings/appearance")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::HOST, "freightdock.test")
            .header(header::COOKIE, &cookie)
            .body(json_body(json!({ "preset": "1" })))
            .unwrap();
        let (s, _, _) = call(&app, req).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "no CSRF token");
        assert_eq!(current(&app.db).await, "");
    }
}
