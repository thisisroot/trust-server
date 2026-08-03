//! User profiles (display name, bio, avatar). In-memory, keyed by account id.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::authed;
use crate::error::AppError;

#[derive(Clone, Default)]
pub struct ProfileData {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_blob_id: Option<Uuid>,
}

#[derive(Default)]
pub struct Profiles {
    map: Mutex<HashMap<Uuid, ProfileData>>,
}

impl Profiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: Uuid) -> ProfileData {
        self.map.lock().unwrap().get(&id).cloned().unwrap_or_default()
    }

    pub fn update(&self, id: Uuid, f: impl FnOnce(&mut ProfileData)) {
        let mut map = self.map.lock().unwrap();
        f(map.entry(id).or_default());
    }

    pub fn export(&self) -> Vec<(Uuid, ProfileData)> {
        self.map.lock().unwrap().iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    pub fn import(&self, entries: Vec<(Uuid, ProfileData)>) {
        let mut map = self.map.lock().unwrap();
        for (k, v) in entries {
            map.insert(k, v);
        }
    }
}

pub fn profile_json(state: &AppState, account_id: Uuid, username: String) -> Value {
    let p = state.profiles.get(account_id);
    json!({
        "userId": account_id,
        "username": username,
        "displayName": p.display_name,
        "bio": p.bio,
        "avatarBlobId": p.avatar_blob_id,
        "online": state.hub.is_online(account_id),
        "lastSeen": state.hub.last_seen(account_id).map(|t| t.format(&Rfc3339).unwrap_or_default()),
    })
}

async fn username_of(state: &AppState, id: Uuid) -> String {
    state
        .auth
        .list_accounts()
        .await
        .ok()
        .and_then(|v| v.into_iter().find(|a| a.id == id).map(|a| a.username))
        .unwrap_or_default()
}

/// GET /v0/profile/me
pub async fn get_my_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (me, _device) = authed(&state, &headers).await?;
    let username = username_of(&state, me).await;
    Ok(Json(profile_json(&state, me, username)))
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
    Ok(Json(profile_json(&state, account.id, account.username)))
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
    state.profiles.update(me, |p| {
        if let Some(d) = req.display_name {
            p.display_name = if d.trim().is_empty() { None } else { Some(d) };
        }
        if let Some(b) = req.bio {
            p.bio = if b.trim().is_empty() { None } else { Some(b) };
        }
        if let Some(a) = req.avatar_blob_id {
            p.avatar_blob_id = if a.is_empty() { None } else { Uuid::parse_str(&a).ok() };
        }
    });
    let username = username_of(&state, me).await;

    // Propagate the change in real time to everyone who has this profile in view:
    // the updater's conversation partners, plus the updater's own other devices.
    let p = state.profiles.get(me);
    let env = crate::messaging::envelope(
        "profile.update",
        json!({
            "userId": me,
            "username": username,
            "displayName": p.display_name,
            "bio": p.bio,
            "avatarBlobId": p.avatar_blob_id,
        }),
    );
    for (other, _cid) in state.hub.conversations_for(me) {
        crate::messaging::deliver_to_account(&state, other, &env, None).await;
    }
    crate::messaging::deliver_to_account(&state, me, &env, None).await;

    Ok(Json(profile_json(&state, me, username)))
}
