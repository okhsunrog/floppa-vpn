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

/// Custom formatter that strips noisy URL suffixes from webview targets.
/// e.g. `webview:error@http://localhost:1420/node_modules/...` → `webview`
/// Other targets (floppa_client_lib, keyring, etc.) are left unchanged.
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

        // Plugin-log events have target "log", Tauri's WebView interception
        // has target "webview:LEVEL@http://...". Both → "webview".
        let short_target = if target == "log" || target.starts_with("webview") {
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
