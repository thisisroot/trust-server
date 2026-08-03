//! In-memory binary blob store for avatars and image messages (dev). A blob is
//! opaque bytes + a content type; uploads are authenticated, downloads are open
//! so the client can load them directly via a URL.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::error::AppError;

#[derive(Clone)]
pub struct Blob {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub struct BlobStore {
    map: Mutex<HashMap<Uuid, Blob>>,
}

impl BlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, content_type: String, bytes: Vec<u8>) -> Uuid {
        let id = Uuid::new_v4();
        self.map.lock().unwrap().insert(id, Blob { content_type, bytes });
        id
    }

    pub fn get(&self, id: Uuid) -> Option<Blob> {
        self.map.lock().unwrap().get(&id).cloned()
    }

    /// Insert a blob under a known id (used when restoring persisted blobs).
    pub fn insert(&self, id: Uuid, content_type: String, bytes: Vec<u8>) {
        self.map.lock().unwrap().insert(id, Blob { content_type, bytes });
    }

    /// (id, content_type) for every stored blob — bytes are persisted separately.
    pub fn manifest(&self) -> Vec<(Uuid, String)> {
        self.map.lock().unwrap().iter().map(|(k, v)| (*k, v.content_type.clone())).collect()
    }

    pub fn bytes_of(&self, id: Uuid) -> Option<Vec<u8>> {
        self.map.lock().unwrap().get(&id).map(|b| b.bytes.clone())
    }
}

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
    let id = state.blobs.put(content_type, body.to_vec());
    Ok(Json(json!({ "blobId": id })))
}

/// GET /v0/blobs/{id} — download bytes (open, so `Image.network` can load it).
pub async fn get_blob(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match state.blobs.get(id) {
        Some(blob) => (
            [
                (header::CONTENT_TYPE, blob.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000".to_string()),
            ],
            blob.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
