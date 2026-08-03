//! Auth + multi-device sessions: register, login, refresh, logout.
//!
//! Passwords are stored only as Argon2id verifiers. Tokens are opaque 256-bit random strings;
//! only their SHA-256 hashes are stored, so a database leak does not expose usable tokens.
//! Every login registers a device with its own session — the multi-device foundation.

use argon2::{
    password_hash::{rand_core::OsRng as ArgonOsRng, PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use axum::{extract::State, http::HeaderMap, http::StatusCode, Json};
use base64::{engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use trust_protocol_types::rest::{
    AuthSession, LoginRequest, RefreshRequest, RegisterRequest, TokenPair,
};
use uuid::Uuid;

use crate::app::AppState;
use crate::error::AppError;

const ACCESS_TTL: Duration = Duration::hours(1);

// ---- primitives ---------------------------------------------------------------------------

/// A fresh opaque token (256 bits of entropy, URL-safe, unpadded).
fn new_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Store only the hash of a token, never the token itself.
fn hash_token(token: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().to_vec()
}

fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut ArgonOsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(format!("password hashing: {e}")))
}

fn verify_password(verifier: &str, password: &str) -> bool {
    match PasswordHash::new(verifier) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Issue a token pair and persist the session for a device.
async fn issue_session(state: &AppState, device_id: Uuid) -> Result<TokenPair, AppError> {
    let access = new_token("tka_");
    let refresh = new_token("tkr_");
    let expires_at = OffsetDateTime::now_utc() + ACCESS_TTL;
    state.auth.create_session(
        Uuid::now_v7(),
        device_id,
        &hash_token(&access),
        &hash_token(&refresh),
        expires_at,
    )
    .await?;
    Ok(TokenPair {
        access_token: access,
        refresh_token: refresh,
        expires_in: ACCESS_TTL.whole_seconds(),
    })
}

fn decode_identity(b64: &str) -> Result<Vec<u8>, AppError> {
    STANDARD
        .decode(b64)
        .map_err(|_| AppError::BadRequest("publicIdentity must be valid base64".into()))
}

/// Extract and validate the bearer token, returning the live session's device id.
pub async fn authed_device(state: &AppState, headers: &HeaderMap) -> Result<Uuid, AppError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;
    let session = state.auth.find_live_session_by_access_hash(&hash_token(token))
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid or expired token".into()))?;
    Ok(session.device_id)
}

/// Authenticated (account_id, device_id) from the bearer token.
pub async fn authed(state: &AppState, headers: &HeaderMap) -> Result<(Uuid, Uuid), AppError> {
    let device_id = authed_device(state, headers).await?;
    let account = state
        .auth
        .find_device_by_id(device_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("device not found".into()))?
        .account_id;
    Ok((account, device_id))
}

/// Authenticated (account_id, device_id) from a raw access token (WebSocket
/// query-param auth, since WS handshakes can't carry an Authorization header).
pub async fn authed_from_token(state: &AppState, token: &str) -> Result<(Uuid, Uuid), AppError> {
    let session = state
        .auth
        .find_live_session_by_access_hash(&hash_token(token))
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid token".into()))?;
    let device_id = session.device_id;
    let account = state
        .auth
        .find_device_by_id(device_id)
        .await?
        .ok_or_else(|| AppError::Unauthorized("device not found".into()))?
        .account_id;
    Ok((account, device_id))
}

// ---- handlers -----------------------------------------------------------------------------

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<AuthSession>), AppError> {
    if req.username.trim().is_empty() || req.password.len() < 8 {
        return Err(AppError::BadRequest(
            "username required and password must be at least 8 characters".into(),
        ));
    }
    let identity = decode_identity(&req.device.public_identity)?;
    let verifier = hash_password(&req.password)?;

    let account_id = Uuid::now_v7();
    state.auth.create_account(account_id, &req.username, &verifier)
        .await
        .map_err(|e| match AppError::from(e) {
            AppError::Conflict(_) => AppError::Conflict("username is taken".into()),
            other => other,
        })?;

    let device_id = Uuid::now_v7();
    state.auth.create_device(
        device_id,
        account_id,
        &req.device.display_name,
        &identity,
    )
    .await?;

    let tokens = issue_session(&state, device_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(AuthSession {
            account_id,
            device_id,
            tokens,
        }),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthSession>, AppError> {
    let identity = decode_identity(&req.device.public_identity)?;

    let account = state.auth.find_account_by_username(&req.username)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid credentials".into()))?;
    if !verify_password(&account.password_verifier, &req.password) {
        return Err(AppError::Unauthorized("invalid credentials".into()));
    }

    // Each install is its own device/session (the multi-device model).
    let device_id = Uuid::now_v7();
    state.auth.create_device(
        device_id,
        account.id,
        &req.device.display_name,
        &identity,
    )
    .await?;

    let tokens = issue_session(&state, device_id).await?;
    Ok(Json(AuthSession {
        account_id: account.id,
        device_id,
        tokens,
    }))
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<TokenPair>, AppError> {
    let session =
        state.auth.find_live_session_by_refresh_hash(&hash_token(&req.refresh_token))
            .await?
            .ok_or_else(|| AppError::Unauthorized("invalid refresh token".into()))?;

    // Rotate: single-use refresh tokens make token theft detectable.
    let access = new_token("tka_");
    let refresh = new_token("tkr_");
    let expires_at = OffsetDateTime::now_utc() + ACCESS_TTL;
    state.auth.rotate_session(
        session.id,
        &hash_token(&access),
        &hash_token(&refresh),
        expires_at,
    )
    .await?;

    Ok(Json(TokenPair {
        access_token: access,
        refresh_token: refresh,
        expires_in: ACCESS_TTL.whole_seconds(),
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;
    if let Some(session) =
        state.auth.find_live_session_by_access_hash(&hash_token(token)).await?
    {
        state.auth.revoke_session(session.id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_prefixed_and_hash_stably() {
        let a = new_token("tka_");
        let b = new_token("tka_");
        assert!(a.starts_with("tka_") && b.starts_with("tka_"));
        assert_ne!(a, b, "tokens must be random");
        assert_eq!(hash_token(&a), hash_token(&a), "hash is deterministic");
        assert_ne!(hash_token(&a), hash_token(&b));
        assert_eq!(hash_token(&a).len(), 32, "sha-256 is 32 bytes");
    }

    #[test]
    fn password_round_trips_and_rejects_wrong() {
        let verifier = hash_password("correct horse battery").unwrap();
        assert!(verify_password(&verifier, "correct horse battery"));
        assert!(!verify_password(&verifier, "wrong password"));
        assert!(!verify_password("not-a-phc-string", "anything"));
    }
}
