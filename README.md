# trust-server

Reference implementation of the [`trust-protocol`](../trust-protocol), in **Rust**.

The official instance runs this exact stack, and self-hosters run pre-built images of it (see [`trust-deploy`](../trust-deploy)) — they configure and run, never build from source.

## Responsibilities

- Implement the versioned REST + WebSocket contract, including the capability handshake.
- Route and store **ciphertext only** — the server never sees plaintext message bodies or call media keys.
- Auth + multi-device sessions; distribute prekey bundles for E2E key exchange.
- Real-time fan-out: messages, receipts, typing, presence.
- Media object storage integration; WebRTC signaling to the SFU for calls.
- Offline delivery hooks for push notifications.

## Dependencies (composed in trust-deploy)

- **Database** — durable state (accounts, sessions, key bundles, envelope metadata).
- **Cache / pub-sub** — presence and real-time fan-out across nodes.
- **Object storage** — encrypted media blobs.
- **SFU** — call relay (so call quality/scale isn't limited by peer-to-peer NAT).
- **Reverse proxy** — automatic HTTPS.

## Getting started

```bash
# 1. Start Postgres (from the trust-deploy repo)
docker compose -f ../trust-deploy/compose/docker-compose.dev.yml up -d

# 2. Run the server (defaults connect to the compose Postgres)
cargo run                      # serves on 0.0.0.0:8080

# Probe it
curl localhost:8080/health
curl -H "Trust-Protocol-Version: 0" localhost:8080/v0/capabilities
```

The server boots and serves `/health` even if Postgres is down (migrations run in the
background). `/v0/*` routes require the `Trust-Protocol-Version` header.

### Configuration

Layered: built-in defaults < `trust-server.toml` < `TRUST_*` env vars. See
[`trust-server.example.toml`](./trust-server.example.toml).

| Setting | Env | Default |
|---|---|---|
| Bind address | `TRUST_BIND_ADDR` | `0.0.0.0:8080` |
| Database URL | `TRUST_DATABASE_URL` | `postgres://trust:trust@localhost:5432/trust` |
| Max DB connections | `TRUST_MAX_CONNECTIONS` | `5` |

## Layout

```
crates/
  server/       # binary: axum HTTP + WS entrypoint, config, version/auth layers
  protocol/     # hand-written Rust wire types, golden-guarded against trust-protocol
  storage/      # sqlx Postgres pool + migrations + repositories
  realtime/     # RealtimeBus trait + InProcessBus (Redis impl later)
  crypto/       # server-side MLS commit sequencer (epoch CAS; never reads bodies)
  signaling/    # placeholder for WebRTC/SFU calls (roadmap step 8)
```

## Status

- **Skeleton** ✅ — workspace builds warning-free, server boots, `/health` +
  `/v0/capabilities` (version-gated), initial migration authored.
- **Auth + multi-device sessions** ✅ — `POST /v0/auth/{register,login,refresh,logout}`.
  Argon2id password verifiers; opaque 256-bit bearer tokens stored only as SHA-256 hashes;
  one session per device; single-use refresh rotation. Unit tests for token/password
  helpers pass; a self-skipping Postgres integration test
  (`crates/storage/tests/auth_repo_it.rs`) covers the full account→device→session lifecycle
  and runs automatically once a database is reachable.
- **Next:** KeyPackage publish/claim, then idempotent messaging + per-device delivery queue,
  then the WebSocket handshake and delivery.

Run the DB integration test once Postgres is up:

```bash
docker compose -f ../trust-deploy/compose/docker-compose.dev.yml up -d
cargo test                      # the skipped integration test now runs for real
```
