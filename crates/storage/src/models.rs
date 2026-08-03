//! Row types mirroring the schema in `migrations/`. Mapped by column name via `FromRow`.

use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub username: String,
    pub password_verifier: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Device {
    pub id: Uuid,
    pub account_id: Uuid,
    pub display_name: String,
    pub public_identity: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub device_id: Uuid,
    pub access_token_hash: Vec<u8>,
    pub refresh_token_hash: Vec<u8>,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}
