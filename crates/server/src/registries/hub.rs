//! Docker Hub lists no catalog (`/v2/_catalog` is refused there), so its repositories come from Hub's own API: an
//! account's or organisation's, private ones too when signed in with a user name and a password or personal access
//! token ("Registries" 5 in DECISIONS.md). Tags come from the registry API as for any other registry.

use reqwest::{StatusCode, header};
use serde::Deserialize;

use super::v2::{Fail, MOST};

/// Docker Hub's API.
pub const API: &str = "https://hub.docker.com";
/// Docker Hub's registry API, where tags are listed and pulls go.
pub const REGISTRY: &str = "https://registry-1.docker.io";

fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(concat!("Freightdock/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
    .clone()
}

fn unreachable(e: reqwest::Error) -> Fail {
    Fail::Unreachable(format!("Docker Hub could not be reached: {}.", e.without_url()))
}

/// A token for Hub's API: `/v2/auth/token` (identifier and secret), or the older `/v2/users/login` where that is not
/// there.
async fn sign_in(api: &str, user: &str, password: &str) -> Result<String, Fail> {
    #[derive(Deserialize)]
    struct T {
        access_token: Option<String>,
        token: Option<String>,
    }
    let tries = [
        (format!("{api}/v2/auth/token"), serde_json::json!({ "identifier": user, "secret": password })),
        (format!("{api}/v2/users/login"), serde_json::json!({ "username": user, "password": password })),
    ];
    for (url, body) in tries {
        let r = http().post(&url).json(&body).send().await.map_err(unreachable)?;
        match r.status() {
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => continue,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Fail::Denied("Docker Hub refused that user name and password.".into())),
            s if s.is_success() => {
                let t: T = r.json().await.map_err(|_| Fail::Unreachable("Docker Hub answered a sign-in without a token.".into()))?;
                return t.access_token.or(t.token).ok_or_else(|| Fail::Unreachable("Docker Hub answered a sign-in without a token.".into()));
            }
            s => return Err(Fail::Unreachable(format!("Docker Hub answered the sign-in with {s}."))),
        }
    }
    Err(Fail::Unreachable("Docker Hub's API has no sign-in Freightdock knows.".into()))
}

/// Every repository of `namespace` (an account or an organisation) on the Hub at `api`, as `namespace/name`.
pub async fn repositories(api: &str, namespace: &str, user: Option<(&str, &str)>) -> Result<Vec<String>, Fail> {
    let not_found = || Fail::NotFound(format!("Docker Hub has no account or organisation {namespace}."));
    if namespace.is_empty() || !namespace.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(not_found());
    }
    let token = match user {
        Some((u, p)) => Some(sign_in(api, u, p).await?),
        None => None,
    };
    #[derive(Deserialize)]
    struct Repo {
        name: String,
        namespace: Option<String>,
    }
    #[derive(Deserialize)]
    struct Page {
        next: Option<String>,
        #[serde(default)]
        results: Vec<Repo>,
    }
    let mut out = Vec::new();
    let mut url = format!("{api}/v2/repositories/{namespace}/?page_size=100");
    loop {
        let mut r = http().get(&url).header(header::ACCEPT, "application/json");
        if let Some(t) = &token {
            r = r.bearer_auth(t);
        }
        let r = r.send().await.map_err(unreachable)?;
        match r.status() {
            s if s.is_success() => {}
            StatusCode::NOT_FOUND => return Err(not_found()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Fail::Denied("Docker Hub refused to list those repositories.".into())),
            s => return Err(Fail::Unreachable(format!("Docker Hub answered {s}."))),
        }
        let page: Page = r.json().await.map_err(|_| Fail::Unreachable("Docker Hub answered something that is not a list of repositories.".into()))?;
        out.extend(page.results.into_iter().map(|r| format!("{}/{}", r.namespace.unwrap_or_else(|| namespace.to_string()), r.name)));
        // The next page, only on the same API: the token goes with it.
        match page.next.filter(|n| n.starts_with(&format!("{api}/"))) {
            Some(n) if out.len() < MOST => url = n,
            _ => break,
        }
    }
    out.sort();
    out.truncate(MOST);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use std::collections::HashMap;

    async fn fake() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let me = base.clone();
        let login = |axum::Json(b): axum::Json<serde_json::Value>| async move {
            if b["identifier"] == "deck" && b["secret"] == "dckr_pat_good" {
                axum::Json(serde_json::json!({ "access_token": "hubjwt" })).into_response()
            } else {
                (StatusCode::UNAUTHORIZED, "{}").into_response()
            }
        };
        let repos = move |Path(ns): Path<String>, Query(q): Query<HashMap<String, String>>, h: HeaderMap| {
            let me = me.clone();
            async move {
                if ns != "deck" {
                    return (StatusCode::NOT_FOUND, "{}").into_response();
                }
                let signed = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some("Bearer hubjwt");
                // Two pages; the private one only for its owner.
                let r: Response = if q.get("page").map(String::as_str) == Some("2") {
                    axum::Json(serde_json::json!({ "next": null, "results": [{ "name": "web", "namespace": "deck" }] })).into_response()
                } else {
                    let mut results = vec![serde_json::json!({ "name": "app", "namespace": "deck" })];
                    if signed {
                        results.push(serde_json::json!({ "name": "secret-app", "namespace": "deck", "is_private": true }));
                    }
                    axum::Json(serde_json::json!({ "next": format!("{me}/v2/repositories/deck/?page=2&page_size=100"), "results": results })).into_response()
                };
                r
            }
        };
        let app = axum::Router::new().route("/v2/auth/token", post(login)).route("/v2/repositories/{ns}/", get(repos));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn an_account_s_repositories_page_by_page_its_private_ones_when_signed_in() {
        let api = fake().await;
        assert_eq!(repositories(&api, "deck", None).await.unwrap(), vec!["deck/app", "deck/web"]);
        assert_eq!(repositories(&api, "deck", Some(("deck", "dckr_pat_good"))).await.unwrap(), vec!["deck/app", "deck/secret-app", "deck/web"]);
        assert!(matches!(repositories(&api, "deck", Some(("deck", "wrong"))).await, Err(Fail::Denied(_))));
        assert!(matches!(repositories(&api, "nobody", None).await, Err(Fail::NotFound(_))));
        assert!(matches!(repositories(&api, "../x", None).await, Err(Fail::NotFound(_))), "a namespace is a name, not a path");
    }
}
