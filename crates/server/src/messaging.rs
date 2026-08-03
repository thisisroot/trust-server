//! User directory + 1:1 messaging (send / history) and the delivery helpers.
//! Message bodies are opaque base64 (dev sends base64 plaintext; MLS ciphertext
//! later). Delivery fans out over the realtime bus to connected devices.

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
    for device in state.hub.connected_devices() {
        state.bus.publish(device, env.clone());
    }
}

/// GET /v0/users — everyone else on this server, with presence.
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let accounts = state.auth.list_accounts().await?;
    let users: Vec<Value> = accounts
        .into_iter()
        .filter(|a| a.id != me)
        .map(|a| {
            json!({
                "userId": a.id,
                "username": a.username,
                "online": state.hub.is_online(a.id),
                "lastSeen": state.hub.last_seen(a.id).map(rfc3339),
            })
        })
        .collect();
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReq {
    to_username: String,
    ciphertext: String,
    /// Optional media descriptor; when present, indexed for the shared-media view.
    attachment: Option<crate::media::AttachmentMeta>,
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

    let convo = state.hub.conversation_id(me, target.id);
    // First message in a fresh (non-self) conversation seeds backdated history (dev).
    if target.id != me && !state.hub.has_messages(convo) {
        state.hub.seed_backdated(convo, me, target.id);
    }
    let msg = state.hub.store_message(convo, me, my_device, bytes.clone());

    // Index any attachment for the conversation's shared-media view.
    if let Some(att) = req.attachment {
        let parse = |s: &Option<String>| s.as_deref().and_then(|v| Uuid::parse_str(v).ok());
        state.media.record(crate::media::MediaEntry {
            id: Uuid::new_v4(),
            conversation_id: convo,
            message_id: msg.id,
            sender_account: me,
            seq: msg.seq,
            kind: att.kind,
            blob_id: parse(&att.blob_id),
            thumb_blob_id: parse(&att.thumb_blob_id),
            mime: att.mime,
            name: att.name,
            url: att.url,
            size: att.size,
            width: att.width,
            height: att.height,
            ts: msg.ts,
        });
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
            "serverTs": rfc3339(msg.ts),
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
        "serverTs": rfc3339(msg.ts),
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
    let accounts = state.auth.list_accounts().await?;
    let mut items: Vec<(Value, i64)> = Vec::new();
    for (other, cid) in state.hub.conversations_for(me) {
        let username = accounts
            .iter()
            .find(|a| a.id == other)
            .map(|a| a.username.clone())
            .unwrap_or_default();
        let last = state.hub.last_message_ts(cid);
        let prof = state.profiles.get(other);
        items.push((
            json!({
                "userId": other,
                "username": username,
                "displayName": prof.display_name,
                "avatarBlobId": prof.avatar_blob_id,
                "online": state.hub.is_online(other),
                "lastSeen": state.hub.last_seen(other).map(rfc3339),
                "conversationId": cid,
            }),
            last.map(|t| t.unix_timestamp()).unwrap_or(0),
        ));
    }
    items.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(Json(json!(items.into_iter().map(|(v, _)| v).collect::<Vec<_>>())))
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
    let a = state
        .auth
        .find_account_by_username(&q.username)
        .await?
        .ok_or_else(|| AppError::NotFound("no user with that username".into()))?;
    let prof = state.profiles.get(a.id);
    Ok(Json(json!({
        "userId": a.id,
        "username": a.username,
        "displayName": prof.display_name,
        "avatarBlobId": prof.avatar_blob_id,
        "online": state.hub.is_online(a.id),
        "lastSeen": state.hub.last_seen(a.id).map(rfc3339),
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
    let convo = state.hub.conversation_id(me, target.id);
    let peer_read = state.hub.read_seq(convo, target.id);
    let peer_delivered = state.hub.delivered_seq(convo, target.id);
    let msgs: Vec<Value> = state
        .hub
        .history(convo)
        .into_iter()
        .map(|m| {
            // Status is only meaningful for my own (outgoing) messages, derived
            // from how far the peer has read/received.
            let status = if m.sender_account == me {
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
                "senderAccountId": m.sender_account,
                "senderDeviceId": m.sender_device,
                "ciphertext": STANDARD.encode(&m.ciphertext),
                "serverTs": rfc3339(m.ts),
                "status": status,
            })
        })
        .collect();
    Ok(Json(json!({
        "conversationId": convo,
        "withUserId": target.id,
        "messages": msgs,
    })))
}
