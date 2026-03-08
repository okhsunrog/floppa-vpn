use serde::{Deserialize, Serialize};

/// Configuration for starting a VPN tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnConfig {
    /// IPv4 address with prefix (e.g., "10.0.0.2/24")
    pub ipv4_addr: String,

    /// Optional IPv6 address with prefix
    #[serde(default)]
    pub ipv6_addr: Option<String>,

    /// Routes to add (CIDR notation, e.g., ["0.0.0.0/0", "::/0"])
    #[serde(default)]
    pub routes: Vec<String>,

    /// DNS server address
    #[serde(default)]
    pub dns: Option<String>,

    /// MTU size (default: 1280)
    #[serde(default = "default_mtu")]
    pub mtu: u32,

    /// Apps to exclude from VPN (split tunneling - exclude mode)
    #[serde(default)]
    pub disallowed_apps: Vec<String>,

    /// Apps to route through VPN exclusively (split tunneling - include mode)
    /// Mutually exclusive with disallowed_apps on Android.
    #[serde(default)]
    pub allowed_apps: Vec<String>,

    /// Raw protocol config string (WG config text or vless:// URI), passed to :vpn process
    #[serde(default)]
    pub protocol_config: Option<String>,
}

fn default_mtu() -> u32 {
    1280
}

/// VPN tunnel status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VpnStatus {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

/// Event payload when VPN starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnStartedEvent {
    /// TUN file descriptor (Android) or tunnel identifier (iOS)
    pub fd: i32,
}

/// Information about an installed app (for split tunneling UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Android package name (e.g., "com.example.app")
    pub package_name: String,
    /// User-visible app name
    pub label: String,
    /// Whether this is a system app
    pub is_system: bool,
    /// App icon as base64-encoded PNG (optional, may be absent if loading failed)
    #[serde(default)]
    pub icon: Option<String>,
}

/// Safe area insets (status bar, nav bar) in dp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeAreaInsets {
    pub top: f64,
    pub bottom: f64,
}

/// Device name response from Android plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceNameResponse {
    pub name: String,
}

/// Device ID response from Android plugin (ANDROID_ID).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdResponse {
    pub id: String,
}

/// Event payload when VPN stops.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnStoppedEvent {
    /// Reason for stopping (if abnormal)
    #[serde(default)]
    pub reason: Option<String>,
}
