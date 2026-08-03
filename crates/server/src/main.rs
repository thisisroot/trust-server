//! trust-server entrypoint.

mod app;
mod auth;
mod blobs;
mod config;
mod error;
mod hub;
mod media;
mod messaging;
mod persist;
mod profile;
mod ws;

use std::error::Error;
use std::sync::Arc;

use config::Config;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use trust_storage::auth_store::{AuthStore, MemoryAuthStore, PgAuthStore};
use trust_storage::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Logging: RUST_LOG controls verbosity; default to info for our crates.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,trust_server=debug,tower_http=debug")
        }))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::load()?;
    tracing::info!(bind = %config.bind_addr, "starting trust-server");

    // Lazy pool: the server boots even if Postgres isn't up yet (health stays serviceable).
    let db = Db::connect_lazy(&config.database_url, config.max_connections)?;

    // Select the auth store. "memory" runs with no database (local dev) and is
    // snapshotted to disk so data survives restarts; the default "postgres" uses
    // the pool and applies migrations in the background.
    let mem_store: Option<Arc<MemoryAuthStore>>;
    let auth: Arc<dyn AuthStore> = if config.store.eq_ignore_ascii_case("memory") {
        let mem = Arc::new(MemoryAuthStore::new());
        mem_store = Some(mem.clone());
        mem
    } else {
        mem_store = None;
        let pool = db.pool.clone();
        let migrate_db = db.clone();
        tokio::spawn(async move {
            match migrate_db.migrate().await {
                Ok(()) => tracing::info!("database migrations applied"),
                Err(e) => {
                    tracing::warn!(error = %e, "could not apply migrations (is Postgres running?)")
                }
            }
        });
        Arc::new(PgAuthStore { pool })
    };

    let state = app::AppState::new(db, auth);

    // Dev persistence: restore any prior snapshot and keep flushing it to disk.
    if let Some(mem) = mem_store {
        let data_dir = std::env::var("TRUST_DATA_DIR").unwrap_or_else(|_| "trust-data".into());
        tracing::info!(dir = %data_dir, "in-memory store with disk persistence");
        persist::load(&data_dir, &mem, &state);
        persist::spawn_autosave(data_dir, mem, state.clone());
    }

    let router = app::router(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
