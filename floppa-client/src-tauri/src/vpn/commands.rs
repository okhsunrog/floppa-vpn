use super::actor::handle::{IntentRequest, TunnelHandle};
use super::actor::types::{
    ConfigsView, CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelParams, TunnelState,
};
use super::backend::VpnBackend;
use super::config as vpn_config;
use super::protocol::Protocol;
use super::store::ConfigError;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, State};
#[allow(unused_imports)]
use tracing::{error, info, warn};

static LOG_CAPTURE_STATE: OnceLock<Mutex<LogCaptureState>> = OnceLock::new();

#[derive(Default)]
struct LogCaptureState {
    active: Option<ActiveLogCapture>,
    latest_capture_id: Option<String>,
}

struct ActiveLogCapture {
    id: String,
    previous_config: crate::logging::LogConfig,
    capture_config: crate::logging::LogConfig,
    started_at: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct LogCaptureStatus {
    pub active: bool,
    pub capture_id: Option<String>,
}

/// Get the persistent device ID.
/// Android: ANDROID_ID (stable across reinstalls, per signing key).
/// Desktop: random UUID persisted in config dir.
#[tauri::command]
#[specta::specta]
pub fn get_device_id(#[allow(unused_variables)] app: AppHandle) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        app.vpn()
            .get_device_id()
            .map_err(|e| format!("Failed to get ANDROID_ID: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    vpn_config::get_or_create_device_id()
}

/// Get the device name (Android: manufacturer+model, desktop: hostname)
#[tauri::command]
#[specta::specta]
pub fn get_device_name(#[allow(unused_variables)] app: AppHandle) -> String {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        match app.vpn().get_device_name() {
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

#[tauri::command]
#[specta::specta]
pub async fn list_configs(tunnel: State<'_, TunnelHandle>) -> Result<ConfigsView, ()> {
    Ok(tunnel.list_configs().await)
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
            .map_err(|e| format!("Failed to set status bar style: {e}"))
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

/// Get the log directory path
#[tauri::command]
#[specta::specta]
pub fn get_log_dir() -> Result<String, String> {
    crate::get_log_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Log directory not initialized".to_string())
}

/// Get current diagnostic capture status.
#[tauri::command]
#[specta::specta]
pub fn get_log_capture_status() -> LogCaptureStatus {
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));
    state
        .lock()
        .map(|guard| LogCaptureStatus {
            active: guard.active.is_some(),
            capture_id: guard
                .active
                .as_ref()
                .map(|capture| capture.id.clone())
                .or_else(|| guard.latest_capture_id.clone())
                .or_else(|| {
                    crate::get_log_dir()
                        .and_then(|log_dir| latest_capture_dir(log_dir.as_path()))
                        .and_then(|path| {
                            path.file_name()
                                .map(|name| name.to_string_lossy().to_string())
                        })
                }),
        })
        .unwrap_or(LogCaptureStatus {
            active: false,
            capture_id: None,
        })
}

/// Start a diagnostic capture. This enables verbose runtime logs and starts
/// writing capture files without changing the user's saved profile permanently.
#[tauri::command]
#[specta::specta]
pub async fn start_log_capture(
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<LogCaptureStatus, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));

    {
        let guard = state.lock().map_err(|_| "Capture state poisoned")?;
        if let Some(active) = &guard.active {
            return Ok(LogCaptureStatus {
                active: true,
                capture_id: Some(active.id.clone()),
            });
        }
    }

    let capture_id = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let previous_config = crate::logging::get_log_config();
    let mut capture_config = previous_config.clone();
    capture_config.profile = crate::logging::LogProfile::Verbose;

    crate::logging::apply_log_config(&capture_config);
    backend.set_log_config(&capture_config).await;
    if let Err(e) = crate::logging::write_active_capture_id(log_dir, &capture_id) {
        crate::logging::apply_log_config(&previous_config);
        backend.set_log_config(&previous_config).await;
        return Err(e);
    }
    if let Err(e) = crate::logging::start_file_capture(log_dir, "ui", &capture_id) {
        crate::logging::clear_active_capture_id(log_dir);
        crate::logging::apply_log_config(&previous_config);
        backend.set_log_config(&previous_config).await;
        return Err(e);
    }
    backend.start_log_capture(&capture_id).await;

    info!(capture_id, "Diagnostic log capture started");

    {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.active = Some(ActiveLogCapture {
            id: capture_id.clone(),
            previous_config,
            capture_config,
            started_at: chrono::Local::now().to_rfc3339(),
        });
        guard.latest_capture_id = Some(capture_id.clone());
    }

    Ok(LogCaptureStatus {
        active: true,
        capture_id: Some(capture_id),
    })
}

/// Stop the active diagnostic capture and restore the previous runtime profile.
#[tauri::command]
#[specta::specta]
pub async fn stop_log_capture(
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<LogCaptureStatus, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));
    let active = {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.active.take()
    };

    let Some(active) = active else {
        return Ok(get_log_capture_status());
    };

    info!(capture_id = active.id, "Diagnostic log capture stopping");
    backend.stop_log_capture().await;
    let _ = crate::logging::stop_file_capture();
    crate::logging::clear_active_capture_id(log_dir);

    crate::logging::apply_log_config(&active.previous_config);
    backend.set_log_config(&active.previous_config).await;

    write_capture_manifest(log_dir, &active)?;
    cleanup_old_captures(log_dir);

    {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.latest_capture_id = Some(active.id.clone());
    }

    Ok(LogCaptureStatus {
        active: false,
        capture_id: Some(active.id),
    })
}

/// Export latest diagnostic capture as a tar.gz archive via native save dialog.
/// Returns `true` if saved successfully, `false` if the user cancelled.
#[tauri::command]
#[specta::specta]
pub async fn export_logs(app: AppHandle) -> Result<bool, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let capture_dir = latest_capture_dir(log_dir).ok_or("No diagnostic captures found")?;
    let capture_id = capture_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or("Invalid capture directory")?;
    let archive_buf = build_log_archive(&capture_dir)?;

    let filename = format!("floppa-logs-{capture_id}.tar.gz");

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

fn latest_capture_dir(log_dir: &Path) -> Option<PathBuf> {
    let captures_dir = log_dir.join("captures");
    let mut dirs = std::fs::read_dir(captures_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.pop()
}

fn write_capture_manifest(log_dir: &Path, capture: &ActiveLogCapture) -> Result<(), String> {
    let capture_dir = log_dir.join("captures").join(&capture.id);
    let stopped_at = chrono::Local::now().to_rfc3339();

    let log_config_json = serde_json::to_vec_pretty(&capture.capture_config)
        .map_err(|e| format!("Failed to serialize capture log config: {e}"))?;
    std::fs::write(capture_dir.join("log-config.json"), log_config_json)
        .map_err(|e| format!("Failed to write capture log config: {e}"))?;

    let manifest = serde_json::json!({
        "schema_version": 1,
        "capture_id": capture.id,
        "started_at": capture.started_at,
        "stopped_at": stopped_at,
        "app_version": env!("CARGO_PKG_VERSION"),
        "profile_during_capture": capture.capture_config.profile.clone(),
        "custom_filter_enabled": capture.capture_config.custom_filter_enabled,
        "custom_filter": capture.capture_config.custom_filter.clone(),
        "files": capture_file_entries(&capture_dir),
    });

    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize capture manifest: {e}"))?;
    std::fs::write(capture_dir.join("manifest.json"), manifest_json)
        .map_err(|e| format!("Failed to write capture manifest: {e}"))?;
    Ok(())
}

fn capture_file_entries(capture_dir: &Path) -> Vec<serde_json::Value> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(capture_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Ok(metadata) = path.metadata()
                && let Some(name) = path.file_name()
            {
                files.push(serde_json::json!({
                    "name": name.to_string_lossy(),
                    "bytes": metadata.len(),
                }));
            }
        }
    }
    files.sort_by_key(|entry| {
        entry
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or_default()
            .to_string()
    });
    files
}

fn cleanup_old_captures(log_dir: &Path) {
    let captures_dir = log_dir.join("captures");
    let Ok(entries) = std::fs::read_dir(&captures_dir) else {
        return;
    };

    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let mut dirs = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();

    let keep_from = dirs.len().saturating_sub(3);
    for (idx, path) in dirs.iter().enumerate() {
        let old_by_count = idx < keep_from;
        let old_by_age = path
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if old_by_count || old_by_age {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn build_log_archive(capture_dir: &Path) -> Result<Vec<u8>, String> {
    let mut archive_buf = Vec::new();
    {
        let gz_encoder =
            flate2::write::GzEncoder::new(&mut archive_buf, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(gz_encoder);

        let entries =
            std::fs::read_dir(capture_dir).map_err(|e| format!("Failed to read capture: {e}"))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name()
            {
                tar_builder
                    .append_path_with_name(&path, name)
                    .map_err(|e| format!("Failed to add file to archive: {e}"))?;
            }
        }

        tar_builder
            .finish()
            .map_err(|e| format!("Failed to finalize archive: {e}"))?;
    }
    Ok(archive_buf)
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
