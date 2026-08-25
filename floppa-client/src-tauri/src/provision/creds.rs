//! What the client needs to speak to the server as this user: where the server is, and the token.
//!
//! It is a file rather than a value held in the UI because the process that needs it most has no
//! UI. A peer deleted while the phone is in a pocket has to be recreated from `:vpn`, and `:vpn`
//! never runs a webview — the token lives in the webview's `localStorage`, so it has to be written
//! somewhere both processes can read. `0600` in the private app directory, like every other secret
//! this client keeps.
//!
//! The base URL is stored beside the token rather than baked in. The frontend gets it at build
//! time from `VITE_API_URL`, and repeating that as a compile-time constant here would mean a
//! server move needs a rebuild of the Rust side too. It is per-install state; it is stored as
//! per-install state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use crate::vpn::private_file::write_private;

const CREDS_FILENAME: &str = "server-credentials.json";

/// Bumped when the shape changes incompatibly. An unreadable file is not migrated — it is
/// dropped, and the next thing the user does in the app writes a fresh one.
const CREDS_VERSION: u32 = 1;

/// Where the server is and who we are to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCredentials {
    #[serde(default)]
    pub version: u32,
    /// Base URL of the API, without a trailing slash (e.g. `https://host.example/api`).
    pub base_url: String,
    /// The bearer token. Rewritten on login, on every sliding refresh, and cleared on logout.
    pub token: String,
}

impl ServerCredentials {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            version: CREDS_VERSION,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
        }
    }

    fn usable(&self) -> bool {
        !self.base_url.is_empty() && !self.token.is_empty()
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(CREDS_FILENAME)
}

/// Write credentials, or remove them.
///
/// Called by the UI process on login, on every token refresh, and — with `None` — on logout.
/// Atomic, so the other process reading the file concurrently sees the old content or the new
/// one and never half of either.
pub fn store(dir: &Path, creds: Option<ServerCredentials>) -> Result<(), String> {
    let file = path(dir);
    let Some(creds) = creds else {
        match std::fs::remove_file(&file) {
            Ok(()) => debug!("the server credentials were cleared"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", file.display())),
        }
        return Ok(());
    };

    let json = serde_json::to_string(&creds).map_err(|e| format!("serialize: {e}"))?;
    write_private(&file, json.as_bytes()).map_err(|e| format!("write {}: {e}", file.display()))?;
    debug!(base_url = %creds.base_url, "the server credentials were written");
    Ok(())
}

/// The credentials, read from disk every time.
///
/// Deliberately uncached. The process that *writes* these is the UI and the process that most
/// needs them is `:vpn`, and a token is rewritten on every sliding refresh — so a copy held in
/// memory by the reader is a copy that goes stale in the ordinary course of things, and the way
/// it fails is a background repair authenticating with a token that expired days ago. The file is
/// a few hundred bytes and is read at most a few times a minute; there is nothing here to save.
///
/// `None` means there is nothing usable — signed out, never signed in, or a file this build
/// cannot read. Every one of those is the same to a caller: it cannot talk to the server as
/// anybody, so it must not try.
pub fn load(dir: &Path) -> Option<ServerCredentials> {
    let file = path(dir);
    let json = match std::fs::read_to_string(&file) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("could not read {}: {e}", file.display());
            return None;
        }
    };
    let creds: ServerCredentials = match serde_json::from_str(&json) {
        Ok(creds) => creds,
        Err(e) => {
            warn!(
                "{} does not parse and is being ignored: {e}",
                file.display()
            );
            return None;
        }
    };
    if creds.version != CREDS_VERSION || !creds.usable() {
        warn!(
            version = creds.version,
            "the stored credentials are not usable by this build"
        );
        return None;
    }

    Some(creds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> ServerCredentials {
        ServerCredentials::new("https://example.test/api/".into(), "a.b.c".into())
    }

    #[test]
    fn a_trailing_slash_is_not_carried_into_every_url() {
        assert_eq!(creds().base_url, "https://example.test/api");
    }

    #[test]
    fn what_was_stored_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), Some(creds())).unwrap();
        assert_eq!(load(dir.path()), Some(creds()));
    }

    #[test]
    fn signing_out_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), Some(creds())).unwrap();
        store(dir.path(), None).unwrap();
        assert_eq!(load(dir.path()), None);
        assert!(!path(dir.path()).exists());
    }

    #[test]
    fn a_file_from_another_shape_is_dropped_rather_than_half_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"version":99,"base_url":"https://x/api","token":"t"}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn credentials_with_nothing_in_them_are_not_credentials() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            r#"{"version":1,"base_url":"","token":"t"}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()), None);
    }
}
