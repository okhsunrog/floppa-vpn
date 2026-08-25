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

    /// Identity of this service start.
    ///
    /// Minted per start by the UI process and never reused, so a reply from an instance that has
    /// since been superseded is rejectable by value. It is deliberately not the intent's epoch,
    /// which every protocol and pass of one cycle shares.
    ///
    /// The tunnel config itself no longer travels here: the service binds its socket first and is
    /// then given a typed config over it, so the protocol is stated rather than re-derived by
    /// inspecting a string at the far end.
    #[serde(default)]
    pub generation: u64,
}

fn default_mtu() -> u32 {
    1280
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
