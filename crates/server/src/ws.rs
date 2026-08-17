//! WebSocket endpoint: authenticated per device via a `?token=` query param
//! (WS handshakes can't carry an Authorization header). Forwards bus events to
//! the socket, relays inbound typing/receipts/reactions, and maintains presence.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed_from_token;
use crate::db;
use crate::messaging::{broadcast_presence, deliver_to_account, envelope};

#[derive(Deserialize)]
pub struct WsQuery {
    token: String,
}

pub async fn ws_handler(
    State(state): State<AppState>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    match authed_from_token(&state, &q.token).await {
        Ok((account, device)) => {
            ws.on_upgrade(move |socket| handle_socket(state, socket, account, device))
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_socket(state: AppState, mut socket: WebSocket, account: Uuid, device: Uuid) {
    if state.presence.on_connect(device, account) {
        broadcast_presence(&state, account, true, None).await;
    }
    let mut rx = state.bus.subscribe(device);

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(env) => {
                        if let Ok(text) = serde_json::to_string(&env) {
                            if socket.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => continue, // lagged; keep going
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => handle_inbound(&state, account, t.as_str()).await,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    if let Some((acc, went_offline)) = state.presence.on_disconnect(device) {
        if went_offline {
            let now = OffsetDateTime::now_utc();
            let _ = db::set_last_seen(&state.db.pool, acc, now).await;
            broadcast_presence(&state, acc, false, Some(now)).await;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inbound {
    #[serde(rename = "type")]
    type_: String,
    to: Option<String>,
    message_id: Option<String>,
    emoji: Option<String>,
    up_to_seq: Option<i64>,
}

/// Relay typing, delivery/read receipts, and reactions to the target peer.
async fn handle_inbound(state: &AppState, account: Uuid, text: &str) {
    let Ok(msg) = serde_json::from_str::<Inbound>(text) else {
        return;
    };
    let Some(to) = msg.to.as_deref() else {
        return;
    };
    let Ok(Some(target)) = state.auth.find_account_by_username(to).await else {
        return;
    };
    let Ok(convo) = db::dm_conversation(&state.db.pool, account, target.id).await else {
        return;
    };

    // Persist delivery/read progress so status survives history reloads.
    match msg.type_.as_str() {
        "receipt.delivered" => {
            let _ = db::mark_delivered(&state.db.pool, convo, account).await;
        }
        "receipt.read" => {
            let _ = db::mark_delivered(&state.db.pool, convo, account).await;
            let _ = db::mark_read(&state.db.pool, convo, account).await;
        }
        _ => {}
    }

    let payload = match msg.type_.as_str() {
        "typing.start" | "typing.stop" | "receipt.delivered" => {
            json!({ "conversationId": convo, "byUserId": account })
        }
        "receipt.read" => {
            json!({ "conversationId": convo, "byUserId": account, "upToSeq": msg.up_to_seq })
        }
        "reaction" => json!({
            "conversationId": convo,
            "byUserId": account,
            "messageId": msg.message_id,
            "emoji": msg.emoji,
        }),
        _ => return,
    };

    let env = envelope(&msg.type_, payload);
    deliver_to_account(state, target.id, &env, None).await;
}
