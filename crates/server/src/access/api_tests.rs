//! In-process tests of the guard: an Administrator and people who are not, each signed in, against the routes.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;

use crate::api::tests::{call, json_body, signed_in};
use crate::state::App;

/// An app with an Administrator (`admin`) and people who are not (`deckhand`, `operator`, `helpdesk`, `nobody`, ids 11 to 14), each signed in.
pub struct World {
    pub app: App,
    pub people: Vec<(&'static str, String, String)>,
}

pub const PEOPLE: [&str; 5] = ["admin", "deckhand", "operator", "helpdesk", "nobody"];

impl World {
    pub async fn new() -> World {
        let db = crate::db::Db::open_in_memory().unwrap();
        let app = crate::state::AppState::new(db, false, crate::secret::Sealer::for_test());
        let (cookie, csrf) = signed_in(&app).await;
        let mut people = vec![("admin", cookie, csrf)];
        for (i, name) in PEOPLE.iter().enumerate().skip(1) {
            let (id, name) = (10 + i as i64, name.to_string());
            app.db.call(move |c| Ok(c.execute("INSERT INTO users (id, name, password_hash, role, created_at) VALUES (?1, ?2, 'x', 'user', 0)", crate::db::rusqlite::params![id, name])?)).await.unwrap();
            let token = crate::auth::start(&app.db, id, None, None).await.unwrap();
            let csrf = crate::auth::lookup(&app.db, &token).await.unwrap().unwrap().csrf;
            people.push((PEOPLE[i], format!("freightdock_session={token}"), csrf));
        }
        World { app, people }
    }

    pub async fn as_(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (_, cookie, csrf) = self.people.iter().find(|p| p.0 == who).unwrap();
        let r = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "freightdock.test")
            .header(header::ORIGIN, "https://freightdock.test")
            .header(header::COOKIE, cookie)
            .header("x-csrf-token", csrf)
            .header(header::CONTENT_TYPE, "application/json");
        let (s, _, b) = call(&self.app, r.body(body.map_or(Body::empty(), json_body)).unwrap()).await;
        (s, b)
    }
}

#[tokio::test]
async fn an_administrator_s_routes_refuse_the_rest_and_a_session_s_take_anybody_signed_in() {
    let w = World::new().await;
    for (method, path) in [("GET", "/api/users"), ("GET", "/api/audit"), ("GET", "/api/settings/sso"), ("POST", "/api/cluster/join-tokens")] {
        assert_eq!(w.as_("nobody", method, path, None).await.0, StatusCode::FORBIDDEN, "{method} {path}");
        assert_ne!(w.as_("admin", method, path, None).await.0, StatusCode::FORBIDDEN, "{method} {path}");
    }
    for who in PEOPLE {
        assert_eq!(w.as_(who, "GET", "/api/me", None).await.0, StatusCode::OK, "{who}");
    }
    let anon = Request::get("/api/me").header(header::HOST, "freightdock.test").body(Body::empty()).unwrap();
    assert_eq!(call(&w.app, anon).await.0, StatusCode::UNAUTHORIZED);
}
