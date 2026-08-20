//! Router construction and shared application state.

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use trust_protocol_types::{Capability, PROTOCOL_VERSION};
use trust_realtime::{InProcessBus, RealtimeBus};
use trust_storage::auth_store::AuthStore;
use trust_storage::Db;

/// Everything handlers need, cheaply cloneable (all fields are handles).
///
/// Durable state lives in Postgres (`db`); the only in-memory piece is live
/// presence/connection tracking, which is runtime state, not data.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// Accounts / devices / sessions store.
    pub auth: Arc<dyn AuthStore>,
    /// Real-time delivery fan-out to connected devices.
    pub bus: Arc<dyn RealtimeBus>,
    /// Live presence / connected devices (in memory; last_seen persists to DB).
    pub presence: Arc<crate::presence::Presence>,
    /// Capabilities this server advertises in the handshake.
    pub capabilities: Arc<Vec<Capability>>,
}

impl AppState {
    pub fn new(db: Db, auth: Arc<dyn AuthStore>) -> Self {
        Self {
            db,
            auth,
            bus: Arc::new(InProcessBus::default()),
            presence: Arc::new(crate::presence::Presence::new()),
            capabilities: Arc::new(vec![
                Capability::Messaging,
                Capability::Receipts,
                Capability::Typing,
                Capability::Presence,
            ]),
        }
    }
}

/// Build the full HTTP/WebSocket router.
pub fn router(state: AppState) -> Router {
    // `/health` is unversioned and dependency-light so liveness checks work even before the DB
    // is reachable. Everything protocol-specific lives under `/v0`, behind the version guard.
    let versioned = Router::new()
        .route("/capabilities", get(capabilities))
        .route("/auth/register", post(crate::auth::register))
        .route("/auth/login", post(crate::auth::login))
        .route("/auth/refresh", post(crate::auth::refresh))
        .route("/auth/logout", post(crate::auth::logout))
        .route("/users", get(crate::messaging::list_users))
        .route("/users/lookup", get(crate::messaging::lookup_user))
        .route("/conversations", get(crate::messaging::list_conversations))
        .route(
            "/messages",
            post(crate::messaging::send_message).get(crate::messaging::history),
        )
        .route(
            "/messages/{id}",
            patch(crate::messaging::edit_message).delete(crate::messaging::delete_message),
        )
        .route("/blobs", post(crate::blobs::upload_blob))
        .route("/conversations/{id}/media", get(crate::media::list_media))
        .route("/profile/me", get(crate::profile::get_my_profile))
        .route(
            "/profile",
            get(crate::profile::get_profile).put(crate::profile::update_profile),
        )
        .layer(middleware::from_fn(require_protocol_version));

    Router::new()
        .route("/health", get(health))
        // WebSocket is authed via ?token= and sits outside the version header guard.
        .route("/v0/ws", get(crate::ws::ws_handler))
        // Blob downloads are open so the client can load them by URL.
        .route("/v0/blobs/{id}", get(crate::blobs::get_blob))
        .nest("/v0", versioned)
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(CorsLayer::very_permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness + basic readiness. Reports DB reachability but never fails on it.
async fn health(State(state): State<AppState>) -> Response {
    let db_ok = state.db.ping().await.is_ok();
    Json(json!({
        "status": "ok",
        "protocolVersion": PROTOCOL_VERSION,
        "db": if db_ok { "up" } else { "down" },
    }))
    .into_response()
}

/// Advertised server capabilities (the REST mirror of the WebSocket `welcome`).
async fn capabilities(State(state): State<AppState>) -> Response {
    let caps = serde_json::to_value(state.capabilities.as_ref()).unwrap_or_default();
    Json(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": caps,
    }))
    .into_response()
}

/// Reject requests that don't declare a compatible protocol version. Applied to all `/v0`
/// routes so version negotiation is enforced for real, not stubbed.
async fn require_protocol_version(req: Request, next: Next) -> Response {
    let declared = req
        .headers()
        .get("trust-protocol-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    match declared {
        Some(v) if v == PROTOCOL_VERSION => next.run(req).await,
        _ => (
            StatusCode::UPGRADE_REQUIRED,
            Json(json!({
                "code": "UNSUPPORTED_VERSION",
                "message": format!(
                    "This server speaks Trust protocol v{PROTOCOL_VERSION}. Send header \
                     'Trust-Protocol-Version: {PROTOCOL_VERSION}'."
                ),
            })),
        )
            .into_response(),
    }
}
