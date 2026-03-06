use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use shoes_lite::api::VlessConfig;
use specta::Type;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Connection status enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    VerifyingHandshake,
    Connected,
    Disconnecting,
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
    pub server_endpoint: Option<String>,
    pub assigned_ip: Option<String>,
    pub connected_at: Option<i64>, // Unix timestamp
    pub last_handshake: Option<i64>,
    pub stats: TrafficStats,
}

impl Default for ConnectionInfo {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            server_endpoint: None,
            assigned_ip: None,
            connected_at: None,
            last_handshake: None,
            stats: TrafficStats::default(),
        }
    }
}

/// Tracks previous stats for computing transfer rates
pub struct SpeedTracker {
    prev_tx_bytes: u64,
    prev_rx_bytes: u64,
    prev_time: std::time::Instant,
}

impl SpeedTracker {
    pub fn new() -> Self {
        Self {
            prev_tx_bytes: 0,
            prev_rx_bytes: 0,
            prev_time: std::time::Instant::now(),
        }
    }

    /// Update with new cumulative byte counts and return computed speeds (bytes/sec)
    pub fn update(&mut self, tx_bytes: u64, rx_bytes: u64) -> (f64, f64) {
        let now = std::time::Instant::now();
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
    #[serde(rename = "vless")]
    #[specta(skip)]
    Vless(VlessVpnConfig),
}

impl ProtocolConfig {
    /// Server endpoint as "host:port" string.
    pub fn endpoint_str(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.peer_endpoint,
            Self::Vless(vless) => &vless.server_addr,
        }
    }

    /// Local tunnel address string (e.g. "10.0.0.2/32").
    pub fn address(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.address,
            Self::Vless(vless) => &vless.address,
        }
    }

    /// Local tunnel address as IpNetwork.
    pub fn address_network(&self) -> Result<IpNetwork, String> {
        match self {
            Self::WireGuard(wg) => wg.address_network(),
            Self::Vless(vless) => vless.address_network(),
        }
    }

    /// DNS servers.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        match self {
            Self::WireGuard(wg) => wg.dns_servers(),
            Self::Vless(vless) => vless.dns_servers(),
        }
    }

    /// Allowed IPs / routes.
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        match self {
            Self::WireGuard(wg) => wg.allowed_ips_networks(),
            Self::Vless(vless) => vless.allowed_ips_networks(),
        }
    }

    /// Tunnel MTU.
    pub fn get_mtu(&self) -> u16 {
        match self {
            Self::WireGuard(wg) => wg.get_mtu(),
            Self::Vless(vless) => vless.get_mtu(),
        }
    }

    /// Protocol name for display / persistence.
    pub fn protocol_name(&self) -> &'static str {
        match self {
            Self::WireGuard(_) => "wireguard",
            Self::Vless(_) => "vless",
        }
    }
}

/// Global VPN state
pub struct VpnState {
    pub config: RwLock<Option<ProtocolConfig>>,
    pub connection: RwLock<ConnectionInfo>,
    pub speed_tracker: RwLock<SpeedTracker>,
}

impl VpnState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(None),
            connection: RwLock::new(ConnectionInfo::default()),
            speed_tracker: RwLock::new(SpeedTracker::new()),
        })
    }
}

impl Default for VpnState {
    fn default() -> Self {
        Self {
            config: RwLock::new(None),
            connection: RwLock::new(ConnectionInfo::default()),
            speed_tracker: RwLock::new(SpeedTracker::new()),
        }
    }
}
