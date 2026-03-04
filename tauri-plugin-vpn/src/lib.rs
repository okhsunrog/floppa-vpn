//! Tauri plugin for VPN functionality on mobile platforms (Android/iOS).
//!
//! This plugin provides a cross-platform API for:
//! - Requesting VPN permissions
//! - Creating and managing VPN tunnels
//! - Receiving TUN file descriptors for WireGuard integration

use tauri::{
    Runtime,
    plugin::{Builder, TauriPlugin},
};

#[cfg(mobile)]
use tauri::Manager;

#[cfg(mobile)]
mod mobile;

mod error;
mod models;

pub use error::{Error, Result};
pub use models::*;

#[cfg(mobile)]
use mobile::Vpn;

/// Extension trait for accessing the VPN plugin.
#[cfg(mobile)]
pub trait VpnExt<R: Runtime> {
    fn vpn(&self) -> &Vpn<R>;
}

#[cfg(mobile)]
impl<R: Runtime, T: Manager<R>> VpnExt<R> for T {
    fn vpn(&self) -> &Vpn<R> {
        self.state::<Vpn<R>>().inner()
    }
}

/// Initialize the VPN plugin.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vpn")
        .setup(|app, api| {
            #[cfg(mobile)]
            {
                let vpn = mobile::init(app, api)?;
                app.manage(vpn);
            }
            #[cfg(not(mobile))]
            {
                let _ = (app, api);
                log::warn!("VPN plugin is only available on mobile platforms");
            }
            Ok(())
        })
        .build()
}
