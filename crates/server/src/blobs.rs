//! Binary blob storage (avatars, image/file messages), Postgres-backed. A blob
//! is opaque bytes + a content type; uploads are authenticated, downloads are
//! open so the client can load them directly via a URL.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::db;
use crate::error::AppError;

/// POST /v0/blobs — upload raw bytes; returns the blob id.
pub async fn upload_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    authed(&state, &headers).await?;
    if body.is_empty() {
        return Err(AppError::BadRequest("empty body".into()));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let id = db::put_blob(&state.db.pool, &content_type, &body).await?;
    Ok(Json(json!({ "blobId": id })))
}

/// GET /v0/blobs/{id} — download bytes (open, so `Image.network` can load it).
pub async fn get_blob(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match db::get_blob(&state.db.pool, id).await {
        Ok(Some(blob)) => (
            [
                (header::CONTENT_TYPE, blob.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000".to_string()),
            ],
            blob.bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
