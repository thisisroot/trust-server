//! User profiles (display name, bio, avatar), Postgres-backed.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::db;
use crate::error::AppError;
use crate::messaging::{deliver_to_account, envelope};

pub async fn profile_json(state: &AppState, account_id: Uuid, username: String) -> Result<Value, AppError> {
    let p = db::get_profile(&state.db.pool, account_id).await?;
    Ok(json!({
        "userId": account_id,
        "username": username,
        "displayName": p.display_name,
        "bio": p.bio,
        "avatarBlobId": p.avatar_blob_id,
        "online": state.presence.is_online(account_id),
        "lastSeen": p.last_seen.map(|t| t.format(&Rfc3339).unwrap_or_default()),
    }))
}

/// GET /v0/profile/me
pub async fn get_my_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let username = db::username_of(&state.db.pool, me).await?;
    Ok(Json(profile_json(&state, me, username).await?))
}

#[derive(Deserialize)]
pub struct ProfileQuery {
    username: String,
}

/// GET /v0/profile?username=…
pub async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ProfileQuery>,
) -> Result<Json<Value>, AppError> {
    authed(&state, &headers).await?;
    let account = state
        .auth
        .find_account_by_username(&q.username)
        .await?
        .ok_or_else(|| AppError::NotFound("no such user".into()))?;
    Ok(Json(profile_json(&state, account.id, account.username).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProfile {
    display_name: Option<String>,
    bio: Option<String>,
    avatar_blob_id: Option<String>,
}

/// PUT /v0/profile
pub async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateProfile>,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let cur = db::get_profile(&state.db.pool, me).await?;

    // Absent field → unchanged; empty string → cleared.
    let display = match req.display_name {
        Some(d) => (!d.trim().is_empty()).then_some(d),
        None => cur.display_name,
    };
    let bio = match req.bio {
        Some(b) => (!b.trim().is_empty()).then_some(b),
        None => cur.bio,
    };
    let avatar = match req.avatar_blob_id {
        Some(a) => if a.is_empty() { None } else { Uuid::parse_str(&a).ok() },
        None => cur.avatar_blob_id,
    };

    db::upsert_profile(&state.db.pool, me, display, bio, avatar).await?;
    let username = db::username_of(&state.db.pool, me).await?;

    // Propagate the change in real time to conversation partners + my own devices.
    let p = db::get_profile(&state.db.pool, me).await?;
    let env = envelope(
        "profile.update",
        json!({
            "userId": me,
            "username": username,
            "displayName": p.display_name,
            "bio": p.bio,
            "avatarBlobId": p.avatar_blob_id,
        }),
    );
    if let Ok(convos) = db::conversations_for(&state.db.pool, me).await {
        for r in convos {
            deliver_to_account(&state, r.other, &env, None).await;
        }
    }
    deliver_to_account(&state, me, &env, None).await;

    Ok(Json(profile_json(&state, me, username).await?))
}
