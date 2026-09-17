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
/// this shape, differing only in the AmneziaWG-only `obfuscation` key and in what a missing
/// `mtu` means.
///
/// AmneziaWG is WireGuard plus interface-wide obfuscation parameters (junk packets, padding,
/// magic headers, signature packets). Both interfaces are managed by floppa-daemon (kernel
/// `wireguard`/`amneziawg` module + `wg`/`awg` tooling), and the obfuscation params are echoed
/// verbatim into each AmneziaWG client's `.conf` so both ends agree — they are the single
/// source of truth.
///
/// [`Config::parse`] settles those keys per section: `obfuscation` is rejected under
/// `[wireguard]` and defaulted under `[amneziawg]`, and `mtu` is optional under `[wireguard]`
/// but defaulted under `[amneziawg]`, where the obfuscation overhead makes plain WireGuard's
/// 1420 too generous. After loading, `obfuscation` is therefore `Some` exactly for the
/// AmneziaWG interface, which is what [`Self::is_amneziawg`] reports.
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
    /// Tunnel MTU: rendered into client configs and applied to the server interface's link
    /// when set.
    ///
    /// Under `[amneziawg]` it defaults to [`DEFAULT_AWG_MTU`], because padding and junk add
    /// overhead on top of WireGuard's own. Under `[wireguard]` there is no default: a client
    /// that is handed no `MTU` line falls back to WireGuard's 1420 and the kernel picks the
    /// interface's own, which is what every deployment has been running. Set it where the last
    /// mile is shorter than 1500 (PPPoE, LTE, a tunnel underneath) and ICMP
    /// fragmentation-needed does not come back.
    #[serde(default)]
    pub mtu: Option<u16>,
    /// Obfuscation parameters (AmneziaWG 2.0), AmneziaWG only. Defaults to the recommended
    /// preset.
    #[serde(default)]
    pub obfuscation: Option<AwgObfuscation>,
}

/// Default client MTU for AmneziaWG (`[amneziawg] mtu`).
pub const DEFAULT_AWG_MTU: u16 = 1280;

/// The narrowest tunnel that can carry IPv6: RFC 8200's minimum link MTU.
const MIN_IPV6_MTU: u16 = 1280;
/// The narrowest tunnel that can carry IPv4 without fragmenting every datagram a host may send
/// unfragmented: RFC 791's minimum reassembly buffer.
const MIN_IPV4_MTU: u16 = 576;
/// A standard Ethernet payload. A tunnel is never wider than the path under it, so anything above
/// this is a slipped digit rather than a request.
const MAX_MTU: u16 = 1500;

/// Listen port assumed when neither `listen_port` nor the endpoint names one.
const DEFAULT_WG_PORT: u16 = 51820;
const DEFAULT_AWG_PORT: u16 = 51821;

/// Which section of `config.toml` a [`TunnelInterfaceConfig`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunnelSection {
    WireGuard,
    AmneziaWg,
}

impl TunnelSection {
    /// The section as it is written in the file, for an error a person has to act on.
    fn name(self) -> &'static str {
        match self {
            Self::WireGuard => "wireguard",
            Self::AmneziaWg => "amneziawg",
        }
    }
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

    /// Settle the per-section keys for the section this interface was read from: `obfuscation`
    /// is rejected under `[wireguard]` and defaulted under `[amneziawg]`; `mtu` is taken as
    /// given under either, and defaulted only under `[amneziawg]`.
    fn settle_section(&mut self, section: TunnelSection) -> Result<(), ConfigError> {
        match section {
            TunnelSection::WireGuard if self.obfuscation.is_some() => {
                return Err(ConfigError::AmneziaWgOnlyKey { key: "obfuscation" });
            }
            TunnelSection::WireGuard => {}
            TunnelSection::AmneziaWg => {
                self.mtu.get_or_insert(DEFAULT_AWG_MTU);
                self.obfuscation.get_or_insert_default();
            }
        }
        self.check_mtu(section)
    }

    /// Bound the MTU here, because the next thing that would object is the kernel.
    ///
    /// `u16` accepts `0`, `68` and `9000`, and a value the link cannot take now reaches
    /// `ip link set … mtu` in the daemon — which fails, propagates out of `ensure_interface`, and
    /// leaves floppa-daemon refusing to start. That is a deploy-time outage for a typo in a
    /// hand-edited variables file, and this is the one place that reads the file with any judgment
    /// at all.
    ///
    /// The floor depends on what the tunnel carries: RFC 8200 makes 1280 the minimum link MTU for
    /// IPv6, so a tunnel routing `::/0` at 1200 is not slow, it is broken. `allowed_ips` is the
    /// only statement of that in the config, and a v6 prefix is exactly the thing in it with a
    /// colon in it. The ceiling is a standard Ethernet payload: a tunnel is never wider than the
    /// path under it, and asking for jumbo here can only mean a slipped digit.
    fn check_mtu(&self, section: TunnelSection) -> Result<(), ConfigError> {
        let Some(mtu) = self.mtu else {
            return Ok(());
        };
        let carries_ipv6 = self.allowed_ips.contains(':');
        let min = if carries_ipv6 {
            MIN_IPV6_MTU
        } else {
            MIN_IPV4_MTU
        };
        if (min..=MAX_MTU).contains(&mtu) {
            return Ok(());
        }
        Err(ConfigError::InvalidMtu {
            section: section.name(),
            mtu,
            min,
            max: MAX_MTU,
            why: if carries_ipv6 {
                " (this tunnel routes IPv6, whose minimum link MTU is 1280)"
            } else {
                ""
            },
        })
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
    /// Max credential-login attempts per login name per 15 minutes — the brute-force cap on
    /// one account, whatever addresses the attempts come from.
    #[serde(default = "default_login_rate_limit_per_15min")]
    pub login_rate_limit_per_15min: u32,
    /// Max credential-login attempts per IP per 15 minutes — one client trying many accounts.
    /// Looser than the per-name cap: a NAT or office shares one address.
    #[serde(default = "default_login_ip_rate_limit_per_15min")]
    pub login_ip_rate_limit_per_15min: u32,
    /// Max Telegram deep-link login starts and code exchanges per IP per 15 minutes. Every
    /// start mints a pending-state entry, so this bounds how fast one client can fill that map.
    #[serde(default = "default_telegram_login_rate_limit_per_15min")]
    pub telegram_login_rate_limit_per_15min: u32,
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
            login_ip_rate_limit_per_15min: default_login_ip_rate_limit_per_15min(),
            telegram_login_rate_limit_per_15min: default_telegram_login_rate_limit_per_15min(),
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

fn default_login_ip_rate_limit_per_15min() -> u32 {
    60
}

fn default_telegram_login_rate_limit_per_15min() -> u32 {
    120
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
    /// An AmneziaWG-only key (`obfuscation`) under the `[wireguard]` section.
    #[error("`{key}` under [wireguard] is an [amneziawg]-only setting")]
    AmneziaWgOnlyKey { key: &'static str },
    /// An `mtu` the interface cannot be brought up with, caught here rather than by the kernel
    /// on a deploy. See [`TunnelInterfaceConfig::check_mtu`].
    #[error("`mtu = {mtu}` under [{section}] is outside {min}..={max}{why}")]
    InvalidMtu {
        section: &'static str,
        mtu: u16,
        min: u16,
        max: u16,
        why: &'static str,
    },
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
        // The commented MTU belongs to [wireguard], not to the table that follows it.
        assert_eq!(config.wireguard.mtu, Some(1380));

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
        assert_eq!(defaults.login_ip_rate_limit_per_15min, 60);
        assert_eq!(defaults.telegram_login_rate_limit_per_15min, 120);

        // Each key can be set on its own; the rest keep their defaults.
        let partial: AuthConfig = toml::from_str("login_ip_rate_limit_per_15min = 7").unwrap();
        assert_eq!(partial.login_ip_rate_limit_per_15min, 7);
        assert_eq!(partial.login_rate_limit_per_15min, 10);
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
    fn obfuscation_is_rejected_under_wireguard() {
        let toml = format!("{MINIMAL_CONFIG}[wireguard.obfuscation]\njc = 6\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(
            matches!(err, ConfigError::AmneziaWgOnlyKey { key: "obfuscation" }),
            "{err:?}"
        );
    }

    #[test]
    fn wireguard_takes_an_mtu_but_is_given_no_default() {
        let config = Config::parse(&format!("{MINIMAL_CONFIG}mtu = 1380\n")).unwrap();
        assert_eq!(config.wireguard.mtu, Some(1380));
        assert!(
            !config.wireguard.is_amneziawg(),
            "an MTU is not what makes an interface AmneziaWG"
        );

        // Without one, nothing is invented: clients fall back to WireGuard's own default and
        // the kernel keeps whatever the interface came up with.
        let config = Config::parse(MINIMAL_CONFIG).unwrap();
        assert_eq!(config.wireguard.mtu, None);
    }

    /// `MINIMAL_CONFIG` routes IPv4 only; this one routes both, as every real deployment does.
    const DUAL_STACK_CONFIG: &str = r#"
        [wireguard]
        interface = "wg0"
        endpoint = "vpn.example.com:51820"
        client_subnet = "10.100.0.0/24"
        dns = ["1.1.1.1"]
        allowed_ips = "0.0.0.0/0, ::/0"
    "#;

    #[test]
    fn an_mtu_the_link_cannot_take_is_refused_here() {
        // The kernel is the next thing that would object, and by then the daemon is failing to
        // start on a deployed machine.
        for mtu in [0, 68, 575, 1501, 9000] {
            let err = Config::parse(&format!("{MINIMAL_CONFIG}mtu = {mtu}\n")).unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidMtu { mtu: got, .. } if got == mtu),
                "mtu = {mtu} should have been refused, got {err:?}"
            );
        }
        for mtu in [576, 1280, 1380, 1500] {
            let config = Config::parse(&format!("{MINIMAL_CONFIG}mtu = {mtu}\n")).unwrap();
            assert_eq!(config.wireguard.mtu, Some(mtu));
        }
    }

    #[test]
    fn a_tunnel_that_routes_ipv6_may_not_go_below_1280() {
        // RFC 8200's minimum link MTU: below it the tunnel does not carry IPv6 at all, which is a
        // different thing from carrying it slowly.
        let err = Config::parse(&format!("{DUAL_STACK_CONFIG}mtu = 1200\n")).unwrap_err();
        let ConfigError::InvalidMtu { min, why, .. } = err else {
            panic!("expected InvalidMtu, got {err:?}");
        };
        assert_eq!(min, MIN_IPV6_MTU);
        assert!(why.contains("IPv6"), "the error must say why: {why:?}");

        // The same value is fine on a tunnel that routes only IPv4.
        let config = Config::parse(&format!("{MINIMAL_CONFIG}mtu = 1200\n")).unwrap();
        assert_eq!(config.wireguard.mtu, Some(1200));

        // And the AmneziaWG default sits exactly on the floor rather than under it.
        assert_eq!(DEFAULT_AWG_MTU, MIN_IPV6_MTU);
    }

    #[test]
    fn the_amneziawg_section_is_bounded_too() {
        let toml = format!(
            "{DUAL_STACK_CONFIG}\n[amneziawg]\ninterface = \"awg0\"\n\
             endpoint = \"vpn.example.com:51821\"\nclient_subnet = \"10.101.0.0/24\"\n\
             dns = [\"1.1.1.1\"]\nallowed_ips = \"0.0.0.0/0, ::/0\"\nmtu = 1200\n"
        );
        let err = Config::parse(&toml).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::InvalidMtu {
                    section: "amneziawg",
                    ..
                }
            ),
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
