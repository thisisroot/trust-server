//! Shared-media view per conversation (photos / videos / music / files / links /
//! voice) — the "shared media" surface on a profile, à la Telegram.
//!
//! Storage + pagination live in `db` (cursor pagination on an index over
//! `(conversation_id, kind, seq DESC)`). This module holds the attachment
//! descriptor type and the HTTP handler.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;

use crate::app::AppState;
use crate::auth::authed;
use crate::db::{self, MediaRow};
use crate::error::AppError;

/// Attachment descriptor the client sends alongside a message. It is metadata,
/// not message content — the blob it points at carries the payload.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub kind: String,
    pub blob_id: Option<String>,
    pub thumb_blob_id: Option<String>,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

fn media_json(e: &MediaRow) -> Value {
    json!({
        "id": e.id,
        "messageId": e.message_id,
        "senderAccountId": e.sender_account_id,
        "seq": e.seq,
        "kind": e.kind,
        "blobId": e.blob_id,
        "thumbBlobId": e.thumb_blob_id,
        "mime": e.mime,
        "name": e.name,
        "url": e.url,
        "size": e.size,
        "width": e.width,
        "height": e.height,
        "createdAt": e.created_at.format(&Rfc3339).unwrap_or_default(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaQuery {
    kind: Option<String>,
    before: Option<i64>,
    limit: Option<i64>,
}

/// GET /v0/conversations/{id}/media?kind=&before=&limit=
pub async fn list_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<uuid::Uuid>,
    Query(q): Query<MediaQuery>,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    if !db::is_participant(&state.db.pool, conversation_id, me).await? {
        return Err(AppError::NotFound("conversation not found".into()));
    }
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let (items, next) =
        db::list_media(&state.db.pool, conversation_id, q.kind.as_deref(), q.before, limit).await?;
    let arr: Vec<Value> = items.iter().map(media_json).collect();
    Ok(Json(json!({ "items": arr, "nextCursor": next })))
}
