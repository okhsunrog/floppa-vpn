//! floppa-vless server configuration.
//!
//! Separate from floppa-core's Config because floppa-vless runs on
//! the EU VPS with different requirements (no WireGuard, no bot, no auth).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct VlessServerConfig {
    pub server: ServerSection,
    pub reality: RealitySection,
    #[serde(default)]
    pub traffic: TrafficSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    /// Listen address (e.g., "0.0.0.0:443")
    pub listen_addr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RealitySection {
    /// SNI hostname for REALITY camouflage (e.g., "www.microsoft.com")
    pub sni: String,
    /// REALITY short IDs (hex strings)
    pub short_ids: Vec<String>,
    /// Camouflage destination (e.g., "www.microsoft.com:443")
    pub dest: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrafficSection {
    /// How often to flush traffic stats to DB (in seconds)
    #[serde(default = "default_flush_interval")]
    pub flush_interval_secs: u64,
    /// How often to do a full registry sync from DB (in seconds)
    #[serde(default = "default_sync_interval")]
    pub sync_interval_secs: u64,
}

impl Default for TrafficSection {
    fn default() -> Self {
        Self {
            flush_interval_secs: default_flush_interval(),
            sync_interval_secs: default_sync_interval(),
        }
    }
}

fn default_flush_interval() -> u64 {
    30
}

fn default_sync_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct VlessServerSecrets {
    /// PostgreSQL connection URL
    pub database_url: String,
    /// REALITY x25519 private key (base64)
    pub reality_private_key: String,
}

impl VlessServerConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let path = std::env::var("FLOPPA_VLESS_CONFIG")
            .unwrap_or_else(|_| "/etc/floppa-vless/config.toml".into());
        Self::load(path)
    }
}

impl VlessServerSecrets {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let path = std::env::var("FLOPPA_VLESS_SECRETS")
            .unwrap_or_else(|_| "/etc/floppa-vless/secrets.toml".into());
        Self::load(path)
    }
}
