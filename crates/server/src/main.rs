//! trust-server entrypoint.

mod app;
mod auth;
mod blobs;
mod config;
mod db;
mod error;
mod media;
mod messaging;
mod presence;
mod profile;
mod ws;

use std::error::Error;
use std::sync::Arc;

use config::Config;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use trust_storage::auth_store::{AuthStore, PgAuthStore};
use trust_storage::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,trust_server=debug,tower_http=debug")
        }))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::load()?;
    tracing::info!(bind = %config.bind_addr, "starting trust-server");

    // Postgres backs all durable state. Lazy pool so the server can boot and
    // serve /health even before the database is reachable.
    let db = Db::connect_lazy(&config.database_url, config.max_connections)?;

    // Apply migrations in the background; retry until the database is reachable.
    {
        let migrate_db = db.clone();
        tokio::spawn(async move {
            let mut attempt = 0u32;
            loop {
                match migrate_db.migrate().await {
                    Ok(()) => {
                        tracing::info!("database migrations applied");
                        break;
                    }
                    Err(e) => {
                        attempt += 1;
                        tracing::warn!(error = %e, attempt, "migrations failed (is Postgres up?); retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                }
            }
        });
    }

    let auth: Arc<dyn AuthStore> = Arc::new(PgAuthStore { pool: db.pool.clone() });
    let state = app::AppState::new(db, auth);
    let router = app::router(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
