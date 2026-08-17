-- Trust v0 initial schema.
--
-- Invariant: message and key bodies are opaque BYTEA. There is deliberately no column that
-- plaintext could live in — the E2E boundary is physical, not just policy.

-- Accounts (users). Password is stored only as a verifier (Argon2id hash).
CREATE TABLE accounts (
    id                UUID PRIMARY KEY,
    username          TEXT NOT NULL UNIQUE,
    password_verifier TEXT NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Devices. One install = one device = its own identity key and session.
CREATE TABLE devices (
    id              UUID PRIMARY KEY,
    account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    display_name    TEXT NOT NULL,
    public_identity BYTEA NOT NULL,          -- public MLS credential / identity key
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at      TIMESTAMPTZ
);
CREATE INDEX devices_account_idx ON devices(account_id);

-- Sessions. Opaque bearer tokens, stored hashed, revocable per device.
CREATE TABLE sessions (
    id                 UUID PRIMARY KEY,
    device_id          UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    access_token_hash  BYTEA NOT NULL UNIQUE,
    refresh_token_hash BYTEA NOT NULL UNIQUE,
    expires_at         TIMESTAMPTZ NOT NULL,
    revoked_at         TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX sessions_device_idx ON sessions(device_id);

-- One-time MLS KeyPackages (opaque public key material), consumed on claim.
CREATE TABLE key_packages (
    id           UUID PRIMARY KEY,
    device_id    UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    key_package  BYTEA NOT NULL,
    last_resort  BOOLEAN NOT NULL DEFAULT FALSE,
    consumed_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX key_packages_claim_idx
    ON key_packages(device_id) WHERE consumed_at IS NULL AND last_resort = FALSE;

-- Conversations. `kind` generalizes 1:1 ('dm') to groups later. `last_seq` is the
-- per-conversation monotonic counter, bumped atomically to assign message seqs.
CREATE TABLE conversations (
    id         UUID PRIMARY KEY,
    kind       TEXT NOT NULL DEFAULT 'dm',
    epoch      BIGINT NOT NULL DEFAULT 0,      -- MLS epoch; server sequences commits via CAS (later)
    last_seq   BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (conversation_id, account_id)
);
CREATE INDEX conversation_members_account_idx ON conversation_members(account_id);

-- Stable 1:1 mapping from an unordered account pair to its conversation. For a
-- self-chat (Saved Messages) account_lo = account_hi.
CREATE TABLE dm_conversations (
    account_lo      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    account_hi      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    PRIMARY KEY (account_lo, account_hi)
);

-- Messages. Ciphertext only; `seq` is gap-free per conversation.
CREATE TABLE messages (
    id                UUID PRIMARY KEY,
    conversation_id   UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    seq               BIGINT NOT NULL,
    sender_account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    sender_device_id  UUID,                    -- null for system/seed; not FK-constrained
    ciphertext        BYTEA NOT NULL,
    server_ts         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (conversation_id, seq)
);
CREATE INDEX messages_conversation_seq_idx ON messages(conversation_id, seq);

-- High-water-mark receipts, per account (so groups stay cheap).
CREATE TABLE read_markers (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    up_to_seq       BIGINT NOT NULL,
    PRIMARY KEY (conversation_id, account_id)
);
CREATE TABLE delivered_markers (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    up_to_seq       BIGINT NOT NULL,
    PRIMARY KEY (conversation_id, account_id)
);

-- User profiles.
CREATE TABLE profiles (
    account_id     UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    display_name   TEXT,
    bio            TEXT,
    avatar_blob_id UUID,
    last_seen      TIMESTAMPTZ
);

-- Binary blobs (avatars, image/file messages). Opaque bytes + content type.
-- (Object storage is the scale target; Postgres BYTEA is fine at this stage.)
CREATE TABLE blobs (
    id           UUID PRIMARY KEY,
    content_type TEXT NOT NULL,
    bytes        BYTEA NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Shared-media index per conversation (photos/videos/music/files/links/voice).
CREATE TABLE media (
    id                UUID PRIMARY KEY,
    conversation_id   UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    message_id        UUID NOT NULL,
    sender_account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    seq               BIGINT NOT NULL,
    kind              TEXT NOT NULL,           -- photo | video | music | file | link | voice
    blob_id           UUID,
    thumb_blob_id     UUID,
    mime              TEXT,
    name              TEXT,
    url               TEXT,
    size              BIGINT,
    width             BIGINT,
    height            BIGINT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Covering index for the paginated shared-media reads: newest-first by kind.
CREATE INDEX media_convo_kind_seq_idx ON media(conversation_id, kind, seq DESC);
