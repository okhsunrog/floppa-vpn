use tracing_subscriber::{EnvFilter, prelude::*};

pub fn init_tracing() {
    let filter = EnvFilter::from_default_env();

    #[cfg(debug_assertions)]
    let filter = filter
        .add_directive("floppa_client_lib=trace".parse().unwrap())
        .add_directive("webview=trace".parse().unwrap())
        .add_directive("tauri=info".parse().unwrap())
        .add_directive("debug".parse().unwrap());

    #[cfg(not(debug_assertions))]
    let filter = filter
        .add_directive("floppa_client_lib=debug".parse().unwrap())
        .add_directive("webview=info".parse().unwrap())
        .add_directive("log=info".parse().unwrap())
        .add_directive("gotatun=info".parse().unwrap())
        .add_directive("tarpc=warn".parse().unwrap())
        .add_directive("warn".parse().unwrap());

    #[cfg(target_os = "android")]
    {
        use tracing_logcat::{LogcatMakeWriter, LogcatTag};
        let tag = LogcatTag::Fixed("FloppaVPN".to_owned());
        let logcat_writer = LogcatMakeWriter::new(tag).expect("Failed to init logcat writer");

        let logcat_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_file(false)
            .with_line_number(false)
            .with_writer(logcat_writer);

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(logcat_layer)
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
            .try_init();
    }
}

/// Custom formatter that resolves the real source for log-crate events.
///
/// `tracing-log` normalizes all `log` crate events to target `log` and stores
/// the original target in a `log.target` field. We extract that field to show
/// the actual source (e.g. `shoes_lite::crypto`, `keyring`).
///
/// Tauri's WebView interception produces targets like
/// `webview:error@http://localhost:1420/...` — we strip the URL suffix.
///
/// Only needed on desktop — on Android the target is already short.
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
        let level = meta.level();
        let target = meta.target();

        // For log-crate events (target "log"), extract the real source from
        // the log.target field that tracing-log stores.
        let mut real_target = String::new();
        if target == "log" {
            let mut visitor = LogTargetVisitor::default();
            event.record(&mut visitor);
            if let Some(t) = visitor.log_target {
                real_target = t;
            }
        }

        let short_target = if !real_target.is_empty() {
            // Shorten webview targets from log.target too
            if real_target.starts_with("webview") {
                "webview"
            } else {
                &real_target
            }
        } else if target.starts_with("webview") {
            // Tauri WebView interception: "webview:LEVEL@URL" → "webview"
            "webview"
        } else {
            target
        };

        // ANSI colors for log levels
        let (color, level_str) = match *level {
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
