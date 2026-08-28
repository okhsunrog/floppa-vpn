//! The configs the client keeps: one per protocol, typed, persisted in the shape older builds
//! wrote.
//!
//! The WireGuard-family parsing lives in `floppa-tunnel-config`; this module wraps its
//! [`TunnelConfig`] for persistence and adds the VLESS side, which the shared crate only has the
//! defaults for.

use super::protocol::{Preference, Protocol};
use floppa_tunnel_config::conf::{DnsEntry, Endpoint, comma_list};
use floppa_tunnel_config::{AwgObfuscation, TunnelConfig, route, vless};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use shoes_lite::api::VlessConfig;
use std::net::IpAddr;
use std::str::FromStr;

/// Why a config could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigParseError {
    /// A WireGuard/AmneziaWG `.conf` the shared parser rejected; names the line and key.
    #[error(transparent)]
    Conf(#[from] floppa_tunnel_config::ConfigParseError),
    #[error("invalid VLESS URI: {0}")]
    Vless(String),
    /// A well-formed config of a protocol the caller cannot take (the legacy WireGuard-only
    /// store handed an AmneziaWG file).
    #[error("expected a {expected} config, got {found}")]
    WrongProtocol { expected: Protocol, found: Protocol },
}

/// A stored field that no longer reads back. Only an older store can produce this: everything is
/// checked at import now.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stored `{field}`: {detail}")]
pub struct StoredConfigError {
    field: &'static str,
    detail: String,
}

impl StoredConfigError {
    fn new(field: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            field,
            detail: detail.to_string(),
        }
    }
}

/// The persisted shape of a WireGuard config: what every build so far has written to the store,
/// the OS keyring and the Android RPC socket. [`WgConfig`] serialises through it, so the typed
/// form can change without a migration.
#[derive(Clone, Serialize, Deserialize)]
struct StoredWgConfig {
    private_key: String,
    address: String,
    dns: Option<String>,
    mtu: Option<u16>,
    peer_public_key: String,
    peer_preshared_key: Option<String>,
    peer_endpoint: String,
    allowed_ips: String,
    persistent_keepalive: Option<u16>,
}

/// A stored list whose items were checked at import. An item that no longer parses can only come
/// from an older store; it is skipped, as it always was.
fn stored_list<T: FromStr>(text: &str) -> Vec<T> {
    text.split(',')
        .filter_map(|item| item.trim().parse().ok())
        .collect()
}

impl TryFrom<StoredWgConfig> for WgConfig {
    type Error = StoredConfigError;

    fn try_from(stored: StoredWgConfig) -> Result<Self, Self::Error> {
        use floppa_tunnel_config::conf::{InterfaceConfig, PeerConfig};

        let mut addresses = stored_list::<IpNetwork>(&stored.address).into_iter();
        let address = addresses
            .next()
            .ok_or_else(|| StoredConfigError::new("address", "not an address"))?;
        let dns = stored
            .dns
            .as_deref()
            .map(stored_list::<DnsEntry>)
            .unwrap_or_default();
        Ok(Self(TunnelConfig {
            interface: InterfaceConfig {
                private_key: stored
                    .private_key
                    .parse()
                    .map_err(|e| StoredConfigError::new("private_key", e))?,
                address,
                extra_addresses: addresses.collect(),
                dns,
                mtu: stored.mtu,
                listen_port: None,
            },
            peer: PeerConfig {
                public_key: stored
                    .peer_public_key
                    .parse()
                    .map_err(|e| StoredConfigError::new("peer_public_key", e))?,
                preshared_key: stored
                    .peer_preshared_key
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|e| StoredConfigError::new("peer_preshared_key", e))?,
                endpoint: stored
                    .peer_endpoint
                    .parse()
                    .map_err(|e| StoredConfigError::new("peer_endpoint", e))?,
                allowed_ips: stored_list(&stored.allowed_ips),
                persistent_keepalive: stored.persistent_keepalive,
            },
            obfuscation: None,
        }))
    }
}

impl From<WgConfig> for StoredWgConfig {
    fn from(config: WgConfig) -> Self {
        let WgConfig(tunnel) = config;
        let addresses = std::iter::once(tunnel.interface.address)
            .chain(tunnel.interface.extra_addresses.iter().copied());
        Self {
            private_key: tunnel.interface.private_key.to_base64(),
            address: comma_list(addresses),
            dns: tunnel.dns_line(),
            mtu: tunnel.interface.mtu,
            peer_public_key: tunnel.peer.public_key.to_base64(),
            peer_preshared_key: tunnel
                .peer
                .preshared_key
                .as_ref()
                .map(|key| key.to_base64()),
            peer_endpoint: tunnel.peer.endpoint.to_string(),
            allowed_ips: tunnel.allowed_ips_line(),
            persistent_keepalive: tunnel.peer.persistent_keepalive,
        }
    }
}

/// A WireGuard config: a [`TunnelConfig`] that carries no obfuscation. Persisted as
/// [`StoredWgConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredWgConfig", into = "StoredWgConfig")]
pub struct WgConfig(TunnelConfig);

impl WgConfig {
    /// Parse a plain WireGuard `.conf`. An AmneziaWG one is [`ConfigParseError::WrongProtocol`]:
    /// this is the legacy single-config path, which only ever held WireGuard.
    pub fn from_config_str(config: &str) -> Result<Self, ConfigParseError> {
        match ProtocolConfig::parse(config)? {
            ProtocolConfig::WireGuard(wg) => Ok(wg),
            other => Err(ConfigParseError::WrongProtocol {
                expected: Protocol::WireGuard,
                found: other.protocol(),
            }),
        }
    }

    /// The typed config, for the tunnel engine.
    pub fn tunnel(&self) -> &TunnelConfig {
        &self.0
    }

    /// Replace the peer endpoint — with a resolved literal, on the Android side of the socket.
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        self.0.peer.endpoint = endpoint;
    }
}

/// Tracks previous stats for computing transfer rates
pub struct SpeedTracker {
    prev_tx_bytes: u64,
    prev_rx_bytes: u64,
    prev_time: std::time::Instant,
    has_baseline: bool,
}

impl SpeedTracker {
    pub fn new() -> Self {
        Self {
            prev_tx_bytes: 0,
            prev_rx_bytes: 0,
            prev_time: std::time::Instant::now(),
            has_baseline: false,
        }
    }

    /// Update with new cumulative byte counts and return computed speeds (bytes/sec)
    pub fn update(&mut self, tx_bytes: u64, rx_bytes: u64) -> (f64, f64) {
        let now = std::time::Instant::now();

        // First sample after reset: just store the baseline, don't compute speed.
        // Without this, reconnecting to an already-running tunnel would divide
        // the full cumulative byte count by a tiny elapsed time → huge spike.
        if !self.has_baseline {
            self.prev_tx_bytes = tx_bytes;
            self.prev_rx_bytes = rx_bytes;
            self.prev_time = now;
            self.has_baseline = true;
            return (0.0, 0.0);
        }

        let elapsed = now.duration_since(self.prev_time).as_secs_f64();

        let (tx_speed, rx_speed) = if elapsed > 0.1 {
            let tx_delta = tx_bytes.saturating_sub(self.prev_tx_bytes);
            let rx_delta = rx_bytes.saturating_sub(self.prev_rx_bytes);
            (tx_delta as f64 / elapsed, rx_delta as f64 / elapsed)
        } else {
            (0.0, 0.0)
        };

        self.prev_tx_bytes = tx_bytes;
        self.prev_rx_bytes = rx_bytes;
        self.prev_time = now;

        (tx_speed, rx_speed)
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for SpeedTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// VLESS config with VPN-specific fields (address, dns, routes).
///
/// Wraps the core VLESS connection parameters from the URI together with
/// tunnel configuration (IP address, DNS, routing) needed for VPN operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlessVpnConfig {
    /// Original VLESS URI (for persistence)
    pub uri: String,
    pub uuid: String,
    /// Server address as "host:port"
    pub server_addr: String,
    /// SNI hostname for REALITY handshake
    pub server_name: String,
    /// REALITY public key (base64url-no-pad)
    pub reality_public_key: String,
    /// REALITY short ID (hex)
    pub reality_short_id: String,
    /// Flow control mode, e.g. "xtls-rprx-vision"
    pub flow: Option<String>,
    /// Tunnel IP address with CIDR prefix, e.g. "10.0.0.2/32"
    pub address: String,
    /// DNS servers, comma-separated
    pub dns: Option<String>,
    /// TUN MTU (default [`vless::MTU`])
    pub mtu: Option<u16>,
    /// Allowed IPs for routing, comma-separated CIDRs
    pub allowed_ips: String,
}

impl VlessVpnConfig {
    /// Parse a VLESS URI and fill VPN-specific fields with the shared defaults.
    pub fn from_uri(uri: &str) -> Result<Self, ConfigParseError> {
        let parsed = VlessConfig::from_uri(uri).map_err(ConfigParseError::Vless)?;
        Ok(Self {
            uri: uri.to_string(),
            uuid: parsed.uuid,
            server_addr: parsed.server_addr,
            server_name: parsed.server_name,
            reality_public_key: parsed.reality_public_key,
            reality_short_id: parsed.reality_short_id,
            flow: parsed.flow,
            address: vless::ADDRESS_NETWORK.to_string(),
            dns: Some(vless::DNS.to_string()),
            mtu: Some(vless::MTU),
            allowed_ips: comma_list(route::CATCH_ALL),
        })
    }

    /// Convert to the shoes library VlessConfig for tunnel creation.
    pub fn to_shoes_config(&self) -> VlessConfig {
        let address = self.address.split('/').next().map(|s| s.to_string());

        VlessConfig {
            uuid: self.uuid.clone(),
            server_addr: self.server_addr.clone(),
            server_name: self.server_name.clone(),
            reality_public_key: self.reality_public_key.clone(),
            reality_short_id: self.reality_short_id.clone(),
            flow: self.flow.clone(),
            address,
            netmask: None,
            dns: self.dns.clone(),
            mtu: self.mtu,
            allowed_ips: Some(self.allowed_ips.clone()),
        }
    }

    /// Get address as IpNetwork
    pub fn address_network(&self) -> Result<IpNetwork, ipnetwork::IpNetworkError> {
        IpNetwork::from_str(&self.address)
    }

    /// The resolvers only; see [`TunnelConfig::dns_servers`].
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        self.dns
            .as_deref()
            .map(stored_list::<DnsEntry>)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                DnsEntry::Server(ip) => Some(ip),
                DnsEntry::SearchDomain(_) => None,
            })
            .collect()
    }

    /// Get allowed IPs as `Vec<IpNetwork>`
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        stored_list(&self.allowed_ips)
    }

    /// Get MTU (default [`vless::MTU`])
    pub fn get_mtu(&self) -> u16 {
        self.mtu.unwrap_or(vless::MTU)
    }
}

/// AmneziaWG config: a WireGuard config plus interface-wide obfuscation. The tunnel runs
/// through the same gotatun device as WireGuard, with the obfuscation applied at build time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwgConfig {
    pub wg: WgConfig,
    pub obfuscation: AwgObfuscation,
}

impl AwgConfig {
    /// The typed config with its obfuscation attached, for the tunnel engine.
    pub fn tunnel(&self) -> TunnelConfig {
        TunnelConfig {
            obfuscation: Some(self.obfuscation.clone()),
            ..self.wg.0.clone()
        }
    }
}

/// Protocol-agnostic VPN configuration.
///
/// Each variant wraps a protocol-specific config. Common VPN concepts
/// (endpoint, address, DNS, etc.) are exposed via methods on this enum,
/// so the connect flow doesn't need to know which protocol is in use.
// The AmneziaWG variant is the WireGuard one plus its obfuscation, so it is the largest by
// construction; a config is held once per protocol, not passed around hot.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", content = "config")]
pub enum ProtocolConfig {
    #[serde(rename = "wireguard")]
    WireGuard(WgConfig),
    /// AmneziaWG — WireGuard + obfuscation. Runs through the same gotatun tunnel path.
    #[serde(rename = "amneziawg")]
    AmneziaWg(AwgConfig),
    #[serde(rename = "vless")]
    Vless(VlessVpnConfig),
}

/// Whether a WireGuard-family interface names an IPv6 address of its own.
///
/// `Address` is a list — the first entry plus any extras — and an IPv6 twin can be anywhere in
/// it, so every entry is checked rather than just the first.
fn interface_has_ipv6(interface: &floppa_tunnel_config::conf::InterfaceConfig) -> bool {
    std::iter::once(&interface.address)
        .chain(&interface.extra_addresses)
        .any(|net| net.is_ipv6())
}

impl ProtocolConfig {
    /// Read a config in whichever of the three forms it comes: a `vless://` URI, an AmneziaWG
    /// `.conf` (an `[Interface]` carrying obfuscation keys), or a plain WireGuard `.conf`.
    pub fn parse(raw: &str) -> Result<Self, ConfigParseError> {
        let trimmed = raw.trim();
        if trimmed.starts_with("vless://") {
            return VlessVpnConfig::from_uri(trimmed).map(Self::Vless);
        }
        let mut tunnel = TunnelConfig::parse(raw)?;
        Ok(match tunnel.obfuscation.take() {
            Some(obfuscation) => Self::AmneziaWg(AwgConfig {
                wg: WgConfig(tunnel),
                obfuscation,
            }),
            None => Self::WireGuard(WgConfig(tunnel)),
        })
    }

    /// Server endpoint as "host:port".
    pub fn endpoint_str(&self) -> String {
        match self {
            Self::WireGuard(wg) => wg.0.peer.endpoint.to_string(),
            Self::AmneziaWg(awg) => awg.wg.0.peer.endpoint.to_string(),
            Self::Vless(vless) => vless.server_addr.clone(),
        }
    }

    /// Local tunnel address with its prefix (e.g. "10.0.0.2/32").
    pub fn address(&self) -> String {
        match self {
            Self::WireGuard(wg) => wg.0.interface.address.to_string(),
            Self::AmneziaWg(awg) => awg.wg.0.interface.address.to_string(),
            Self::Vless(vless) => vless.address.clone(),
        }
    }

    /// Local tunnel address as IpNetwork.
    pub fn address_network(&self) -> Result<IpNetwork, ipnetwork::IpNetworkError> {
        match self {
            Self::WireGuard(wg) => Ok(wg.0.interface.address),
            Self::AmneziaWg(awg) => Ok(awg.wg.0.interface.address),
            Self::Vless(vless) => vless.address_network(),
        }
    }

    /// Whether the tunnel gets an IPv6 address of its own.
    ///
    /// The question a route decision has to ask, and it is not the same as "does this host have
    /// IPv6". A tunnel with only an IPv4 address that nevertheless installs `::/1` and `8000::/1`
    /// captures the machine's whole IPv6 traffic into an interface that cannot carry it: packets
    /// leave with a link-local source and are never answered. What makes that a total outage
    /// rather than a slow path is Happy Eyeballs — `curl`, Android and every browser *prefer*
    /// IPv6 when a route says it is available, so they choose the black hole first.
    ///
    /// Observed exactly that way: VLESS connected, verified, and carried nothing, because
    /// `curl https://ifconfig.me` went to `[2600:1901:…]:443` from `fe80::…` and hung. Forcing
    /// `-4` on the same tunnel returned the exit node's address immediately.
    ///
    /// WireGuard and AmneziaWG answer honestly from their config: `Address` may list an IPv6
    /// twin, and when it does the tunnel really can route IPv6. VLESS gets a fixed IPv4 address
    /// and no v6 at all.
    pub fn has_ipv6_address(&self) -> bool {
        match self {
            Self::WireGuard(wg) => interface_has_ipv6(&wg.0.interface),
            Self::AmneziaWg(awg) => interface_has_ipv6(&awg.wg.0.interface),
            Self::Vless(_) => false,
        }
    }

    /// DNS servers.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        match self {
            Self::WireGuard(wg) => wg.0.dns_servers(),
            Self::AmneziaWg(awg) => awg.wg.0.dns_servers(),
            Self::Vless(vless) => vless.dns_servers(),
        }
    }

    /// The `DNS` line as it was imported — resolvers and search domains — for display.
    pub fn dns_line(&self) -> Option<String> {
        match self {
            Self::WireGuard(wg) => wg.0.dns_line(),
            Self::AmneziaWg(awg) => awg.wg.0.dns_line(),
            Self::Vless(vless) => vless.dns.clone(),
        }
    }

    /// Allowed IPs / routes.
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        match self {
            Self::WireGuard(wg) => wg.0.peer.allowed_ips.clone(),
            Self::AmneziaWg(awg) => awg.wg.0.peer.allowed_ips.clone(),
            Self::Vless(vless) => vless.allowed_ips_networks(),
        }
    }

    /// The `AllowedIPs` line, for display.
    pub fn allowed_ips_line(&self) -> String {
        match self {
            Self::WireGuard(wg) => wg.0.allowed_ips_line(),
            Self::AmneziaWg(awg) => awg.wg.0.allowed_ips_line(),
            Self::Vless(vless) => vless.allowed_ips.clone(),
        }
    }

    /// Tunnel MTU.
    pub fn get_mtu(&self) -> u16 {
        match self {
            Self::WireGuard(wg) => wg.0.mtu(),
            Self::AmneziaWg(awg) => awg.wg.0.mtu(),
            Self::Vless(vless) => vless.get_mtu(),
        }
    }

    /// Protocol name for display / persistence.
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::WireGuard(_) => Protocol::WireGuard,
            Self::AmneziaWg(_) => Protocol::AmneziaWg,
            Self::Vless(_) => Protocol::Vless,
        }
    }
}

/// Multi-config storage: holds WG, AmneziaWG, and VLESS configs with an active selector.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedVpnConfigs {
    /// The protocol that last actually worked. `None` until something has connected.
    ///
    /// `serde(alias)` keeps reading the pre-enum `active_protocol` string already on disk and in
    /// OS keyrings; an unparseable legacy value migrates to `None` rather than silently becoming
    /// WireGuard (see [`Preference`]).
    #[serde(alias = "active_protocol", default)]
    pub preferred_protocol: Preference,
    /// Cached WireGuard config (if any)
    pub wireguard: Option<WgConfig>,
    /// Cached AmneziaWG config (if any)
    #[serde(default)]
    pub amneziawg: Option<AwgConfig>,
    /// Cached VLESS config (if any)
    #[serde(default)]
    pub vless: Option<VlessVpnConfig>,
}

impl SavedVpnConfigs {
    /// The cached config for one specific protocol, if it is stored.
    pub fn get(&self, protocol: Protocol) -> Option<ProtocolConfig> {
        match protocol {
            Protocol::WireGuard => self.wireguard.clone().map(ProtocolConfig::WireGuard),
            Protocol::AmneziaWg => self.amneziawg.clone().map(ProtocolConfig::AmneziaWg),
            Protocol::Vless => self.vless.clone().map(ProtocolConfig::Vless),
        }
    }

    /// Which protocols have a cached config. This is a SET: the order is [`Protocol::ALL`] for
    /// determinism, not a statement of preference.
    pub fn available_protocols(&self) -> Vec<Protocol> {
        Protocol::ALL
            .iter()
            .copied()
            .filter(|&p| self.get(p).is_some())
            .collect()
    }

    /// Whether any config is stored.
    pub fn has_any(&self) -> bool {
        self.wireguard.is_some() || self.amneziawg.is_some() || self.vless.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape `floppa_core::services::generate_wg_config` produces, with real keys.
    const SERVER_WG_CONF: &str = "\
[Interface]
PrivateKey = gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=
Address = 10.200.0.5/32
DNS = 8.8.8.8

[Peer]
PublicKey = HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=
Endpoint = vpn.test.com:51820
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
";

    /// The shape `floppa_core::services::generate_awg_config` produces with the default preset.
    const SERVER_AWG_CONF: &str = "\
[Interface]
PrivateKey = gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=
Address = 10.101.0.5/32
DNS = 1.1.1.1
MTU = 1280
Jc = 6
Jmin = 55
Jmax = 205
S1 = 72
S2 = 56
S3 = 32
S4 = 16
H1 = 234567-345678
H2 = 3456789-4567890
H3 = 56789012-67890123
H4 = 456789012-567890123
I1 = <b 0xc30000000108><r 8><b 0x08><r 8><b 0x0045dc><t><r 16>

[Peer]
PublicKey = HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=
Endpoint = vpn.test.com:51821
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
";

    #[test]
    fn a_server_generated_wireguard_conf_round_trips() {
        let config = ProtocolConfig::parse(SERVER_WG_CONF).unwrap();
        assert_eq!(config.protocol(), Protocol::WireGuard);
        assert_eq!(config.address(), "10.200.0.5/32");
        assert_eq!(config.endpoint_str(), "vpn.test.com:51820");
        assert_eq!(config.dns_line().as_deref(), Some("8.8.8.8"));
        assert_eq!(config.allowed_ips_line(), "0.0.0.0/0, ::/0");
        assert_eq!(config.get_mtu(), 1420);
        let ProtocolConfig::WireGuard(wg) = config else {
            unreachable!()
        };
        assert_eq!(wg.tunnel().obfuscation, None);
    }

    #[test]
    fn a_server_generated_amneziawg_conf_round_trips_with_its_obfuscation() {
        let config = ProtocolConfig::parse(SERVER_AWG_CONF).unwrap();
        assert_eq!(config.protocol(), Protocol::AmneziaWg);
        assert_eq!(config.get_mtu(), 1280);
        let ProtocolConfig::AmneziaWg(awg) = config else {
            unreachable!()
        };
        assert_eq!(awg.obfuscation, AwgObfuscation::default());
        assert_eq!(
            awg.tunnel().obfuscation.as_ref(),
            Some(&awg.obfuscation),
            "the engine gets the obfuscation back on the typed config"
        );
        assert_eq!(
            awg.wg.tunnel().obfuscation,
            None,
            "the WireGuard half carries none"
        );
    }

    #[test]
    fn the_stored_form_is_the_shape_older_builds_wrote() {
        // These keys are on users' disks and in their keyrings; the typed form must keep
        // reading and writing exactly them.
        let ProtocolConfig::WireGuard(wg) = ProtocolConfig::parse(SERVER_WG_CONF).unwrap() else {
            unreachable!()
        };
        let stored = serde_json::to_value(&wg).unwrap();
        assert_eq!(
            stored,
            json!({
                "private_key": "gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=",
                "address": "10.200.0.5/32",
                "dns": "8.8.8.8",
                "mtu": null,
                "peer_public_key": "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=",
                "peer_preshared_key": null,
                "peer_endpoint": "vpn.test.com:51820",
                "allowed_ips": "0.0.0.0/0, ::/0",
                "persistent_keepalive": 25,
            })
        );
        let back: WgConfig = serde_json::from_value(stored).unwrap();
        assert_eq!(back, wg);
    }

    #[test]
    fn an_older_store_with_null_signature_slots_and_a_bad_route_item_still_loads() {
        let legacy = json!({
            "wg": {
                "private_key": "gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=",
                "address": "10.101.0.5/32",
                "dns": "1.1.1.1, corp.example",
                "mtu": 1280,
                "peer_public_key": "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=",
                "peer_preshared_key": null,
                "peer_endpoint": "vpn.test.com:51821",
                "allowed_ips": "0.0.0.0/0, ::/O",
                "persistent_keepalive": null,
            },
            "obfuscation": {
                "jc": 4, "jmin": 40, "jmax": 70, "s1": 15, "s2": 18, "s3": 0, "s4": 0,
                "h1": "5-10", "h2": "2", "h3": "3", "h4": "4",
                "i1": "<b 0xf6>", "i2": null, "i3": null, "i4": null, "i5": null,
            },
        });
        let awg: AwgConfig = serde_json::from_value(legacy).unwrap();
        let tunnel = awg.tunnel();
        assert_eq!(tunnel.mtu(), 1280);
        assert_eq!(tunnel.keepalive(), 25);
        assert_eq!(tunnel.dns_search_domains(), vec!["corp.example"]);
        assert_eq!(
            tunnel.peer.allowed_ips,
            vec!["0.0.0.0/0".parse::<IpNetwork>().unwrap()],
            "a lenient-era typo is dropped, as it always was"
        );
        assert_eq!(awg.obfuscation.i1, "<b 0xf6>");
        assert_eq!(awg.obfuscation.i2, "");
    }

    #[test]
    fn a_stored_key_that_no_longer_reads_is_an_error_naming_the_field() {
        let stored = json!({
            "private_key": "aGVsbG8=",
            "address": "10.0.0.2/32",
            "dns": null,
            "mtu": null,
            "peer_public_key": "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=",
            "peer_preshared_key": null,
            "peer_endpoint": "vpn.test.com:51820",
            "allowed_ips": "0.0.0.0/0",
            "persistent_keepalive": null,
        });
        let err = serde_json::from_value::<WgConfig>(stored).unwrap_err();
        assert!(err.to_string().contains("stored `private_key`"), "{err}");
    }

    #[test]
    fn a_conf_error_keeps_its_line_and_key() {
        let raw = SERVER_WG_CONF.replace("DNS = 8.8.8.8", "DNS = 8.8.8");
        assert!(matches!(
            ProtocolConfig::parse(&raw),
            Err(ConfigParseError::Conf(
                floppa_tunnel_config::ConfigParseError::InvalidValue { line: 4, key, .. }
            )) if key == "dns"
        ));
    }

    #[test]
    fn the_legacy_wireguard_path_refuses_an_amneziawg_conf() {
        assert!(WgConfig::from_config_str(SERVER_WG_CONF).is_ok());
        assert_eq!(
            WgConfig::from_config_str(SERVER_AWG_CONF).unwrap_err(),
            ConfigParseError::WrongProtocol {
                expected: Protocol::WireGuard,
                found: Protocol::AmneziaWg,
            }
        );
    }

    #[test]
    fn a_vless_uri_is_a_vless_config_with_the_shared_defaults() {
        let raw = "vless://0f7f6d3c-0a1c-4f1e-9d3a-1b2c3d4e5f60@vpn.example.com:443?security=reality&sni=example.com&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=0123abcd";
        let config = ProtocolConfig::parse(raw).unwrap();
        assert_eq!(config.protocol(), Protocol::Vless);
        assert_eq!(config.address(), "10.0.0.2/32");
        assert_eq!(config.dns_servers(), vec![vless::DNS]);
        assert_eq!(config.get_mtu(), vless::MTU);
        assert_eq!(config.allowed_ips_networks(), route::CATCH_ALL.to_vec());
        assert!(matches!(
            ProtocolConfig::parse("vless://nobody"),
            Err(ConfigParseError::Vless(_))
        ));
    }
}
