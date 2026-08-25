//! Everything needed to talk to the server as this user on this device — in a file both
//! processes read.
//!
//! It is a file rather than a value held in the UI because the process that needs it most has no
//! UI. A peer deleted while the phone is in a pocket has to be recreated from `:vpn`, and `:vpn`
//! never runs a webview — so the token, which lives in the webview's `localStorage`, has to be
//! written somewhere else. `0600` in the private app directory, like every other secret this
//! client keeps.
//!
//! Three of the four fields are here for the same reason: the frontend is where they are known.
//! The base URL is baked into the frontend at build time (`VITE_API_URL`), and the device id and
//! name come from the Android plugin, which only the UI process loads. Repeating any of them as a
//! compile-time constant in Rust would mean two places to change and one of them silently wrong.
//! What is *not* here is the platform and the app version, because those describe the binary and
//! the binary can say them itself.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use crate::vpn::private_file::write_private;
use floppa_api_client::DeviceIdentity;

const SESSION_FILENAME: &str = "server-session.json";

/// Bumped when the shape changes incompatibly. An unreadable file is not migrated — it is
/// dropped, and the next thing the user does in the app writes a fresh one.
const SESSION_VERSION: u32 = 1;

/// Where the server is, who we are to it, and which device is asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSession {
    #[serde(default)]
    pub version: u32,
    /// Base URL of the API, without a trailing slash (e.g. `https://host.example/api`).
    pub base_url: String,
    /// The bearer token. Rewritten on sign-in, on every sliding refresh, removed on sign-out.
    pub token: String,
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

impl ServerSession {
    pub fn new(
        base_url: String,
        token: String,
        device_id: String,
        device_name: Option<String>,
    ) -> Self {
        Self {
            version: SESSION_VERSION,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            device_id,
            device_name,
        }
    }

    fn usable(&self) -> bool {
        !self.base_url.is_empty() && !self.token.is_empty() && !self.device_id.is_empty()
    }

    /// Whether a [`load`] of this session would hand it back rather than refuse it.
    ///
    /// For the writer, which would otherwise have to guess whether what it just stored is enough
    /// to provision with — the frontend learns the token and the device identity at different
    /// moments, so an incomplete session is an ordinary intermediate state, not an error.
    pub fn is_usable(&self) -> bool {
        self.usable()
    }

    /// How this device introduces itself when a peer is created.
    ///
    /// The platform and the app version are read from the running binary rather than the file:
    /// they describe whoever is asking, and after an update that is not who wrote the file.
    pub fn identity(&self) -> DeviceIdentity {
        DeviceIdentity {
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            platform: std::env::consts::OS.to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(SESSION_FILENAME)
}

/// Write the session, or remove it.
///
/// Called by the UI process whenever any part of it changes, and — with `None` — on sign-out.
/// Atomic, so the other process reading the file concurrently sees the old content or the new
/// one and never half of either.
pub fn store(dir: &Path, session: Option<ServerSession>) -> Result<(), String> {
    let file = path(dir);
    let Some(session) = session else {
        match std::fs::remove_file(&file) {
            Ok(()) => debug!("the server session was cleared"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", file.display())),
        }
        return Ok(());
    };

    let json = serde_json::to_string(&session).map_err(|e| format!("serialize: {e}"))?;
    write_private(&file, json.as_bytes()).map_err(|e| format!("write {}: {e}", file.display()))?;
    debug!(base_url = %session.base_url, "the server session was written");
    Ok(())
}

/// The session, read from disk every time.
///
/// Deliberately uncached. The process that *writes* it is the UI and the process that most needs
/// it is `:vpn`, and the token is rewritten on every sliding refresh — so a copy held in memory by
/// the reader is a copy that goes stale in the ordinary course of things, and the way it fails is
/// a background repair authenticating with a token that expired days ago. The file is a few
/// hundred bytes and is read at most a few times a minute; there is nothing here to save.
///
/// `None` means there is nothing usable — signed out, never signed in, or a file this build
/// cannot read. Every one of those is the same to a caller: it cannot talk to the server as
/// anybody, so it must not try.
pub fn load(dir: &Path) -> Option<ServerSession> {
    let file = path(dir);
    let json = match std::fs::read_to_string(&file) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("could not read {}: {e}", file.display());
            return None;
        }
    };
    let session: ServerSession = match serde_json::from_str(&json) {
        Ok(session) => session,
        Err(e) => {
            warn!(
                "{} does not parse and is being ignored: {e}",
                file.display()
            );
            return None;
        }
    };
    if session.version != SESSION_VERSION || !session.usable() {
        warn!(
            version = session.version,
            "the stored session is not usable by this build"
        );
        return None;
    }

    Some(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> ServerSession {
        ServerSession::new(
            "https://example.test/api/".into(),
            "a.b.c".into(),
            "device-1".into(),
            Some("test phone".into()),
        )
    }

    #[test]
    fn a_trailing_slash_is_not_carried_into_every_url() {
        assert_eq!(session().base_url, "https://example.test/api");
    }

    #[test]
    fn what_was_stored_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), Some(session())).unwrap();
        assert_eq!(load(dir.path()), Some(session()));
    }

    #[test]
    fn signing_out_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), Some(session())).unwrap();
        store(dir.path(), None).unwrap();
        assert_eq!(load(dir.path()), None);
        assert!(!path(dir.path()).exists());
    }

    #[test]
    fn a_file_from_another_shape_is_dropped_rather_than_half_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"version":99,"base_url":"https://x/api","token":"t","device_id":"d"}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn a_session_that_cannot_ask_for_a_peer_is_not_a_session() {
        let dir = tempfile::tempdir().unwrap();
        // No device id: the server has no way to know which device is asking, so every peer
        // lookup would answer about nothing.
        std::fs::write(
            path(dir.path()),
            r#"{"version":1,"base_url":"https://x/api","token":"t","device_id":""}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn the_binary_says_what_the_binary_is() {
        let identity = session().identity();
        assert_eq!(identity.device_id, "device-1");
        assert_eq!(identity.platform, std::env::consts::OS);
        assert_eq!(identity.app_version, env!("CARGO_PKG_VERSION"));
    }
}
