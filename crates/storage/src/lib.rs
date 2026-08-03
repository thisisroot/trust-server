//! Postgres storage for trust-server: connection pool, migrations, and (later) repositories.
//!
//! Repositories are added per feature step (auth, keys, messaging). For the skeleton this
//! provides the pool, embedded migrations, and a health ping.

use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod auth_repo;
pub mod auth_store;
pub mod models;

/// Embedded migrations, compiled from `crates/storage/migrations/` at build time (no database
/// connection needed to compile).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// The database handle shared across the app.
#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

impl Db {
    /// Create a lazily-connected pool. Connections are established on first use, so the server
    /// can boot (and serve `/health`) before Postgres is reachable.
    pub fn connect_lazy(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            // Fail fast when the database is unreachable rather than hanging for 30s.
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_lazy(database_url)?;
        Ok(Self { pool })
    }

    /// Apply all pending migrations.
    pub async fn migrate(&self) -> Result<(), StorageError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Round-trip the database to confirm connectivity.
    pub async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
