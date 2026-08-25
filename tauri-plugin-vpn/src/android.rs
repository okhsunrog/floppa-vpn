//! Android implementation: every method is one invoke of the Kotlin `VpnPlugin` class.

use serde::{Serialize, de::DeserializeOwned};
use tauri::{
    AppHandle, Runtime,
    plugin::{PluginApi, PluginHandle},
};

use crate::{Error, Result, models::*};

const PLUGIN_IDENTIFIER: &str = "dev.okhsunrog.floppavpn.vpn";

/// Handle to the Android VPN plugin.
pub struct Vpn<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Vpn<R> {
    /// One invoke of the Kotlin side.
    ///
    /// Async on purpose: the synchronous `run_mobile_plugin` parks the calling thread on a channel
    /// until Kotlin answers, and a permission dialog can take as long as the user likes. Called
    /// from an async command that is a tokio worker held for the duration.
    async fn call<T: DeserializeOwned>(
        &self,
        command: &'static str,
        payload: impl Serialize,
    ) -> Result<T> {
        self.0
            .run_mobile_plugin_async(command, payload)
            .await
            .map_err(|source| Error::PluginInvoke { command, source })
    }

    /// Request VPN consent from the user via the system dialog.
    ///
    /// Returns `Ok(true)` if permission was granted, `Ok(false)` if denied.
    pub async fn prepare(&self) -> Result<bool> {
        self.call::<PrepareResponse>("prepareVpn", ())
            .await
            .map(|r| r.granted)
    }

    /// Make sure the `:vpn` process is running and *started*, not merely bound.
    ///
    /// Binding keeps the process alive while this app is open, which is enough to talk to the
    /// actor. It is not enough to survive the app going away — a bound-only service dies with its
    /// last client — so before asking for a tunnel the service is also started, which is the
    /// lifecycle that outlives the UI.
    ///
    /// The call carries no tunnel configuration. It used to carry a whole TUN spec, because the
    /// process on the other end had no idea what to build; it has the actor now, and asks for the
    /// descriptor itself when it needs one.
    pub async fn start_service(&self) -> Result<()> {
        self.call::<()>("startVpnService", ()).await
    }

    /// Get list of installed apps for split tunneling.
    pub async fn get_installed_apps(&self) -> Result<Vec<AppInfo>> {
        self.call::<InstalledAppsResponse>("getInstalledApps", ())
            .await
            .map(|r| r.apps)
    }

    /// Get safe area insets (status bar, nav bar heights) in dp.
    pub async fn get_safe_area_insets(&self) -> Result<SafeAreaInsets> {
        self.call::<SafeAreaInsets>("getSafeAreaInsets", ()).await
    }

    /// Get stable device ID (ANDROID_ID) that persists across reinstalls.
    pub async fn get_device_id(&self) -> Result<String> {
        self.call::<DeviceIdResponse>("getDeviceId", ())
            .await
            .map(|r| r.id)
    }

    /// Get device name (manufacturer + model) from Android Build properties.
    pub async fn get_device_name(&self) -> Result<String> {
        self.call::<DeviceNameResponse>("getDeviceName", ())
            .await
            .map(|r| r.name)
    }

    /// Check if battery optimization is disabled for this app.
    pub async fn is_battery_optimization_disabled(&self) -> Result<bool> {
        self.call::<BatteryOptResponse>("isBatteryOptimizationDisabled", ())
            .await
            .map(|r| r.disabled)
    }

    /// Request the user to disable battery optimization for this app.
    /// Returns whether battery optimization is now disabled after the user responds.
    pub async fn request_disable_battery_optimization(&self) -> Result<bool> {
        self.call::<BatteryOptResponse>("requestDisableBatteryOptimization", ())
            .await
            .map(|r| r.disabled)
    }

    /// Check if notifications are enabled for this app.
    pub async fn are_notifications_enabled(&self) -> Result<bool> {
        self.call::<NotificationsEnabledResponse>("areNotificationsEnabled", ())
            .await
            .map(|r| r.enabled)
    }

    /// Request notification permission. Returns whether notifications are now enabled.
    pub async fn open_notification_settings(&self) -> Result<bool> {
        self.call::<NotificationsEnabledResponse>("openNotificationSettings", ())
            .await
            .map(|r| r.enabled)
    }

    /// Set status bar icon style to match app theme.
    pub async fn set_status_bar_style(&self, is_dark: bool) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            is_dark: bool,
        }
        self.call::<()>("setStatusBarStyle", Args { is_dark }).await
    }
}

#[derive(serde::Deserialize)]
struct InstalledAppsResponse {
    apps: Vec<AppInfo>,
}

#[derive(serde::Deserialize)]
struct NotificationsEnabledResponse {
    enabled: bool,
}

#[derive(serde::Deserialize)]
struct BatteryOptResponse {
    disabled: bool,
}

#[derive(serde::Deserialize)]
struct PrepareResponse {
    granted: bool,
}

/// Register the Kotlin plugin class.
pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<Vpn<R>> {
    let handle = api
        .register_android_plugin(PLUGIN_IDENTIFIER, "VpnPlugin")
        .map_err(Error::Register)?;
    Ok(Vpn(handle))
}
