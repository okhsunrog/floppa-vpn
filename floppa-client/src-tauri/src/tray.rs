//! The tray icon, and what closing the window means once there is one.
//!
//! Desktop only, and that is the point of it: on desktop this process *is* the tunnel — the actor,
//! the backend and the rollback journal all live here — so closing the window used to end the VPN
//! with it. A tray is the smallest thing that separates "I am done looking at this" from "I am
//! done using this", and it is what the desktop has instead of the service Android gets for free.
//!
//! Two ids are answered here rather than passed to the UI: showing the window and quitting are
//! window operations with no VPN meaning, and answering them in Rust is what keeps the tray usable
//! when the webview is not — a build whose frontend failed to load can still be quit from its own
//! icon. Everything else is the UI's, because everything else needs to know what the button under
//! the user's cursor would have done.
//!
//! On Android this module is a set of no-ops: there is no tray, no window to close, and the
//! tunnel already outlives the UI process by design (`docs/ANDROID-TUNNEL-PROCESS.md`).

use serde::{Deserialize, Serialize};
use specta::Type;

/// One tray row the UI can change: its words, and whether it can be clicked.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct TrayAction {
    pub label: String,
    pub enabled: bool,
}

/// What the tray says, in the language the app is running in.
///
/// Labels, never a menu. On Linux a tray menu's *content* can be changed but the menu itself can
/// never be replaced once set — `TrayIcon::set_menu` is documented as having no effect there — so
/// the rows are built once, here, and only their text and enabled state ever travel. That also
/// settles where the words come from: the locale files, through the UI, rather than a second copy
/// of the translations kept in Rust and left to drift.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct TrayView {
    /// Windows only. Linux tray implementations have no tooltip and ignore it.
    pub tooltip: String,
    pub show: String,
    /// Connect, Cancel or Disconnect — which one is the UI's call, because it is the side that
    /// knows what its own button would do.
    pub toggle: TrayAction,
    pub quit: String,
}

#[cfg(desktop)]
mod imp {
    use super::TrayView;
    use crate::vpn::events::TrayToggleRequested;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri::WebviewWindow;
    use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Manager, WindowEvent, Wry};
    use tauri_specta::Event as _;
    use tracing::{info, warn};

    /// The window every one of these operations is about. The config declares no label, so it is
    /// Tauri's default.
    const MAIN: &str = "main";

    const SHOW: &str = "show";
    const TOGGLE: &str = "toggle";
    const QUIT: &str = "quit";

    /// The tray, kept so its rows can be reworded — see [`TrayView`] for why they are reworded
    /// rather than rebuilt.
    struct Tray {
        icon: TrayIcon<Wry>,
        show: MenuItem<Wry>,
        toggle: MenuItem<Wry>,
        quit: MenuItem<Wry>,
        /// Whether the UI has ever described the tray.
        ///
        /// Stands for "there is a frontend able to answer a question". Until it is true, a close
        /// request is answered here by hiding the window: asking a webview that never came up
        /// would leave the window unclosable, and hiding is the answer that loses nothing — the
        /// tray can still quit.
        described: AtomicBool,
    }

    /// Build the tray and start watching the window's close button.
    ///
    /// Failing to build one is reported and survived: a desktop without a system tray (a bare
    /// window manager, a session where the status area is gone) still gets an app whose window
    /// closes the way it always did — see [`close_requested`].
    pub fn setup(app: &AppHandle) {
        match build(app) {
            Ok(tray) => {
                app.manage(tray);
                info!("tray icon created");
                // Only now: the close button is intercepted because there is somewhere for the
                // window to go. Without a tray, a window put away would be a window lost.
                if let Some(window) = app.get_webview_window(MAIN) {
                    watch_window(app, &window);
                } else {
                    warn!(window = MAIN, "no window to watch for close requests");
                }
            }
            Err(err) => warn!(error = %err, "no tray icon; closing the window will quit"),
        }
    }

    fn build(app: &AppHandle) -> tauri::Result<Tray> {
        // Placeholder English, replaced by `update` as soon as the UI mounts. It is what a user
        // sees only if the frontend never loads, which is exactly when a working Quit matters.
        let show = MenuItemBuilder::with_id(SHOW, "Open Floppa VPN").build(app)?;
        let toggle = MenuItemBuilder::with_id(TOGGLE, "Connect")
            .enabled(false)
            .build(app)?;
        let quit = MenuItemBuilder::with_id(QUIT, "Quit").build(app)?;
        let menu = MenuBuilder::new(app)
            .item(&show)
            .separator()
            .item(&toggle)
            .separator()
            .item(&quit)
            .build()?;

        let mut builder = TrayIconBuilder::with_id(MAIN)
            .menu(&menu)
            // Left-click opens the window; the menu is the right-click gesture. Windows only —
            // on Linux the tray implementation owns the click and always shows the menu.
            .show_menu_on_left_click(false)
            .tooltip("Floppa VPN")
            .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show_window(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }

        let icon = builder.build(app)?;
        Ok(Tray {
            icon,
            show,
            toggle,
            quit,
            described: AtomicBool::new(false),
        })
    }

    fn on_menu(app: &AppHandle, id: &str) {
        match id {
            SHOW => show_window(app),
            QUIT => quit(app),
            TOGGLE => {
                if let Err(err) = TrayToggleRequested.emit(app) {
                    warn!(error = %err, "the tray's connect/disconnect reached nobody");
                }
            }
            other => warn!(id = other, "unknown tray menu item"),
        }
    }

    /// Prevent the window from being closed, and let the UI decide what closing meant.
    ///
    /// Prevented unconditionally, because the alternative — deciding here — would need this side
    /// to hold the user's preference, and the preference belongs with the settings that are
    /// already persisted next to it. What Rust keeps is the fallback: if no UI has ever spoken,
    /// put the window away rather than ask.
    ///
    /// Attached per window rather than once at startup, because going to the tray destroys the
    /// window and coming back builds a new one — a listener on the old one watches nothing.
    fn watch_window(app: &AppHandle, window: &WebviewWindow) {
        let app = app.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                close_requested(&app);
            }
        });
    }

    fn close_requested(app: &AppHandle) {
        // Logged on the happy path deliberately: it is the one half of this exchange Rust can
        // see. A close that produces this line and no visible dialog is a UI that did not answer;
        // one that produces no line at all never reached us.
        info!("the window was asked to close");
        let described = app
            .try_state::<Tray>()
            .is_some_and(|tray| tray.described.load(Ordering::Relaxed));
        if !described {
            info!("close requested before the UI described the tray; hiding");
            hide(app);
            return;
        }
        if let Err(err) = crate::vpn::events::WindowCloseRequested.emit(app) {
            warn!(error = %err, "could not ask the UI what closing means; hiding");
            hide(app);
        }
    }

    /// Reword the tray. Called by the UI on mount, on a locale change, and whenever what the
    /// toggle would do changes.
    pub fn update(app: &AppHandle, view: TrayView) -> Result<(), String> {
        let tray = app
            .try_state::<Tray>()
            .ok_or_else(|| "there is no tray icon on this desktop".to_string())?;
        let describe =
            |what: &str, err: tauri::Error| format!("could not set the tray {what}: {err}");
        tray.show
            .set_text(&view.show)
            .map_err(|e| describe("show item", e))?;
        tray.toggle
            .set_text(&view.toggle.label)
            .map_err(|e| describe("toggle item", e))?;
        tray.toggle
            .set_enabled(view.toggle.enabled)
            .map_err(|e| describe("toggle item", e))?;
        tray.quit
            .set_text(&view.quit)
            .map_err(|e| describe("quit item", e))?;
        // Unsupported on Linux, where it fails rather than lying about it; nothing depends on it.
        let _ = tray.icon.set_tooltip(Some(&view.tooltip));
        tray.described.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Bring the window back, wherever it was: hidden, minimised, or behind everything else.
    pub fn show_window(app: &AppHandle) {
        let Some(window) = app.get_webview_window(MAIN) else {
            return;
        };
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }

    /// Put the window away and leave everything else running.
    pub fn hide(app: &AppHandle) {
        if let Some(window) = app.get_webview_window(MAIN) {
            let _ = window.hide();
        }
    }

    /// Quit for real. `RunEvent::Exit` in `lib.rs` takes the tunnel down on the way out, which is
    /// why this asks the app to exit rather than reaching for the actor itself.
    pub fn quit(app: &AppHandle) {
        info!("quitting on request");
        app.exit(0);
    }
}

#[cfg(not(desktop))]
mod imp {
    use super::TrayView;
    use tauri::AppHandle;

    pub fn setup(_app: &AppHandle) {}

    pub fn update(_app: &AppHandle, _view: TrayView) -> Result<(), String> {
        Ok(())
    }

    pub fn show_window(_app: &AppHandle) {}

    pub fn hide(_app: &AppHandle) {}

    pub fn quit(_app: &AppHandle) {}
}

pub use imp::{hide, quit, setup, show_window, update};
