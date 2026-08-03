//! In-memory messaging + presence state for local/dev running (no database).
//! Holds conversations, message history, per-account online counts, and
//! last-seen times. Delivery itself goes through the realtime bus.

use std::collections::HashMap;
use std::sync::Mutex;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(Clone)]
pub struct StoredMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub seq: i64,
    pub sender_account: Uuid,
    pub sender_device: Uuid,
    pub ciphertext: Vec<u8>,
    pub ts: OffsetDateTime,
}

/// A full snapshot of the hub's persistent state (for dev persistence). Presence
/// / connection state is intentionally excluded — it's rebuilt on reconnect.
pub struct HubSnapshot {
    pub convos: Vec<((Uuid, Uuid), Uuid)>,
    pub seqs: Vec<(Uuid, i64)>,
    pub messages: Vec<StoredMessage>,
    pub read_markers: Vec<((Uuid, Uuid), i64)>,
    pub delivered_markers: Vec<((Uuid, Uuid), i64)>,
}

#[derive(Default)]
pub struct Hub {
    convos: Mutex<HashMap<(Uuid, Uuid), Uuid>>,
    seqs: Mutex<HashMap<Uuid, i64>>,
    messages: Mutex<Vec<StoredMessage>>,
    online: Mutex<HashMap<Uuid, usize>>, // account -> connected device count
    last_seen: Mutex<HashMap<Uuid, OffsetDateTime>>,
    connected: Mutex<HashMap<Uuid, Uuid>>, // device -> account
    // (conversation, reader_account) -> highest seq that reader has read / received.
    read_markers: Mutex<HashMap<(Uuid, Uuid), i64>>,
    delivered_markers: Mutex<HashMap<(Uuid, Uuid), i64>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn pair(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Stable conversation id for a pair of accounts (created on first use).
    pub fn conversation_id(&self, a: Uuid, b: Uuid) -> Uuid {
        *self
            .convos
            .lock()
            .unwrap()
            .entry(Self::pair(a, b))
            .or_insert_with(Uuid::new_v4)
    }

    pub fn store_message(
        &self,
        conversation_id: Uuid,
        sender_account: Uuid,
        sender_device: Uuid,
        ciphertext: Vec<u8>,
    ) -> StoredMessage {
        let seq = {
            let mut seqs = self.seqs.lock().unwrap();
            let s = seqs.entry(conversation_id).or_insert(0);
            *s += 1;
            *s
        };
        let msg = StoredMessage {
            id: Uuid::new_v4(),
            conversation_id,
            seq,
            sender_account,
            sender_device,
            ciphertext,
            ts: OffsetDateTime::now_utc(),
        };
        self.messages.lock().unwrap().push(msg.clone());
        msg
    }

    pub fn history(&self, conversation_id: Uuid) -> Vec<StoredMessage> {
        let mut v: Vec<StoredMessage> = self
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.conversation_id == conversation_id)
            .cloned()
            .collect();
        v.sort_by_key(|m| m.seq);
        v
    }

    pub fn has_messages(&self, conversation_id: Uuid) -> bool {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.conversation_id == conversation_id)
    }

    pub fn last_message_ts(&self, conversation_id: Uuid) -> Option<OffsetDateTime> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.conversation_id == conversation_id)
            .map(|m| m.ts)
            .max()
    }

    /// Whether `account` is one of the two participants in `conversation_id`.
    pub fn is_participant(&self, conversation_id: Uuid, account: Uuid) -> bool {
        self.convos
            .lock()
            .unwrap()
            .iter()
            .any(|((a, b), cid)| *cid == conversation_id && (*a == account || *b == account))
    }

    /// Conversations `account` participates in that have at least one message,
    /// as (other_account, conversation_id).
    pub fn conversations_for(&self, account: Uuid) -> Vec<(Uuid, Uuid)> {
        let convos = self.convos.lock().unwrap();
        let msgs = self.messages.lock().unwrap();
        let mut out = Vec::new();
        for ((a, b), cid) in convos.iter() {
            if (*a == account || *b == account) && msgs.iter().any(|m| m.conversation_id == *cid) {
                let other = if *a == account { *b } else { *a };
                // Self-conversation ("Saved Messages") is surfaced separately.
                if other != account {
                    out.push((other, *cid));
                }
            }
        }
        out
    }

    /// Seed a fresh conversation with a few backdated messages (dev/testing, so
    /// date separators and history have something to show). Bodies are utf8 text
    /// bytes — the same shape as a decoded dev "ciphertext".
    pub fn seed_backdated(&self, conversation_id: Uuid, a: Uuid, b: Uuid) {
        let now = OffsetDateTime::now_utc();
        let seeds: [(Uuid, &str, OffsetDateTime); 3] = [
            (a, "Hey! 👋", now - Duration::days(3)),
            (b, "Good to see Trust actually working", now - Duration::days(1)),
            (a, "Scroll up — the date separators come from these older ones", now - Duration::hours(3)),
        ];
        for (sender, text, ts) in seeds {
            let seq = {
                let mut seqs = self.seqs.lock().unwrap();
                let c = seqs.entry(conversation_id).or_insert(0);
                *c += 1;
                *c
            };
            self.messages.lock().unwrap().push(StoredMessage {
                id: Uuid::new_v4(),
                conversation_id,
                seq,
                sender_account: sender,
                sender_device: Uuid::nil(),
                ciphertext: text.as_bytes().to_vec(),
                ts,
            });
        }
    }

    /// Register a device connection. Returns true if the account just came online.
    pub fn on_connect(&self, device: Uuid, account: Uuid) -> bool {
        self.connected.lock().unwrap().insert(device, account);
        let mut online = self.online.lock().unwrap();
        let c = online.entry(account).or_insert(0);
        *c += 1;
        *c == 1
    }

    /// Deregister a device. Returns (account, went_offline, at).
    pub fn on_disconnect(&self, device: Uuid) -> Option<(Uuid, bool, OffsetDateTime)> {
        let account = self.connected.lock().unwrap().remove(&device)?;
        let now = OffsetDateTime::now_utc();
        let mut online = self.online.lock().unwrap();
        let went_offline = match online.get_mut(&account) {
            Some(c) => {
                *c -= 1;
                if *c == 0 {
                    online.remove(&account);
                    true
                } else {
                    false
                }
            }
            None => true,
        };
        if went_offline {
            self.last_seen.lock().unwrap().insert(account, now);
        }
        Some((account, went_offline, now))
    }

    pub fn is_online(&self, account: Uuid) -> bool {
        self.online.lock().unwrap().contains_key(&account)
    }

    pub fn last_seen(&self, account: Uuid) -> Option<OffsetDateTime> {
        self.last_seen.lock().unwrap().get(&account).copied()
    }

    pub fn connected_devices(&self) -> Vec<Uuid> {
        self.connected.lock().unwrap().keys().copied().collect()
    }

    /// Highest seq present in a conversation.
    pub fn last_seq(&self, conversation_id: Uuid) -> i64 {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.conversation_id == conversation_id)
            .map(|m| m.seq)
            .max()
            .unwrap_or(0)
    }

    /// Record that `reader` has read (or received) everything in the conversation.
    pub fn mark_read(&self, conversation_id: Uuid, reader: Uuid) {
        let seq = self.last_seq(conversation_id);
        let mut m = self.read_markers.lock().unwrap();
        let e = m.entry((conversation_id, reader)).or_insert(0);
        if seq > *e {
            *e = seq;
        }
    }

    pub fn mark_delivered(&self, conversation_id: Uuid, reader: Uuid) {
        let seq = self.last_seq(conversation_id);
        let mut m = self.delivered_markers.lock().unwrap();
        let e = m.entry((conversation_id, reader)).or_insert(0);
        if seq > *e {
            *e = seq;
        }
    }

    pub fn read_seq(&self, conversation_id: Uuid, reader: Uuid) -> i64 {
        *self.read_markers.lock().unwrap().get(&(conversation_id, reader)).unwrap_or(&0)
    }

    pub fn delivered_seq(&self, conversation_id: Uuid, reader: Uuid) -> i64 {
        *self.delivered_markers.lock().unwrap().get(&(conversation_id, reader)).unwrap_or(&0)
    }

    /// Snapshot persistent state (for dev persistence).
    pub fn export(&self) -> HubSnapshot {
        HubSnapshot {
            convos: self.convos.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect(),
            seqs: self.seqs.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect(),
            messages: self.messages.lock().unwrap().clone(),
            read_markers: self.read_markers.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect(),
            delivered_markers: self
                .delivered_markers
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
        }
    }

    /// Restore persistent state from a snapshot (for dev persistence).
    pub fn import(&self, snap: HubSnapshot) {
        *self.convos.lock().unwrap() = snap.convos.into_iter().collect();
        *self.seqs.lock().unwrap() = snap.seqs.into_iter().collect();
        *self.messages.lock().unwrap() = snap.messages;
        *self.read_markers.lock().unwrap() = snap.read_markers.into_iter().collect();
        *self.delivered_markers.lock().unwrap() = snap.delivered_markers.into_iter().collect();
    }
}
