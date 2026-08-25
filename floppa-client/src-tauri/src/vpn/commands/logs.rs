//! Log configuration and diagnostic captures.
//!
//! The capture lifecycle lives in [`CaptureSession`]; these commands only forward to it and turn
//! its typed errors into the strings the bindings carry.

use crate::logging::capture::{CaptureSession, LogCaptureStatus};
use crate::logging::{self, LogConfig};
use crate::vpn::backend::VpnBackend;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tracing::info;

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
    super::save_archive(&app, &archive.filename, &archive.bytes).await
}

/// Get the current log configuration.
#[tauri::command]
#[specta::specta]
pub fn get_log_config() -> LogConfig {
    logging::get_log_config()
}

/// Apply a new log configuration. Persists to disk and propagates to VPN process.
#[tauri::command]
#[specta::specta]
pub async fn set_log_config(
    config: LogConfig,
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<(), String> {
    logging::apply_log_config(&config);
    logging::save_log_config_to_disk(&config);
    backend.set_log_config(&config).await;
    info!("Log config updated");
    Ok(())
}
