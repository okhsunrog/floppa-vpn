//! Tauri plugin for the Android VPN service.
//!
//! Android only. It wraps `VpnService` consent, starting and stopping the `:vpn` service, and a
//! handful of device queries (installed apps, safe-area insets, device identity, notification and
//! battery settings). There is no iOS implementation; the design one would follow is in
//! `docs/IOS-BACKEND-PLAN.md`. On every other platform [`init`] registers nothing.

use tauri::{
    Runtime,
    plugin::{Builder, TauriPlugin},
};

#[cfg(target_os = "android")]
use tauri::Manager;

#[cfg(target_os = "android")]
mod android;

mod error;
mod models;

pub use error::{Error, Result};
pub use models::*;

#[cfg(target_os = "android")]
pub use android::Vpn;

/// Extension trait for accessing the VPN plugin.
#[cfg(target_os = "android")]
pub trait VpnExt<R: Runtime> {
    fn vpn(&self) -> &Vpn<R>;
}

#[cfg(target_os = "android")]
impl<R: Runtime, T: Manager<R>> VpnExt<R> for T {
    fn vpn(&self) -> &Vpn<R> {
        self.state::<Vpn<R>>().inner()
    }
}

/// Initialize the VPN plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vpn")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let vpn = android::init(app, api)?;
                app.manage(vpn);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = (app, api);
                log::warn!("VPN plugin is only available on Android");
            }
            Ok(())
        })
        .build()
}
