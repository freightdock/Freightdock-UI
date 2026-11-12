//! The web UI: `web/` compiled into the binary (a debug build reads it from disk on every request, so an edit shows on
//! the next refresh). FinUI's stylesheets are served joined as `/assets/finui.css`, in `registry.json`'s order:
//! the foundation first, then each component's CSS, with the preset this Freightdock wears (appearance.rs) right after
//! `tokens.css`, whose values it replaces. Any address that is not a file or the API is the app's own page,
//! `index.html`, and the router in the browser draws it.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use serde::Deserialize;
use sha2::Digest;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web"]
pub struct WebAssets;

/// What FinUI's `registry.json` says. The names are read by the tests that hold the copy to FinUI.
#[derive(Deserialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Registry {
    pub name: String,
    pub prefix: String,
    pub foundation: Vec<String>,
    pub components: Vec<Component>,
}

#[derive(Deserialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Component {
    pub name: String,
    pub files: Vec<String>,
}

fn text(path: &str) -> anyhow::Result<String> {
    let f = WebAssets::get(path).ok_or_else(|| anyhow::anyhow!("{path} is listed but not there"))?;
    Ok(String::from_utf8(f.data.into_owned())?)
}

pub fn registry() -> anyhow::Result<Registry> {
    Ok(serde_json::from_str(&text("assets/finui/registry.json")?)?)
}

/// Every CSS file the registry lists, foundation first, each once, in order, and a preset's overlay right after
/// `tokens.css` (FinUI's `stylesheet()` does the same).
pub fn finui_stylesheet(overlay: &str) -> anyhow::Result<String> {
    let r = registry()?;
    let mut out = String::from("/* FinUI, from web/assets/finui, in registry.json's order. */\n");
    let mut seen = std::collections::HashSet::new();
    for path in r.foundation.iter().chain(r.components.iter().flat_map(|c| c.files.iter())).filter(|p| p.ends_with(".css")) {
        if !seen.insert(path.as_str()) {
            continue;
        }
        out.push('\n');
        out.push_str(text(&format!("assets/finui/{path}"))?.trim());
        out.push('\n');
        if path == "tokens.css" && !overlay.is_empty() {
            out.push('\n');
            out.push_str(overlay);
        }
    }
    Ok(out)
}

fn etag_of(bytes: &[u8]) -> String {
    format!("\"{}\"", hex::encode(&sha2::Sha256::digest(bytes)[..8]))
}

/// The stylesheet in a preset, with its ETag. A release build keeps the last one made: the preset changes seldom.
fn finui_served(preset: &str) -> (Arc<String>, String) {
    let make = || {
        let css = crate::appearance::presets().overlay(preset).and_then(|o| finui_stylesheet(&o)).unwrap_or_else(|e| {
            tracing::error!("FinUI's stylesheet could not be put together: {e:#}");
            String::new()
        });
        let tag = etag_of(css.as_bytes());
        (Arc::new(css), tag)
    };
    if cfg!(debug_assertions) {
        return make();
    }
    static LAST: Mutex<Option<(String, Arc<String>, String)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match &*last {
        Some((p, css, tag)) if p == preset => (css.clone(), tag.clone()),
        _ => {
            let (css, tag) = make();
            *last = Some((preset.to_string(), css.clone(), tag.clone()));
            (css, tag)
        }
    }
}

fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag))
}

fn cached(body: Vec<u8>, mime: &str, etag: String, headers: &HeaderMap, immutable: bool) -> Response {
    if not_modified(headers, &etag) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let cache = if immutable && !cfg!(debug_assertions) { "public, max-age=604800" } else { "no-cache" };
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    r
}

/// The app's page: what every address the browser's router owns answers.
fn index(headers: &HeaderMap) -> Response {
    match WebAssets::get("index.html") {
        Some(f) => {
            let tag = etag_of(&f.data);
            cached(f.data.into_owned(), "text/html; charset=utf-8", tag, headers, false)
        }
        None => (StatusCode::INTERNAL_SERVER_ERROR, "the web UI is missing from this build").into_response(),
    }
}

pub async fn serve(State(app): State<crate::state::App>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "assets/finui.css" {
        let (css, tag) = finui_served(&crate::appearance::current(&app.db).await);
        return cached(css.as_bytes().to_vec(), "text/css; charset=utf-8", tag, &headers, false);
    }
    // finui.css is served from /assets/, so the fonts its CSS names relatively (`fonts/inter...`) are asked for at
    // /assets/fonts/: they are FinUI's own, in its folder.
    let path = match path.strip_prefix("assets/fonts/") {
        Some(font) => format!("assets/finui/fonts/{font}"),
        None => path.to_string(),
    };
    let path = path.as_str();
    if path.starts_with("api/") || path == "api" {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "No such endpoint." }))).into_response();
    }
    // A file the UI is made of; a path with a dot-dot never matches one, since the embed lists only real files.
    if !path.is_empty() && path != "index.html" {
        if let Some(f) = WebAssets::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mime = if mime.type_() == "text" || mime.subtype() == "javascript" || mime.subtype() == "json" {
                format!("{mime}; charset=utf-8")
            } else {
                mime.to_string()
            };
            let tag = etag_of(&f.data);
            let fonts = path.starts_with("assets/finui/fonts/");
            return cached(f.data.into_owned(), &mime, tag, &headers, fonts);
        }
        if path.starts_with("assets/") || path.contains('.') {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
    }
    index(&headers)
}

/// What the page may load and who may frame it ("UI" 16). Scripts and modules only from Freightdock itself (the vendored
/// CodeMirror is files here, loaded by `import()`), no eval. Styles from here and inline, since CodeMirror adds its own
/// sheet. Images from anywhere, as an avatar is wherever its provider keeps it. Nothing may frame it.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https: http:; \
font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Every answer, the page, its files and the API's, gets the headers that keep it from being framed, sniffed or named
/// in a referrer elsewhere; an answer of the API's, when it says nothing of caching, is kept by none.
pub fn harden(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(|req: axum::extract::Request, next: axum::middleware::Next| async move {
        let api = req.uri().path().starts_with("/api/") || req.uri().path() == "/mcp";
        let mut r = next.run(req).await;
        let h = r.headers_mut();
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
        if api && !h.contains_key(header::CACHE_CONTROL) {
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
        r
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finui_is_finui_and_every_file_it_lists_is_there() {
        let r = registry().unwrap();
        assert_eq!((r.name.as_str(), r.prefix.as_str()), ("finui", "fui-"));
        for path in r.foundation.iter().chain(r.components.iter().flat_map(|c| c.files.iter())) {
            assert!(WebAssets::get(&format!("assets/finui/{path}")).is_some(), "{path} is listed but not there");
        }
        assert!(r.components.iter().any(|c| c.name == "data-table"));
    }

    #[test]
    fn the_stylesheet_starts_with_the_tokens_and_holds_every_component() {
        let css = finui_stylesheet("").unwrap();
        let tokens = text("assets/finui/tokens.css").unwrap();
        let base = text("assets/finui/base.css").unwrap();
        let first = css.find(tokens.trim()).expect("tokens.css is in it");
        let then = css.find(base.trim()).expect("base.css is in it");
        assert!(first < then, "tokens come before base");
        assert!(css.contains(".fui-button"), "a component's CSS is in it");
    }

    #[tokio::test]
    async fn an_address_of_the_app_is_its_page_and_a_missing_file_is_not() {
        let r = serve(State(crate::api::tests::app()), "/containers/abc".parse().unwrap(), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
        let r = serve(State(crate::api::tests::app()), "/assets/nope.js".parse().unwrap(), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = serve(State(crate::api::tests::app()), "/api/nope".parse().unwrap(), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_fonts_finui_css_names_are_found_beside_it() {
        let css = finui_stylesheet("").unwrap();
        let font = css.split("url(\"fonts/").nth(1).and_then(|r| r.split('"').next()).or_else(|| css.split("url(fonts/").nth(1).and_then(|r| r.split(')').next())).expect("the stylesheet names a font");
        let r = serve(State(crate::api::tests::app()), format!("/assets/fonts/{font}").parse().unwrap(), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::OK, "{font}");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "font/woff2");
    }

    #[tokio::test]
    async fn the_preset_kept_is_joined_right_after_the_tokens() {
        let app = crate::api::tests::app();
        let css = |app: crate::state::App| async move {
            let r = serve(State(app), "/assets/finui.css".parse().unwrap(), HeaderMap::new()).await;
            let tag = r.headers()[header::ETAG].to_str().unwrap().to_string();
            let body = http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes();
            (String::from_utf8(body.to_vec()).unwrap(), tag)
        };
        let (washi, washi_tag) = css(app.clone()).await;
        assert!(!washi.contains("FinUI preset"), "Washi is FinUI as it ships");
        app.db.call(|c| Ok(c.execute("INSERT INTO settings (key, value) VALUES ('appearance', '{\"preset\":\"1\"}')", [])?)).await.unwrap();
        let (neutral, tag) = css(app.clone()).await;
        assert_ne!(tag, washi_tag, "a browser that had Washi is sent the new one");
        let tokens = text("assets/finui/tokens.css").unwrap();
        let after_tokens = neutral.find(tokens.trim()).unwrap() + tokens.trim().len();
        let preset = neutral.find("/* FinUI preset 1,").expect("the overlay is in it");
        let base = neutral.find(text("assets/finui/base.css").unwrap().trim()).unwrap();
        assert!(after_tokens <= preset && preset < base, "after tokens.css, whose values it replaces, and before the rest");
        assert!(neutral.contains("/* Base colour: Neutral */"));
    }

    /// The whole app as main.rs puts it together, with the header layer.
    fn whole(app: crate::state::App) -> axum::Router {
        let ui = axum::Router::new().fallback(serve).with_state(app.clone());
        harden(crate::api::router(app).fallback_service(ui))
    }

    async fn headers_of(path: &str) -> HeaderMap {
        let r = tower::ServiceExt::oneshot(whole(crate::api::tests::app()), axum::http::Request::get(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
        r.headers().clone()
    }

    #[tokio::test]
    async fn no_page_elsewhere_may_frame_freightdock_or_run_a_script_in_it() {
        for path in ["/", "/containers/abc", "/api/status", "/assets/js/main.js"] {
            let h = headers_of(path).await;
            let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
            assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
            assert!(csp.contains("script-src 'self'") && !csp.contains("'unsafe-eval'"), "{path}: {csp}");
            assert!(csp.contains("object-src 'none'") && csp.contains("base-uri 'none'"), "{path}: {csp}");
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY", "{path}");
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
            assert_eq!(h[header::REFERRER_POLICY], "same-origin", "{path}");
        }
    }

    #[tokio::test]
    async fn an_api_answer_is_never_kept_by_a_cache_unless_it_says_so() {
        assert_eq!(headers_of("/api/status").await[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers_of("/api/me").await[header::CACHE_CONTROL], "no-store", "a refusal too");
        assert_eq!(headers_of("/").await[header::CACHE_CONTROL], "no-cache", "the page keeps its own");
    }

    #[tokio::test]
    async fn a_file_seen_before_is_not_sent_again() {
        let r = serve(State(crate::api::tests::app()), "/assets/finui.css".parse().unwrap(), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::OK);
        let tag = r.headers()[header::ETAG].clone();
        let mut h = HeaderMap::new();
        h.insert(header::IF_NONE_MATCH, tag);
        let r = serve(State(crate::api::tests::app()), "/assets/finui.css".parse().unwrap(), h).await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    }

    /// The languages the UI speaks ("UI" 17): the files in `web/assets/i18n/`, English the source.
    const LANGUAGES: [&str; 2] = ["en", "nb"];

    /// Every text of a catalogue by its dotted key, a plural object being one entry; `$meta` left out.
    fn leaves(v: &serde_json::Value, at: &str, out: &mut std::collections::BTreeMap<String, serde_json::Value>) {
        let obj = v.as_object().unwrap_or_else(|| panic!("{at} is neither a text nor a group"));
        let plural = obj.get("other").is_some_and(|o| o.is_string()) && obj.keys().all(|k| ["zero", "one", "two", "few", "many", "other"].contains(&k.as_str()));
        if plural && !at.is_empty() { out.insert(at.to_string(), v.clone()); return }
        for (k, x) in obj {
            if at.is_empty() && k == "$meta" { continue }
            let key = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
            if x.is_string() { out.insert(key, x.clone()); } else { leaves(x, &key, out); }
        }
    }

    fn catalogue(tag: &str) -> std::collections::BTreeMap<String, serde_json::Value> {
        let raw = text(&format!("assets/i18n/{tag}.json")).unwrap_or_else(|e| panic!("{tag}: {e}"));
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{tag}.json does not parse: {e}"));
        assert_eq!(v["$meta"]["tag"], tag, "{tag}.json says it is {}", v["$meta"]["tag"]);
        let mut out = Default::default();
        leaves(&v, "", &mut out);
        out
    }

    /// The `{placeholders}` of a text, or of every form of a plural.
    fn placeholders(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
        let texts: Vec<&str> = match v { serde_json::Value::String(s) => vec![s], _ => v.as_object().unwrap().values().filter_map(|x| x.as_str()).collect() };
        texts.iter().flat_map(|t| t.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string()))).collect()
    }

    #[test]
    fn every_language_has_the_sources_texts_and_no_other() {
        let source = catalogue(LANGUAGES[0]);
        assert!(source.len() > 100, "the English catalogue is the whole UI");
        for tag in &LANGUAGES[1..] {
            let other = catalogue(tag);
            let missing: Vec<_> = source.keys().filter(|k| !other.contains_key(*k)).collect();
            let extra: Vec<_> = other.keys().filter(|k| !source.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{tag} lacks {missing:?}");
            assert!(extra.is_empty(), "{tag} has keys English lacks: {extra:?}");
            for (k, v) in &source {
                assert_eq!(placeholders(v), placeholders(&other[k]), "{tag}: {k} fills other placeholders than English");
                assert_eq!(v.is_string(), other[k].is_string(), "{tag}: {k} is a plural in one language and not the other");
            }
        }
    }

    #[test]
    fn a_plural_has_the_forms_its_language_has_and_no_text_an_em_dash() {
        for tag in LANGUAGES {
            for (k, v) in catalogue(tag) {
                if let Some(forms) = v.as_object() {
                    let mut names: Vec<_> = forms.keys().map(String::as_str).collect();
                    names.sort();
                    // English and Bokmål both have one and other (CLDR), and Intl.PluralRules picks between them.
                    assert_eq!(names, ["one", "other"], "{tag}: {k}");
                }
                assert!(!v.to_string().contains('\u{2014}'), "{tag}: {k} has an em-dash");
            }
        }
    }

    #[test]
    fn the_language_list_of_the_page_is_the_catalogues() {
        let js = text("assets/js/i18n.js").unwrap();
        let tags: Vec<&str> = js.split("{ tag: '").skip(1).filter_map(|p| p.split('\'').next()).collect();
        assert_eq!(tags, LANGUAGES, "LANGUAGES in i18n.js");
        let theme = text("assets/js/theme.js").unwrap();
        assert!(LANGUAGES.iter().all(|t| theme.contains(&format!("'{t}'"))), "theme.js preloads every language");
    }
}
