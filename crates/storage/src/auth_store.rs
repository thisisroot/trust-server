//! Storage abstraction for auth so the server can run either against Postgres or
//! a zero-dependency in-memory store (handy for local dev without a database).

use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::models::{Account, Device, Session};
use crate::{auth_repo, StorageError};

/// Accounts, devices, and sessions. Both backends satisfy this identically.
#[async_trait]
pub trait AuthStore: Send + Sync {
    async fn create_account(
        &self,
        id: Uuid,
        username: &str,
        verifier: &str,
    ) -> Result<Account, StorageError>;

    async fn find_account_by_username(
        &self,
        username: &str,
    ) -> Result<Option<Account>, StorageError>;

    async fn create_device(
        &self,
        id: Uuid,
        account_id: Uuid,
        display_name: &str,
        public_identity: &[u8],
    ) -> Result<Device, StorageError>;

    async fn create_session(
        &self,
        id: Uuid,
        device_id: Uuid,
        access_hash: &[u8],
        refresh_hash: &[u8],
        expires_at: OffsetDateTime,
    ) -> Result<Session, StorageError>;

    async fn find_live_session_by_access_hash(
        &self,
        hash: &[u8],
    ) -> Result<Option<Session>, StorageError>;

    async fn find_live_session_by_refresh_hash(
        &self,
        hash: &[u8],
    ) -> Result<Option<Session>, StorageError>;

    async fn rotate_session(
        &self,
        id: Uuid,
        access_hash: &[u8],
        refresh_hash: &[u8],
        expires_at: OffsetDateTime,
    ) -> Result<(), StorageError>;

    async fn revoke_session(&self, id: Uuid) -> Result<(), StorageError>;

    /// All accounts — for the user directory.
    async fn list_accounts(&self) -> Result<Vec<Account>, StorageError>;

    /// Resolve a device to its account (message routing).
    async fn find_device_by_id(&self, id: Uuid) -> Result<Option<Device>, StorageError>;

    /// Non-revoked devices for an account (fan-out targets).
    async fn list_devices_for_account(&self, account_id: Uuid) -> Result<Vec<Device>, StorageError>;
}

/// Postgres-backed store — delegates to the SQL repository.
pub struct PgAuthStore {
    pub pool: PgPool,
}

#[async_trait]
impl AuthStore for PgAuthStore {
    async fn create_account(&self, id: Uuid, username: &str, verifier: &str) -> Result<Account, StorageError> {
        auth_repo::create_account(&self.pool, id, username, verifier).await
    }
    async fn find_account_by_username(&self, username: &str) -> Result<Option<Account>, StorageError> {
        auth_repo::find_account_by_username(&self.pool, username).await
    }
    async fn create_device(&self, id: Uuid, account_id: Uuid, display_name: &str, public_identity: &[u8]) -> Result<Device, StorageError> {
        auth_repo::create_device(&self.pool, id, account_id, display_name, public_identity).await
    }
    async fn create_session(&self, id: Uuid, device_id: Uuid, access_hash: &[u8], refresh_hash: &[u8], expires_at: OffsetDateTime) -> Result<Session, StorageError> {
        auth_repo::create_session(&self.pool, id, device_id, access_hash, refresh_hash, expires_at).await
    }
    async fn find_live_session_by_access_hash(&self, hash: &[u8]) -> Result<Option<Session>, StorageError> {
        auth_repo::find_live_session_by_access_hash(&self.pool, hash).await
    }
    async fn find_live_session_by_refresh_hash(&self, hash: &[u8]) -> Result<Option<Session>, StorageError> {
        auth_repo::find_live_session_by_refresh_hash(&self.pool, hash).await
    }
    async fn rotate_session(&self, id: Uuid, access_hash: &[u8], refresh_hash: &[u8], expires_at: OffsetDateTime) -> Result<(), StorageError> {
        auth_repo::rotate_session(&self.pool, id, access_hash, refresh_hash, expires_at).await
    }
    async fn revoke_session(&self, id: Uuid) -> Result<(), StorageError> {
        auth_repo::revoke_session(&self.pool, id).await
    }
    async fn list_accounts(&self) -> Result<Vec<Account>, StorageError> {
        auth_repo::list_accounts(&self.pool).await
    }
    async fn find_device_by_id(&self, id: Uuid) -> Result<Option<Device>, StorageError> {
        auth_repo::find_device_by_id(&self.pool, id).await
    }
    async fn list_devices_for_account(&self, account_id: Uuid) -> Result<Vec<Device>, StorageError> {
        auth_repo::list_devices_for_account(&self.pool, account_id).await
    }
}

/// In-memory store — no database required. State lives for the process lifetime.
#[derive(Default)]
pub struct MemoryAuthStore {
    accounts: Mutex<Vec<Account>>,
    devices: Mutex<Vec<Device>>,
    sessions: Mutex<Vec<Session>>,
}

impl MemoryAuthStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the full store (for dev persistence).
    pub fn export(&self) -> (Vec<Account>, Vec<Device>, Vec<Session>) {
        (
            self.accounts.lock().unwrap().clone(),
            self.devices.lock().unwrap().clone(),
            self.sessions.lock().unwrap().clone(),
        )
    }

    /// Replace the store contents from a snapshot (for dev persistence).
    pub fn import(&self, accounts: Vec<Account>, devices: Vec<Device>, sessions: Vec<Session>) {
        *self.accounts.lock().unwrap() = accounts;
        *self.devices.lock().unwrap() = devices;
        *self.sessions.lock().unwrap() = sessions;
    }
}

#[async_trait]
impl AuthStore for MemoryAuthStore {
    async fn create_account(&self, id: Uuid, username: &str, verifier: &str) -> Result<Account, StorageError> {
        let mut accounts = self.accounts.lock().unwrap();
        if accounts.iter().any(|a| a.username == username) {
            return Err(StorageError::Conflict("username taken".into()));
        }
        let account = Account {
            id,
            username: username.to_string(),
            password_verifier: verifier.to_string(),
            created_at: OffsetDateTime::now_utc(),
        };
        accounts.push(account.clone());
        Ok(account)
    }

    async fn find_account_by_username(&self, username: &str) -> Result<Option<Account>, StorageError> {
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.username == username)
            .cloned())
    }

    async fn create_device(&self, id: Uuid, account_id: Uuid, display_name: &str, public_identity: &[u8]) -> Result<Device, StorageError> {
        let device = Device {
            id,
            account_id,
            display_name: display_name.to_string(),
            public_identity: public_identity.to_vec(),
            created_at: OffsetDateTime::now_utc(),
            revoked_at: None,
        };
        self.devices.lock().unwrap().push(device.clone());
        Ok(device)
    }

    async fn create_session(&self, id: Uuid, device_id: Uuid, access_hash: &[u8], refresh_hash: &[u8], expires_at: OffsetDateTime) -> Result<Session, StorageError> {
        let session = Session {
            id,
            device_id,
            access_token_hash: access_hash.to_vec(),
            refresh_token_hash: refresh_hash.to_vec(),
            expires_at,
            revoked_at: None,
            created_at: OffsetDateTime::now_utc(),
        };
        self.sessions.lock().unwrap().push(session.clone());
        Ok(session)
    }

    async fn find_live_session_by_access_hash(&self, hash: &[u8]) -> Result<Option<Session>, StorageError> {
        let now = OffsetDateTime::now_utc();
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.access_token_hash == hash && s.revoked_at.is_none() && s.expires_at > now)
            .cloned())
    }

    async fn find_live_session_by_refresh_hash(&self, hash: &[u8]) -> Result<Option<Session>, StorageError> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.refresh_token_hash == hash && s.revoked_at.is_none())
            .cloned())
    }

    async fn rotate_session(&self, id: Uuid, access_hash: &[u8], refresh_hash: &[u8], expires_at: OffsetDateTime) -> Result<(), StorageError> {
        if let Some(s) = self.sessions.lock().unwrap().iter_mut().find(|s| s.id == id) {
            s.access_token_hash = access_hash.to_vec();
            s.refresh_token_hash = refresh_hash.to_vec();
            s.expires_at = expires_at;
        }
        Ok(())
    }

    async fn revoke_session(&self, id: Uuid) -> Result<(), StorageError> {
        if let Some(s) = self.sessions.lock().unwrap().iter_mut().find(|s| s.id == id) {
            if s.revoked_at.is_none() {
                s.revoked_at = Some(OffsetDateTime::now_utc());
            }
        }
        Ok(())
    }

    async fn list_accounts(&self) -> Result<Vec<Account>, StorageError> {
        Ok(self.accounts.lock().unwrap().clone())
    }

    async fn find_device_by_id(&self, id: Uuid) -> Result<Option<Device>, StorageError> {
        Ok(self.devices.lock().unwrap().iter().find(|d| d.id == id).cloned())
    }

    async fn list_devices_for_account(&self, account_id: Uuid) -> Result<Vec<Device>, StorageError> {
        Ok(self
            .devices
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.account_id == account_id && d.revoked_at.is_none())
            .cloned()
            .collect())
    }
}
