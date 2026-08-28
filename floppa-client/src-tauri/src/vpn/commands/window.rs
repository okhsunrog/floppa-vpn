//! The window and its tray.
//!
//! Platform-free, unlike the two modules beside it: the cfg is one level down, in [`crate::tray`],
//! which answers all three with nothing on Android. So there is one copy of each signature here
//! rather than a matching pair, and the bindings look the same on every platform either way.

use crate::tray::{self, TrayView};
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
/// The exit handler in `lib.rs` takes the tunnel down and flushes the config store on the way out,
/// which is why this asks the app to exit rather than doing either of those itself.
#[tauri::command]
#[specta::specta]
pub fn quit_app(app: AppHandle) {
    tray::quit(&app);
}
