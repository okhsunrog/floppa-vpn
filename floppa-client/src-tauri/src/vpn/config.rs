use super::state::{ProtocolConfig, SavedVpnConfigs, WgConfig};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(not(target_os = "android"))]
const KEYRING_SERVICE: &str = "floppa-vpn";
#[cfg(not(target_os = "android"))]
const KEYRING_ENTRY: &str = "vpn-config";
const CONFIG_FILENAME: &str = "vpn-config.json";
/// Legacy WG config filename — checked during load for backwards compatibility.
const LEGACY_CONFIG_FILENAME: &str = "wg.conf";

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

/// Save all VPN configs to OS keyring (fallback to file on Android / keyring failure).
pub fn save_configs(configs: &SavedVpnConfigs) {
    let json = match serde_json::to_string(configs) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize configs: {e}");
            return;
        }
    };

    #[cfg(not(target_os = "android"))]
    {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            Ok(entry) => match entry.set_password(&json) {
                Ok(()) => {
                    info!("VPN configs saved to OS keyring");
                    return;
                }
                Err(e) => warn!("Keyring save failed, falling back to file: {e}"),
            },
            Err(e) => warn!("Keyring unavailable, falling back to file: {e}"),
        }
    }

    // File fallback (always used on Android, fallback on desktop)
    save_config_file(&json);
}

/// Load VPN configs from OS keyring (fallback to file).
///
/// Backwards-compatible: migrates old single-config format to dual-config.
pub fn load_configs() -> Option<SavedVpnConfigs> {
    #[cfg(not(target_os = "android"))]
    {
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            Ok(entry) => match entry.get_password() {
                Ok(stored) => {
                    info!("VPN config loaded from OS keyring");
                    return parse_stored_configs(&stored);
                }
                Err(keyring::Error::NoEntry) => {
                    // Also check legacy keyring entry
                    if let Some(config) = load_legacy_keyring() {
                        return Some(migrate_single_config(config));
                    }
                }
                Err(e) => warn!("Keyring load failed, trying file fallback: {e}"),
            },
            Err(e) => warn!("Keyring unavailable, trying file fallback: {e}"),
        }
    }

    // File fallback — try new format first, then legacy
    if let Some(configs) = load_configs_file() {
        return Some(configs);
    }
    load_legacy_config_file().map(migrate_single_config)
}

/// Delete saved VPN config from both keyring and file.
pub fn delete_configs() {
    #[cfg(not(target_os = "android"))]
    {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
            match entry.delete_credential() {
                Ok(()) => info!("VPN config deleted from OS keyring"),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => warn!("Failed to delete VPN config from keyring: {e}"),
            }
        }
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, "wg-config") {
            match entry.delete_credential() {
                Ok(()) => info!("Legacy WG config deleted from OS keyring"),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => warn!("Failed to delete legacy WG config from keyring: {e}"),
            }
        }
    }

    if let Ok(dir) = get_config_dir() {
        for filename in [CONFIG_FILENAME, LEGACY_CONFIG_FILENAME] {
            let path = dir.join(filename);
            if path.exists() {
                let _ = std::fs::remove_file(&path);
                info!("Config file deleted: {path:?}");
            }
        }
    }
}

/// Parse stored configs — try new SavedVpnConfigs format first, fall back to old single ProtocolConfig, then legacy WG.
fn parse_stored_configs(stored: &str) -> Option<SavedVpnConfigs> {
    // Try new dual-config format
    if let Ok(configs) = serde_json::from_str::<SavedVpnConfigs>(stored)
        && configs.has_any()
    {
        return Some(configs);
    }
    // Try old single ProtocolConfig format
    if let Ok(config) = serde_json::from_str::<ProtocolConfig>(stored) {
        return Some(migrate_single_config(config));
    }
    // Fall back to legacy WG config format
    match WgConfig::from_config_str(stored) {
        Ok(wg) => {
            info!("Loaded legacy WireGuard config, migrating to new format");
            Some(migrate_single_config(ProtocolConfig::WireGuard(wg)))
        }
        Err(e) => {
            warn!("Failed to parse stored config: {e}");
            None
        }
    }
}

/// Migrate a single ProtocolConfig to the new dual-config format.
fn migrate_single_config(config: ProtocolConfig) -> SavedVpnConfigs {
    match config {
        ProtocolConfig::WireGuard(wg) => SavedVpnConfigs {
            active_protocol: "wireguard".to_string(),
            wireguard: Some(wg),
            vless: None,
        },
        ProtocolConfig::Vless(vless) => SavedVpnConfigs {
            active_protocol: "vless".to_string(),
            wireguard: None,
            vless: Some(vless),
        },
    }
}

/// Try loading from legacy keyring entry ("wg-config").
#[cfg(not(target_os = "android"))]
fn load_legacy_keyring() -> Option<ProtocolConfig> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, "wg-config").ok()?;
    match entry.get_password() {
        Ok(config_str) => {
            info!("Legacy WG config loaded from OS keyring");
            match WgConfig::from_config_str(&config_str) {
                Ok(wg) => Some(ProtocolConfig::WireGuard(wg)),
                Err(e) => {
                    warn!("Failed to parse legacy WG config from keyring: {e}");
                    None
                }
            }
        }
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            warn!("Failed to load legacy WG config from keyring: {e}");
            None
        }
    }
}

fn save_config_file(json: &str) {
    let path = match get_config_dir() {
        Ok(dir) => dir.join(CONFIG_FILENAME),
        Err(e) => {
            warn!("Failed to save config to file: {e}");
            return;
        }
    };

    if let Err(e) = std::fs::write(&path, json) {
        warn!("Failed to write config file: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    info!("VPN config saved to file: {path:?}");
}

fn load_configs_file() -> Option<SavedVpnConfigs> {
    let path = get_config_dir().ok()?.join(CONFIG_FILENAME);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(json) => {
            info!("VPN config loaded from file: {path:?}");
            parse_stored_configs(&json)
        }
        Err(e) => {
            warn!("Failed to read config file: {e}");
            None
        }
    }
}

/// Try loading from legacy `wg.conf` file.
fn load_legacy_config_file() -> Option<ProtocolConfig> {
    let path = get_config_dir().ok()?.join(LEGACY_CONFIG_FILENAME);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(config_str) => {
            info!("Legacy WG config loaded from file: {path:?}");
            match WgConfig::from_config_str(&config_str) {
                Ok(wg) => Some(ProtocolConfig::WireGuard(wg)),
                Err(e) => {
                    warn!("Failed to parse legacy WG config file: {e}");
                    None
                }
            }
        }
        Err(e) => {
            warn!("Failed to read legacy WG config file: {e}");
            None
        }
    }
}
