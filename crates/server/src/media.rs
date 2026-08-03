//! Shared-media index per conversation (photos / videos / music / files / links
//! / voice) — the "shared media" surface on a profile, à la Telegram.
//!
//! Designed for scale: append-only, queried newest-first with an exclusive
//! `seq` cursor. In Postgres this is one table with a covering index on
//! `(conversation_id, kind, seq DESC)`; the in-memory version here mirrors that
//! access pattern exactly so the swap is mechanical.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::error::AppError;

#[derive(Clone)]
pub struct MediaEntry {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub message_id: Uuid,
    pub sender_account: Uuid,
    /// Conversation seq of the owning message — the pagination cursor.
    pub seq: i64,
    /// photo | video | music | file | link | voice
    pub kind: String,
    pub blob_id: Option<Uuid>,
    pub thumb_blob_id: Option<Uuid>,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub ts: OffsetDateTime,
}

/// Attachment descriptor the client sends alongside a message. It is metadata,
/// not message content — the blob it points at is what carries the payload.
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

#[derive(Default)]
pub struct MediaIndex {
    // conversation_id -> entries (insertion order; we sort per query).
    by_convo: Mutex<HashMap<Uuid, Vec<MediaEntry>>>,
}

impl MediaIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, entry: MediaEntry) {
        self.by_convo
            .lock()
            .unwrap()
            .entry(entry.conversation_id)
            .or_default()
            .push(entry);
    }

    pub fn export(&self) -> Vec<MediaEntry> {
        self.by_convo.lock().unwrap().values().flatten().cloned().collect()
    }

    pub fn import(&self, entries: Vec<MediaEntry>) {
        let mut map = self.by_convo.lock().unwrap();
        for e in entries {
            map.entry(e.conversation_id).or_default().push(e);
        }
    }

    /// A newest-first page. `kind` filters (None = every kind); `before` is an
    /// exclusive `seq` cursor (None = from newest). Returns (page, next_cursor)
    /// where next_cursor is Some only when more rows remain.
    pub fn list(
        &self,
        conversation_id: Uuid,
        kind: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> (Vec<MediaEntry>, Option<i64>) {
        let map = self.by_convo.lock().unwrap();
        let Some(all) = map.get(&conversation_id) else {
            return (Vec::new(), None);
        };
        let mut rows: Vec<&MediaEntry> = all
            .iter()
            .filter(|e| kind.map_or(true, |k| e.kind == k))
            .filter(|e| before.map_or(true, |b| e.seq < b))
            .collect();
        rows.sort_by(|a, b| b.seq.cmp(&a.seq)); // newest first
        let has_more = rows.len() > limit;
        let page: Vec<MediaEntry> = rows.into_iter().take(limit).cloned().collect();
        let next = if has_more { page.last().map(|e| e.seq) } else { None };
        (page, next)
    }
}

fn media_json(e: &MediaEntry) -> Value {
    json!({
        "id": e.id,
        "messageId": e.message_id,
        "senderAccountId": e.sender_account,
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
        "createdAt": e.ts.format(&Rfc3339).unwrap_or_default(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaQuery {
    kind: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
}

/// GET /v0/conversations/{id}/media?kind=&before=&limit=
pub async fn list_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Query(q): Query<MediaQuery>,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    if !state.hub.is_participant(conversation_id, me) {
        return Err(AppError::NotFound("conversation not found".into()));
    }
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let (items, next) = state
        .media
        .list(conversation_id, q.kind.as_deref(), q.before, limit);
    let arr: Vec<Value> = items.iter().map(media_json).collect();
    Ok(Json(json!({ "items": arr, "nextCursor": next })))
}
