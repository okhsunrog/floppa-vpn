use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Layer, prelude::*};

type FilterHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static RELOAD_HANDLE: OnceLock<FilterHandle> = OnceLock::new();

/// Current in-memory log config. Protected by RwLock for concurrent reads.
static LOG_CONFIG: OnceLock<RwLock<LogConfig>> = OnceLock::new();

/// Directory where log-config.json is persisted.
static LOG_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

const LOG_CONFIG_FILENAME: &str = "log-config.json";

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Persistent logging configuration with per-component verbosity levels.
#[derive(Serialize, Deserialize, Clone, Debug, specta::Type)]
pub struct LogConfig {
    /// Per-component log levels. Keys are component IDs (e.g., "app", "tunnel").
    pub components: HashMap<String, LogLevel>,
    /// Optional raw RUST_LOG-style filter string. When set, REPLACES component-based config.
    pub custom_filter: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "trace"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        let mut components = HashMap::new();
        #[cfg(debug_assertions)]
        {
            components.insert("app".to_string(), LogLevel::Trace);
            components.insert("tunnel".to_string(), LogLevel::Debug);
            components.insert("webview".to_string(), LogLevel::Trace);
            components.insert("ipc".to_string(), LogLevel::Debug);
        }
        #[cfg(not(debug_assertions))]
        {
            components.insert("app".to_string(), LogLevel::Debug);
            components.insert("tunnel".to_string(), LogLevel::Info);
            components.insert("webview".to_string(), LogLevel::Info);
            components.insert("ipc".to_string(), LogLevel::Warn);
        }
        Self {
            components,
            custom_filter: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Component → tracing target mapping
// ---------------------------------------------------------------------------

/// Maps component IDs to the tracing targets they control.
fn component_targets() -> &'static HashMap<&'static str, &'static [&'static str]> {
    static MAP: OnceLock<HashMap<&str, &[&str]>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("app", ["floppa_client_lib"].as_slice());
        m.insert("tunnel", ["shoes_lite", "gotatun"].as_slice());
        m.insert("webview", ["webview", "log"].as_slice());
        m.insert("ipc", ["tarpc"].as_slice());
        m
    })
}

// ---------------------------------------------------------------------------
// Filter building
// ---------------------------------------------------------------------------

fn default_base_level() -> &'static str {
    #[cfg(debug_assertions)]
    {
        "debug"
    }
    #[cfg(not(debug_assertions))]
    {
        "warn"
    }
}

fn build_filter_from_config(config: &LogConfig) -> EnvFilter {
    // If custom_filter is set, try using it directly (replaces component-based config)
    if let Some(custom) = &config.custom_filter {
        if let Ok(filter) = EnvFilter::try_new(custom) {
            return filter;
        }
        // Fall through to component-based if invalid
        eprintln!("Invalid custom log filter '{custom}', using component-based config");
    }

    let mut filter =
        EnvFilter::from_default_env().add_directive(default_base_level().parse().unwrap());

    let targets = component_targets();
    for (component_id, level) in &config.components {
        if let Some(target_list) = targets.get(component_id.as_str()) {
            for target in *target_list {
                let directive = format!("{target}={level}");
                if let Ok(d) = directive.parse() {
                    filter = filter.add_directive(d);
                }
            }
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

fn save_log_config_to_disk(config: &LogConfig) {
    let Some(dir) = LOG_CONFIG_DIR.get() else {
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

/// Apply a new log config: update in-memory, reload filter, persist to disk.
pub fn apply_log_config(config: &LogConfig) {
    if let Some(rw) = LOG_CONFIG.get() {
        if let Ok(mut guard) = rw.write() {
            *guard = config.clone();
        }
    }
    if let Some(handle) = RELOAD_HANDLE.get() {
        let filter = build_filter_from_config(config);
        let _ = handle.reload(filter);
    }
    save_log_config_to_disk(config);
}

/// Initialize tracing for the main UI process.
///
/// Loads persisted log config from disk to set the initial filter.
/// Outputs go to a rotating log file (`floppa-ui`) plus either stdout
/// (desktop) or logcat (Android).
pub fn init_tracing(log_dir: &Path) {
    let _ = std::fs::create_dir_all(log_dir);
    let _ = LOG_CONFIG_DIR.set(log_dir.to_path_buf());

    let config = load_log_config_from_disk(log_dir);
    let initial_filter = build_filter_from_config(&config);
    let _ = LOG_CONFIG.set(RwLock::new(config));

    let file_layer = build_file_layer(log_dir, "floppa-ui");

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
}

/// Initialize tracing for the Android `:vpn` process (separate from UI).
///
/// Loads persisted log config from disk to set the initial filter.
/// Outputs go to a rotating log file (`floppa-vpn`) plus logcat.
/// Stores a reload handle so log config can be updated via RPC.
#[cfg(target_os = "android")]
pub fn init_tracing_vpn_process(log_dir: &Path) {
    let _ = std::fs::create_dir_all(log_dir);
    let _ = LOG_CONFIG_DIR.set(log_dir.to_path_buf());

    let config = load_log_config_from_disk(log_dir);
    let initial_filter = build_filter_from_config(&config);
    let _ = LOG_CONFIG.set(RwLock::new(config));

    let file_layer = build_file_layer(log_dir, "floppa-vpn");

    let (filter, reload_handle) = reload::Layer::new(initial_filter);
    let _ = RELOAD_HANDLE.set(reload_handle);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(build_logcat_layer())
        .with(file_layer)
        .try_init();
}

// ---------------------------------------------------------------------------
// Shared layer builders
// ---------------------------------------------------------------------------

/// Create a size-rotating file layer (2 MB per file, 1 backup).
fn build_file_layer<S>(log_dir: &Path, prefix: &str) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let file_writer = logroller::LogRollerBuilder::new(log_dir, Path::new(prefix))
        .rotation(logroller::Rotation::SizeBased(logroller::RotationSize::MB(
            2,
        )))
        .max_keep_files(1)
        .build()
        .expect("Failed to create log file writer");

    tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_file(false)
        .with_line_number(false)
        .with_writer(Mutex::new(file_writer))
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
