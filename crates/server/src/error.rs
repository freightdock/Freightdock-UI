//! The one error an API handler answers: a status, a sentence a person can read, and the form field it is about when
//! it is about one. Anything unexpected is logged in full and answered as a plain 500, never with its details.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub field: Option<&'static str>,
    pub retry_after: Option<i64>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError { status, message: message.into(), field: None, retry_after: None }
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Sign in first.")
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "You may not do that.")
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("No such {what}."))
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server. Its log says what.")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = axum::Json(serde_json::json!({ "error": self.message, "field": self.field }));
        let mut r = (self.status, body).into_response();
        if let Some(v) = self.retry_after.and_then(|s| s.to_string().parse().ok()) {
            r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
        }
        r
    }
}
