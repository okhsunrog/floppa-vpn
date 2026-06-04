//! Configuration and secrets management.
//!
//! Configuration is split into two files:
//! - `config.toml` (0644) - public settings
//! - `secrets.toml` (0600) - sensitive data

use serde::Deserialize;
use std::path::Path;
use veil::Redact;

// =============================================================================
// Public Configuration (config.toml)
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub wireguard: WireGuardConfig,
    /// AmneziaWG configuration (optional — only needed if AmneziaWG is offered).
    /// AmneziaWG is WireGuard plus interface-wide obfuscation params; it runs on its
    /// own interface/port/subnet on the same daemon.
    #[serde(default)]
    pub amneziawg: Option<AmneziaWgConfig>,
    /// VLESS+REALITY configuration (optional — only needed if VLESS is offered)
    #[serde(default)]
    pub vless: Option<VlessConfig>,
    #[serde(default)]
    pub bot: Option<BotConfig>,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    /// Allowed CORS origins (e.g., ["https://vpn.example.com"]). Empty = permissive.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Minimum client version required (semver, e.g. "0.2.0"). Older clients get 426.
    #[serde(default)]
    pub min_client_version: Option<String>,
    /// Metrics / observability configuration
    #[serde(default)]
    pub metrics: Option<MetricsConfig>,
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

/// AmneziaWG configuration. AmneziaWG is WireGuard plus interface-wide obfuscation
/// parameters (junk packets, padding, magic headers, signature packets). The server
/// interface is managed by floppa-daemon (kernel `amneziawg` module + `awg` tooling),
/// and the obfuscation params are echoed verbatim into each client's `.conf` so both
/// ends agree — they are the single source of truth.
#[derive(Debug, Clone, Deserialize)]
pub struct AmneziaWgConfig {
    /// Interface name (e.g., "awg-floppa")
    pub interface: String,
    /// Server's AmneziaWG endpoint (e.g., "vpn.example.com:51821")
    pub endpoint: String,
    /// Listen port (parsed from endpoint if not specified)
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// VPN subnet for client IPs (e.g., "10.101.0.0/24") — must differ from the WireGuard subnet
    pub client_subnet: String,
    /// Server IP within the subnet (e.g., "10.101.0.1")
    #[serde(default)]
    pub server_ip: Option<String>,
    /// DNS servers for clients
    pub dns: Vec<String>,
    /// Allowed IPs for clients (typically "0.0.0.0/0, ::/0")
    pub allowed_ips: String,
    /// Client MTU. AmneziaWG padding/junk adds overhead, so this is lower than plain WG.
    #[serde(default = "default_awg_mtu")]
    pub mtu: u16,
    /// Rate limiting configuration (shared tc machinery with WireGuard)
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    /// Obfuscation parameters (AmneziaWG 2.0). Defaults to the recommended preset.
    #[serde(default)]
    pub obfuscation: AwgObfuscation,
}

fn default_awg_mtu() -> u16 {
    1280
}

impl AmneziaWgConfig {
    /// Get listen port (from config or parsed from endpoint)
    pub fn get_listen_port(&self) -> u16 {
        self.listen_port.unwrap_or_else(|| {
            self.endpoint
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(51821)
        })
    }

    /// Get server IP (from config or derived from subnet as .1)
    pub fn get_server_ip(&self) -> String {
        self.server_ip.clone().unwrap_or_else(|| {
            let base = self.client_subnet.split('/').next().unwrap_or("10.101.0.0");
            let parts: Vec<&str> = base.split('.').collect();
            if parts.len() == 4 {
                format!("{}.{}.{}.1", parts[0], parts[1], parts[2])
            } else {
                "10.101.0.1".to_string()
            }
        })
    }
}

/// AmneziaWG 2.0 obfuscation parameters. Defaults match the recommended preset.
///
/// `H1`–`H4` and `S1`–`S4` are bidirectional and must match on both ends. `Jc`/`Jmin`/`Jmax`
/// (junk packets) and `I1`–`I5` (signature packets) are initiator-only (sent by the client),
/// but we store the full set centrally and apply it to both the server interface and clients.
#[derive(Debug, Clone, Deserialize)]
pub struct AwgObfuscation {
    /// Junk packet count sent before the handshake (initiator only).
    #[serde(default = "awg_default_jc")]
    pub jc: u32,
    /// Minimum junk packet size in bytes.
    #[serde(default = "awg_default_jmin")]
    pub jmin: u32,
    /// Maximum junk packet size in bytes (keep below MTU).
    #[serde(default = "awg_default_jmax")]
    pub jmax: u32,
    /// Padding prepended to Handshake Initiation.
    #[serde(default = "awg_default_s1")]
    pub s1: u32,
    /// Padding prepended to Handshake Response.
    #[serde(default = "awg_default_s2")]
    pub s2: u32,
    /// Padding prepended to Cookie Reply (AmneziaWG 2.0).
    #[serde(default = "awg_default_s3")]
    pub s3: u32,
    /// Padding prepended to Transport Data (AmneziaWG 2.0).
    #[serde(default = "awg_default_s4")]
    pub s4: u32,
    /// Magic header for Handshake Initiation. A single value ("1") or range ("234567-345678").
    #[serde(default = "awg_default_h1")]
    pub h1: String,
    /// Magic header for Handshake Response.
    #[serde(default = "awg_default_h2")]
    pub h2: String,
    /// Magic header for Cookie Reply.
    #[serde(default = "awg_default_h3")]
    pub h3: String,
    /// Magic header for Transport Data.
    #[serde(default = "awg_default_h4")]
    pub h4: String,
    /// Signature packet 1 (AmneziaWG 2.0 CPS) — protocol-mimicry tag spec. Empty = unset.
    #[serde(default = "awg_default_i1")]
    pub i1: String,
    /// Signature packets 2–5. Empty = unset.
    #[serde(default)]
    pub i2: String,
    #[serde(default)]
    pub i3: String,
    #[serde(default)]
    pub i4: String,
    #[serde(default)]
    pub i5: String,
}

// Recommended AmneziaWG 2.0 preset (Amnezia default preset).
fn awg_default_jc() -> u32 {
    6
}
fn awg_default_jmin() -> u32 {
    55
}
fn awg_default_jmax() -> u32 {
    205
}
fn awg_default_s1() -> u32 {
    72
}
fn awg_default_s2() -> u32 {
    56
}
fn awg_default_s3() -> u32 {
    32
}
fn awg_default_s4() -> u32 {
    16
}
fn awg_default_h1() -> String {
    "234567-345678".to_string()
}
fn awg_default_h2() -> String {
    "3456789-4567890".to_string()
}
fn awg_default_h3() -> String {
    "56789012-67890123".to_string()
}
fn awg_default_h4() -> String {
    "456789012-567890123".to_string()
}
/// QUIC v1 long-header mimic for the first signature packet.
fn awg_default_i1() -> String {
    "<b 0xc30000000108><r 8><b 0x08><r 8><b 0x0045dc><t><r 16>".to_string()
}

impl Default for AwgObfuscation {
    fn default() -> Self {
        Self {
            jc: awg_default_jc(),
            jmin: awg_default_jmin(),
            jmax: awg_default_jmax(),
            s1: awg_default_s1(),
            s2: awg_default_s2(),
            s3: awg_default_s3(),
            s4: awg_default_s4(),
            h1: awg_default_h1(),
            h2: awg_default_h2(),
            h3: awg_default_h3(),
            h4: awg_default_h4(),
            i1: awg_default_i1(),
            i2: String::new(),
            i3: String::new(),
            i4: String::new(),
            i5: String::new(),
        }
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

/// VLESS+REALITY configuration for client config generation.
/// The actual VLESS server runs as a separate binary (floppa-vless) on the EU VPS;
/// this section provides the parameters needed to construct `vless://` URIs.
#[derive(Debug, Clone, Deserialize)]
pub struct VlessConfig {
    /// VLESS+REALITY endpoint for client configs (e.g., "eu.example.com:443")
    pub endpoint: String,
    /// SNI hostname for REALITY (e.g., "www.microsoft.com")
    pub sni: String,
    /// REALITY short ID (hex string)
    pub short_id: String,
    /// Flow control (default: "xtls-rprx-vision")
    #[serde(default = "default_vless_flow")]
    pub flow: String,
    /// DNS servers for client configs
    pub dns: Vec<String>,
    /// Allowed IPs for client configs (typically "0.0.0.0/0, ::/0")
    pub allowed_ips: String,
}

fn default_vless_flow() -> String {
    "xtls-rprx-vision".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BotConfig {
    /// Bot username (without @) for Telegram Login Widget
    pub username: Option<String>,
    /// Public URL where floppa-face is served (for Telegram Mini App)
    pub web_app_url: Option<String>,
    /// Approximate Stars-to-RUB rate for displaying ruble equivalent in /buy (e.g. 1.8)
    pub stars_rub_rate: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    /// JWT token expiration in hours (default: 24 * 7 = 1 week)
    #[serde(default = "default_jwt_expiration_hours")]
    pub jwt_expiration_hours: u64,
    /// Max account-registration attempts per IP per hour.
    #[serde(default = "default_register_rate_limit_per_hour")]
    pub register_rate_limit_per_hour: u32,
    /// Max credential-login attempts per IP per 15 minutes.
    #[serde(default = "default_login_rate_limit_per_15min")]
    pub login_rate_limit_per_15min: u32,
}

fn default_jwt_expiration_hours() -> u64 {
    24 * 7 // 1 week
}

fn default_register_rate_limit_per_hour() -> u32 {
    5
}

fn default_login_rate_limit_per_15min() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// VictoriaMetrics query URL (default: http://127.0.0.1:8428)
    #[serde(default = "default_vm_url")]
    pub victoria_metrics_url: String,
}

fn default_vm_url() -> String {
    "http://127.0.0.1:8428".to_string()
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

#[derive(Redact, Clone, Deserialize)]
pub struct Secrets {
    /// PostgreSQL connection URL
    #[redact]
    pub database_url: String,
    /// WireGuard server private key (base64)
    #[redact]
    pub wg_private_key: String,
    /// AmneziaWG server private key (base64). Optional — only needed if AmneziaWG is offered.
    /// AmneziaWG uses ordinary x25519 keys; only the obfuscation layer differs from WireGuard.
    #[redact]
    #[serde(default)]
    pub awg_private_key: Option<String>,
    #[serde(default)]
    pub bot: Option<BotSecrets>,
    #[serde(default)]
    pub auth: Option<AuthSecrets>,
    /// VLESS REALITY keys (optional — only needed if VLESS is offered)
    #[serde(default)]
    pub vless: Option<VlessSecrets>,
}

#[derive(Redact, Clone, Deserialize)]
pub struct VlessSecrets {
    /// REALITY x25519 public key (base64), embedded in client `vless://` URIs
    pub reality_public_key: String,
    /// REALITY x25519 private key (base64), used by floppa-vless server only
    #[redact]
    pub reality_private_key: String,
}

#[derive(Redact, Clone, Deserialize)]
pub struct BotSecrets {
    /// Telegram bot token from @BotFather
    #[redact]
    pub token: String,
}

#[derive(Redact, Clone, Deserialize)]
pub struct AuthSecrets {
    /// Secret key for signing JWT tokens (hex-encoded, 32 bytes)
    #[redact]
    pub jwt_secret: String,
    /// Key for encrypting WireGuard private keys at rest (hex-encoded, 32 bytes)
    #[redact]
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
        derive_x25519_public(&self.wg_private_key)
    }

    /// Derive the AmneziaWG server public key. Errors if `awg_private_key` is unset.
    pub fn awg_public_key(&self) -> Result<String, ConfigError> {
        let key = self
            .awg_private_key
            .as_deref()
            .ok_or_else(|| ConfigError::InvalidKey("awg_private_key is not configured".into()))?;
        derive_x25519_public(key)
    }
}

/// Derive an x25519 public key (base64) from a base64 private key.
fn derive_x25519_public(private_key_b64: &str) -> Result<String, ConfigError> {
    use base64::prelude::*;
    use x25519_dalek::{PublicKey, StaticSecret};

    let private_bytes = BASE64_STANDARD
        .decode(private_key_b64.trim())
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
