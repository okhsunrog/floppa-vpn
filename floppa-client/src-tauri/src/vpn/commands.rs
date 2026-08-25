use super::actor::handle::{IntentRequest, TunnelHandle};
use super::actor::types::{
    CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelParams, TunnelState,
};
use super::backend::VpnBackend;
use super::config as vpn_config;
use super::protocol::Protocol;
use super::store::ConfigError;
use crate::logging::capture::{CaptureSession, LogCaptureStatus};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
#[allow(unused_imports)]
use tracing::{error, info, warn};

/// Get the persistent device ID.
/// Android: ANDROID_ID (stable across reinstalls, per signing key).
/// Desktop: random UUID persisted in config dir.
#[tauri::command]
#[specta::specta]
pub async fn get_device_id(#[allow(unused_variables)] app: AppHandle) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .get_device_id()
            .await
            .map_err(|e| format!("Failed to get ANDROID_ID: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    vpn_config::get_or_create_device_id()
}

/// Get the device name (Android: manufacturer+model, desktop: hostname)
#[tauri::command]
#[specta::specta]
pub async fn get_device_name(#[allow(unused_variables)] app: AppHandle) -> String {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        match app.vpn().get_device_name().await {
            Ok(name) => return name,
            Err(e) => {
                warn!("Failed to get Android device name: {e}");
            }
        }
    }
    vpn_config::get_device_name()
}

// ---------------------------------------------------------------------------------- the tunnel
//
// Every command here is a thin wrapper over the actor. None of them touches tunnel state, and
// none of them can block on the tunnel: setting an intent returns as soon as the actor has
// accepted it, and waiting for the result is a separate call the caller may drop.

/// Ask for a tunnel.
///
/// Returns as soon as the actor accepts the intent — the epoch it returns identifies this request
/// for [`tunnel_await_cycle`]. There is deliberately no "busy" failure: with a single owner and a
/// write-only intent queue, there is no bad moment to ask.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_set_intent_up(
    order: Vec<Protocol>,
    params: TunnelParams,
    tunnel: State<'_, TunnelHandle>,
) -> Result<IntentAccepted, IntentError> {
    tunnel.set_intent(IntentRequest::Up { order, params }).await
}

/// Ask for no tunnel. Also the cancel button: an intent change is how an in-flight attempt is
/// stopped.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_set_intent_down(
    tunnel: State<'_, TunnelHandle>,
) -> Result<IntentAccepted, IntentError> {
    tunnel.set_intent(IntentRequest::Down).await
}

/// Wait for a request to reach a terminal outcome.
///
/// Safe to drop: dropping the future only discards the answer, it never cancels what the actor is
/// doing. A caller that asks after the fact still gets the answer, because recent outcomes are
/// retained.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_await_cycle(
    epoch: IntentEpoch,
    tunnel: State<'_, TunnelHandle>,
) -> Result<CycleOutcome, IntentError> {
    tunnel.await_cycle(epoch).await
}

/// The current snapshot. A local read of the published state — no IPC, no lock.
#[tauri::command]
#[specta::specta]
pub fn tunnel_get_state(tunnel: State<'_, TunnelHandle>) -> TunnelState {
    tunnel.snapshot()
}

/// Store a config under its own protocol key.
///
/// Storing is not choosing: this does not change which protocol the next connect would use. The
/// previous behaviour of switching to whatever was imported last is what let a server sync
/// silently reorder the user's preference.
#[tauri::command]
#[specta::specta]
pub async fn import_config(
    raw: String,
    tunnel: State<'_, TunnelHandle>,
) -> Result<Protocol, ConfigError> {
    tunnel.import_config(raw).await
}

/// Forget every stored config.
///
/// Goes down and waits for the tunnel to actually be gone before wiping, rather than deciding from
/// a status snapshot — which is how a live adopted tunnel could survive being forgotten.
#[tauri::command]
#[specta::specta]
pub async fn clear_configs(tunnel: State<'_, TunnelHandle>) -> Result<(), IntentError> {
    tunnel.clear_configs().await
}

/// Forget which protocol last worked, so the next connect probes from the top of the order again.
#[tauri::command]
#[specta::specta]
pub async fn forget_preferred_protocol(tunnel: State<'_, TunnelHandle>) -> Result<(), ()> {
    tunnel.forget_preferred().await;
    Ok(())
}

/// Information about an installed app (for split tunneling UI)
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AppInfo {
    pub package_name: String,
    pub label: String,
    pub is_system: bool,
    pub icon: Option<String>,
}

/// Safe area insets (status bar, nav bar) in dp
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SafeAreaInsets {
    pub top: f64,
    pub bottom: f64,
}

/// Get list of installed apps for split tunneling (Android only)
#[tauri::command]
#[specta::specta]
pub async fn get_installed_apps(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<Vec<AppInfo>, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
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

    #[cfg(not(target_os = "android"))]
    {
        Ok(vec![])
    }
}

/// Check if battery optimization is disabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn is_battery_optimization_disabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .is_battery_optimization_disabled()
            .await
            .map_err(|e| format!("Failed to check battery optimization: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true) // Not applicable on desktop
    }
}

/// Request the user to disable battery optimization (Android only)
/// Returns whether battery optimization is now disabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn request_disable_battery_optimization(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .request_disable_battery_optimization()
            .await
            .map_err(|e| format!("Failed to request battery optimization: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Check if notifications are enabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn are_notifications_enabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .are_notifications_enabled()
            .await
            .map_err(|e| format!("Failed to check notifications: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Request notification permission (Android only)
/// Returns whether notifications are now enabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn open_notification_settings(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .open_notification_settings()
            .await
            .map_err(|e| format!("Failed to request notification permission: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Set status bar icon style to match app theme (Android only)
#[tauri::command]
#[specta::specta]
pub async fn set_status_bar_style(
    #[allow(unused_variables)] app: AppHandle,
    #[allow(unused_variables)] is_dark: bool,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .set_status_bar_style(is_dark)
            .await
            .map_err(|e| format!("Failed to set status bar style: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

/// Get current diagnostic capture status.
#[tauri::command]
#[specta::specta]
pub async fn get_log_capture_status(app: AppHandle) -> LogCaptureStatus {
    app.state::<CaptureSession>().status().await
}

/// Start a diagnostic capture. This enables verbose runtime logs and starts
/// writing capture files without changing the user's saved profile permanently.
#[tauri::command]
#[specta::specta]
pub async fn start_log_capture(
    session: State<'_, CaptureSession>,
) -> Result<LogCaptureStatus, String> {
    session.start().await.map_err(|e| e.to_string())
}

/// Stop the active diagnostic capture and restore the previous runtime profile.
#[tauri::command]
#[specta::specta]
pub async fn stop_log_capture(
    session: State<'_, CaptureSession>,
) -> Result<LogCaptureStatus, String> {
    session.stop().await.map_err(|e| e.to_string())
}

/// Export latest diagnostic capture as a tar.gz archive via native save dialog.
/// Returns `true` if saved successfully, `false` if the user cancelled.
#[tauri::command]
#[specta::specta]
pub async fn export_logs(app: AppHandle) -> Result<bool, String> {
    let archive = app
        .state::<CaptureSession>()
        .export()
        .await
        .map_err(|e| e.to_string())?;
    let filename = archive.filename;
    let archive_buf = archive.bytes;

    #[cfg(not(target_os = "android"))]
    {
        use tauri_plugin_dialog::DialogExt;

        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .file()
            .set_file_name(&filename)
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

        std::fs::write(&path, &archive_buf).map_err(|e| format!("Failed to write archive: {e}"))?;
    }

    #[cfg(target_os = "android")]
    {
        use tauri_plugin_android_fs::AndroidFsExt;

        let api = app.android_fs_async();
        let uri = api
            .picker()
            .save_file(None, &filename, Some("application/gzip"), false)
            .await
            .map_err(|e| format!("Save dialog failed: {e}"))?;

        let Some(uri) = uri else {
            return Ok(false);
        };

        api.write(&uri, &archive_buf)
            .await
            .map_err(|e| format!("Failed to write archive: {e}"))?;
    }

    Ok(true)
}

/// Get the current log configuration.
#[tauri::command]
#[specta::specta]
pub fn get_log_config() -> crate::logging::LogConfig {
    crate::logging::get_log_config()
}

/// Apply a new log configuration. Persists to disk and propagates to VPN process.
#[tauri::command]
#[specta::specta]
pub async fn set_log_config(
    config: crate::logging::LogConfig,
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<(), String> {
    crate::logging::apply_log_config(&config);
    crate::logging::save_log_config_to_disk(&config);
    backend.set_log_config(&config).await;
    info!("Log config updated");
    Ok(())
}

/// Get safe area insets (status bar, nav bar heights) in dp
#[tauri::command]
#[specta::specta]
pub async fn get_safe_area_insets(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<SafeAreaInsets, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
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

    #[cfg(not(target_os = "android"))]
    {
        Ok(SafeAreaInsets {
            top: 0.0,
            bottom: 0.0,
        })
    }
}
