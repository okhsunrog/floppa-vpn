//! Mobile platform implementation for the VPN plugin.

use serde::de::DeserializeOwned;
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

use crate::{models::*, Result};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "dev.okhsunrog.floppavpn.vpn";

/// Handle to the mobile VPN plugin.
pub struct Vpn<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Vpn<R> {
    /// Request VPN permission from the user.
    ///
    /// On Android, this shows the system VPN permission dialog.
    /// On iOS, this triggers the VPN configuration permission.
    ///
    /// Returns `Ok(true)` if permission was granted, `Ok(false)` if denied.
    pub fn prepare(&self) -> Result<bool> {
        self.0
            .run_mobile_plugin::<PrepareResponse>("prepareVpn", ())
            .map(|r| r.granted)
            .map_err(Into::into)
    }

    /// Start the VPN tunnel with the given configuration.
    ///
    /// The TUN file descriptor will be delivered via the `vpn_started` event.
    pub fn start(&self, config: VpnConfig) -> Result<()> {
        self.0
            .run_mobile_plugin::<()>("startVpn", config)
            .map_err(Into::into)
    }

    /// Stop the VPN tunnel.
    pub fn stop(&self) -> Result<()> {
        self.0
            .run_mobile_plugin::<()>("stopVpn", ())
            .map_err(Into::into)
    }

    /// Get current VPN status.
    pub fn status(&self) -> Result<VpnStatus> {
        self.0
            .run_mobile_plugin::<StatusResponse>("getVpnStatus", ())
            .map(|r| r.status)
            .map_err(Into::into)
    }

    /// Get list of installed apps for split tunneling.
    pub fn get_installed_apps(&self) -> Result<Vec<AppInfo>> {
        self.0
            .run_mobile_plugin::<InstalledAppsResponse>("getInstalledApps", ())
            .map(|r| r.apps)
            .map_err(Into::into)
    }

    /// Get safe area insets (status bar, nav bar heights) in dp.
    pub fn get_safe_area_insets(&self) -> Result<SafeAreaInsets> {
        self.0
            .run_mobile_plugin::<SafeAreaInsets>("getSafeAreaInsets", ())
            .map_err(Into::into)
    }

    /// Get device name (manufacturer + model) from Android Build properties.
    pub fn get_device_name(&self) -> Result<String> {
        self.0
            .run_mobile_plugin::<DeviceNameResponse>("getDeviceName", ())
            .map(|r| r.name)
            .map_err(Into::into)
    }

    /// Check if battery optimization is disabled for this app.
    pub fn is_battery_optimization_disabled(&self) -> Result<bool> {
        self.0
            .run_mobile_plugin::<BatteryOptResponse>("isBatteryOptimizationDisabled", ())
            .map(|r| r.disabled)
            .map_err(Into::into)
    }

    /// Request the user to disable battery optimization for this app.
    pub fn request_disable_battery_optimization(&self) -> Result<()> {
        self.0
            .run_mobile_plugin::<()>("requestDisableBatteryOptimization", ())
            .map_err(Into::into)
    }

    /// Protect a socket from VPN routing (bypass the VPN tunnel).
    ///
    /// This must be called for UDP sockets used by WireGuard to communicate
    /// with the server, otherwise packets would loop through the VPN.
    ///
    /// Returns `Ok(true)` if protection succeeded, `Ok(false)` if it failed.
    pub fn protect_socket(&self, fd: i32) -> Result<bool> {
        #[derive(serde::Serialize)]
        struct ProtectArgs {
            fd: i32,
        }

        self.0
            .run_mobile_plugin::<ProtectResponse>("protectSocket", ProtectArgs { fd })
            .map(|r| r.protected)
            .map_err(Into::into)
    }
}

#[derive(serde::Deserialize)]
struct InstalledAppsResponse {
    apps: Vec<AppInfo>,
}

#[derive(serde::Deserialize)]
struct BatteryOptResponse {
    disabled: bool,
}

#[derive(serde::Deserialize)]
struct ProtectResponse {
    protected: bool,
}

#[derive(serde::Deserialize)]
struct PrepareResponse {
    granted: bool,
}

#[derive(serde::Deserialize)]
struct StatusResponse {
    status: VpnStatus,
}

/// Initialize the mobile plugin.
#[cfg(target_os = "android")]
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<Vpn<R>> {
    let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "VpnPlugin")?;
    Ok(Vpn(handle))
}

/// Initialize the mobile plugin (iOS - not yet implemented).
#[cfg(target_os = "ios")]
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<Vpn<R>> {
    // iOS implementation would use NetworkExtension framework
    // For now, return an error as it's not implemented
    Err(crate::Error::Platform("iOS VPN not yet implemented".into()))
}
