use super::state::{ProtocolConfig, VlessVpnConfig, WgConfig};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(not(target_os = "android"))]
const KEYRING_SERVICE: &str = "floppa-vpn";
#[cfg(not(target_os = "android"))]
const KEYRING_ENTRY: &str = "wg-config";
const CONFIG_FILENAME: &str = "wg.conf";

/// Tauri app config dir, set once at startup
static APP_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Initialize the config directory from Tauri's path resolver.
/// Must be called during app setup.
pub fn init_config_dir(path: PathBuf) {
    let _ = APP_CONFIG_DIR.set(path);
}

/// On-disk device identity
#[derive(Serialize, Deserialize)]
struct DeviceIdentity {
    device_id: String,
}

/// Get the config directory for the app
fn get_config_dir() -> Result<PathBuf, String> {
    let config_dir = APP_CONFIG_DIR
        .get()
        .cloned()
        .or_else(|| dirs::config_dir().map(|d| d.join("floppa-vpn")))
        .ok_or("Could not determine config directory")?;

    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config dir: {e}"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700));
        }
    }

    Ok(config_dir)
}

/// Get or create a persistent device UUID.
/// Stored at `~/.config/floppa-vpn/device.json`.
pub fn get_or_create_device_id() -> Result<String, String> {
    let path = get_config_dir()?.join("device.json");

    if path.exists() {
        let json = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read device identity: {e}"))?;
        let identity: DeviceIdentity = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse device identity: {e}"))?;
        return Ok(identity.device_id);
    }

    let device_id = Uuid::new_v4().to_string();
    let identity = DeviceIdentity {
        device_id: device_id.clone(),
    };

    let json = serde_json::to_string_pretty(&identity)
        .map_err(|e| format!("Failed to serialize device identity: {e}"))?;

    std::fs::write(&path, &json).map_err(|e| format!("Failed to write device identity: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    info!("Created new device identity: {device_id}");
    Ok(device_id)
}

/// Get the device hostname.
pub fn get_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Multi-protocol config persistence
// ---------------------------------------------------------------------------

/// JSON envelope for VLESS config persistence.
#[derive(Serialize, Deserialize)]
struct SavedVlessConfig {
    protocol: String, // always "vless"
    #[serde(flatten)]
    config: VlessVpnConfig,
}

/// Parse a raw config string into a ProtocolConfig.
///
/// Detection: if the string starts with `{` it's JSON (VLESS envelope),
/// otherwise it's a WireGuard config file.
pub fn parse_config_str(config_str: &str) -> Result<ProtocolConfig, String> {
    let trimmed = config_str.trim();
    if trimmed.starts_with('{') {
        // Try VLESS JSON envelope
        let saved: SavedVlessConfig =
            serde_json::from_str(trimmed).map_err(|e| format!("Invalid VLESS config JSON: {e}"))?;
        if saved.protocol != "vless" {
            return Err(format!("Unknown protocol: {}", saved.protocol));
        }
        Ok(ProtocolConfig::Vless(saved.config))
    } else if trimmed.starts_with("vless://") {
        // Bare VLESS URI — parse and wrap with defaults
        let vless = VlessVpnConfig::from_uri(trimmed)?;
        Ok(ProtocolConfig::Vless(vless))
    } else {
        // WireGuard config format
        let wg = WgConfig::from_config_str(trimmed)?;
        Ok(ProtocolConfig::WireGuard(wg))
    }
}

/// Serialize a ProtocolConfig for persistence.
fn serialize_config(config: &ProtocolConfig) -> String {
    match config {
        ProtocolConfig::WireGuard(wg) => wg.to_config_str(),
        ProtocolConfig::Vless(vless) => {
            let saved = SavedVlessConfig {
                protocol: "vless".to_string(),
                config: vless.clone(),
            };
            serde_json::to_string_pretty(&saved).unwrap_or_default()
        }
    }
}

/// Save VPN config to OS keyring (fallback to file on Android / keyring failure).
pub fn save_vpn_config(config: &ProtocolConfig) {
    let config_str = serialize_config(config);

    #[cfg(not(target_os = "android"))]
    {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            Ok(entry) => match entry.set_password(&config_str) {
                Ok(()) => {
                    info!("VPN config saved to OS keyring");
                    return;
                }
                Err(e) => warn!("Keyring save failed, falling back to file: {e}"),
            },
            Err(e) => warn!("Keyring unavailable, falling back to file: {e}"),
        }
    }

    // File fallback (always used on Android, fallback on desktop)
    save_config_file(&config_str);
}

/// Load VPN config from OS keyring (fallback to file).
pub fn load_vpn_config() -> Option<ProtocolConfig> {
    let config_str = load_raw_config()?;
    match parse_config_str(&config_str) {
        Ok(config) => Some(config),
        Err(e) => {
            warn!("Failed to parse saved config: {e}");
            None
        }
    }
}

/// Load raw config string from keyring/file (for persistence layer).
fn load_raw_config() -> Option<String> {
    #[cfg(not(target_os = "android"))]
    {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            Ok(entry) => match entry.get_password() {
                Ok(config_str) => {
                    info!("VPN config loaded from OS keyring");
                    return Some(config_str);
                }
                Err(keyring::Error::NoEntry) => {}
                Err(e) => warn!("Keyring load failed, trying file fallback: {e}"),
            },
            Err(e) => warn!("Keyring unavailable, trying file fallback: {e}"),
        }
    }

    // File fallback
    load_config_file()
}

/// Delete saved VPN config from both keyring and file.
pub fn delete_vpn_config() {
    #[cfg(not(target_os = "android"))]
    {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            match entry.delete_credential() {
                Ok(()) => info!("VPN config deleted from OS keyring"),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => warn!("Failed to delete VPN config from keyring: {e}"),
            }
        }
    }

    // Also remove file fallback if it exists
    if let Ok(dir) = get_config_dir() {
        let path = dir.join(CONFIG_FILENAME);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            info!("VPN config file deleted");
        }
    }
}

fn save_config_file(config_str: &str) {
    let path = match get_config_dir() {
        Ok(dir) => dir.join(CONFIG_FILENAME),
        Err(e) => {
            warn!("Failed to save VPN config to file: {e}");
            return;
        }
    };

    if let Err(e) = std::fs::write(&path, config_str) {
        warn!("Failed to write VPN config file: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    info!("VPN config saved to file: {path:?}");
}

fn load_config_file() -> Option<String> {
    let path = get_config_dir().ok()?.join(CONFIG_FILENAME);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(config_str) => {
            info!("VPN config loaded from file: {path:?}");
            Some(config_str)
        }
        Err(e) => {
            warn!("Failed to read VPN config file: {e}");
            None
        }
    }
}
