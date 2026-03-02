//! Configuration and secrets management.
//!
//! Configuration is split into two files:
//! - `config.toml` (0644) - public settings
//! - `secrets.toml` (0600) - sensitive data

use serde::Deserialize;
use std::path::Path;

// =============================================================================
// Public Configuration (config.toml)
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub wireguard: WireGuardConfig,
    #[serde(default)]
    pub bot: Option<BotConfig>,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    /// Allowed CORS origins (e.g., ["https://vpn.example.com"]). Empty = permissive.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireGuardConfig {
    /// Interface name (e.g., "wg-floppa")
    pub interface: String,
    /// Server's WireGuard endpoint (e.g., "vpn.example.com:51820")
    pub endpoint: String,
    /// Listen port for WireGuard (parsed from endpoint if not specified)
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// VPN subnet for client IPs (e.g., "10.100.0.0/24")
    pub client_subnet: String,
    /// Server IP within the subnet (e.g., "10.100.0.1")
    #[serde(default)]
    pub server_ip: Option<String>,
    /// DNS servers for clients
    pub dns: Vec<String>,
    /// Allowed IPs for clients (typically "0.0.0.0/0, ::/0")
    pub allowed_ips: String,
    /// Rate limiting configuration
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
}

impl WireGuardConfig {
    /// Get listen port (from config or parsed from endpoint)
    pub fn get_listen_port(&self) -> u16 {
        self.listen_port.unwrap_or_else(|| {
            self.endpoint
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(51820)
        })
    }

    /// Get server IP (from config or derived from subnet as .1)
    pub fn get_server_ip(&self) -> String {
        self.server_ip.clone().unwrap_or_else(|| {
            let base = self.client_subnet.split('/').next().unwrap_or("10.100.0.0");
            let parts: Vec<&str> = base.split('.').collect();
            if parts.len() == 4 {
                format!("{}.{}.{}.1", parts[0], parts[1], parts[2])
            } else {
                "10.100.0.1".to_string()
            }
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Enable traffic control rate limiting
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Total available bandwidth in Mbps (for the tc root class)
    #[serde(default = "default_total_bandwidth")]
    pub total_bandwidth_mbps: u32,
}

fn default_enabled() -> bool {
    true
}

fn default_total_bandwidth() -> u32 {
    1000 // 1 Gbps default
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BotConfig {
    /// Bot username (without @) for Telegram Login Widget
    pub username: Option<String>,
    /// Public URL where floppa-face is served (for Telegram Mini App)
    pub web_app_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    /// JWT token expiration in hours (default: 24 * 7 = 1 week)
    #[serde(default = "default_jwt_expiration_hours")]
    pub jwt_expiration_hours: u64,
}

fn default_jwt_expiration_hours() -> u64 {
    24 * 7 // 1 week
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let path =
            std::env::var("FLOPPA_CONFIG").unwrap_or_else(|_| "/etc/floppa-vpn/config.toml".into());
        Self::load(path)
    }
}

// =============================================================================
// Secrets (secrets.toml)
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct Secrets {
    /// PostgreSQL connection URL
    pub database_url: String,
    /// WireGuard server private key (base64)
    pub wg_private_key: String,
    #[serde(default)]
    pub bot: Option<BotSecrets>,
    #[serde(default)]
    pub auth: Option<AuthSecrets>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BotSecrets {
    /// Telegram bot token from @BotFather
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthSecrets {
    /// Secret key for signing JWT tokens (hex-encoded, 32 bytes)
    pub jwt_secret: String,
    /// Key for encrypting WireGuard private keys at rest (hex-encoded, 32 bytes)
    pub encryption_key: String,
    /// Telegram user IDs that are automatically admins
    #[serde(default)]
    pub admin_telegram_ids: Vec<i64>,
}

impl AuthSecrets {
    /// Parse and return the encryption key as bytes
    pub fn get_encryption_key(&self) -> Result<[u8; 32], crate::crypto::CryptoError> {
        crate::crypto::parse_encryption_key(&self.encryption_key)
    }
}

impl Secrets {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let secrets: Secrets = toml::from_str(&content)?;
        Ok(secrets)
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let path = std::env::var("FLOPPA_SECRETS")
            .unwrap_or_else(|_| "/etc/floppa-vpn/secrets.toml".into());
        Self::load(path)
    }

    /// Derive WireGuard public key from private key using x25519
    pub fn wg_public_key(&self) -> Result<String, ConfigError> {
        use base64::prelude::*;
        use x25519_dalek::{PublicKey, StaticSecret};

        let private_bytes = BASE64_STANDARD
            .decode(self.wg_private_key.trim())
            .map_err(|e| ConfigError::InvalidKey(format!("Invalid base64: {}", e)))?;

        if private_bytes.len() != 32 {
            return Err(ConfigError::InvalidKey(format!(
                "Private key must be 32 bytes, got {}",
                private_bytes.len()
            )));
        }

        let mut key_array = [0u8; 32];
        key_array.copy_from_slice(&private_bytes);

        let secret = StaticSecret::from(key_array);
        let public = PublicKey::from(&secret);

        Ok(BASE64_STANDARD.encode(public.as_bytes()))
    }
}

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Invalid key: {0}")]
    InvalidKey(String),
}
