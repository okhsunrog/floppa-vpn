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

/// Where a line the webview wrote sits against the rest of the log.
#[derive(Debug, Clone, Copy, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum WebviewLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Record a line the frontend wrote, under the frontend's own target.
///
/// The webview used to reach the log through `tauri-plugin-log`, which hands the line to the
/// `log` crate — and every `log` record arrives in tracing as the *same* callsite, target `log`,
/// with the real source demoted to a `log.target` field. Third-party crates come in that way too,
/// so the one directive that could quiet a noisy dependency also silenced the frontend: `log=warn`
/// in the normal profile, and `console.info` — the level everything in the frontend is told to
/// use — never reached logcat at all.
///
/// Five callsites rather than one dynamic target, because a tracing target is fixed at its
/// callsite: that is exactly what makes `webview=…` a filter directive of its own.
#[tauri::command]
#[specta::specta]
pub fn webview_log(level: WebviewLevel, message: String) {
    match level {
        WebviewLevel::Trace => tracing::trace!(target: "webview", "{message}"),
        WebviewLevel::Debug => tracing::debug!(target: "webview", "{message}"),
        WebviewLevel::Info => tracing::info!(target: "webview", "{message}"),
        WebviewLevel::Warn => tracing::warn!(target: "webview", "{message}"),
        WebviewLevel::Error => tracing::error!(target: "webview", "{message}"),
    }
}
