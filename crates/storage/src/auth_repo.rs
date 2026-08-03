//! Repository for accounts, devices, and sessions.
//!
//! Uses runtime (not compile-time-checked) queries so the crate builds without a live database.
//! Once a dev database is standard in CI we can switch hot paths to the `query!` macro with an
//! offline `.sqlx` cache for compile-time verification.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::models::{Account, Device, Session};
use crate::StorageError;

/// Insert a new account. Fails with a unique violation if the username is taken.
pub async fn create_account(
    pool: &PgPool,
    id: Uuid,
    username: &str,
    password_verifier: &str,
) -> Result<Account, StorageError> {
    let account = sqlx::query_as::<_, Account>(
        "INSERT INTO accounts (id, username, password_verifier)
         VALUES ($1, $2, $3)
         RETURNING id, username, password_verifier, created_at",
    )
    .bind(id)
    .bind(username)
    .bind(password_verifier)
    .fetch_one(pool)
    .await?;
    Ok(account)
}

pub async fn find_account_by_username(
    pool: &PgPool,
    username: &str,
) -> Result<Option<Account>, StorageError> {
    let account = sqlx::query_as::<_, Account>(
        "SELECT id, username, password_verifier, created_at
         FROM accounts WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(account)
}

/// Register a device for an account.
pub async fn create_device(
    pool: &PgPool,
    id: Uuid,
    account_id: Uuid,
    display_name: &str,
    public_identity: &[u8],
) -> Result<Device, StorageError> {
    let device = sqlx::query_as::<_, Device>(
        "INSERT INTO devices (id, account_id, display_name, public_identity)
         VALUES ($1, $2, $3, $4)
         RETURNING id, account_id, display_name, public_identity, created_at, revoked_at",
    )
    .bind(id)
    .bind(account_id)
    .bind(display_name)
    .bind(public_identity)
    .fetch_one(pool)
    .await?;
    Ok(device)
}

/// Create a session (one per device) holding hashed tokens.
#[allow(clippy::too_many_arguments)]
pub async fn create_session(
    pool: &PgPool,
    id: Uuid,
    device_id: Uuid,
    access_token_hash: &[u8],
    refresh_token_hash: &[u8],
    expires_at: OffsetDateTime,
) -> Result<Session, StorageError> {
    let session = sqlx::query_as::<_, Session>(
        "INSERT INTO sessions (id, device_id, access_token_hash, refresh_token_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, device_id, access_token_hash, refresh_token_hash, expires_at, revoked_at, created_at",
    )
    .bind(id)
    .bind(device_id)
    .bind(access_token_hash)
    .bind(refresh_token_hash)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;
    Ok(session)
}

/// Look up a live (non-revoked) session by its hashed access token.
pub async fn find_live_session_by_access_hash(
    pool: &PgPool,
    access_token_hash: &[u8],
) -> Result<Option<Session>, StorageError> {
    let session = sqlx::query_as::<_, Session>(
        "SELECT id, device_id, access_token_hash, refresh_token_hash, expires_at, revoked_at, created_at
         FROM sessions
         WHERE access_token_hash = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(access_token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(session)
}

/// Look up a live session by its hashed refresh token (for rotation).
pub async fn find_live_session_by_refresh_hash(
    pool: &PgPool,
    refresh_token_hash: &[u8],
) -> Result<Option<Session>, StorageError> {
    let session = sqlx::query_as::<_, Session>(
        "SELECT id, device_id, access_token_hash, refresh_token_hash, expires_at, revoked_at, created_at
         FROM sessions
         WHERE refresh_token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(refresh_token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(session)
}

/// Rotate a session's tokens (single-use refresh tokens → theft is detectable).
pub async fn rotate_session(
    pool: &PgPool,
    session_id: Uuid,
    new_access_hash: &[u8],
    new_refresh_hash: &[u8],
    expires_at: OffsetDateTime,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE sessions
         SET access_token_hash = $2, refresh_token_hash = $3, expires_at = $4
         WHERE id = $1",
    )
    .bind(session_id)
    .bind(new_access_hash)
    .bind(new_refresh_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// All accounts (for the user directory).
pub async fn list_accounts(pool: &PgPool) -> Result<Vec<Account>, StorageError> {
    let rows = sqlx::query_as::<_, Account>(
        "SELECT id, username, password_verifier, created_at FROM accounts ORDER BY username",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Look up a device (to resolve its account for message routing).
pub async fn find_device_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Device>, StorageError> {
    let device = sqlx::query_as::<_, Device>(
        "SELECT id, account_id, display_name, public_identity, created_at, revoked_at
         FROM devices WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(device)
}

/// All non-revoked devices for an account (fan-out targets).
pub async fn list_devices_for_account(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<Device>, StorageError> {
    let rows = sqlx::query_as::<_, Device>(
        "SELECT id, account_id, display_name, public_identity, created_at, revoked_at
         FROM devices WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Revoke a session (logout / device removal).
pub async fn revoke_session(pool: &PgPool, session_id: Uuid) -> Result<(), StorageError> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}
