//! Server configuration, layered: built-in defaults < `trust-server.toml` < `TRUST_*` env vars.

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Address to bind the HTTP/WebSocket listener to.
    pub bind_addr: String,
    /// Postgres connection string.
    pub database_url: String,
    /// Max pooled database connections.
    pub max_connections: u32,
    /// Auth store backend: "postgres" (default) or "memory" (no database, for
    /// local dev). Set with `TRUST_STORE=memory`.
    pub store: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".to_string(),
            database_url: "postgres://trust:trust@localhost:5432/trust".to_string(),
            max_connections: 5,
            store: "postgres".to_string(),
        }
    }
}

impl Config {
    /// Load configuration from defaults, an optional `trust-server.toml`, and `TRUST_` env vars
    /// (e.g. `TRUST_BIND_ADDR`, `TRUST_DATABASE_URL`).
    pub fn load() -> Result<Self, figment::Error> {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file("trust-server.toml"))
            .merge(Env::prefixed("TRUST_"))
            .extract()
    }
}
