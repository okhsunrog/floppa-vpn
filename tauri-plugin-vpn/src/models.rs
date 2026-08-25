use serde::{Deserialize, Serialize};

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
