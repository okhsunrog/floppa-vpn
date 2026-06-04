use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use shoes_lite::api::VlessConfig;
use specta::Type;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use tokio::sync::RwLock;

/// Connection status enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    VerifyingConnection,
    Connected,
    Disconnecting,
}

/// Category of a connect failure. Lets the frontend decide what to do without
/// string-matching error messages: `verify_failed` is worth trying another
/// protocol (and may mean the peer was deleted), `permission_denied` needs user
/// action, `tunnel_error` is usually environmental, `busy` is a re-entrancy guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConnectErrorCode {
    Busy,
    PermissionDenied,
    VerifyFailed,
    TunnelError,
}

/// Structured error returned from the `connect` command.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ConnectError {
    pub code: ConnectErrorCode,
    pub message: String,
}

impl ConnectError {
    pub fn busy(message: impl Into<String>) -> Self {
        Self {
            code: ConnectErrorCode::Busy,
            message: message.into(),
        }
    }
    pub fn permission(message: impl Into<String>) -> Self {
        Self {
            code: ConnectErrorCode::PermissionDenied,
            message: message.into(),
        }
    }
    pub fn verify(message: impl Into<String>) -> Self {
        Self {
            code: ConnectErrorCode::VerifyFailed,
            message: message.into(),
        }
    }
    pub fn tunnel(message: impl Into<String>) -> Self {
        Self {
            code: ConnectErrorCode::TunnelError,
            message: message.into(),
        }
    }
}

/// Setup/IO errors propagated via `?` (DNS, TUN, routing) are environmental —
/// classify them as `tunnel_error`. Verify/permission/busy sites construct their
/// variant explicitly instead of relying on this.
impl From<String> for ConnectError {
    fn from(message: String) -> Self {
        Self::tunnel(message)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// WireGuard configuration
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct WgConfig {
    pub private_key: String,
    pub address: String,
    pub dns: Option<String>,
    pub mtu: Option<u16>,
    pub peer_public_key: String,
    pub peer_preshared_key: Option<String>,
    pub peer_endpoint: String,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u16>,
}

impl WgConfig {
    /// Get private key as 32-byte array for gotatun
    pub fn private_key_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = BASE64
            .decode(&self.private_key)
            .map_err(|e| format!("Invalid private key base64: {}", e))?;
        bytes
            .try_into()
            .map_err(|_| "Private key must be 32 bytes".to_string())
    }

    /// Get peer public key as 32-byte array for gotatun
    pub fn peer_public_key_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = BASE64
            .decode(&self.peer_public_key)
            .map_err(|e| format!("Invalid public key base64: {}", e))?;
        bytes
            .try_into()
            .map_err(|_| "Public key must be 32 bytes".to_string())
    }

    /// Get peer preshared key as 32-byte array for gotatun (if set)
    pub fn peer_preshared_key_bytes(&self) -> Result<Option<[u8; 32]>, String> {
        match &self.peer_preshared_key {
            Some(psk) => {
                let bytes = BASE64
                    .decode(psk)
                    .map_err(|e| format!("Invalid preshared key base64: {}", e))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| "Preshared key must be 32 bytes".to_string())?;
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    /// Get address as IpNetwork
    pub fn address_network(&self) -> Result<IpNetwork, String> {
        IpNetwork::from_str(&self.address).map_err(|e| format!("Invalid address: {}", e))
    }

    /// Get DNS servers as Vec<IpAddr>
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        self.dns
            .as_ref()
            .map(|dns| {
                dns.split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get allowed IPs as Vec<IpNetwork>
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Get MTU (default 1420 for WireGuard)
    pub fn get_mtu(&self) -> u16 {
        self.mtu.unwrap_or(1420)
    }
}

impl WgConfig {
    /// Parse from WireGuard config file format
    pub fn from_config_str(config: &str) -> Result<Self, String> {
        let mut private_key = None;
        let mut address = None;
        let mut dns = None;
        let mut mtu = None;
        let mut peer_public_key = None;
        let mut peer_preshared_key = None;
        let mut peer_endpoint = None;
        let mut allowed_ips = None;
        let mut persistent_keepalive = None;

        let mut in_interface = false;
        let mut in_peer = false;

        for line in config.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.eq_ignore_ascii_case("[Interface]") {
                in_interface = true;
                in_peer = false;
                continue;
            }
            if line.eq_ignore_ascii_case("[Peer]") {
                in_interface = false;
                in_peer = true;
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();

                if in_interface {
                    match key.as_str() {
                        "privatekey" => private_key = Some(value),
                        "address" => address = Some(value),
                        "dns" => dns = Some(value),
                        "mtu" => mtu = value.parse().ok(),
                        _ => {}
                    }
                } else if in_peer {
                    match key.as_str() {
                        "publickey" => peer_public_key = Some(value),
                        "presharedkey" => peer_preshared_key = Some(value),
                        "endpoint" => peer_endpoint = Some(value),
                        "allowedips" => allowed_ips = Some(value),
                        "persistentkeepalive" => {
                            persistent_keepalive = value.parse().ok();
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(WgConfig {
            private_key: private_key.ok_or("Missing PrivateKey")?,
            address: address.ok_or("Missing Address")?,
            dns,
            mtu,
            peer_public_key: peer_public_key.ok_or("Missing Peer PublicKey")?,
            peer_preshared_key,
            peer_endpoint: peer_endpoint.ok_or("Missing Peer Endpoint")?,
            allowed_ips: allowed_ips.unwrap_or_else(|| "0.0.0.0/0, ::/0".to_string()),
            persistent_keepalive,
        })
    }

    /// Convert to WireGuard config file format
    pub fn to_config_str(&self) -> String {
        let mut config = String::new();
        config.push_str("[Interface]\n");
        config.push_str(&format!("PrivateKey = {}\n", self.private_key));
        config.push_str(&format!("Address = {}\n", self.address));
        if let Some(dns) = &self.dns {
            config.push_str(&format!("DNS = {}\n", dns));
        }
        if let Some(mtu) = self.mtu {
            config.push_str(&format!("MTU = {}\n", mtu));
        }
        config.push_str("\n[Peer]\n");
        config.push_str(&format!("PublicKey = {}\n", self.peer_public_key));
        if let Some(psk) = &self.peer_preshared_key {
            config.push_str(&format!("PresharedKey = {}\n", psk));
        }
        config.push_str(&format!("Endpoint = {}\n", self.peer_endpoint));
        config.push_str(&format!("AllowedIPs = {}\n", self.allowed_ips));
        if let Some(keepalive) = self.persistent_keepalive {
            config.push_str(&format!("PersistentKeepalive = {}\n", keepalive));
        }
        config
    }
}

/// Traffic statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct TrafficStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_bytes_per_sec: f64,
    pub rx_bytes_per_sec: f64,
}

/// Connection information
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ConnectionInfo {
    pub status: ConnectionStatus,
    pub protocol: Option<Protocol>,
    pub server_endpoint: Option<String>,
    pub assigned_ip: Option<String>,
    pub connected_at: Option<i64>, // Unix timestamp
    pub last_packet_received: Option<i64>,
    pub stats: TrafficStats,
    /// Whether the configured DNS servers were successfully applied at connect time
    /// (desktop only). `false` means DNS config failed, so queries may leak to the
    /// local/ISP resolver — the UI should warn the user. Defaults to `true`.
    pub dns_ok: bool,
}

impl Default for ConnectionInfo {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            protocol: None,
            server_endpoint: None,
            assigned_ip: None,
            connected_at: None,
            last_packet_received: None,
            stats: TrafficStats::default(),
            dns_ok: true,
        }
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
    /// TUN MTU (default 1500)
    pub mtu: Option<u16>,
    /// Allowed IPs for routing, comma-separated CIDRs
    pub allowed_ips: String,
}

impl VlessVpnConfig {
    /// Parse a VLESS URI and fill VPN-specific fields with defaults.
    pub fn from_uri(uri: &str) -> Result<Self, String> {
        let parsed = VlessConfig::from_uri(uri)?;
        Ok(Self {
            uri: uri.to_string(),
            uuid: parsed.uuid,
            server_addr: parsed.server_addr,
            server_name: parsed.server_name,
            reality_public_key: parsed.reality_public_key,
            reality_short_id: parsed.reality_short_id,
            flow: parsed.flow,
            address: "10.0.0.2/32".to_string(),
            dns: Some("1.1.1.1".to_string()),
            mtu: Some(1500),
            allowed_ips: "0.0.0.0/0, ::/0".to_string(),
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
    pub fn address_network(&self) -> Result<IpNetwork, String> {
        IpNetwork::from_str(&self.address).map_err(|e| format!("Invalid VLESS address: {}", e))
    }

    /// Get DNS servers as Vec<IpAddr>
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        self.dns
            .as_ref()
            .map(|dns| {
                dns.split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get allowed IPs as Vec<IpNetwork>
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Get MTU (default 1500 for VLESS)
    pub fn get_mtu(&self) -> u16 {
        self.mtu.unwrap_or(1500)
    }
}

/// AmneziaWG 2.0 obfuscation parameters, parsed from the `[Interface]` section of an
/// AmneziaWG `.conf`. Applied to the gotatun device via `.with_awg(...)`. `H1`–`H4` are
/// strings (single value or "lo-hi" range); `I1`–`I5` are CPS tag specs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwgObfuscation {
    pub jc: u32,
    pub jmin: u32,
    pub jmax: u32,
    pub s1: u32,
    pub s2: u32,
    pub s3: u32,
    pub s4: u32,
    pub h1: String,
    pub h2: String,
    pub h3: String,
    pub h4: String,
    pub i1: Option<String>,
    pub i2: Option<String>,
    pub i3: Option<String>,
    pub i4: Option<String>,
    pub i5: Option<String>,
}

impl Default for AwgObfuscation {
    /// Defaults to standard-WireGuard behaviour (no obfuscation); real values come from the
    /// server-issued config.
    fn default() -> Self {
        Self {
            jc: 0,
            jmin: 0,
            jmax: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            h1: "1".into(),
            h2: "2".into(),
            h3: "3".into(),
            h4: "4".into(),
            i1: None,
            i2: None,
            i3: None,
            i4: None,
            i5: None,
        }
    }
}

/// AmneziaWG config: a WireGuard config plus interface-wide obfuscation. The tunnel runs
/// through the same gotatun device as WireGuard, with the obfuscation applied at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwgConfig {
    pub wg: WgConfig,
    pub obfuscation: AwgObfuscation,
}

/// AmneziaWG `[Interface]` obfuscation keys, used to tell an AmneziaWG `.conf` from a plain
/// WireGuard one.
const AWG_OBF_KEYS: &[&str] = &[
    "jc", "jmin", "jmax", "s1", "s2", "s3", "s4", "h1", "h2", "h3", "h4", "i1", "i2", "i3", "i4",
    "i5",
];

/// True if a config string is an AmneziaWG `.conf` (i.e. its `[Interface]` carries obfuscation
/// params). Used to route content-sniffed configs.
pub fn config_str_is_amneziawg(config: &str) -> bool {
    let mut in_interface = false;
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case("[Interface]") {
            in_interface = true;
            continue;
        }
        if line.starts_with('[') {
            in_interface = false;
            continue;
        }
        if in_interface
            && let Some((k, _)) = line.split_once('=')
            && AWG_OBF_KEYS.contains(&k.trim().to_lowercase().as_str())
        {
            return true;
        }
    }
    false
}

impl AwgConfig {
    /// Parse an AmneziaWG `.conf` (WireGuard config + obfuscation params).
    pub fn from_config_str(config: &str) -> Result<Self, String> {
        let wg = WgConfig::from_config_str(config)?;
        let mut obf = AwgObfuscation::default();

        let mut in_interface = false;
        for line in config.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.eq_ignore_ascii_case("[Interface]") {
                in_interface = true;
                continue;
            }
            if line.starts_with('[') {
                in_interface = false;
                continue;
            }
            if !in_interface {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim().to_lowercase();
                let v = v.trim().to_string();
                match k.as_str() {
                    "jc" => obf.jc = v.parse().unwrap_or(0),
                    "jmin" => obf.jmin = v.parse().unwrap_or(0),
                    "jmax" => obf.jmax = v.parse().unwrap_or(0),
                    "s1" => obf.s1 = v.parse().unwrap_or(0),
                    "s2" => obf.s2 = v.parse().unwrap_or(0),
                    "s3" => obf.s3 = v.parse().unwrap_or(0),
                    "s4" => obf.s4 = v.parse().unwrap_or(0),
                    "h1" => obf.h1 = v,
                    "h2" => obf.h2 = v,
                    "h3" => obf.h3 = v,
                    "h4" => obf.h4 = v,
                    "i1" => obf.i1 = (!v.is_empty()).then_some(v),
                    "i2" => obf.i2 = (!v.is_empty()).then_some(v),
                    "i3" => obf.i3 = (!v.is_empty()).then_some(v),
                    "i4" => obf.i4 = (!v.is_empty()).then_some(v),
                    "i5" => obf.i5 = (!v.is_empty()).then_some(v),
                    _ => {}
                }
            }
        }

        Ok(Self {
            wg,
            obfuscation: obf,
        })
    }

    /// Render back to an AmneziaWG `.conf` (used for the Android IPC handoff and export).
    pub fn to_config_str(&self) -> String {
        let wg = &self.wg;
        let o = &self.obfuscation;
        let mut s = String::from("[Interface]\n");
        s.push_str(&format!("PrivateKey = {}\n", wg.private_key));
        s.push_str(&format!("Address = {}\n", wg.address));
        if let Some(dns) = &wg.dns {
            s.push_str(&format!("DNS = {dns}\n"));
        }
        if let Some(mtu) = wg.mtu {
            s.push_str(&format!("MTU = {mtu}\n"));
        }
        s.push_str(&format!(
            "Jc = {}\nJmin = {}\nJmax = {}\n",
            o.jc, o.jmin, o.jmax
        ));
        s.push_str(&format!(
            "S1 = {}\nS2 = {}\nS3 = {}\nS4 = {}\n",
            o.s1, o.s2, o.s3, o.s4
        ));
        s.push_str(&format!(
            "H1 = {}\nH2 = {}\nH3 = {}\nH4 = {}\n",
            o.h1, o.h2, o.h3, o.h4
        ));
        for (n, val) in [(1, &o.i1), (2, &o.i2), (3, &o.i3), (4, &o.i4), (5, &o.i5)] {
            if let Some(spec) = val {
                s.push_str(&format!("I{n} = {spec}\n"));
            }
        }
        s.push_str("\n[Peer]\n");
        s.push_str(&format!("PublicKey = {}\n", wg.peer_public_key));
        if let Some(psk) = &wg.peer_preshared_key {
            s.push_str(&format!("PresharedKey = {psk}\n"));
        }
        s.push_str(&format!("Endpoint = {}\n", wg.peer_endpoint));
        s.push_str(&format!("AllowedIPs = {}\n", wg.allowed_ips));
        if let Some(keepalive) = wg.persistent_keepalive {
            s.push_str(&format!("PersistentKeepalive = {keepalive}\n"));
        }
        s
    }
}

/// Protocol-agnostic VPN configuration.
///
/// Each variant wraps a protocol-specific config. Common VPN concepts
/// (endpoint, address, DNS, etc.) are exposed via methods on this enum,
/// so the connect flow doesn't need to know which protocol is in use.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "protocol", content = "config")]
pub enum ProtocolConfig {
    #[serde(rename = "wireguard")]
    WireGuard(WgConfig),
    /// AmneziaWG — WireGuard + obfuscation. Runs through the same gotatun tunnel path.
    #[serde(rename = "amneziawg")]
    #[specta(skip)]
    AmneziaWg(AwgConfig),
    #[serde(rename = "vless")]
    #[specta(skip)]
    Vless(VlessVpnConfig),
}

impl ProtocolConfig {
    /// Server endpoint as "host:port" string.
    pub fn endpoint_str(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.peer_endpoint,
            Self::AmneziaWg(awg) => &awg.wg.peer_endpoint,
            Self::Vless(vless) => &vless.server_addr,
        }
    }

    /// Local tunnel address string (e.g. "10.0.0.2/32").
    pub fn address(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.address,
            Self::AmneziaWg(awg) => &awg.wg.address,
            Self::Vless(vless) => &vless.address,
        }
    }

    /// Local tunnel address as IpNetwork.
    pub fn address_network(&self) -> Result<IpNetwork, String> {
        match self {
            Self::WireGuard(wg) => wg.address_network(),
            Self::AmneziaWg(awg) => awg.wg.address_network(),
            Self::Vless(vless) => vless.address_network(),
        }
    }

    /// DNS servers.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        match self {
            Self::WireGuard(wg) => wg.dns_servers(),
            Self::AmneziaWg(awg) => awg.wg.dns_servers(),
            Self::Vless(vless) => vless.dns_servers(),
        }
    }

    /// Allowed IPs / routes.
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        match self {
            Self::WireGuard(wg) => wg.allowed_ips_networks(),
            Self::AmneziaWg(awg) => awg.wg.allowed_ips_networks(),
            Self::Vless(vless) => vless.allowed_ips_networks(),
        }
    }

    /// Tunnel MTU.
    pub fn get_mtu(&self) -> u16 {
        match self {
            Self::WireGuard(wg) => wg.get_mtu(),
            Self::AmneziaWg(awg) => awg.wg.get_mtu(),
            Self::Vless(vless) => vless.get_mtu(),
        }
    }

    /// Protocol of this config.
    pub fn protocol_name(&self) -> Protocol {
        match self {
            Self::WireGuard(_) => Protocol::WireGuard,
            Self::AmneziaWg(_) => Protocol::AmneziaWg,
            Self::Vless(_) => Protocol::Vless,
        }
    }
}

/// VPN protocol. Canonical wire/persist/display tokens are "wireguard",
/// "amneziawg", "vless" — pinned via explicit serde renames so existing
/// persisted configs, the tarpc/IPC string form, and the frontend contract are
/// unchanged. (`floppa-core::Protocol` only has two variants, so the client
/// defines its own three-variant enum.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub enum Protocol {
    #[default]
    #[serde(rename = "wireguard")]
    WireGuard,
    #[serde(rename = "amneziawg")]
    AmneziaWg,
    #[serde(rename = "vless")]
    Vless,
}

impl Protocol {
    /// Canonical lowercase token, matching the serde rename / persisted form.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::WireGuard => "wireguard",
            Self::AmneziaWg => "amneziawg",
            Self::Vless => "vless",
        }
    }

    /// Parse a canonical token back into a `Protocol`. Used at the tarpc/IPC
    /// boundary, where the protocol travels as a `String` (bincode encodes enums
    /// as a variant index, which would break across a version-skewed `:vpn`).
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "wireguard" => Some(Self::WireGuard),
            "amneziawg" => Some(Self::AmneziaWg),
            "vless" => Some(Self::Vless),
            _ => None,
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_token())
    }
}

/// Multi-config storage: holds WG, AmneziaWG, and VLESS configs with an active selector.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedVpnConfigs {
    /// Currently active protocol.
    pub active_protocol: Protocol,
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
    /// Get the active ProtocolConfig for the connect flow.
    pub fn active_config(&self) -> Option<ProtocolConfig> {
        self.config_for(self.active_protocol)
    }

    /// Get the saved config for a specific protocol, regardless of which is
    /// currently active. Used by the connection auto-detect to label a surviving
    /// tunnel from the protocol the backend actually reports.
    pub fn config_for(&self, protocol: Protocol) -> Option<ProtocolConfig> {
        match protocol {
            Protocol::AmneziaWg => self.amneziawg.clone().map(ProtocolConfig::AmneziaWg),
            Protocol::Vless => self.vless.clone().map(ProtocolConfig::Vless),
            Protocol::WireGuard => self.wireguard.clone().map(ProtocolConfig::WireGuard),
        }
    }

    /// Which protocols have cached configs. AmneziaWG is listed first — it is the default.
    pub fn available_protocols(&self) -> Vec<Protocol> {
        let mut protocols = Vec::new();
        if self.amneziawg.is_some() {
            protocols.push(Protocol::AmneziaWg);
        }
        if self.vless.is_some() {
            protocols.push(Protocol::Vless);
        }
        if self.wireguard.is_some() {
            protocols.push(Protocol::WireGuard);
        }
        protocols
    }

    /// Whether any config is stored.
    pub fn has_any(&self) -> bool {
        self.wireguard.is_some() || self.amneziawg.is_some() || self.vless.is_some()
    }
}

/// Global VPN state
pub struct VpnState {
    pub configs: RwLock<SavedVpnConfigs>,
    pub connection: RwLock<ConnectionInfo>,
    pub speed_tracker: RwLock<SpeedTracker>,
    /// Consecutive polls where the backend was unreachable (`get_all_info` → None).
    /// Used by `get_connection_info` to avoid mistaking a transient IPC gap (UI
    /// process reconnecting to the Android :vpn process) for a stopped tunnel.
    pub unreachable_polls: AtomicU32,
}

impl VpnState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            configs: RwLock::new(SavedVpnConfigs::default()),
            connection: RwLock::new(ConnectionInfo::default()),
            speed_tracker: RwLock::new(SpeedTracker::new()),
            unreachable_polls: AtomicU32::new(0),
        })
    }
}

impl Default for VpnState {
    fn default() -> Self {
        Self {
            configs: RwLock::new(SavedVpnConfigs::default()),
            connection: RwLock::new(ConnectionInfo::default()),
            speed_tracker: RwLock::new(SpeedTracker::new()),
            unreachable_polls: AtomicU32::new(0),
        }
    }
}
