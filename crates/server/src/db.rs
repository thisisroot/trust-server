//! Postgres queries for messaging, conversations, receipts, profiles, blobs, and
//! shared media. Runtime (not compile-time-checked) queries so the crate builds
//! without a live database, mirroring `trust_storage::auth_repo`.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

// ── conversations ────────────────────────────────────────────────────────────

/// Stable conversation id for an unordered account pair, created on first use.
/// A self-chat (Saved Messages) has account_lo == account_hi.
pub async fn dm_conversation(pool: &PgPool, a: Uuid, b: Uuid) -> Result<Uuid, sqlx::Error> {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };

    if let Some(cid) = sqlx::query_scalar::<_, Uuid>(
        "SELECT conversation_id FROM dm_conversations WHERE account_lo = $1 AND account_hi = $2",
    )
    .bind(lo)
    .bind(hi)
    .fetch_optional(pool)
    .await?
    {
        return Ok(cid);
    }

    // Create the conversation, then claim the pair mapping. If we lose the race,
    // the mapping insert is a no-op and we return the winner's id (our fresh
    // conversation row is left as a harmless empty orphan).
    let cid = Uuid::new_v4();
    sqlx::query("INSERT INTO conversations (id, kind) VALUES ($1, 'dm')")
        .bind(cid)
        .execute(pool)
        .await?;

    // Members (one row for a self-chat, two otherwise).
    if lo == hi {
        sqlx::query("INSERT INTO conversation_members (conversation_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(cid).bind(lo).execute(pool).await?;
    } else {
        sqlx::query("INSERT INTO conversation_members (conversation_id, account_id) VALUES ($1, $2), ($1, $3) ON CONFLICT DO NOTHING")
            .bind(cid).bind(lo).bind(hi).execute(pool).await?;
    }

    sqlx::query(
        "INSERT INTO dm_conversations (account_lo, account_hi, conversation_id)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(lo)
    .bind(hi)
    .bind(cid)
    .execute(pool)
    .await?;

    let actual = sqlx::query_scalar::<_, Uuid>(
        "SELECT conversation_id FROM dm_conversations WHERE account_lo = $1 AND account_hi = $2",
    )
    .bind(lo)
    .bind(hi)
    .fetch_one(pool)
    .await?;
    Ok(actual)
}

pub async fn is_participant(pool: &PgPool, cid: Uuid, account: Uuid) -> Result<bool, sqlx::Error> {
    let n = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM conversation_members WHERE conversation_id = $1 AND account_id = $2",
    )
    .bind(cid)
    .bind(account)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

// ── messages ─────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
pub struct MsgRow {
    pub id: Uuid,
    pub seq: i64,
    pub sender_account_id: Uuid,
    pub sender_device_id: Option<Uuid>,
    pub ciphertext: Vec<u8>,
    pub server_ts: OffsetDateTime,
    pub reply_to: Option<Uuid>,
    pub edited_at: Option<OffsetDateTime>,
}

/// Append a message, assigning the next per-conversation seq atomically.
pub async fn store_message(
    pool: &PgPool,
    cid: Uuid,
    sender_account: Uuid,
    sender_device: Uuid,
    ciphertext: &[u8],
    reply_to: Option<Uuid>,
) -> Result<MsgRow, sqlx::Error> {
    // Row-locks the conversation → concurrent sends get distinct, gap-free seqs.
    let seq = sqlx::query_scalar::<_, i64>(
        "UPDATE conversations SET last_seq = last_seq + 1 WHERE id = $1 RETURNING last_seq",
    )
    .bind(cid)
    .fetch_one(pool)
    .await?;

    let id = Uuid::new_v4();
    let server_ts = sqlx::query_scalar::<_, OffsetDateTime>(
        "INSERT INTO messages (id, conversation_id, seq, sender_account_id, sender_device_id, ciphertext, reply_to)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING server_ts",
    )
    .bind(id)
    .bind(cid)
    .bind(seq)
    .bind(sender_account)
    .bind(sender_device)
    .bind(ciphertext)
    .bind(reply_to)
    .fetch_one(pool)
    .await?;

    Ok(MsgRow {
        id,
        seq,
        sender_account_id: sender_account,
        sender_device_id: Some(sender_device),
        ciphertext: ciphertext.to_vec(),
        server_ts,
        reply_to,
        edited_at: None,
    })
}

pub async fn history(pool: &PgPool, cid: Uuid, me: Uuid) -> Result<Vec<MsgRow>, sqlx::Error> {
    sqlx::query_as::<_, MsgRow>(
        "SELECT id, seq, sender_account_id, sender_device_id, ciphertext, server_ts, reply_to, edited_at
         FROM messages m
         WHERE m.conversation_id = $1 AND m.deleted_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM hidden_messages h
                           WHERE h.message_id = m.id AND h.account_id = $2)
         ORDER BY m.seq",
    )
    .bind(cid)
    .bind(me)
    .fetch_all(pool)
    .await
}

/// Hide a message from just `account`'s history (delete for me). Idempotent.
pub async fn hide_message_for(pool: &PgPool, msg_id: Uuid, account: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hidden_messages (message_id, account_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(msg_id)
    .bind(account)
    .execute(pool)
    .await?;
    Ok(())
}

/// Overwrite a message's ciphertext (edit), only if `sender` owns it and it's not
/// deleted. Returns (conversation_id, seq, edited_at) when a row was changed.
pub async fn edit_message(
    pool: &PgPool,
    msg_id: Uuid,
    sender: Uuid,
    ciphertext: &[u8],
) -> Result<Option<(Uuid, i64, OffsetDateTime)>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (Uuid, i64, OffsetDateTime)>(
        "UPDATE messages SET ciphertext = $3, edited_at = now()
         WHERE id = $1 AND sender_account_id = $2 AND deleted_at IS NULL
         RETURNING conversation_id, seq, edited_at",
    )
    .bind(msg_id)
    .bind(sender)
    .bind(ciphertext)
    .fetch_optional(pool)
    .await?)
}

/// Soft-delete a message (delete for everyone). Any participant of the message's
/// conversation may do this. Clears the ciphertext and drops any shared-media
/// index rows. Returns (conversation_id, seq) when a row was changed.
pub async fn delete_message(
    pool: &PgPool,
    msg_id: Uuid,
    actor: Uuid,
) -> Result<Option<(Uuid, i64)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, i64)>(
        "UPDATE messages SET deleted_at = now(), ciphertext = ''
         WHERE id = $1 AND deleted_at IS NULL
           AND conversation_id IN
               (SELECT conversation_id FROM conversation_members WHERE account_id = $2)
         RETURNING conversation_id, seq",
    )
    .bind(msg_id)
    .bind(actor)
    .fetch_optional(pool)
    .await?;
    if row.is_some() {
        sqlx::query("DELETE FROM media WHERE message_id = $1")
            .bind(msg_id)
            .execute(pool)
            .await?;
    }
    Ok(row)
}

/// The account ids that are members of a conversation (both sides of a DM).
pub async fn conversation_member_ids(pool: &PgPool, cid: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT account_id FROM conversation_members WHERE conversation_id = $1",
    )
    .bind(cid)
    .fetch_all(pool)
    .await
}

// ── receipts (high-water markers) ────────────────────────────────────────────

async fn mark(pool: &PgPool, table: &str, cid: Uuid, account: Uuid) -> Result<(), sqlx::Error> {
    // Set the marker to the conversation's current max seq, monotonically.
    let sql = format!(
        "INSERT INTO {table} (conversation_id, account_id, up_to_seq)
         SELECT $1, $2, COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = $1
         ON CONFLICT (conversation_id, account_id)
         DO UPDATE SET up_to_seq = GREATEST({table}.up_to_seq, EXCLUDED.up_to_seq)"
    );
    sqlx::query(&sql).bind(cid).bind(account).execute(pool).await?;
    Ok(())
}

pub async fn mark_read(pool: &PgPool, cid: Uuid, account: Uuid) -> Result<(), sqlx::Error> {
    mark(pool, "read_markers", cid, account).await
}
pub async fn mark_delivered(pool: &PgPool, cid: Uuid, account: Uuid) -> Result<(), sqlx::Error> {
    mark(pool, "delivered_markers", cid, account).await
}

async fn marker(pool: &PgPool, table: &str, cid: Uuid, account: Uuid) -> Result<i64, sqlx::Error> {
    let sql = format!("SELECT up_to_seq FROM {table} WHERE conversation_id = $1 AND account_id = $2");
    Ok(sqlx::query_scalar::<_, i64>(&sql)
        .bind(cid)
        .bind(account)
        .fetch_optional(pool)
        .await?
        .unwrap_or(0))
}

pub async fn read_seq(pool: &PgPool, cid: Uuid, account: Uuid) -> Result<i64, sqlx::Error> {
    marker(pool, "read_markers", cid, account).await
}
pub async fn delivered_seq(pool: &PgPool, cid: Uuid, account: Uuid) -> Result<i64, sqlx::Error> {
    marker(pool, "delivered_markers", cid, account).await
}

// ── conversation / user listing ──────────────────────────────────────────────

#[derive(sqlx::FromRow)]
pub struct ConvoListRow {
    pub other: Uuid,
    pub cid: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_blob_id: Option<Uuid>,
    pub last_seen: Option<OffsetDateTime>,
}

/// The account's non-self conversations that have at least one message, newest first.
pub async fn conversations_for(pool: &PgPool, account: Uuid) -> Result<Vec<ConvoListRow>, sqlx::Error> {
    // One "other" member per DM conversation, so no grouping is needed. Order by
    // each conversation's latest message time.
    sqlx::query_as::<_, ConvoListRow>(
        "SELECT o.account_id AS other, me.conversation_id AS cid,
                a.username, p.display_name, p.avatar_blob_id, p.last_seen
         FROM conversation_members me
         JOIN conversation_members o
              ON o.conversation_id = me.conversation_id AND o.account_id <> me.account_id
         JOIN accounts a ON a.id = o.account_id
         LEFT JOIN profiles p ON p.account_id = o.account_id
         WHERE me.account_id = $1
           AND EXISTS (SELECT 1 FROM messages m WHERE m.conversation_id = me.conversation_id)
         ORDER BY (SELECT max(server_ts) FROM messages m2
                   WHERE m2.conversation_id = me.conversation_id) DESC",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}

#[derive(sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_blob_id: Option<Uuid>,
    pub last_seen: Option<OffsetDateTime>,
}

pub async fn list_users(pool: &PgPool, me: Uuid) -> Result<Vec<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>(
        "SELECT a.id, a.username, p.display_name, p.avatar_blob_id, p.last_seen
         FROM accounts a LEFT JOIN profiles p ON p.account_id = a.id
         WHERE a.id <> $1 ORDER BY a.username",
    )
    .bind(me)
    .fetch_all(pool)
    .await
}

pub async fn find_user_by_username(pool: &PgPool, username: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as::<_, UserRow>(
        "SELECT a.id, a.username, p.display_name, p.avatar_blob_id, p.last_seen
         FROM accounts a LEFT JOIN profiles p ON p.account_id = a.id
         WHERE a.username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
}

pub async fn username_of(pool: &PgPool, id: Uuid) -> Result<String, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, String>("SELECT username FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .unwrap_or_default())
}

// ── profiles ─────────────────────────────────────────────────────────────────

#[derive(Default, sqlx::FromRow)]
pub struct ProfileRow {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_blob_id: Option<Uuid>,
    pub last_seen: Option<OffsetDateTime>,
}

pub async fn get_profile(pool: &PgPool, account: Uuid) -> Result<ProfileRow, sqlx::Error> {
    Ok(sqlx::query_as::<_, ProfileRow>(
        "SELECT display_name, bio, avatar_blob_id, last_seen FROM profiles WHERE account_id = $1",
    )
    .bind(account)
    .fetch_optional(pool)
    .await?
    .unwrap_or_default())
}

pub async fn upsert_profile(
    pool: &PgPool,
    account: Uuid,
    display_name: Option<String>,
    bio: Option<String>,
    avatar_blob_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO profiles (account_id, display_name, bio, avatar_blob_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (account_id) DO UPDATE
         SET display_name = EXCLUDED.display_name,
             bio = EXCLUDED.bio,
             avatar_blob_id = EXCLUDED.avatar_blob_id",
    )
    .bind(account)
    .bind(display_name)
    .bind(bio)
    .bind(avatar_blob_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_last_seen(pool: &PgPool, account: Uuid, at: OffsetDateTime) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO profiles (account_id, last_seen) VALUES ($1, $2)
         ON CONFLICT (account_id) DO UPDATE SET last_seen = EXCLUDED.last_seen",
    )
    .bind(account)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(())
}

// ── blobs ────────────────────────────────────────────────────────────────────

pub async fn put_blob(pool: &PgPool, content_type: &str, bytes: &[u8]) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO blobs (id, content_type, bytes) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(content_type)
        .bind(bytes)
        .execute(pool)
        .await?;
    Ok(id)
}

#[derive(sqlx::FromRow)]
pub struct BlobRow {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

pub async fn get_blob(pool: &PgPool, id: Uuid) -> Result<Option<BlobRow>, sqlx::Error> {
    sqlx::query_as::<_, BlobRow>("SELECT content_type, bytes FROM blobs WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

// ── media (shared-media index) ───────────────────────────────────────────────

#[derive(sqlx::FromRow)]
pub struct MediaRow {
    pub id: Uuid,
    pub message_id: Uuid,
    pub sender_account_id: Uuid,
    pub seq: i64,
    pub kind: String,
    pub blob_id: Option<Uuid>,
    pub thumb_blob_id: Option<Uuid>,
    pub mime: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub created_at: OffsetDateTime,
}

#[allow(clippy::too_many_arguments)]
pub async fn record_media(
    pool: &PgPool,
    cid: Uuid,
    message_id: Uuid,
    sender_account: Uuid,
    seq: i64,
    kind: &str,
    blob_id: Option<Uuid>,
    thumb_blob_id: Option<Uuid>,
    mime: Option<String>,
    name: Option<String>,
    url: Option<String>,
    size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media
         (id, conversation_id, message_id, sender_account_id, seq, kind,
          blob_id, thumb_blob_id, mime, name, url, size, width, height)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(Uuid::new_v4())
    .bind(cid)
    .bind(message_id)
    .bind(sender_account)
    .bind(seq)
    .bind(kind)
    .bind(blob_id)
    .bind(thumb_blob_id)
    .bind(mime)
    .bind(name)
    .bind(url)
    .bind(size)
    .bind(width)
    .bind(height)
    .execute(pool)
    .await?;
    Ok(())
}

/// Newest-first page of a conversation's media. `before` is an exclusive seq
/// cursor. Returns (rows, next_cursor) where next_cursor is Some iff more remain.
pub async fn list_media(
    pool: &PgPool,
    cid: Uuid,
    kind: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> Result<(Vec<MediaRow>, Option<i64>), sqlx::Error> {
    let mut rows = sqlx::query_as::<_, MediaRow>(
        "SELECT id, message_id, sender_account_id, seq, kind, blob_id, thumb_blob_id,
                mime, name, url, size, width, height, created_at
         FROM media
         WHERE conversation_id = $1
           AND ($2::text IS NULL OR kind = $2)
           AND ($3::bigint IS NULL OR seq < $3)
         ORDER BY seq DESC
         LIMIT $4",
    )
    .bind(cid)
    .bind(kind)
    .bind(before)
    .bind(limit + 1) // fetch one extra to detect "more"
    .fetch_all(pool)
    .await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let next = if has_more { rows.last().map(|r| r.seq) } else { None };
    Ok((rows, next))
}
