//! Peer provisioning: what the frontend hands over, and what it asks for.

use crate::provision::session::{self, ServerSession};
use crate::vpn::config::config_dir;
use tracing::info;

/// Hand the server session over to Rust, or take it away.
///
/// Called by the frontend whenever any part of it changes: the token on sign-in, on every sliding
/// refresh and on sign-out, and the device identity as soon as the plugin reports it. `token` is
/// `None` exactly when the user is signed out, and then the stored session is removed rather than
/// left to rot — it is what a background repair would authenticate with, and a signed-out device
/// must not be able to make peers.
///
/// A session that arrives incomplete (the token is there but the device id has not been read yet)
/// is written anyway and replaced when the rest lands: [`session::load`] refuses to return
/// anything it could not provision with, so a half-written one is simply not used.
#[tauri::command]
#[specta::specta]
pub fn set_server_session(
    base_url: String,
    token: Option<String>,
    device_id: Option<String>,
    device_name: Option<String>,
) -> Result<(), String> {
    let dir = config_dir()?;
    match token {
        Some(token) => {
            let stored =
                ServerSession::new(base_url, token, device_id.unwrap_or_default(), device_name);
            let usable = stored.is_usable();
            session::store(&dir, Some(stored))?;
            if usable {
                info!("the server session is available to the tunnel process");
            }
        }
        None => {
            session::store(&dir, None)?;
            info!("the server session was cleared");
        }
    }
    Ok(())
}
