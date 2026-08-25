//! Configuration and secrets management.
//!
//! Configuration is split into two files:
//! - `config.toml` (0644) - public settings
//! - `secrets.toml` (0600) - sensitive data

use ipnetwork::Ipv4Network;
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::path::Path;
use veil::Redact;

// =============================================================================
// Public Configuration (config.toml)
// =============================================================================

/// Both files are parsed strictly: a key the schema does not know — a typo, or a key that landed
/// in the wrong table — is an error rather than a silently ignored setting.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub wireguard: TunnelInterfaceConfig,
    /// AmneziaWG configuration (optional — only needed if AmneziaWG is offered).
    /// AmneziaWG is WireGuard plus interface-wide obfuscation params; it runs on its
    /// own interface/port/subnet on the same daemon.
    #[serde(default)]
    pub amneziawg: Option<TunnelInterfaceConfig>,
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

/// One WireGuard-family server interface: the `[wireguard]` and `[amneziawg]` sections share
/// this shape, differing only in the two AmneziaWG-only keys.
///
/// AmneziaWG is WireGuard plus interface-wide obfuscation parameters (junk packets, padding,
/// magic headers, signature packets). Both interfaces are managed by floppa-daemon (kernel
/// `wireguard`/`amneziawg` module + `wg`/`awg` tooling), and the obfuscation params are echoed
/// verbatim into each AmneziaWG client's `.conf` so both ends agree — they are the single
/// source of truth.
///
/// [`Config::parse`] settles the AmneziaWG-only keys per section: under `[wireguard]` they are
/// rejected, under `[amneziawg]` a missing one gets its default. After loading, `obfuscation`
/// is therefore `Some` exactly for the AmneziaWG interface, which is what
/// [`Self::is_amneziawg`] reports.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelInterfaceConfig {
    /// Interface name (e.g., "wg-floppa" / "awg-floppa")
    pub interface: String,
    /// Server endpoint as seen by clients (e.g., "vpn.example.com:51820")
    pub endpoint: String,
    /// Listen port (parsed from endpoint if not specified)
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// VPN subnet for client IPs (e.g., "10.100.0.0/24"); the two interfaces need distinct ones
    pub client_subnet: Ipv4Network,
    /// Server IP within the subnet (e.g., "10.100.0.1"). Reserved by the client IP allocator.
    #[serde(default)]
    pub server_ip: Option<Ipv4Addr>,
    /// DNS servers for clients
    pub dns: Vec<String>,
    /// Allowed IPs for clients (typically "0.0.0.0/0, ::/0")
    pub allowed_ips: String,
    /// Rate limiting configuration (the same tc machinery on either interface)
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    /// Client MTU, AmneziaWG only: padding/junk adds overhead, so it sits below plain WG's
    /// (default [`DEFAULT_AWG_MTU`]). Rendered into client configs when set.
    #[serde(default)]
    pub mtu: Option<u16>,
    /// Obfuscation parameters (AmneziaWG 2.0), AmneziaWG only. Defaults to the recommended
    /// preset.
    #[serde(default)]
    pub obfuscation: Option<AwgObfuscation>,
}

/// Default client MTU for AmneziaWG (`[amneziawg] mtu`).
pub const DEFAULT_AWG_MTU: u16 = 1280;

/// Listen port assumed when neither `listen_port` nor the endpoint names one.
const DEFAULT_WG_PORT: u16 = 51820;
const DEFAULT_AWG_PORT: u16 = 51821;

/// Which section of `config.toml` a [`TunnelInterfaceConfig`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunnelSection {
    WireGuard,
    AmneziaWg,
}

impl TunnelInterfaceConfig {
    /// Whether this is the AmneziaWG interface (see the type-level note on `obfuscation`).
    pub fn is_amneziawg(&self) -> bool {
        self.obfuscation.is_some()
    }

    /// Get listen port (from config, parsed from endpoint, or the protocol's usual port)
    pub fn get_listen_port(&self) -> u16 {
        self.listen_port.unwrap_or_else(|| {
            self.endpoint
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(if self.is_amneziawg() {
                    DEFAULT_AWG_PORT
                } else {
                    DEFAULT_WG_PORT
                })
        })
    }

    /// Server IP: from config, or the first host address of `client_subnet` (the ".1").
    pub fn get_server_ip(&self) -> Ipv4Addr {
        self.server_ip
            .unwrap_or_else(|| default_server_ip(self.client_subnet))
    }

    /// Settle the AmneziaWG-only keys for the section this interface was read from: rejected
    /// under `[wireguard]`, defaulted under `[amneziawg]`.
    fn settle_section(&mut self, section: TunnelSection) -> Result<(), ConfigError> {
        match section {
            TunnelSection::WireGuard => {
                let key = match (&self.mtu, &self.obfuscation) {
                    (Some(_), _) => "mtu",
                    (None, Some(_)) => "obfuscation",
                    (None, None) => return Ok(()),
                };
                Err(ConfigError::AmneziaWgOnlyKey { key })
            }
            TunnelSection::AmneziaWg => {
                self.mtu.get_or_insert(DEFAULT_AWG_MTU);
                self.obfuscation.get_or_insert_default();
                Ok(())
            }
        }
    }
}

/// First host address of the subnet (e.g. 10.100.0.1 for 10.100.0.0/24).
fn default_server_ip(subnet: Ipv4Network) -> Ipv4Addr {
    subnet.nth(1).unwrap_or_else(|| subnet.network())
}

/// AmneziaWG 2.0 obfuscation parameters, shared with the clients (they parse the same values back
/// out of the configs this crate renders). Re-exported so `floppa_core::config::AwgObfuscation`
/// keeps working for the daemon and server.
///
/// Deliberately not `deny_unknown_fields`, unlike every section here: the Tauri client persists
/// the same struct in its saved-config store, and a strict schema would make an older build
/// refuse a store written by a newer one.
pub use floppa_tunnel_config::AwgObfuscation;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
}

fn default_vless_flow() -> String {
    "xtls-rprx-vision".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    /// Bot username (without @) for Telegram Login Widget
    pub username: Option<String>,
    /// Public URL where floppa-face is served (for Telegram Mini App)
    pub web_app_url: Option<url::Url>,
    /// Approximate Stars-to-RUB rate for displaying ruble equivalent in /buy (e.g. 1.8)
    pub stars_rub_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// The same values serde fills in for keys missing from `[auth]`, so a config without the
/// section behaves exactly like an empty one (`derive(Default)` would have produced zeros:
/// tokens expiring at issue time and every login rate-limited).
impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_expiration_hours: default_jwt_expiration_hours(),
            register_rate_limit_per_hour: default_register_rate_limit_per_hour(),
            login_rate_limit_per_15min: default_login_rate_limit_per_15min(),
        }
    }
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
#[serde(deny_unknown_fields)]
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
        Self::parse(&content)
    }

    /// Parse `config.toml` text and settle the per-section rules `toml` alone cannot express
    /// (see [`TunnelInterfaceConfig`]). This is the only supported way to build a `Config`
    /// from text; a bare `toml::from_str` leaves an `[amneziawg]` section without its defaults.
    pub fn parse(toml: &str) -> Result<Self, ConfigError> {
        let mut config: Config = toml::from_str(toml)?;
        config.wireguard.settle_section(TunnelSection::WireGuard)?;
        if let Some(awg) = &mut config.amneziawg {
            awg.settle_section(TunnelSection::AmneziaWg)?;
        }
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct VlessSecrets {
    /// REALITY x25519 public key (base64), embedded in client `vless://` URIs
    pub reality_public_key: String,
    /// REALITY x25519 private key (base64), used by floppa-vless server only
    #[redact]
    pub reality_private_key: String,
}

#[derive(Redact, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotSecrets {
    /// Telegram bot token from @BotFather
    #[redact]
    pub token: String,
}

#[derive(Redact, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// An AmneziaWG-only key (`mtu`, `obfuscation`) under the `[wireguard]` section.
    #[error("`{key}` under [wireguard] is an [amneziawg]-only setting")]
    AmneziaWgOnlyKey { key: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG_EXAMPLE: &str = include_str!("../../config.example.toml");
    const SECRETS_EXAMPLE: &str = include_str!("../../secrets.example.toml");

    /// Uncomment every `# key = value` and `# [table]` line, leaving prose comments (and doubly
    /// commented `# # key = ...` lines) alone — i.e. enable every optional section of an example.
    fn uncomment_optional(example: &str) -> String {
        example
            .lines()
            .map(|line| {
                let Some(rest) = line.strip_prefix("# ") else {
                    return line;
                };
                let is_table = rest.starts_with('[');
                let is_key = rest.split_once(" = ").is_some_and(|(key, _)| {
                    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if is_table || is_key { rest } else { line }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn config_example_parses_as_is() {
        let config = Config::parse(CONFIG_EXAMPLE).unwrap();
        assert_eq!(config.wireguard.interface, "wg-floppa");
        assert_eq!(
            config.wireguard.client_subnet,
            "10.100.0.0/24".parse::<Ipv4Network>().unwrap()
        );
        assert_eq!(
            config.wireguard.get_server_ip(),
            Ipv4Addr::new(10, 100, 0, 1)
        );
        assert!(!config.wireguard.is_amneziawg());
        assert_eq!(config.wireguard.get_listen_port(), 51820);
        assert!(config.amneziawg.is_none());
        assert!(config.vless.is_none());
        assert!(config.metrics.is_none());
        assert!(config.min_client_version.is_none());
        assert_eq!(
            config.bot.as_ref().and_then(|b| b.username.as_deref()),
            Some("YourBotUsername")
        );
        assert_eq!(config.auth.map(|a| a.jwt_expiration_hours), Some(168));
    }

    #[test]
    fn config_example_parses_with_every_optional_key_enabled() {
        let toml = uncomment_optional(CONFIG_EXAMPLE);
        let config = Config::parse(&toml).unwrap();

        // Top-level optional keys must land at the top level, not inside the last [table].
        assert_eq!(config.min_client_version.as_deref(), Some("0.2.0"));
        assert_eq!(config.allowed_origins, vec!["https://vpn.example.com"]);

        let awg = config.amneziawg.expect("[amneziawg] enabled");
        assert_eq!(
            awg.client_subnet,
            "10.101.0.0/24".parse::<Ipv4Network>().unwrap()
        );
        assert_eq!(awg.get_server_ip(), Ipv4Addr::new(10, 101, 0, 1));
        assert!(awg.is_amneziawg());
        assert_eq!(awg.get_listen_port(), 51821);
        assert_eq!(awg.mtu, Some(1280));
        let obfuscation = awg
            .obfuscation
            .as_ref()
            .expect("[amneziawg.obfuscation] enabled");
        assert_eq!(obfuscation.jc, 6);
        assert_eq!(obfuscation.h1, "234567-345678");

        let auth = config.auth.expect("[auth] present");
        assert_eq!(auth, AuthConfig::default());

        let bot = config.bot.expect("[bot] present");
        assert_eq!(
            bot.web_app_url.as_ref().map(url::Url::as_str),
            Some("https://vpn.example.com/")
        );
        assert_eq!(bot.stars_rub_rate, Some(1.8));

        assert!(config.vless.is_some());
        assert_eq!(
            config.metrics.map(|m| m.victoria_metrics_url).as_deref(),
            Some("http://127.0.0.1:8428")
        );
    }

    #[test]
    fn secrets_example_parses_as_is_and_fully_enabled() {
        let secrets: Secrets = toml::from_str(SECRETS_EXAMPLE).unwrap();
        assert!(secrets.awg_private_key.is_none());
        assert!(secrets.vless.is_none());
        assert_eq!(
            secrets.auth.map(|a| a.admin_telegram_ids),
            Some(vec![123456789])
        );

        let secrets: Secrets = toml::from_str(&uncomment_optional(SECRETS_EXAMPLE)).unwrap();
        assert!(secrets.awg_private_key.is_some());
        assert!(secrets.vless.is_some());
    }

    const MINIMAL_CONFIG: &str = r#"
        [wireguard]
        interface = "wg0"
        endpoint = "vpn.example.com:51820"
        client_subnet = "10.100.0.0/24"
        dns = ["1.1.1.1"]
        allowed_ips = "0.0.0.0/0"
    "#;

    #[test]
    fn auth_defaults_match_serde_defaults() {
        // A config without [auth] falls back to AuthConfig::default() in the server; it must be
        // the same thing as an empty [auth] table.
        let config = Config::parse(MINIMAL_CONFIG).unwrap();
        assert!(config.auth.is_none());

        let empty_section: AuthConfig = toml::from_str("").unwrap();
        let defaults = AuthConfig::default();
        assert_eq!(empty_section, defaults);
        assert_eq!(defaults.jwt_expiration_hours, 24 * 7);
        assert_eq!(defaults.register_rate_limit_per_hour, 5);
        assert_eq!(defaults.login_rate_limit_per_15min, 10);
    }

    #[test]
    fn invalid_client_subnet_is_a_parse_error() {
        let err =
            Config::parse(&MINIMAL_CONFIG.replace("10.100.0.0/24", "not-a-subnet")).unwrap_err();
        assert!(err.to_string().contains("client_subnet"), "{err}");
    }

    #[test]
    fn unknown_keys_are_rejected_by_name() {
        // A top-level key that slipped under a table header — the classic silent misconfig.
        let toml = format!("{MINIMAL_CONFIG}\n[auth]\nmin_client_version = \"0.2.0\"\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "{err:?}");
        assert!(err.to_string().contains("min_client_version"), "{err}");

        // Typos in nested tables and in secrets are caught the same way.
        let toml = format!("{MINIMAL_CONFIG}\n[wireguard.rate_limit]\ntotal_bandwidth = 1000\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(err.to_string().contains("total_bandwidth"), "{err}");

        let secrets = format!("{SECRETS_EXAMPLE}\njwt_secrett = \"x\"\n");
        let err = toml::from_str::<Secrets>(&secrets).unwrap_err();
        assert!(err.to_string().contains("jwt_secrett"), "{err}");
    }

    #[test]
    fn amneziawg_only_keys_are_rejected_under_wireguard() {
        let toml = format!("{MINIMAL_CONFIG}mtu = 1280\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(
            matches!(err, ConfigError::AmneziaWgOnlyKey { key: "mtu" }),
            "{err:?}"
        );

        let toml = format!("{MINIMAL_CONFIG}[wireguard.obfuscation]\njc = 6\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(
            matches!(err, ConfigError::AmneziaWgOnlyKey { key: "obfuscation" }),
            "{err:?}"
        );
    }

    #[test]
    fn amneziawg_section_gets_the_default_mtu_and_preset() {
        let toml = format!(
            "{MINIMAL_CONFIG}\n[amneziawg]\ninterface = \"awg0\"\nendpoint = \"vpn.example.com\"\n\
             client_subnet = \"10.101.0.0/24\"\ndns = [\"1.1.1.1\"]\nallowed_ips = \"0.0.0.0/0\"\n"
        );
        let config = Config::parse(&toml).unwrap();
        let awg = config.amneziawg.expect("[amneziawg] present");
        assert!(awg.is_amneziawg());
        assert_eq!(awg.mtu, Some(DEFAULT_AWG_MTU));
        assert_eq!(awg.obfuscation, Some(AwgObfuscation::default()));
        // No port in the endpoint → the AmneziaWG default, not WireGuard's.
        assert_eq!(awg.get_listen_port(), 51821);
        assert!(!config.wireguard.is_amneziawg());
    }
}
