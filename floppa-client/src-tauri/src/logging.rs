use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Layer, prelude::*};

type FilterHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static RELOAD_HANDLE: OnceLock<FilterHandle> = OnceLock::new();

/// Current in-memory log config. Protected by RwLock for concurrent reads.
static LOG_CONFIG: OnceLock<RwLock<LogConfig>> = OnceLock::new();

/// The log directory: where `log-config.json`, the active-capture marker and the captures live.
/// Set once by [`init_tracing`]; the single source for every path under it.
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// File capture state. Logcat/stdout remains active independently from this.
static FILE_CAPTURE: OnceLock<Mutex<Option<FileCapture>>> = OnceLock::new();

const LOG_CONFIG_FILENAME: &str = "log-config.json";
const ACTIVE_CAPTURE_FILENAME: &str = "active-capture";

/// Upper bound on one process's capture file.
///
/// A capture is meant to be minutes long and exported; one that is forgotten — a UI that died
/// mid-capture with the `:vpn` process still writing — must not grow until the disk is full.
/// Writes past the cap are dropped, not rotated: the beginning of a capture is the useful part.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

/// Which process is initialising tracing.
///
/// The two used to be two near-identical functions. The differences are exactly these: the name
/// on the capture file, and what to do with a leftover `active-capture` marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogProcess {
    /// The Tauri UI process. Owns the capture marker: it writes it when a capture starts and
    /// removes it when the capture stops.
    Ui,
    /// The Android `:vpn` process. Reads the marker so a capture survives a service restart.
    Vpn,
}

impl LogProcess {
    const fn capture_name(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Vpn => "vpn",
        }
    }
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Persistent runtime logging configuration.
#[derive(Serialize, Deserialize, Clone, Debug, specta::Type)]
#[serde(default)]
pub struct LogConfig {
    /// Active profile for runtime logs (logcat/stdout and capture files).
    pub profile: LogProfile,
    /// Optional raw RUST_LOG-style filter string, stored separately from activation.
    pub custom_filter: Option<String>,
    /// When true, `custom_filter` replaces the selected profile.
    pub custom_filter_enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum LogProfile {
    #[default]
    Normal,
    Verbose,
}

impl std::fmt::Display for LogProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogProfile::Normal => write!(f, "normal"),
            LogProfile::Verbose => write!(f, "verbose"),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            profile: LogProfile::Normal,
            custom_filter: None,
            custom_filter_enabled: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Filter building
// ---------------------------------------------------------------------------

fn default_base_level() -> &'static str {
    "warn"
}

fn build_filter_from_config(config: &LogConfig) -> EnvFilter {
    if config.custom_filter_enabled
        && let Some(custom) = &config.custom_filter
    {
        if let Ok(filter) = EnvFilter::try_new(custom) {
            return filter;
        }
        eprintln!("Invalid custom log filter '{custom}', using selected profile");
    }

    let mut filter =
        EnvFilter::from_default_env().add_directive(default_base_level().parse().unwrap());

    let directives: &[&str] = match config.profile {
        LogProfile::Normal => &[
            "floppa_client_lib=info",
            "shoes_lite=info",
            "gotatun=info",
            "webview=warn",
            "log=warn",
            "tarpc=warn",
        ],
        LogProfile::Verbose => &[
            "floppa_client_lib=trace",
            "shoes_lite=trace",
            "gotatun=trace",
            "webview=debug",
            "log=debug",
            "tarpc=trace",
        ],
    };

    for directive in directives {
        if let Ok(d) = directive.parse() {
            filter = filter.add_directive(d);
        }
    }

    filter
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn load_log_config_from_disk(log_dir: &Path) -> LogConfig {
    let path = log_dir.join(LOG_CONFIG_FILENAME);
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<LogConfig>(&json) {
                Ok(config) => return config,
                Err(e) => eprintln!("Failed to parse {}: {e}, using defaults", path.display()),
            },
            Err(e) => eprintln!("Failed to read {}: {e}, using defaults", path.display()),
        }
    }
    LogConfig::default()
}

pub fn save_log_config_to_disk(config: &LogConfig) {
    let Some(dir) = LOG_DIR.get() else {
        return;
    };
    let path = dir.join(LOG_CONFIG_FILENAME);
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to save log config: {e}");
            }
        }
        Err(e) => tracing::warn!("Failed to serialize log config: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Get the current log config (cloned).
pub fn get_log_config() -> LogConfig {
    LOG_CONFIG
        .get()
        .and_then(|rw| rw.read().ok())
        .map(|c| c.clone())
        .unwrap_or_default()
}

/// The log directory, once [`init_tracing`] has run.
pub fn get_log_dir() -> Option<&'static PathBuf> {
    LOG_DIR.get()
}

/// Apply a new log config at runtime: update in-memory and reload filter.
pub fn apply_log_config(config: &LogConfig) {
    if let Some(rw) = LOG_CONFIG.get()
        && let Ok(mut guard) = rw.write()
    {
        *guard = config.clone();
    }
    if let Some(handle) = RELOAD_HANDLE.get() {
        let filter = build_filter_from_config(config);
        let _ = handle.reload(filter);
    }
}

/// Start writing log events to the capture file for this process.
pub fn start_file_capture(
    log_dir: &Path,
    process_name: &'static str,
    capture_id: &str,
) -> Result<PathBuf, String> {
    let capture_dir = log_dir.join("captures").join(capture_id);
    std::fs::create_dir_all(&capture_dir)
        .map_err(|e| format!("Failed to create capture directory: {e}"))?;
    let path = capture_dir.join(format!("{process_name}.log"));
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Failed to open capture log: {e}"))?;
    // Resuming an existing file (a restarted `:vpn` process) counts what is already there.
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);

    let lock = FILE_CAPTURE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock() {
        *guard = Some(FileCapture {
            process_name,
            file,
            path: path.clone(),
            written,
        });
    }

    tracing::info!(
        capture_id,
        process = process_name,
        "DIAGNOSTIC_CAPTURE_START"
    );
    Ok(path)
}

/// Stop writing log events to a capture file for this process.
pub fn stop_file_capture() -> Option<PathBuf> {
    tracing::info!("DIAGNOSTIC_CAPTURE_STOP");
    let lock = FILE_CAPTURE.get_or_init(|| Mutex::new(None));
    lock.lock()
        .ok()
        .and_then(|mut guard| guard.take().map(|capture| capture.path))
}

pub fn active_capture_id(log_dir: &Path) -> Option<String> {
    std::fs::read_to_string(log_dir.join(ACTIVE_CAPTURE_FILENAME))
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

pub fn write_active_capture_id(log_dir: &Path, capture_id: &str) -> Result<(), String> {
    std::fs::write(log_dir.join(ACTIVE_CAPTURE_FILENAME), capture_id)
        .map_err(|e| format!("Failed to write active capture marker: {e}"))
}

pub fn clear_active_capture_id(log_dir: &Path) {
    let _ = std::fs::remove_file(log_dir.join(ACTIVE_CAPTURE_FILENAME));
}

/// Initialize tracing for `process`.
///
/// Loads persisted log config from disk to set the initial filter. Output goes to stdout
/// (desktop) or logcat (Android); file output is enabled only while a diagnostic capture is
/// active. Stores a reload handle so the log config can be changed at runtime (and, in the `:vpn`
/// process, over RPC).
pub fn init_tracing(log_dir: &Path, process: LogProcess) {
    let _ = std::fs::create_dir_all(log_dir);
    let _ = LOG_DIR.set(log_dir.to_path_buf());

    let config = load_log_config_from_disk(log_dir);
    let initial_filter = build_filter_from_config(&config);
    let _ = LOG_CONFIG.set(RwLock::new(config));

    let file_layer = build_file_layer(process.capture_name());

    let (filter, reload_handle) = reload::Layer::new(initial_filter);
    let _ = RELOAD_HANDLE.set(reload_handle);

    #[cfg(target_os = "android")]
    {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(build_logcat_layer())
            .with(file_layer)
            .try_init();
    }

    #[cfg(not(target_os = "android"))]
    {
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_ansi(true)
            .event_format(ShortTargetFormat)
            .with_writer(std::io::stdout);

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(file_layer)
            .try_init();
    }

    match (process, active_capture_id(log_dir)) {
        // The service restarted under a running capture: keep writing to the same one.
        (LogProcess::Vpn, Some(capture_id)) => {
            let _ = start_file_capture(log_dir, process.capture_name(), &capture_id);
        }
        // The UI owns the marker and clears it when a capture stops, so a marker present at UI
        // start means the previous UI died mid-capture. Left in place, every later `:vpn` start
        // resumed a capture nobody was going to export.
        (LogProcess::Ui, Some(capture_id)) => {
            tracing::warn!(
                capture_id,
                "removing a capture marker left by a previous run"
            );
            clear_active_capture_id(log_dir);
        }
        (_, None) => {}
    }
}

// ---------------------------------------------------------------------------
// Shared layer builders
// ---------------------------------------------------------------------------

struct FileCapture {
    process_name: &'static str,
    file: File,
    path: PathBuf,
    /// Bytes in the file so far, against [`MAX_CAPTURE_BYTES`].
    written: u64,
}

#[derive(Clone, Copy)]
struct CaptureMakeWriter {
    process_name: &'static str,
}

struct CaptureWriter {
    process_name: &'static str,
}

impl<'a> MakeWriter<'a> for CaptureMakeWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter {
            process_name: self.process_name,
        }
    }
}

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(lock) = FILE_CAPTURE.get() else {
            return Ok(buf.len());
        };
        let Ok(mut guard) = lock.lock() else {
            return Ok(buf.len());
        };
        let Some(capture) = guard.as_mut() else {
            return Ok(buf.len());
        };
        if capture.process_name != self.process_name {
            return Ok(buf.len());
        }
        if capture.written >= MAX_CAPTURE_BYTES {
            // Full. Dropped silently: logging from inside the log writer would recurse.
            return Ok(buf.len());
        }
        capture.file.write_all(buf)?;
        capture.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some(lock) = FILE_CAPTURE.get() else {
            return Ok(());
        };
        let Ok(mut guard) = lock.lock() else {
            return Ok(());
        };
        if let Some(capture) = guard.as_mut()
            && capture.process_name == self.process_name
        {
            capture.file.flush()?;
        }
        Ok(())
    }
}

/// Create a file layer that writes only while a diagnostic capture is active.
fn build_file_layer<S>(process_name: &'static str) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_file(false)
        .with_line_number(false)
        .with_writer(CaptureMakeWriter { process_name })
        .boxed()
}

/// Create a logcat layer tagged `FloppaVPN`.
#[cfg(target_os = "android")]
fn build_logcat_layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_logcat::{LogcatMakeWriter, LogcatTag};

    let tag = LogcatTag::Fixed("FloppaVPN".to_owned());
    let logcat_writer = LogcatMakeWriter::new(tag).ok()?;

    Some(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_file(false)
            .with_line_number(false)
            .with_writer(logcat_writer)
            .boxed(),
    )
}

// ---------------------------------------------------------------------------
// Desktop-only custom formatter
// ---------------------------------------------------------------------------

/// Custom formatter that resolves the real source for log-crate events.
///
/// `tracing-log` normalizes all `log` crate events to target `log` and stores
/// the original target in a `log.target` field. We extract that field to show
/// the actual source (e.g. `shoes_lite::crypto`, `keyring`).
///
/// Tauri's WebView interception produces targets like
/// `webview:error@http://localhost:1420/...` — we strip the URL suffix.
#[cfg(not(target_os = "android"))]
struct ShortTargetFormat;

#[cfg(not(target_os = "android"))]
impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for ShortTargetFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let target = meta.target();

        // For log-crate events (target "log"), extract the real source from
        // the `log.target` field that tracing-log stores.
        let real_target = if target == "log" {
            let mut visitor = LogTargetVisitor::default();
            event.record(&mut visitor);
            visitor.log_target.unwrap_or_default()
        } else {
            String::new()
        };

        let short_target = if !real_target.is_empty() && real_target.starts_with("webview") {
            "webview"
        } else if !real_target.is_empty() {
            &real_target
        } else if target.starts_with("webview") {
            "webview"
        } else {
            target
        };

        let (color, level_str) = match *meta.level() {
            tracing::Level::ERROR => ("\x1b[31m", "ERROR"),
            tracing::Level::WARN => ("\x1b[33m", " WARN"),
            tracing::Level::INFO => ("\x1b[32m", " INFO"),
            tracing::Level::DEBUG => ("\x1b[34m", "DEBUG"),
            tracing::Level::TRACE => ("\x1b[35m", "TRACE"),
        };

        write!(
            writer,
            " {color}{level_str}\x1b[0m \x1b[2m{short_target}\x1b[0m: "
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Visitor that extracts the `log.target` field from tracing-log events.
#[cfg(not(target_os = "android"))]
#[derive(Default)]
struct LogTargetVisitor {
    log_target: Option<String>,
}

#[cfg(not(target_os = "android"))]
impl tracing::field::Visit for LogTargetVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "log.target" {
            self.log_target = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}
