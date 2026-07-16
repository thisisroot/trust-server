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
cargo run
```

Configuration is via environment variables / a config file (documented as endpoints land). For a full local stack (DB, cache, storage, SFU, proxy) use [`trust-deploy`](../trust-deploy).

## Layout (planned)

```
crates/
  trust-server/     # binary: HTTP + WS entrypoint
  protocol/         # Rust types for the trust-protocol envelopes
  storage/          # DB + object storage adapters
  realtime/         # pub-sub fan-out, presence
  signaling/        # WebRTC / SFU signaling
```
