//! The platform commands on desktop.
//!
//! Device identity and the save dialog are real; the Android-only questions get their
//! "not applicable" answer. Keep the names, signatures and doc comments in step with
//! `android.rs`: this is the side the bindings are generated from.

use super::{AppInfo, SafeAreaInsets};
use crate::vpn::config as vpn_config;
use tauri::AppHandle;

/// Get the persistent device ID.
/// Android: ANDROID_ID (stable across reinstalls, per signing key).
/// Desktop: random UUID persisted in config dir.
#[tauri::command]
#[specta::specta]
pub async fn get_device_id(#[allow(unused_variables)] app: AppHandle) -> Result<String, String> {
    vpn_config::get_or_create_device_id()
}

/// Get the device name (Android: manufacturer+model, desktop: hostname)
#[tauri::command]
#[specta::specta]
pub async fn get_device_name(#[allow(unused_variables)] app: AppHandle) -> String {
    vpn_config::get_device_name()
}

/// Get list of installed apps for split tunneling (Android only)
#[tauri::command]
#[specta::specta]
pub async fn get_installed_apps(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<Vec<AppInfo>, String> {
    Ok(vec![])
}

/// Check if battery optimization is disabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn is_battery_optimization_disabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    Ok(true) // Not applicable on desktop
}

/// Request the user to disable battery optimization (Android only)
/// Returns whether battery optimization is now disabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn request_disable_battery_optimization(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    Ok(true)
}

/// Check if notifications are enabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn are_notifications_enabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    Ok(true)
}

/// Request notification permission (Android only)
/// Returns whether notifications are now enabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn open_notification_settings(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    Ok(true)
}

/// Set status bar icon style to match app theme (Android only)
#[tauri::command]
#[specta::specta]
pub async fn set_status_bar_style(
    #[allow(unused_variables)] app: AppHandle,
    #[allow(unused_variables)] is_dark: bool,
) -> Result<(), String> {
    Ok(())
}

/// Get safe area insets (status bar, nav bar heights) in dp
#[tauri::command]
#[specta::specta]
pub async fn get_safe_area_insets(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<SafeAreaInsets, String> {
    Ok(SafeAreaInsets {
        top: 0.0,
        bottom: 0.0,
    })
}

/// Offer `bytes` through the native save dialog under `filename`.
/// `Ok(false)` when the user cancelled.
pub(super) async fn save_archive(
    app: &AppHandle,
    filename: &str,
    bytes: &[u8],
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(filename)
        .add_filter("Archive", &["tar.gz", "gz"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });

    let file_path = rx.await.map_err(|_| "Dialog closed unexpectedly")?;
    let Some(file_path) = file_path else {
        return Ok(false);
    };

    let path = file_path
        .into_path()
        .map_err(|e| format!("Invalid save path: {e}"))?;

    std::fs::write(&path, bytes).map_err(|e| format!("Failed to write archive: {e}"))?;
    Ok(true)
}
