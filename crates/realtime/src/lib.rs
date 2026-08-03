//! Real-time delivery fan-out.
//!
//! The WebSocket layer only ever talks to the [`RealtimeBus`] trait. The slice ships the
//! single-node [`InProcessBus`]; a Redis-backed implementation drops in later for multi-node
//! without touching the transport. Presence (a later step) rides the same bus as another topic.

use dashmap::DashMap;
use tokio::sync::broadcast;
use trust_protocol_types::Envelope;
use uuid::Uuid;

/// A recipient device — the addressing unit for delivery (never a user).
pub type DeviceId = Uuid;

/// Publish/subscribe of envelopes to connected devices.
pub trait RealtimeBus: Send + Sync {
    /// Deliver an envelope to a device's live subscribers. A no-op if the device is offline
    /// (durable delivery is the delivery_queue's job, not the bus's).
    fn publish(&self, device: DeviceId, event: Envelope);

    /// Subscribe to live envelopes for a device (one call per WebSocket connection).
    fn subscribe(&self, device: DeviceId) -> broadcast::Receiver<Envelope>;
}

/// Single-process bus backed by per-device broadcast channels.
pub struct InProcessBus {
    channels: DashMap<DeviceId, broadcast::Sender<Envelope>>,
    capacity: usize,
}

impl InProcessBus {
    /// `capacity` bounds the per-device backlog a slow live subscriber may lag before it drops
    /// frames (it then recovers via GET /sync — the durable path).
    pub fn new(capacity: usize) -> Self {
        Self {
            channels: DashMap::new(),
            capacity,
        }
    }

    fn sender(&self, device: DeviceId) -> broadcast::Sender<Envelope> {
        self.channels
            .entry(device)
            .or_insert_with(|| broadcast::channel(self.capacity).0)
            .clone()
    }
}

impl Default for InProcessBus {
    fn default() -> Self {
        Self::new(256)
    }
}

impl RealtimeBus for InProcessBus {
    fn publish(&self, device: DeviceId, event: Envelope) {
        // Err just means no live receivers; the message stays queued in the DB regardless.
        let _ = self.sender(device).send(event);
    }

    fn subscribe(&self, device: DeviceId) -> broadcast::Receiver<Envelope> {
        self.sender(device).subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn sample(device: &str) -> (DeviceId, Envelope) {
        let id = Uuid::new_v4();
        (
            Uuid::parse_str(device).unwrap(),
            Envelope {
                v: 0,
                type_: "message.new".into(),
                id,
                ts: OffsetDateTime::UNIX_EPOCH,
                payload: serde_json::json!({}),
            },
        )
    }

    #[tokio::test]
    async fn delivers_to_subscribers() {
        let bus = InProcessBus::default();
        let (device, env) = sample("00000000-0000-4000-8000-000000000001");
        let mut rx = bus.subscribe(device);
        bus.publish(device, env.clone());
        let got = rx.recv().await.expect("receive");
        assert_eq!(got.id, env.id);
    }
}
