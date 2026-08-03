//! One error type for handlers, rendered as the protocol's `Error` body.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use trust_storage::StorageError;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl AppError {
    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", m),
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "UNAUTHENTICATED", m),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, "NOT_FOUND", m),
            AppError::Conflict(m) => (StatusCode::CONFLICT, "CONFLICT", m),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", m),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(%code, %message, "request failed");
        }
        (status, Json(json!({ "code": code, "message": message }))).into_response()
    }
}

/// Map storage failures: surface unique-constraint violations as 409, everything else as 500
/// (without leaking internals to the client).
impl From<StorageError> for AppError {
    fn from(err: StorageError) -> Self {
        if let StorageError::Conflict(msg) = &err {
            return AppError::Conflict(msg.clone());
        }
        if let StorageError::Sqlx(sqlx::Error::Database(ref db)) = err {
            if db.is_unique_violation() {
                return AppError::Conflict("already exists".to_string());
            }
        }
        AppError::Internal(err.to_string())
    }
}
