//! The platform commands on Android: every one of them is a call into the `tauri-plugin-vpn`
//! Kotlin side. Keep the names, signatures and doc comments in step with `desktop.rs`, which is
//! the side the bindings are generated from.

use super::{AppInfo, SafeAreaInsets};
use crate::vpn::config as vpn_config;
use tauri::AppHandle;
use tauri_plugin_vpn::VpnExt;
use tracing::warn;

/// Get the persistent device ID.
/// Android: ANDROID_ID (stable across reinstalls, per signing key).
/// Desktop: random UUID persisted in config dir.
#[tauri::command]
#[specta::specta]
pub async fn get_device_id(app: AppHandle) -> Result<String, String> {
    app.vpn()
        .get_device_id()
        .await
        .map_err(|e| format!("Failed to get ANDROID_ID: {e}"))
}

/// Get the device name (Android: manufacturer+model, desktop: hostname)
#[tauri::command]
#[specta::specta]
pub async fn get_device_name(app: AppHandle) -> String {
    match app.vpn().get_device_name().await {
        Ok(name) => name,
        Err(e) => {
            warn!("Failed to get Android device name: {e}");
            vpn_config::get_device_name()
        }
    }
}

/// Get list of installed apps for split tunneling (Android only)
#[tauri::command]
#[specta::specta]
pub async fn get_installed_apps(app: AppHandle) -> Result<Vec<AppInfo>, String> {
    let plugin_apps = app
        .vpn()
        .get_installed_apps()
        .await
        .map_err(|e| format!("Failed to get installed apps: {e}"))?;
    Ok(plugin_apps
        .into_iter()
        .map(|a| AppInfo {
            package_name: a.package_name,
            label: a.label,
            is_system: a.is_system,
            icon: a.icon,
        })
        .collect())
}

/// Check if battery optimization is disabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn is_battery_optimization_disabled(app: AppHandle) -> Result<bool, String> {
    app.vpn()
        .is_battery_optimization_disabled()
        .await
        .map_err(|e| format!("Failed to check battery optimization: {e}"))
}

/// Request the user to disable battery optimization (Android only)
/// Returns whether battery optimization is now disabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn request_disable_battery_optimization(app: AppHandle) -> Result<bool, String> {
    app.vpn()
        .request_disable_battery_optimization()
        .await
        .map_err(|e| format!("Failed to request battery optimization: {e}"))
}

/// Check if notifications are enabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn are_notifications_enabled(app: AppHandle) -> Result<bool, String> {
    app.vpn()
        .are_notifications_enabled()
        .await
        .map_err(|e| format!("Failed to check notifications: {e}"))
}

/// Request notification permission (Android only)
/// Returns whether notifications are now enabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn open_notification_settings(app: AppHandle) -> Result<bool, String> {
    app.vpn()
        .open_notification_settings()
        .await
        .map_err(|e| format!("Failed to request notification permission: {e}"))
}

/// Set status bar icon style to match app theme (Android only)
#[tauri::command]
#[specta::specta]
pub async fn set_status_bar_style(app: AppHandle, is_dark: bool) -> Result<(), String> {
    app.vpn()
        .set_status_bar_style(is_dark)
        .await
        .map_err(|e| format!("Failed to set status bar style: {e}"))
}

/// Get safe area insets (status bar, nav bar heights) in dp
#[tauri::command]
#[specta::specta]
pub async fn get_safe_area_insets(app: AppHandle) -> Result<SafeAreaInsets, String> {
    let insets = app
        .vpn()
        .get_safe_area_insets()
        .await
        .map_err(|e| format!("Failed to get safe area insets: {e}"))?;
    Ok(SafeAreaInsets {
        top: insets.top,
        bottom: insets.bottom,
    })
}

/// Offer `bytes` through the system document picker under `filename`.
/// `Ok(false)` when the user cancelled.
pub(super) async fn save_archive(
    app: &AppHandle,
    filename: &str,
    bytes: &[u8],
) -> Result<bool, String> {
    use tauri_plugin_android_fs::AndroidFsExt;

    let api = app.android_fs_async();
    let uri = api
        .picker()
        .save_file(None, filename, Some("application/gzip"), false)
        .await
        .map_err(|e| format!("Save dialog failed: {e}"))?;

    let Some(uri) = uri else {
        return Ok(false);
    };

    api.write(&uri, bytes)
        .await
        .map_err(|e| format!("Failed to write archive: {e}"))?;
    Ok(true)
}
