//! Diagnostic log capture.
//!
//! A capture is three things at once: the runtime profile forced to Verbose in this process and,
//! through the backend, in the tunnel process; a capture file per process under
//! `captures/<id>/`; and the `active-capture` marker that lets a restarted `:vpn` process rejoin
//! the capture it was part of. [`CaptureSession`] owns all three.
//!
//! It lives in Tauri state, and every operation runs under one lock for its whole duration. Two
//! Start requests can no longer both pass the "already active?" check, and a Stop can no longer
//! interleave with the Start it undoes — the gap the previous design had, where a global lock was
//! taken once to look and again, after the side effects, to record.

use super::{FileCaptureError, LogConfig, LogProcess, LogProfile};
use serde::Serialize;
use specta::Type;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tracing::info;

const ACTIVE_CAPTURE_FILENAME: &str = "active-capture";
const CAPTURES_DIRNAME: &str = "captures";

/// How many finished captures stay on disk, newest first.
const KEEP_CAPTURES: usize = 3;
/// A capture older than this is removed even if it is among the newest.
const MAX_CAPTURE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug, Serialize, Type)]
pub struct LogCaptureStatus {
    pub active: bool,
    pub capture_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("No diagnostic captures found")]
    NoCaptures,
    #[error("Invalid capture directory")]
    InvalidCaptureDir,
    #[error("Failed to write active capture marker: {0}")]
    Marker(#[source] io::Error),
    #[error(transparent)]
    File(#[from] FileCaptureError),
    #[error("Failed to serialize capture log config: {0}")]
    SerializeLogConfig(#[source] serde_json::Error),
    #[error("Failed to write capture log config: {0}")]
    WriteLogConfig(#[source] io::Error),
    #[error("Failed to serialize capture manifest: {0}")]
    SerializeManifest(#[source] serde_json::Error),
    #[error("Failed to write capture manifest: {0}")]
    WriteManifest(#[source] io::Error),
    #[error("Failed to read capture: {0}")]
    ReadCapture(#[source] io::Error),
    #[error("Failed to add file to archive: {0}")]
    ArchiveAppend(#[source] io::Error),
    #[error("Failed to finalize archive: {0}")]
    ArchiveFinish(#[source] io::Error),
}

/// The latest capture, packed for a save dialog.
#[derive(Debug)]
pub struct LogArchive {
    pub filename: String,
    pub bytes: Vec<u8>,
}

struct ActiveCapture {
    id: String,
    previous_config: LogConfig,
    capture_config: LogConfig,
    started_at: String,
}

#[derive(Default)]
struct CaptureState {
    active: Option<ActiveCapture>,
    latest_capture_id: Option<String>,
}

/// Whoever can reach the process that writes the tunnel's log.
///
/// On desktop that is this process and the relay is a direct call; on Android it is `:vpn`, and the
/// relay is the socket the actor is on. A capture is only complete when both processes are in it,
/// which is the whole reason this is not just a local function.
#[async_trait::async_trait]
pub trait LogRelay: Send + Sync {
    async fn set_log_config(&self, config: &LogConfig);
    async fn start_log_capture(&self, capture_id: &str);
    async fn stop_log_capture(&self);
}

/// The desktop relay: the tunnel is in this process, so "relaying" is telling the backend.
#[cfg(not(target_os = "android"))]
pub struct BackendRelay(pub Arc<dyn crate::backend::VpnBackend>);

#[cfg(not(target_os = "android"))]
#[async_trait::async_trait]
impl LogRelay for BackendRelay {
    async fn set_log_config(&self, config: &LogConfig) {
        self.0.set_log_config(config).await;
    }
    async fn start_log_capture(&self, capture_id: &str) {
        self.0.start_log_capture(capture_id).await;
    }
    async fn stop_log_capture(&self) {
        self.0.stop_log_capture().await;
    }
}

/// The one owner of diagnostic captures in the UI process.
pub struct CaptureSession {
    log_dir: PathBuf,
    relay: Arc<dyn LogRelay>,
    state: Mutex<CaptureState>,
}

impl CaptureSession {
    /// `log_dir` is the directory [`super::init_tracing`] was given: the marker and the captures
    /// live under it. Taken explicitly rather than read from the global, so a session is usable
    /// wherever tracing was initialised — including a test that never initialises it.
    pub fn new(log_dir: PathBuf, relay: Arc<dyn LogRelay>) -> Self {
        Self {
            log_dir,
            relay,
            state: Mutex::new(CaptureState::default()),
        }
    }

    pub async fn status(&self) -> LogCaptureStatus {
        self.state.lock().await.status(&self.log_dir)
    }

    /// Start a capture. Idempotent: a second Start reports the capture already running.
    ///
    /// Switches the runtime profile to Verbose here and in the tunnel process without touching
    /// the saved profile; Stop restores it. Every failure path restores it too, so a capture that
    /// could not start leaves no trace.
    pub async fn start(&self) -> Result<LogCaptureStatus, CaptureError> {
        let mut state = self.state.lock().await;
        if let Some(active) = &state.active {
            return Ok(LogCaptureStatus {
                active: true,
                capture_id: Some(active.id.clone()),
            });
        }

        let capture_id = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let previous_config = super::get_log_config();
        let capture_config = LogConfig {
            profile: LogProfile::Verbose,
            ..previous_config.clone()
        };

        self.apply(&capture_config).await;
        if let Err(e) = write_active_capture_id(&self.log_dir, &capture_id) {
            self.apply(&previous_config).await;
            return Err(CaptureError::Marker(e));
        }
        if let Err(e) =
            super::start_file_capture(&self.log_dir, LogProcess::Ui.capture_name(), &capture_id)
        {
            clear_active_capture_id(&self.log_dir);
            self.apply(&previous_config).await;
            return Err(e.into());
        }
        self.relay.start_log_capture(&capture_id).await;

        info!(capture_id, "Diagnostic log capture started");

        state.active = Some(ActiveCapture {
            id: capture_id.clone(),
            previous_config,
            capture_config,
            started_at: chrono::Local::now().to_rfc3339(),
        });
        state.latest_capture_id = Some(capture_id.clone());

        Ok(LogCaptureStatus {
            active: true,
            capture_id: Some(capture_id),
        })
    }

    /// Stop the active capture, restore the previous runtime profile and write the manifest.
    /// A Stop with nothing active is a status read.
    pub async fn stop(&self) -> Result<LogCaptureStatus, CaptureError> {
        let mut state = self.state.lock().await;
        let Some(active) = state.active.take() else {
            return Ok(state.status(&self.log_dir));
        };

        info!(capture_id = active.id, "Diagnostic log capture stopping");
        self.relay.stop_log_capture().await;
        let _ = super::stop_file_capture();
        clear_active_capture_id(&self.log_dir);
        self.apply(&active.previous_config).await;

        write_capture_manifest(&self.log_dir, &active)?;
        cleanup_old_captures(&self.log_dir);

        state.latest_capture_id = Some(active.id.clone());

        Ok(LogCaptureStatus {
            active: false,
            capture_id: Some(active.id),
        })
    }

    /// Pack the latest capture as a tar.gz. Held under the lock so it cannot read a capture
    /// directory while a Stop is still writing its manifest into it.
    pub async fn export(&self) -> Result<LogArchive, CaptureError> {
        let _state = self.state.lock().await;
        let capture_dir = latest_capture_dir(&self.log_dir).ok_or(CaptureError::NoCaptures)?;
        let capture_id = dir_name(&capture_dir).ok_or(CaptureError::InvalidCaptureDir)?;
        let bytes = build_log_archive(&capture_dir)?;
        Ok(LogArchive {
            filename: format!("floppa-logs-{capture_id}.tar.gz"),
            bytes,
        })
    }

    /// Apply a runtime profile in this process and in the tunnel process.
    async fn apply(&self, config: &LogConfig) {
        super::apply_log_config(config);
        self.relay.set_log_config(config).await;
    }
}

impl CaptureState {
    fn status(&self, log_dir: &Path) -> LogCaptureStatus {
        LogCaptureStatus {
            active: self.active.is_some(),
            capture_id: self
                .active
                .as_ref()
                .map(|capture| capture.id.clone())
                .or_else(|| self.latest_capture_id.clone())
                .or_else(|| latest_capture_dir(log_dir).and_then(|path| dir_name(&path))),
        }
    }
}

// ------------------------------------------------------------------------------------ the marker

/// The capture id a `:vpn` process should rejoin, if a capture is running.
pub fn active_capture_id(log_dir: &Path) -> Option<String> {
    std::fs::read_to_string(log_dir.join(ACTIVE_CAPTURE_FILENAME))
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

fn write_active_capture_id(log_dir: &Path, capture_id: &str) -> io::Result<()> {
    std::fs::write(log_dir.join(ACTIVE_CAPTURE_FILENAME), capture_id)
}

pub fn clear_active_capture_id(log_dir: &Path) {
    let _ = std::fs::remove_file(log_dir.join(ACTIVE_CAPTURE_FILENAME));
}

// --------------------------------------------------------------------------------- the captures

fn captures_dir(log_dir: &Path) -> PathBuf {
    log_dir.join(CAPTURES_DIRNAME)
}

/// Capture directories, oldest first. Ids are timestamps, so lexical order is chronological.
fn capture_dirs(log_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = std::fs::read_dir(captures_dir(log_dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn latest_capture_dir(log_dir: &Path) -> Option<PathBuf> {
    capture_dirs(log_dir).pop()
}

fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
}

fn write_capture_manifest(log_dir: &Path, capture: &ActiveCapture) -> Result<(), CaptureError> {
    let capture_dir = captures_dir(log_dir).join(&capture.id);
    let stopped_at = chrono::Local::now().to_rfc3339();

    let log_config_json = serde_json::to_vec_pretty(&capture.capture_config)
        .map_err(CaptureError::SerializeLogConfig)?;
    std::fs::write(capture_dir.join("log-config.json"), log_config_json)
        .map_err(CaptureError::WriteLogConfig)?;

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

    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(CaptureError::SerializeManifest)?;
    std::fs::write(capture_dir.join("manifest.json"), manifest_json)
        .map_err(CaptureError::WriteManifest)?;
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

/// Keep the newest [`KEEP_CAPTURES`], and none older than [`MAX_CAPTURE_AGE`].
fn cleanup_old_captures(log_dir: &Path) {
    let now = SystemTime::now();
    let dirs = capture_dirs(log_dir);
    let keep_from = dirs.len().saturating_sub(KEEP_CAPTURES);
    for (idx, path) in dirs.iter().enumerate() {
        let old_by_count = idx < keep_from;
        let old_by_age = path
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MAX_CAPTURE_AGE);
        if old_by_count || old_by_age {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn build_log_archive(capture_dir: &Path) -> Result<Vec<u8>, CaptureError> {
    let mut archive_buf = Vec::new();
    {
        let gz_encoder =
            flate2::write::GzEncoder::new(&mut archive_buf, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(gz_encoder);

        let entries = std::fs::read_dir(capture_dir).map_err(CaptureError::ReadCapture)?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name()
            {
                tar_builder
                    .append_path_with_name(&path, name)
                    .map_err(CaptureError::ArchiveAppend)?;
            }
        }

        tar_builder.finish().map_err(CaptureError::ArchiveFinish)?;
    }
    Ok(archive_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::io::Read;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Records what the tunnel's process would have been told.
    #[derive(Default)]
    struct RecordingRelay {
        profiles: StdMutex<Vec<LogProfile>>,
        started: StdMutex<Vec<String>>,
        stops: AtomicU32,
    }

    #[async_trait]
    impl LogRelay for RecordingRelay {
        async fn set_log_config(&self, config: &LogConfig) {
            self.profiles.lock().unwrap().push(config.profile.clone());
        }

        async fn start_log_capture(&self, capture_id: &str) {
            self.started.lock().unwrap().push(capture_id.to_string());
        }

        async fn stop_log_capture(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A fresh directory per test. No `tempfile` dependency, and the directory is removed on
    /// drop so a passing run leaves nothing behind.
    struct TempLogDir(PathBuf);

    impl TempLogDir {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "floppa-capture-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempLogDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session(dir: &TempLogDir) -> (CaptureSession, Arc<RecordingRelay>) {
        let relay = Arc::new(RecordingRelay::default());
        let session = CaptureSession::new(dir.path().to_path_buf(), relay.clone());
        (session, relay)
    }

    fn fake_capture(log_dir: &Path, id: &str) -> PathBuf {
        let dir = captures_dir(log_dir).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ui.log"), "line\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn a_fresh_session_reports_nothing_until_a_capture_exists_on_disk() {
        let dir = TempLogDir::new();
        let (session, _) = session(&dir);

        let status = session.status().await;
        assert!(!status.active);
        assert_eq!(status.capture_id, None);

        // A capture left by a previous run is still the latest one, and thus exportable.
        fake_capture(dir.path(), "2026-01-01T00-00-00");
        assert_eq!(
            session.status().await.capture_id.as_deref(),
            Some("2026-01-01T00-00-00")
        );
    }

    #[tokio::test]
    async fn start_is_idempotent_and_stop_writes_the_manifest() {
        let dir = TempLogDir::new();
        let (session, relay) = session(&dir);

        let started = session.start().await.unwrap();
        assert!(started.active);
        let id = started
            .capture_id
            .clone()
            .expect("a started capture has an id");
        assert_eq!(active_capture_id(dir.path()).as_deref(), Some(id.as_str()));
        assert_eq!(*relay.started.lock().unwrap(), vec![id.clone()]);

        let again = session.start().await.unwrap();
        assert_eq!(again.capture_id, started.capture_id, "no second capture");
        assert_eq!(relay.started.lock().unwrap().len(), 1);

        let stopped = session.stop().await.unwrap();
        assert!(!stopped.active);
        assert_eq!(stopped.capture_id.as_deref(), Some(id.as_str()));
        assert_eq!(active_capture_id(dir.path()), None, "marker removed");
        assert_eq!(relay.stops.load(Ordering::SeqCst), 1);

        let capture_dir = captures_dir(dir.path()).join(&id);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(capture_dir.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["capture_id"], id);
        assert_eq!(manifest["profile_during_capture"], "verbose");
        let files = manifest["files"].as_array().unwrap();
        assert!(
            files.iter().any(|f| f["name"] == "log-config.json"),
            "{files:?}"
        );
        assert!(files.iter().any(|f| f["name"] == "ui.log"), "{files:?}");

        // Still the latest after the capture is over.
        assert_eq!(session.status().await.capture_id, Some(id));
    }

    #[tokio::test]
    async fn the_tunnel_process_is_switched_to_verbose_and_back() {
        let dir = TempLogDir::new();
        let (session, relay) = session(&dir);

        session.start().await.unwrap();
        session.stop().await.unwrap();

        // Whatever profile the user runs, the capture forces Verbose and then puts it back.
        let profiles = relay.profiles.lock().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0], LogProfile::Verbose);
        assert_eq!(profiles[1], super::super::get_log_config().profile);
    }

    #[tokio::test]
    async fn stop_without_a_capture_is_a_status_read() {
        let dir = TempLogDir::new();
        let (session, relay) = session(&dir);

        let status = session.stop().await.unwrap();
        assert!(!status.active);
        assert_eq!(relay.stops.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn export_packs_the_latest_capture() {
        let dir = TempLogDir::new();
        let (session, _) = session(&dir);
        fake_capture(dir.path(), "2026-01-01T00-00-00");
        fake_capture(dir.path(), "2026-01-02T00-00-00");

        let archive = session.export().await.unwrap();
        assert_eq!(archive.filename, "floppa-logs-2026-01-02T00-00-00.tar.gz");

        let mut names = Vec::new();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive.bytes.as_slice()));
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            names.push(entry.path().unwrap().to_string_lossy().to_string());
            let mut body = String::new();
            entry.read_to_string(&mut body).unwrap();
            assert_eq!(body, "line\n");
        }
        assert_eq!(names, vec!["ui.log"]);
    }

    #[tokio::test]
    async fn export_with_no_capture_is_a_typed_error() {
        let dir = TempLogDir::new();
        let (session, _) = session(&dir);
        assert!(matches!(
            session.export().await,
            Err(CaptureError::NoCaptures)
        ));
    }

    #[test]
    fn cleanup_keeps_the_newest_three() {
        let dir = TempLogDir::new();
        for id in ["a", "b", "c", "d", "e"] {
            fake_capture(dir.path(), id);
        }
        cleanup_old_captures(dir.path());
        let left = capture_dirs(dir.path())
            .iter()
            .filter_map(|p| dir_name(p))
            .collect::<Vec<_>>();
        assert_eq!(left, vec!["c", "d", "e"]);
    }

    #[test]
    fn a_blank_marker_is_no_marker() {
        let dir = TempLogDir::new();
        std::fs::write(dir.path().join(ACTIVE_CAPTURE_FILENAME), " \n").unwrap();
        assert_eq!(active_capture_id(dir.path()), None);
        write_active_capture_id(dir.path(), "x").unwrap();
        assert_eq!(active_capture_id(dir.path()).as_deref(), Some("x"));
        clear_active_capture_id(dir.path());
        assert_eq!(active_capture_id(dir.path()), None);
    }
}
