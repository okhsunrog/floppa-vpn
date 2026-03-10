use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Layer, prelude::*};

static DIAGNOSTIC_MODE: AtomicBool = AtomicBool::new(false);

type FilterHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static RELOAD_HANDLE: std::sync::OnceLock<FilterHandle> = std::sync::OnceLock::new();

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

fn normal_filter() -> EnvFilter {
    #[cfg(debug_assertions)]
    {
        EnvFilter::from_default_env()
            .add_directive("floppa_client_lib=trace".parse().unwrap())
            .add_directive("webview=trace".parse().unwrap())
            .add_directive("tauri=info".parse().unwrap())
            .add_directive("debug".parse().unwrap())
    }

    #[cfg(not(debug_assertions))]
    {
        EnvFilter::from_default_env()
            .add_directive("floppa_client_lib=debug".parse().unwrap())
            .add_directive("shoes_lite=info".parse().unwrap())
            .add_directive("webview=info".parse().unwrap())
            .add_directive("log=info".parse().unwrap())
            .add_directive("gotatun=info".parse().unwrap())
            .add_directive("tarpc=warn".parse().unwrap())
            .add_directive("warn".parse().unwrap())
    }
}

fn diagnostic_filter() -> EnvFilter {
    EnvFilter::from_default_env()
        .add_directive("floppa_client_lib=trace".parse().unwrap())
        .add_directive("shoes_lite=debug".parse().unwrap())
        .add_directive("gotatun=debug".parse().unwrap())
        .add_directive("webview=debug".parse().unwrap())
        .add_directive("log=debug".parse().unwrap())
        .add_directive("tarpc=info".parse().unwrap())
        .add_directive("debug".parse().unwrap())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn set_diagnostic_mode(enabled: bool) {
    DIAGNOSTIC_MODE.store(enabled, Ordering::Relaxed);
    if let Some(handle) = RELOAD_HANDLE.get() {
        let filter = if enabled {
            diagnostic_filter()
        } else {
            normal_filter()
        };
        let _ = handle.reload(filter);
    }
}

pub fn is_diagnostic_mode() -> bool {
    DIAGNOSTIC_MODE.load(Ordering::Relaxed)
}

/// Initialize tracing for the main UI process.
///
/// Outputs go to a rotating log file (`floppa-ui`) plus either stdout
/// (desktop) or logcat (Android).
pub fn init_tracing(log_dir: &Path) {
    let _ = std::fs::create_dir_all(log_dir);

    let file_layer = build_file_layer(log_dir, "floppa-ui");

    let (filter, reload_handle) = reload::Layer::new(normal_filter());
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
/// Outputs go to a rotating log file (`floppa-vpn`) plus logcat.
#[cfg(target_os = "android")]
pub fn init_tracing_vpn_process(log_dir: &Path) {
    let _ = std::fs::create_dir_all(log_dir);

    let file_layer = build_file_layer(log_dir, "floppa-vpn");

    let _ = tracing_subscriber::registry()
        .with(normal_filter())
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

        let short_target = match () {
            _ if !real_target.is_empty() && real_target.starts_with("webview") => "webview",
            _ if !real_target.is_empty() => &real_target,
            _ if target.starts_with("webview") => "webview",
            _ => target,
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
