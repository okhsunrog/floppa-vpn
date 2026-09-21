//! The window and its tray.
//!
//! Platform-free, unlike the two modules beside it: the cfg is one level down, in [`crate::tray`],
//! which answers all three with nothing on Android. So there is one copy of each signature here
//! rather than a matching pair, and the bindings look the same on every platform either way.

use crate::tray::{self, TrayView};
use crate::vpn::owner::TunnelOwner;
use tauri::AppHandle;

/// Tell the tray what to say.
///
/// Called by the UI on mount, on a locale change, and whenever what the toggle would do changes —
/// never on every state tick: the words are what travels, and they change far less often than the
/// state they are derived from.
#[tauri::command]
#[specta::specta]
pub fn update_tray(view: TrayView, app: AppHandle) -> Result<(), String> {
    tray::update(&app, view)
}

/// Put the window away. The tunnel, the actor and the tray all carry on.
#[tauri::command]
#[specta::specta]
pub fn hide_to_tray(app: AppHandle) {
    tray::hide(&app);
}

/// Quit for real.
///
/// The exit handler in `lib.rs` takes the tunnel down and flushes the config store on the way out
/// — when the tunnel is this process's — which is why this asks the app to exit rather than doing
/// either of those itself.
#[tauri::command]
#[specta::specta]
pub fn quit_app(app: AppHandle) {
    tray::quit(&app);
}

/// Who holds the tunnel this app is showing.
///
/// The UI needs it because the promises differ. "Quitting will disconnect you" is true when the
/// tunnel is in this process and false when the service has it, and a close dialog that says it
/// either way is wrong half the time. It is also the only place a user can be told that a service
/// *is* installed and they are not allowed to use it — a state where nothing looks broken and
/// everything is quietly worse.
#[tauri::command]
#[specta::specta]
pub fn get_tunnel_owner(#[allow(unused_variables)] app: AppHandle) -> TunnelOwner {
    #[cfg(not(target_os = "android"))]
    {
        use tauri::Manager as _;
        app.state::<TunnelOwner>().inner().clone()
    }
    // Android's tunnel is always in `:vpn`, and that process is not a thing the app quits.
    #[cfg(target_os = "android")]
    TunnelOwner::Service
}
