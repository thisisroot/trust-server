//! REST request/response types, matching `trust-protocol/schemas/rest/openapi.yaml`.
//!
//! Field names use camelCase on the wire (`#[serde(rename_all = "camelCase")]`) to match the
//! schema. Opaque key material (`publicIdentity`) and ciphertext are base64 strings here; the
//! server decodes them to bytes at the edge.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A device presented at registration/login.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRegistration {
    pub display_name: String,
    /// Base64 public MLS credential / identity signature key.
    pub public_identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub device: DeviceRegistration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub device: DeviceRegistration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    /// Access-token lifetime in seconds.
    pub expires_in: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSession {
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub tokens: TokenPair,
}

/// Uniform error body (matches the `Error` schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}
