use super::state::SavedVpnConfigs;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::{info, warn};

#[cfg(not(target_os = "android"))]
const KEYRING_SERVICE: &str = "floppa-vpn";
#[cfg(not(target_os = "android"))]
const KEYRING_ENTRY: &str = "vpn-config";
/// The keyring entry releases before 0.5.1 wrote. Nothing reads it any more; logout still
/// clears it so an install that upgraded through 0.5.1 does not keep a stale key around.
#[cfg(not(target_os = "android"))]
const LEGACY_KEYRING_ENTRY: &str = "wg-config";
const CONFIG_FILENAME: &str = "vpn-config.json";
/// What the keyring is called when a stored shape is reported.
#[cfg(not(target_os = "android"))]
const KEYRING_SOURCE: &str = "the OS keyring";

/// Tauri app config dir, set once at startup
static APP_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Initialize the config directory from Tauri's path resolver.
/// Must be called during app setup.
pub fn init_config_dir(path: PathBuf) {
    let _ = APP_CONFIG_DIR.set(path);
}

/// Get the config directory for the app, creating it if needed.
pub fn config_dir() -> Result<PathBuf, String> {
    get_config_dir()
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

/// On-disk device identity (desktop only — Android uses ANDROID_ID).
#[cfg(not(target_os = "android"))]
#[derive(Serialize, Deserialize)]
struct DeviceIdentity {
    device_id: String,
}

/// Get or create a persistent device UUID (desktop only).
/// Stored at `~/.config/floppa-vpn/device.json`.
#[cfg(not(target_os = "android"))]
pub fn get_or_create_device_id() -> Result<String, String> {
    use uuid::Uuid;

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

/// Where the configs live.
///
/// Desktop prefers the keyring; the file is the fallback for when the keyring is unavailable and
/// the only storage on Android. The two must not silently coexist: a plaintext copy of the private
/// keys left behind by a single keyring outage used to survive every later successful keyring save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Storage {
    #[cfg(not(target_os = "android"))]
    Keyring,
    File,
}

/// The envelope actually written to either storage.
///
/// `updated_at` decides which copy wins when both exist — the one written last, whichever storage
/// it landed in. The bare payload 0.5.1 wrote reads through [`parse_stored`] with
/// `updated_at = 0`, so anything written since always beats it.
#[derive(Serialize)]
struct StoredConfigsRef<'a> {
    updated_at: i64,
    configs: &'a SavedVpnConfigs,
}

#[derive(Deserialize)]
struct StoredConfigs {
    updated_at: i64,
    configs: SavedVpnConfigs,
}

/// A copy of the configs, from wherever it was read.
struct Loaded {
    /// Only compared on desktop, where there are two storages to choose between.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    updated_at: i64,
    configs: SavedVpnConfigs,
}

fn envelope(configs: &SavedVpnConfigs) -> Option<String> {
    let stored = StoredConfigsRef {
        updated_at: chrono::Utc::now().timestamp(),
        configs,
    };
    match serde_json::to_string(&stored) {
        Ok(json) => Some(json),
        Err(e) => {
            warn!("Failed to serialize configs: {e}");
            None
        }
    }
}

/// Save all VPN configs.
///
/// Desktop: to the OS keyring, and on success the plaintext fallback file is removed so a
/// keyring outage in the past does not leave the keys on disk forever. Only when the keyring
/// fails is the file written. Android: the file, always.
pub fn save_configs(configs: &SavedVpnConfigs) {
    let Some(json) = envelope(configs) else {
        return;
    };

    #[cfg(not(target_os = "android"))]
    {
        if save_to_keyring(&json) {
            remove_config_file();
            return;
        }
    }

    save_config_file(&json);
}

#[cfg(not(target_os = "android"))]
fn save_to_keyring(json: &str) -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
        Ok(entry) => match entry.set_password(json) {
            Ok(()) => {
                info!(storage = ?Storage::Keyring, "VPN configs saved");
                true
            }
            Err(e) => {
                warn!("Keyring save failed, falling back to file: {e}");
                false
            }
        },
        Err(e) => {
            warn!("Keyring unavailable, falling back to file: {e}");
            false
        }
    }
}

/// What the keyring had to say.
#[cfg(not(target_os = "android"))]
enum KeyringRead {
    Found(Box<Loaded>),
    Empty,
    /// Could not be asked. Distinct from `Empty` so a file is not migrated into a keyring that is
    /// known to be broken.
    Unavailable,
}

#[cfg(not(target_os = "android"))]
fn load_from_keyring() -> KeyringRead {
    let entry = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
        Ok(entry) => entry,
        Err(e) => {
            warn!("Keyring unavailable, trying file fallback: {e}");
            return KeyringRead::Unavailable;
        }
    };
    match entry.get_password() {
        Ok(stored) => match parse_stored(&stored, KEYRING_SOURCE) {
            Some(loaded) => KeyringRead::Found(Box::new(loaded)),
            None => KeyringRead::Empty,
        },
        Err(keyring::Error::NoEntry) => KeyringRead::Empty,
        Err(e) => {
            warn!("Keyring load failed, trying file fallback: {e}");
            KeyringRead::Unavailable
        }
    }
}

/// Load VPN configs.
///
/// Desktop: whichever of keyring and file was written last wins. A winning file is moved into the
/// keyring (and deleted) when the keyring is usable; a losing file is a stale plaintext copy and is
/// deleted. Android: the file.
pub fn load_configs() -> Option<SavedVpnConfigs> {
    let from_file = load_configs_file();

    #[cfg(not(target_os = "android"))]
    {
        let from_keyring = load_from_keyring();
        match (from_keyring, from_file) {
            (KeyringRead::Found(k), Some(f)) if f.updated_at > k.updated_at => {
                info!(
                    storage = ?Storage::File,
                    "VPN configs loaded; the file is newer than the keyring, migrating"
                );
                migrate_file_to_keyring(&f.configs);
                Some(f.configs)
            }
            (KeyringRead::Found(k), Some(_)) => {
                info!(
                    storage = ?Storage::Keyring,
                    "VPN configs loaded; removing the stale plaintext copy"
                );
                remove_config_file();
                Some(k.configs)
            }
            (KeyringRead::Found(k), None) => {
                info!(storage = ?Storage::Keyring, "VPN configs loaded");
                Some(k.configs)
            }
            (KeyringRead::Empty, Some(f)) => {
                info!(
                    storage = ?Storage::File,
                    "VPN configs loaded; the keyring is empty, migrating"
                );
                migrate_file_to_keyring(&f.configs);
                Some(f.configs)
            }
            (KeyringRead::Unavailable, Some(f)) => {
                info!(storage = ?Storage::File, "VPN configs loaded; keyring unavailable");
                Some(f.configs)
            }
            (KeyringRead::Empty | KeyringRead::Unavailable, None) => None,
        }
    }

    #[cfg(target_os = "android")]
    {
        from_file.map(|f| {
            info!(storage = ?Storage::File, "VPN configs loaded");
            f.configs
        })
    }
}

/// Put a file-only copy into the keyring, and drop the file once it is safely there.
#[cfg(not(target_os = "android"))]
fn migrate_file_to_keyring(configs: &SavedVpnConfigs) {
    if let Some(json) = envelope(configs)
        && save_to_keyring(&json)
    {
        remove_config_file();
    }
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
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, LEGACY_KEYRING_ENTRY) {
            match entry.delete_credential() {
                Ok(()) => info!("Legacy WG config deleted from OS keyring"),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => warn!("Failed to delete legacy WG config from keyring: {e}"),
            }
        }
    }

    remove_config_file();
}

fn remove_config_file() {
    let Ok(dir) = get_config_dir() else {
        return;
    };
    let path = dir.join(CONFIG_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => info!("Config file deleted: {path:?}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Failed to delete config file {path:?}: {e}"),
    }
}

/// Parse what a storage held, naming `source` (the file, or the keyring) when it is unusable.
///
/// Two shapes are read: the envelope, and the bare [`SavedVpnConfigs`] payload that 0.5.1 wrote,
/// which has no timestamp and so counts as older than anything. Whatever else is there — the
/// shapes releases before 0.5.1 wrote, or a hand-edited file — is dropped with a warning: the app
/// provisions a fresh peer from the server anyway.
fn parse_stored(stored: &str, source: impl Display) -> Option<Loaded> {
    let (updated_at, configs) = match serde_json::from_str::<StoredConfigs>(stored) {
        Ok(StoredConfigs {
            updated_at,
            configs,
        }) => (updated_at, configs),
        Err(_) => match serde_json::from_str::<SavedVpnConfigs>(stored) {
            Ok(configs) if configs.has_any() => {
                info!("VPN configs in {source} predate the envelope; migrating");
                (0, configs)
            }
            Ok(_) => {
                warn!("Ignoring VPN configs in {source}: not a shape this version reads");
                return None;
            }
            Err(e) => {
                warn!("Ignoring VPN configs in {source}: {e}");
                return None;
            }
        },
    };
    configs.has_any().then_some(Loaded {
        updated_at,
        configs,
    })
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

    info!(storage = ?Storage::File, "VPN configs saved: {path:?}");
}

fn load_configs_file() -> Option<Loaded> {
    let path = get_config_dir().ok()?.join(CONFIG_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(json) => parse_stored(&json, path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!("Failed to read config file {path:?}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Preference, Protocol};
    use crate::state::WgConfig;

    const WG_CONF: &str = "[Interface]\nPrivateKey = gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=\nAddress = 10.0.0.2/24\n[Peer]\nPublicKey = HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=\nAllowedIPs = 0.0.0.0/0\nEndpoint = vpn.example.com:51820\n";

    /// What 0.5.1 wrote to the keyring and the file: the bare payload, with the protocol still
    /// the `active_protocol` string.
    const V0_5_1_PAYLOAD: &str = r#"{"active_protocol":"wireguard","wireguard":{"private_key":"gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=","address":"10.0.0.2/24","dns":"1.1.1.1","mtu":null,"peer_public_key":"HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=","peer_preshared_key":null,"peer_endpoint":"vpn.example.com:51820","allowed_ips":"0.0.0.0/0","persistent_keepalive":25},"amneziawg":null,"vless":null}"#;

    fn configs() -> SavedVpnConfigs {
        SavedVpnConfigs {
            preferred_protocol: Preference(Some(Protocol::WireGuard)),
            wireguard: Some(WgConfig::from_config_str(WG_CONF).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn the_envelope_round_trips_with_its_timestamp() {
        let json = envelope(&configs()).unwrap();
        let loaded = parse_stored(&json, "test").unwrap();
        assert!(loaded.updated_at > 0);
        assert_eq!(
            loaded.configs.preferred_protocol,
            Preference(Some(Protocol::WireGuard))
        );
        assert!(loaded.configs.wireguard.is_some());
    }

    #[test]
    fn an_envelope_holding_no_config_is_nothing() {
        let json = envelope(&SavedVpnConfigs::default()).unwrap();
        assert!(parse_stored(&json, "test").is_none());
    }

    #[test]
    fn the_0_5_1_payload_loads_as_older_than_anything_and_resaves_as_an_envelope() {
        let loaded = parse_stored(V0_5_1_PAYLOAD, "test").unwrap();
        assert_eq!(loaded.updated_at, 0);
        assert_eq!(
            loaded.configs.preferred_protocol,
            Preference(Some(Protocol::WireGuard))
        );
        let wg = serde_json::to_value(loaded.configs.wireguard.as_ref().unwrap()).unwrap();
        assert_eq!(wg["peer_endpoint"], "vpn.example.com:51820");
        assert_eq!(wg["persistent_keepalive"], 25);

        let json = envelope(&loaded.configs).unwrap();
        let resaved: StoredConfigs = serde_json::from_str(&json).unwrap();
        assert!(resaved.updated_at > 0);
        assert!(resaved.configs.wireguard.is_some());
    }

    #[test]
    fn shapes_older_than_0_5_1_are_ignored() {
        // Pre-0.5: one `ProtocolConfig`.
        let single = format!(
            r#"{{"protocol":"wireguard","config":{}}}"#,
            serde_json::to_string(configs().wireguard.as_ref().unwrap()).unwrap()
        );
        assert!(parse_stored(&single, "test").is_none());
        // Older still: the raw WireGuard `.conf` text.
        assert!(parse_stored(WG_CONF, "test").is_none());
        assert!(parse_stored("", "test").is_none());
    }
}
