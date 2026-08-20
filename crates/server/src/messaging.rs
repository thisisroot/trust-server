//! User directory + 1:1 messaging (send / history) and delivery helpers.
//! Message bodies are opaque base64 (dev sends base64 plaintext; MLS ciphertext
//! later). Durable state is in Postgres; delivery fans out over the realtime bus.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trust_protocol_types::Envelope;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::db;
use crate::error::AppError;

fn rfc3339(dt: OffsetDateTime) -> String {
    dt.format(&Rfc3339).unwrap_or_default()
}

/// Build an event envelope the client can consume over the WebSocket.
pub fn envelope(type_: &str, payload: Value) -> Envelope {
    Envelope {
        v: 0,
        type_: type_.to_string(),
        id: Uuid::new_v4(),
        ts: OffsetDateTime::now_utc(),
        payload,
    }
}

/// Publish an envelope to all of an account's connected devices (optionally
/// skipping one, e.g. the sender's own device).
pub async fn deliver_to_account(
    state: &AppState,
    account: Uuid,
    env: &Envelope,
    except_device: Option<Uuid>,
) {
    if let Ok(devices) = state.auth.list_devices_for_account(account).await {
        for d in devices {
            if Some(d.id) == except_device {
                continue;
            }
            state.bus.publish(d.id, env.clone());
        }
    }
}

/// Tell everyone currently connected about a presence change.
pub async fn broadcast_presence(
    state: &AppState,
    account: Uuid,
    online: bool,
    last_seen: Option<OffsetDateTime>,
) {
    let env = envelope(
        "presence.update",
        json!({
            "userId": account,
            "online": online,
            "lastSeen": last_seen.map(rfc3339),
        }),
    );
    for device in state.presence.connected_devices() {
        state.bus.publish(device, env.clone());
    }
}

/// GET /v0/users — everyone else on this server, with presence.
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let users = db::list_users(&state.db.pool, me).await?;
    let out: Vec<Value> = users
        .into_iter()
        .map(|u| {
            json!({
                "userId": u.id,
                "username": u.username,
                "displayName": u.display_name,
                "avatarBlobId": u.avatar_blob_id,
                "online": state.presence.is_online(u.id),
                "lastSeen": u.last_seen.map(rfc3339),
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReq {
    to_username: String,
    ciphertext: String,
    /// A single media descriptor (legacy) …
    attachment: Option<crate::media::AttachmentMeta>,
    /// … or an album of them; each is indexed for the shared-media view.
    attachments: Option<Vec<crate::media::AttachmentMeta>>,
    /// The message this one quotes (a reply), if any.
    reply_to_message_id: Option<String>,
}

/// POST /v0/messages — send a message to a user; delivers over the bus.
pub async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendReq>,
) -> Result<Json<Value>, AppError> {
    let (me, my_device) = authed(&state, &headers).await?;
    let target = state
        .auth
        .find_account_by_username(&req.to_username)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let bytes = STANDARD
        .decode(req.ciphertext.as_bytes())
        .map_err(|_| AppError::BadRequest("ciphertext must be base64".into()))?;

    let convo = db::dm_conversation(&state.db.pool, me, target.id).await?;
    let reply_to = req
        .reply_to_message_id
        .as_deref()
        .and_then(|v| Uuid::parse_str(v).ok());
    let msg = db::store_message(&state.db.pool, convo, me, my_device, &bytes, reply_to).await?;

    // Index any attachments (single or album) for the shared-media view.
    let mut atts = req.attachments.unwrap_or_default();
    if atts.is_empty() {
        atts.extend(req.attachment);
    }
    let parse = |s: &Option<String>| s.as_deref().and_then(|v| Uuid::parse_str(v).ok());
    for att in atts {
        db::record_media(
            &state.db.pool,
            convo,
            msg.id,
            me,
            msg.seq,
            &att.kind,
            parse(&att.blob_id),
            parse(&att.thumb_blob_id),
            att.mime,
            att.name,
            att.url,
            att.size,
            att.width,
            att.height,
        )
        .await?;
    }

    let env = envelope(
        "message.new",
        json!({
            "conversationId": convo,
            "messageId": msg.id,
            "seq": msg.seq,
            "senderAccountId": me,
            "senderDeviceId": my_device,
            "ciphertext": STANDARD.encode(&bytes),
            "serverTs": rfc3339(msg.server_ts),
            "replyTo": reply_to,
        }),
    );
    if target.id == me {
        // Saved Messages (self-chat): deliver only to my other devices.
        deliver_to_account(&state, me, &env, Some(my_device)).await;
    } else {
        deliver_to_account(&state, target.id, &env, None).await;
        deliver_to_account(&state, me, &env, Some(my_device)).await;
    }

    Ok(Json(json!({
        "messageId": msg.id,
        "seq": msg.seq,
        "serverTs": rfc3339(msg.server_ts),
        "conversationId": convo,
    })))
}

/// GET /v0/conversations — the people you've actually messaged (with presence),
/// most-recent first. This is the chats list.
pub async fn list_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let rows = db::conversations_for(&state.db.pool, me).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "userId": r.other,
                "username": r.username,
                "displayName": r.display_name,
                "avatarBlobId": r.avatar_blob_id,
                "online": state.presence.is_online(r.other),
                "lastSeen": r.last_seen.map(rfc3339),
                "conversationId": r.cid,
            })
        })
        .collect();
    Ok(Json(json!(items)))
}

#[derive(Deserialize)]
pub struct LookupQuery {
    username: String,
}

/// GET /v0/users/lookup?username= — resolve one user (for starting a new chat).
pub async fn lookup_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LookupQuery>,
) -> Result<Json<Value>, AppError> {
    let (_me, _device) = authed(&state, &headers).await?;
    let u = db::find_user_by_username(&state.db.pool, &q.username)
        .await?
        .ok_or_else(|| AppError::NotFound("no user with that username".into()))?;
    Ok(Json(json!({
        "userId": u.id,
        "username": u.username,
        "displayName": u.display_name,
        "avatarBlobId": u.avatar_blob_id,
        "online": state.presence.is_online(u.id),
        "lastSeen": u.last_seen.map(rfc3339),
    })))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    with: String,
}

/// GET /v0/messages?with=username — conversation history.
pub async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let target = state
        .auth
        .find_account_by_username(&q.with)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;
    let convo = db::dm_conversation(&state.db.pool, me, target.id).await?;
    let peer_read = db::read_seq(&state.db.pool, convo, target.id).await?;
    let peer_delivered = db::delivered_seq(&state.db.pool, convo, target.id).await?;

    let msgs: Vec<Value> = db::history(&state.db.pool, convo, me)
        .await?
        .into_iter()
        .map(|m| {
            // Status is only meaningful for my own (outgoing) messages, derived
            // from how far the peer has read/received.
            let status = if m.sender_account_id == me {
                if peer_read >= m.seq {
                    "read"
                } else if peer_delivered >= m.seq {
                    "delivered"
                } else {
                    "sent"
                }
            } else {
                "sent"
            };
            json!({
                "messageId": m.id,
                "seq": m.seq,
                "senderAccountId": m.sender_account_id,
                "senderDeviceId": m.sender_device_id,
                "ciphertext": STANDARD.encode(&m.ciphertext),
                "serverTs": rfc3339(m.server_ts),
                "status": status,
                "replyTo": m.reply_to,
                "editedAt": m.edited_at.map(rfc3339),
            })
        })
        .collect();
    Ok(Json(json!({
        "conversationId": convo,
        "withUserId": target.id,
        "messages": msgs,
    })))
}

/// Fan out an envelope to every member of a conversation (skipping the actor's
/// own sending device).
async fn deliver_to_conversation(
    state: &AppState,
    cid: Uuid,
    env: &Envelope,
    actor: Uuid,
    actor_device: Uuid,
) {
    if let Ok(members) = db::conversation_member_ids(&state.db.pool, cid).await {
        for m in members {
            let except = if m == actor { Some(actor_device) } else { None };
            deliver_to_account(state, m, env, except).await;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditReq {
    ciphertext: String,
}

/// PATCH /v0/messages/{id} — edit your own message (replaces its ciphertext).
pub async fn edit_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<EditReq>,
) -> Result<Json<Value>, AppError> {
    let (me, my_device) = authed(&state, &headers).await?;
    let msg_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("bad message id".into()))?;
    let bytes = STANDARD
        .decode(req.ciphertext.as_bytes())
        .map_err(|_| AppError::BadRequest("ciphertext must be base64".into()))?;

    let Some((cid, _seq, edited_at)) = db::edit_message(&state.db.pool, msg_id, me, &bytes).await?
    else {
        return Err(AppError::NotFound("no such message you can edit".into()));
    };

    let env = envelope(
        "message.edited",
        json!({
            "conversationId": cid,
            "messageId": msg_id,
            "ciphertext": STANDARD.encode(&bytes),
            "editedAt": rfc3339(edited_at),
        }),
    );
    deliver_to_conversation(&state, cid, &env, me, my_device).await;

    Ok(Json(json!({ "messageId": msg_id, "editedAt": rfc3339(edited_at) })))
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    /// "me" hides it from just my history; anything else deletes for everyone.
    scope: Option<String>,
}

/// DELETE /v0/messages/{id}?scope={me|everyone} — delete for me (hide) or, if I
/// own it, for everyone.
pub async fn delete_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<Value>, AppError> {
    let (me, my_device) = authed(&state, &headers).await?;
    let msg_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("bad message id".into()))?;

    // Delete for me: hide from my history only, no broadcast.
    if q.scope.as_deref() == Some("me") {
        db::hide_message_for(&state.db.pool, msg_id, me).await?;
        return Ok(Json(json!({ "messageId": msg_id, "hidden": true })));
    }

    // Delete for everyone (only my own messages).
    let Some((cid, _seq)) = db::delete_message(&state.db.pool, msg_id, me).await? else {
        return Err(AppError::NotFound("no such message you can delete".into()));
    };

    let env = envelope(
        "message.deleted",
        json!({ "conversationId": cid, "messageId": msg_id }),
    );
    deliver_to_conversation(&state, cid, &env, me, my_device).await;

    Ok(Json(json!({ "messageId": msg_id, "deleted": true })))
}
