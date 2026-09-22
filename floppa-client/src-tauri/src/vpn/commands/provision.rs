//! Peer provisioning: what the frontend hands over, and what it asks for.

use crate::provision::SyncOutcome;
use crate::provision::server::{ActorSink, client};
use crate::provision::session::{self, ServerSession};
use crate::vpn::actor::handle::TunnelHandle;
use crate::vpn::config::config_dir;
use tauri::State;
use tracing::{info, warn};

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
pub async fn set_server_session(
    #[allow(unused_variables)] app: tauri::AppHandle,
    base_url: String,
    token: Option<String>,
    device_id: Option<String>,
    device_name: Option<String>,
) -> Result<(), String> {
    let dir = config_dir()?;
    let stored = token.map(|token| {
        ServerSession::new(base_url, token, device_id.unwrap_or_default(), device_name)
    });

    // Always here: this process talks to the server too — `sync_peers` provisions from the screen
    // the user is looking at — and reads the session back out of this file to do it.
    session::store(&dir, stored.clone())?;
    match &stored {
        Some(session) if session.is_usable() => {
            info!("the server session is available to the tunnel process")
        }
        Some(_) => {}
        None => info!("the server session was cleared"),
    }

    #[cfg(target_os = "linux")]
    hand_to_service(&app, stored).await;

    Ok(())
}

/// Give the same session to the system service, when there is one holding the tunnel.
///
/// Without this the service can reconnect a tunnel but never replace a peer the server deleted,
/// because reaching the server needs credentials it has no way to obtain: they are a user's, and
/// its store is root's. Which was worse than it sounds — the watcher runs where the actor is, so
/// with the service holding the tunnel this app no longer runs one, and nobody was repairing
/// anything at all for someone who only ever opens the app.
///
/// Only a *usable* session is sent. An incomplete one is an ordinary intermediate state here — the
/// frontend learns the token and the device identity at different moments — but sending it would
/// replace a good session on the service with one it cannot provision with. A sign-out is always
/// sent: a device that has signed out must not be able to make peers, and that is exactly the
/// process that would make them.
///
/// Best effort. A connect or a sign-in must not fail because a repair that might be needed later
/// could not be arranged, and a service left without a session merely behaves as it did before
/// this existed.
#[cfg(target_os = "linux")]
async fn hand_to_service(app: &tauri::AppHandle, session: Option<ServerSession>) {
    use tauri::Manager as _;

    let Some(remote) = app.try_state::<std::sync::Arc<crate::vpn::remote::RemoteActor>>() else {
        // No service is holding the tunnel: this process is, and it has the session already.
        return;
    };
    let payload = match session {
        Some(session) if !session.is_usable() => return,
        Some(session) => match serde_json::to_string(&session) {
            Ok(payload) => Some(payload),
            Err(e) => {
                warn!("the session could not be serialised for the tunnel service: {e}");
                return;
            }
        },
        None => None,
    };
    match remote.set_session(payload).await {
        Ok(()) => info!("the tunnel service can reach the server as this user"),
        Err(e) => warn!(
            "the tunnel service was not given the session, so it cannot replace a deleted peer \
             on its own: {e}"
        ),
    }
}

/// Provision this device's peers, and store what the server hands back.
///
/// The whole of it runs here, in the process the user is looking at: it talks to the server, and
/// the configs it fetches go into the store through the actor — over the socket on Android, which
/// is exactly what that boundary is for. The same logic replaces a deleted peer with nobody
/// looking, from `:vpn`; both go through `floppa-api-client`, so there is one description of what
/// a device is entitled to rather than one per process.
#[tauri::command]
#[specta::specta]
pub async fn sync_peers(handle: State<'_, TunnelHandle>) -> Result<SyncOutcome, String> {
    let Some((api, identity)) = client(env!("CARGO_PKG_VERSION")) else {
        // Signed out, or a session this build cannot read. Reported as offline because that is
        // what it means to the card — nothing was learned, nothing was changed — and because the
        // auth guard is what deals with being signed out.
        warn!("no server session; the peers were not synced");
        return Ok(SyncOutcome::Offline);
    };
    let sink = ActorSink(handle.inner().clone());
    Ok(floppa_api_client::sync_peers(&api, &sink, &identity)
        .await
        .into())
}
