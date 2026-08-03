//! Dev persistence: snapshot the in-memory stores to disk so accounts, profiles,
//! chats, media, and blobs survive a restart. This is a local-dev convenience —
//! production durability is Postgres + object storage. Everything except blob
//! bytes lives in one JSON snapshot; blobs are written as individual files and
//! only ever written once (they're immutable).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use trust_storage::auth_store::MemoryAuthStore;
use trust_storage::models::{Account, Device, Session};

use crate::app::AppState;
use crate::hub::{HubSnapshot, StoredMessage};
use crate::media::MediaEntry;
use crate::profile::ProfileData;

// ── small conversions ───────────────────────────────────────────────────────
fn secs(t: OffsetDateTime) -> i64 {
    t.unix_timestamp()
}
fn at(s: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(s).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}
fn pid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap_or(Uuid::nil())
}
fn oid(u: Option<Uuid>) -> Option<String> {
    u.map(|x| x.to_string())
}
fn poid(s: &Option<String>) -> Option<Uuid> {
    s.as_deref().and_then(|v| Uuid::parse_str(v).ok())
}

// ── DTOs (String uuids, unix-second timestamps → stable, human-diffable) ──────
#[derive(Serialize, Deserialize)]
struct StateFile {
    accounts: Vec<AccountDto>,
    devices: Vec<DeviceDto>,
    sessions: Vec<SessionDto>,
    profiles: Vec<ProfileDto>,
    convos: Vec<ConvoDto>,
    seqs: Vec<SeqDto>,
    messages: Vec<MessageDto>,
    read_markers: Vec<MarkerDto>,
    delivered_markers: Vec<MarkerDto>,
    media: Vec<MediaDto>,
    blobs: Vec<BlobMetaDto>,
}

#[derive(Serialize, Deserialize)]
struct AccountDto {
    id: String,
    username: String,
    password_verifier: String,
    created_at: i64,
}

#[derive(Serialize, Deserialize)]
struct DeviceDto {
    id: String,
    account_id: String,
    display_name: String,
    public_identity: Vec<u8>,
    created_at: i64,
    revoked_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct SessionDto {
    id: String,
    device_id: String,
    access_token_hash: Vec<u8>,
    refresh_token_hash: Vec<u8>,
    expires_at: i64,
    revoked_at: Option<i64>,
    created_at: i64,
}

#[derive(Serialize, Deserialize)]
struct ProfileDto {
    user: String,
    display_name: Option<String>,
    bio: Option<String>,
    avatar_blob_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ConvoDto {
    a: String,
    b: String,
    cid: String,
}

#[derive(Serialize, Deserialize)]
struct SeqDto {
    cid: String,
    seq: i64,
}

#[derive(Serialize, Deserialize)]
struct MarkerDto {
    cid: String,
    account: String,
    seq: i64,
}

#[derive(Serialize, Deserialize)]
struct MessageDto {
    id: String,
    conversation_id: String,
    seq: i64,
    sender_account: String,
    sender_device: String,
    ciphertext: Vec<u8>,
    ts: i64,
}

#[derive(Serialize, Deserialize)]
struct MediaDto {
    id: String,
    conversation_id: String,
    message_id: String,
    sender_account: String,
    seq: i64,
    kind: String,
    blob_id: Option<String>,
    thumb_blob_id: Option<String>,
    mime: Option<String>,
    name: Option<String>,
    url: Option<String>,
    size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    ts: i64,
}

#[derive(Serialize, Deserialize)]
struct BlobMetaDto {
    id: String,
    content_type: String,
}

// ── snapshot ↔ live state ────────────────────────────────────────────────────
fn snapshot(mem: &MemoryAuthStore, state: &AppState) -> StateFile {
    let (accounts, devices, sessions) = mem.export();
    let hub = state.hub.export();
    StateFile {
        accounts: accounts
            .iter()
            .map(|a| AccountDto {
                id: a.id.to_string(),
                username: a.username.clone(),
                password_verifier: a.password_verifier.clone(),
                created_at: secs(a.created_at),
            })
            .collect(),
        devices: devices
            .iter()
            .map(|d| DeviceDto {
                id: d.id.to_string(),
                account_id: d.account_id.to_string(),
                display_name: d.display_name.clone(),
                public_identity: d.public_identity.clone(),
                created_at: secs(d.created_at),
                revoked_at: d.revoked_at.map(secs),
            })
            .collect(),
        sessions: sessions
            .iter()
            .map(|s| SessionDto {
                id: s.id.to_string(),
                device_id: s.device_id.to_string(),
                access_token_hash: s.access_token_hash.clone(),
                refresh_token_hash: s.refresh_token_hash.clone(),
                expires_at: secs(s.expires_at),
                revoked_at: s.revoked_at.map(secs),
                created_at: secs(s.created_at),
            })
            .collect(),
        profiles: state
            .profiles
            .export()
            .into_iter()
            .map(|(id, p)| ProfileDto {
                user: id.to_string(),
                display_name: p.display_name,
                bio: p.bio,
                avatar_blob_id: oid(p.avatar_blob_id),
            })
            .collect(),
        convos: hub
            .convos
            .iter()
            .map(|((a, b), c)| ConvoDto {
                a: a.to_string(),
                b: b.to_string(),
                cid: c.to_string(),
            })
            .collect(),
        seqs: hub
            .seqs
            .iter()
            .map(|(c, s)| SeqDto { cid: c.to_string(), seq: *s })
            .collect(),
        messages: hub
            .messages
            .iter()
            .map(|m| MessageDto {
                id: m.id.to_string(),
                conversation_id: m.conversation_id.to_string(),
                seq: m.seq,
                sender_account: m.sender_account.to_string(),
                sender_device: m.sender_device.to_string(),
                ciphertext: m.ciphertext.clone(),
                ts: secs(m.ts),
            })
            .collect(),
        read_markers: hub
            .read_markers
            .iter()
            .map(|((c, a), s)| MarkerDto {
                cid: c.to_string(),
                account: a.to_string(),
                seq: *s,
            })
            .collect(),
        delivered_markers: hub
            .delivered_markers
            .iter()
            .map(|((c, a), s)| MarkerDto {
                cid: c.to_string(),
                account: a.to_string(),
                seq: *s,
            })
            .collect(),
        media: state
            .media
            .export()
            .iter()
            .map(|e| MediaDto {
                id: e.id.to_string(),
                conversation_id: e.conversation_id.to_string(),
                message_id: e.message_id.to_string(),
                sender_account: e.sender_account.to_string(),
                seq: e.seq,
                kind: e.kind.clone(),
                blob_id: oid(e.blob_id),
                thumb_blob_id: oid(e.thumb_blob_id),
                mime: e.mime.clone(),
                name: e.name.clone(),
                url: e.url.clone(),
                size: e.size,
                width: e.width,
                height: e.height,
                ts: secs(e.ts),
            })
            .collect(),
        blobs: state
            .blobs
            .manifest()
            .into_iter()
            .map(|(id, content_type)| BlobMetaDto { id: id.to_string(), content_type })
            .collect(),
    }
}

/// Restore persisted dev state (if any) into the live stores. Best-effort: a
/// missing or unparseable file just starts fresh.
pub fn load(dir: &str, mem: &MemoryAuthStore, state: &AppState) {
    let root = PathBuf::from(dir);
    let path = root.join("state.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let file: StateFile = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "could not parse persisted state; starting fresh");
            return;
        }
    };

    mem.import(
        file.accounts
            .into_iter()
            .map(|d| Account {
                id: pid(&d.id),
                username: d.username,
                password_verifier: d.password_verifier,
                created_at: at(d.created_at),
            })
            .collect(),
        file.devices
            .into_iter()
            .map(|d| Device {
                id: pid(&d.id),
                account_id: pid(&d.account_id),
                display_name: d.display_name,
                public_identity: d.public_identity,
                created_at: at(d.created_at),
                revoked_at: d.revoked_at.map(at),
            })
            .collect(),
        file.sessions
            .into_iter()
            .map(|d| Session {
                id: pid(&d.id),
                device_id: pid(&d.device_id),
                access_token_hash: d.access_token_hash,
                refresh_token_hash: d.refresh_token_hash,
                expires_at: at(d.expires_at),
                revoked_at: d.revoked_at.map(at),
                created_at: at(d.created_at),
            })
            .collect(),
    );

    state.profiles.import(
        file.profiles
            .into_iter()
            .map(|p| {
                (
                    pid(&p.user),
                    ProfileData {
                        display_name: p.display_name,
                        bio: p.bio,
                        avatar_blob_id: poid(&p.avatar_blob_id),
                    },
                )
            })
            .collect(),
    );

    state.hub.import(HubSnapshot {
        convos: file
            .convos
            .into_iter()
            .map(|c| ((pid(&c.a), pid(&c.b)), pid(&c.cid)))
            .collect(),
        seqs: file.seqs.into_iter().map(|s| (pid(&s.cid), s.seq)).collect(),
        messages: file
            .messages
            .into_iter()
            .map(|m| StoredMessage {
                id: pid(&m.id),
                conversation_id: pid(&m.conversation_id),
                seq: m.seq,
                sender_account: pid(&m.sender_account),
                sender_device: pid(&m.sender_device),
                ciphertext: m.ciphertext,
                ts: at(m.ts),
            })
            .collect(),
        read_markers: file
            .read_markers
            .into_iter()
            .map(|m| ((pid(&m.cid), pid(&m.account)), m.seq))
            .collect(),
        delivered_markers: file
            .delivered_markers
            .into_iter()
            .map(|m| ((pid(&m.cid), pid(&m.account)), m.seq))
            .collect(),
    });

    state.media.import(
        file.media
            .into_iter()
            .map(|m| MediaEntry {
                id: pid(&m.id),
                conversation_id: pid(&m.conversation_id),
                message_id: pid(&m.message_id),
                sender_account: pid(&m.sender_account),
                seq: m.seq,
                kind: m.kind,
                blob_id: poid(&m.blob_id),
                thumb_blob_id: poid(&m.thumb_blob_id),
                mime: m.mime,
                name: m.name,
                url: m.url,
                size: m.size,
                width: m.width,
                height: m.height,
                ts: at(m.ts),
            })
            .collect(),
    );

    let blob_dir = root.join("blobs");
    for b in file.blobs {
        let id = pid(&b.id);
        if let Ok(bytes) = std::fs::read(blob_dir.join(format!("{id}.bin"))) {
            state.blobs.insert(id, b.content_type, bytes);
        }
    }

    tracing::info!(path = %path.display(), "restored persisted dev state");
}

struct SaveCtx {
    dir: PathBuf,
    last: String,
    blobs_written: HashSet<Uuid>,
}

fn write_once(ctx: &mut SaveCtx, mem: &MemoryAuthStore, state: &AppState) {
    // New (immutable) blobs → their own files, written exactly once.
    let blob_dir = ctx.dir.join("blobs");
    let _ = std::fs::create_dir_all(&blob_dir);
    for (id, _ct) in state.blobs.manifest() {
        if ctx.blobs_written.contains(&id) {
            continue;
        }
        if let Some(bytes) = state.blobs.bytes_of(id) {
            if std::fs::write(blob_dir.join(format!("{id}.bin")), &bytes).is_ok() {
                ctx.blobs_written.insert(id);
            }
        }
    }

    // The JSON snapshot, only when it actually changed (atomic replace).
    if let Ok(json) = serde_json::to_string(&snapshot(mem, state)) {
        if json != ctx.last {
            let path = ctx.dir.join("state.json");
            let tmp = ctx.dir.join("state.json.tmp");
            if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                ctx.last = json;
            }
        }
    }
}

/// Periodically flush the dev state to disk (every few seconds, only on change).
pub fn spawn_autosave(dir: String, mem: Arc<MemoryAuthStore>, state: AppState) {
    tokio::spawn(async move {
        let root = PathBuf::from(&dir);
        let _ = std::fs::create_dir_all(root.join("blobs"));

        let mut blobs_written = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(root.join("blobs")) {
            for e in entries.flatten() {
                if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                    if let Ok(id) = Uuid::parse_str(stem) {
                        blobs_written.insert(id);
                    }
                }
            }
        }

        let mut ctx = SaveCtx { dir: root, last: String::new(), blobs_written };
        let mut ticker = tokio::time::interval(Duration::from_secs(3));
        loop {
            ticker.tick().await;
            write_once(&mut ctx, &mem, &state);
        }
    });
}
