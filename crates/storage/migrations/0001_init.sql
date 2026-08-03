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
-- Fast "claim one unconsumed package for this device" lookups.
CREATE INDEX key_packages_claim_idx
    ON key_packages(device_id) WHERE consumed_at IS NULL AND last_resort = FALSE;

-- Conversations. `kind` generalizes 1:1 ('dm') to groups later.
CREATE TABLE conversations (
    id         UUID PRIMARY KEY,
    kind       TEXT NOT NULL DEFAULT 'dm',
    epoch      BIGINT NOT NULL DEFAULT 0,      -- MLS epoch; server sequences commits via CAS
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (conversation_id, account_id)
);

-- Messages. Ciphertext only. `seq` is gap-free per conversation; idempotent on client_msg_id.
CREATE TABLE messages (
    id               UUID PRIMARY KEY,
    conversation_id  UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    seq              BIGINT NOT NULL,
    sender_device_id UUID NOT NULL REFERENCES devices(id),
    client_msg_id    UUID NOT NULL,
    ciphertext       BYTEA NOT NULL,
    server_ts        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (conversation_id, seq),
    UNIQUE (conversation_id, client_msg_id)
);
CREATE INDEX messages_conversation_seq_idx ON messages(conversation_id, seq);

-- Per-recipient-device delivery queue: the store-and-forward backbone for offline + multi-device.
CREATE TYPE delivery_state AS ENUM ('queued', 'delivered', 'read');
CREATE TABLE delivery_queue (
    id                  UUID PRIMARY KEY,
    message_id          UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    recipient_device_id UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    state               delivery_state NOT NULL DEFAULT 'queued',
    delivered_at        TIMESTAMPTZ,
    UNIQUE (message_id, recipient_device_id)
);
CREATE INDEX delivery_queue_device_idx
    ON delivery_queue(recipient_device_id) WHERE state = 'queued';

-- High-water-mark read receipts (not per-message, so groups stay cheap).
CREATE TABLE read_markers (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    device_id       UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    up_to_seq       BIGINT NOT NULL,
    PRIMARY KEY (conversation_id, device_id)
);
