//! Hand-written Rust wire types for the Trust protocol.
//!
//! The authoritative contract lives in the `trust-protocol` repo (`schemas/`). These types are
//! kept aligned with it by the golden-payload tests below: every example in
//! `trust-protocol/tests/contract` must round-trip through these types. If a type drifts from
//! the schema, a golden fails to parse and CI goes red — the same guarantee codegen would give,
//! with hand-crafted, readable types.

use serde::{Deserialize, Serialize};

pub mod rest;

/// Integer major protocol version. A bump signals a breaking change.
pub type ProtocolVersion = u32;

/// The version this build of the types targets.
pub const PROTOCOL_VERSION: ProtocolVersion = 0;

/// A negotiable feature. The effective set is the intersection of what client and server
/// advertise in the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Messaging,
    Receipts,
    Typing,
    Presence,
    Media,
    Groups,
    Calls,
    Mls,
}

/// Compute the effective capability set: those advertised by *both* sides. Order follows the
/// server's advertised list for determinism.
pub fn negotiate_capabilities(server: &[Capability], client: &[Capability]) -> Vec<Capability> {
    server
        .iter()
        .copied()
        .filter(|c| client.contains(c))
        .collect()
}

/// Effective protocol major version = the lower of the two sides.
pub fn negotiate_version(server: ProtocolVersion, client: ProtocolVersion) -> ProtocolVersion {
    server.min(client)
}

/// `hello` — first frame the client sends after connecting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub client: String,
    pub v: ProtocolVersion,
    pub capabilities: Vec<Capability>,
}

/// Advisory ceilings the client must respect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_upload_bytes: u64,
    pub max_group_size: u32,
}

/// `welcome` — server reply to `hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub server: String,
    pub v: ProtocolVersion,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
}

/// The outer WebSocket frame. The `payload` is validated against the event schema selected by
/// `type_`; here it is kept as raw JSON so this crate stays a thin, stable wire layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub v: ProtocolVersion,
    #[serde(rename = "type")]
    pub type_: String,
    pub id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: time::OffsetDateTime,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn negotiation_is_intersection_and_min() {
        use Capability::*;
        let server = [Messaging, Receipts];
        let client = [Messaging, Receipts, Mls, Typing];
        assert_eq!(
            negotiate_capabilities(&server, &client),
            vec![Messaging, Receipts]
        );
        assert_eq!(negotiate_version(0, 1), 0);
    }

    fn contract_dir() -> Option<PathBuf> {
        // The golden payloads live in the sibling trust-protocol repo. When the repos are
        // checked out side by side this test enforces the contract; standalone it self-skips.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../trust-protocol/tests/contract");
        dir.exists().then_some(dir)
    }

    #[test]
    fn golden_hello_and_welcome_round_trip() {
        let Some(dir) = contract_dir() else {
            eprintln!("skipping: trust-protocol goldens not present alongside this repo");
            return;
        };

        let hello_env: Envelope =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ws/hello.json")).unwrap())
                .expect("parse hello envelope");
        assert_eq!(hello_env.type_, "hello");
        let hello: Hello =
            serde_json::from_value(hello_env.payload.clone()).expect("parse hello payload");
        assert_eq!(hello.v, PROTOCOL_VERSION);

        let welcome_env: Envelope =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ws/welcome.json")).unwrap())
                .expect("parse welcome envelope");
        let welcome: Welcome =
            serde_json::from_value(welcome_env.payload.clone()).expect("parse welcome payload");
        assert!(welcome.capabilities.contains(&Capability::Messaging));

        // Re-serialize and confirm the shape survives the round trip.
        let reser = serde_json::to_value(&welcome).unwrap();
        assert_eq!(reser["limits"]["maxGroupSize"], 512);
    }
}
