//! Who this installation is, as the server needs to be told.
//!
//! One implementation, because there used to be three and they disagreed. The app kept a
//! `device.json` in the Tauri config directory and read the hostname through `gethostname`; the
//! command-line client kept a bare `device_id` file beside its token and read `$HOSTNAME`, falling
//! back to `/etc/hostname`; and the session file kept a third copy of the answer, written by
//! whichever of them had last spoken to a webview. Three generators of a value whose entire job is
//! to be stable is two too many — and the cost of them drifting is not cosmetic: the server tells
//! devices apart by this id, so a second one is a second device, with its own peers taken from the
//! account's limit.
//!
//! The directory is a parameter rather than something resolved here. It is genuinely different per
//! caller — the app's Tauri config directory, the command-line client's sudo-aware one, and, once
//! there is a service, a root-owned one under `/var/lib` — and this module has no way to guess
//! which it is being asked about.

use floppa_api_client::DeviceIdentity;
use floppa_vpn_core::private_file::write_private;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// What the app has always written. The command-line client's bare `device_id` file is migrated
/// into it on first use.
const DEVICE_FILENAME: &str = "device.json";

/// The file the command-line client wrote before this module existed: the uuid and nothing else.
const LEGACY_DEVICE_FILENAME: &str = "device_id";

#[derive(Serialize, Deserialize)]
struct StoredDeviceId {
    device_id: String,
}

fn path(dir: &Path) -> PathBuf {
    dir.join(DEVICE_FILENAME)
}

/// This installation's device id, created once and kept in `dir`.
///
/// Whatever is already stored is returned as-is, and that is deliberate: the id is an opaque key
/// as far as the server is concerned, so refusing one that does not parse as a uuid would mint a
/// new one — and orphan the peers the old one owns, which is the very thing this exists to
/// prevent. Only an empty or unreadable file starts over.
pub fn device_id(dir: &Path) -> Result<String, String> {
    let file = path(dir);

    match std::fs::read_to_string(&file) {
        Ok(json) => match serde_json::from_str::<StoredDeviceId>(&json) {
            Ok(stored) if !stored.device_id.is_empty() => return Ok(stored.device_id),
            Ok(_) => warn!("{} holds no device id; making a new one", file.display()),
            Err(e) => warn!("{} does not parse ({e}); making a new one", file.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(id) = take_legacy(dir, &file)? {
                return Ok(id);
            }
        }
        Err(e) => return Err(format!("could not read {}: {e}", file.display())),
    }

    let id = uuid::Uuid::new_v4().to_string();
    store(&file, &id)?;
    info!("created a new device identity: {id}");
    Ok(id)
}

/// Adopt a bare `device_id` file, if one is there, rather than registering as a new device.
fn take_legacy(dir: &Path, file: &Path) -> Result<Option<String>, String> {
    let legacy = dir.join(LEGACY_DEVICE_FILENAME);
    let Ok(raw) = std::fs::read_to_string(&legacy) else {
        return Ok(None);
    };
    let id = raw.trim();
    if id.is_empty() {
        return Ok(None);
    }
    store(file, id)?;
    // Only after the new file is safely written: a crash in between leaves both, and the next run
    // reads the new one.
    if let Err(e) = std::fs::remove_file(&legacy) {
        warn!("could not remove {}: {e}", legacy.display());
    }
    info!("adopted the device identity from {}", legacy.display());
    Ok(Some(id.to_owned()))
}

fn store(file: &Path, id: &str) -> Result<(), String> {
    let json = serde_json::to_string(&StoredDeviceId {
        device_id: id.to_owned(),
    })
    .map_err(|e| format!("serialize: {e}"))?;
    write_private(file, json.as_bytes()).map_err(|e| format!("write {}: {e}", file.display()))
}

/// What this machine calls itself.
///
/// Through `gethostname` rather than `$HOSTNAME`: the variable is not exported by every shell and
/// is absent in most of the places a headless client runs, and `/etc/hostname` is a configuration
/// file that a running system is free to disagree with.
pub fn device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// How this device introduces itself when a peer is created.
///
/// `app_version` is passed in rather than read from `env!`, because this crate is not the running
/// binary — it is linked into the app, into the command-line client and into the service, and each
/// of those has its own version. Reading it here would report this crate's version for all three.
pub fn device_identity(dir: &Path, app_version: &str) -> Result<DeviceIdentity, String> {
    Ok(DeviceIdentity {
        device_id: device_id(dir)?,
        device_name: Some(device_name()),
        platform: std::env::consts::OS.to_owned(),
        app_version: app_version.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_made_once_and_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = device_id(dir.path()).unwrap();
        assert_eq!(device_id(dir.path()).unwrap(), first);
    }

    /// The whole point of the module: the command-line client's old file is the same device, not
    /// a new one.
    #[test]
    fn a_bare_device_id_file_is_adopted_rather_than_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_DEVICE_FILENAME), "abc-123\n").unwrap();

        assert_eq!(device_id(dir.path()).unwrap(), "abc-123");
        assert!(!dir.path().join(LEGACY_DEVICE_FILENAME).exists());
        // And it stays adopted once the old file is gone.
        assert_eq!(device_id(dir.path()).unwrap(), "abc-123");
    }

    /// An id that is not a uuid is still an id. Minting a fresh one would leave the peers the old
    /// one owns with no device to belong to.
    #[test]
    fn a_stored_id_that_is_not_a_uuid_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(DEVICE_FILENAME),
            r#"{"device_id":"not-a-uuid"}"#,
        )
        .unwrap();
        assert_eq!(device_id(dir.path()).unwrap(), "not-a-uuid");
    }

    #[test]
    fn a_file_that_does_not_parse_starts_over_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DEVICE_FILENAME), "{not json").unwrap();
        let id = device_id(dir.path()).unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn the_binary_says_what_version_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let identity = device_identity(dir.path(), "9.9.9").unwrap();
        assert_eq!(identity.app_version, "9.9.9");
        assert_eq!(identity.platform, std::env::consts::OS);
    }
}
