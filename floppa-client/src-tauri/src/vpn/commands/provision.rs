//! Peer provisioning: what the frontend hands over, and what it asks for.

use crate::provision::creds::{self, ServerCredentials};
use crate::vpn::config::config_dir;
use tracing::info;

/// Hand the session over to Rust, or take it away.
///
/// Called by the frontend whenever the auth store's token changes — sign-in, every sliding
/// refresh, sign-out — and once at startup for a token restored from `localStorage`. `token` is
/// `None` exactly when the user is signed out, and then the stored credentials are removed rather
/// than left to rot: they are what an autonomous repair would authenticate with, and a signed-out
/// device must not be able to make peers.
///
/// The `base_url` comes from the frontend because the frontend is where it is configured
/// (`VITE_API_URL`, baked in at build time). Keeping a second copy compiled into Rust would mean
/// two places to change and one of them silently wrong.
#[tauri::command]
#[specta::specta]
pub fn set_server_credentials(base_url: String, token: Option<String>) -> Result<(), String> {
    let dir = config_dir()?;
    match token {
        Some(token) => {
            creds::store(&dir, Some(ServerCredentials::new(base_url, token)))?;
            info!("the server session is available to the tunnel process");
        }
        None => {
            creds::store(&dir, None)?;
            info!("the server session was cleared");
        }
    }
    Ok(())
}
