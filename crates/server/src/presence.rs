//! Live presence + connection tracking. This is runtime state (who is connected
//! right now), not durable data — so it stays in memory. Durable `last_seen` is
//! persisted to the `profiles` table on disconnect.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

#[derive(Default)]
pub struct Presence {
    online: Mutex<HashMap<Uuid, usize>>, // account -> connected device count
    connected: Mutex<HashMap<Uuid, Uuid>>, // device -> account
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a device connection. Returns true if the account just came online.
    pub fn on_connect(&self, device: Uuid, account: Uuid) -> bool {
        self.connected.lock().unwrap().insert(device, account);
        let mut online = self.online.lock().unwrap();
        let c = online.entry(account).or_insert(0);
        *c += 1;
        *c == 1
    }

    /// Deregister a device. Returns (account, went_offline) if it was connected.
    pub fn on_disconnect(&self, device: Uuid) -> Option<(Uuid, bool)> {
        let account = self.connected.lock().unwrap().remove(&device)?;
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
        Some((account, went_offline))
    }

    pub fn is_online(&self, account: Uuid) -> bool {
        self.online.lock().unwrap().contains_key(&account)
    }

    pub fn connected_devices(&self) -> Vec<Uuid> {
        self.connected.lock().unwrap().keys().copied().collect()
    }
}
