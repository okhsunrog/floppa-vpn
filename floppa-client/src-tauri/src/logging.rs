use tracing_subscriber::{prelude::*, EnvFilter};

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
            .event_format(ShortTargetFormat)
            .with_writer(logcat_writer);

        tracing_subscriber::registry()
            .with(filter)
            .with(logcat_layer)
            .init();
    }

    #[cfg(not(target_os = "android"))]
    {
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_ansi(true)
            .with_writer(std::io::stdout);

        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .init();
    }
}

/// Custom formatter that truncates webview targets to just "webview".
/// e.g. `webview:error@http://localhost:1420/node_modules/...` → `webview`
struct ShortTargetFormat;

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

        // Shorten webview targets: "webview:error@http://..." → "webview"
        let short_target = if target.starts_with("webview") {
            "webview"
        } else {
            target
        };

        write!(writer, " {level} {short_target}: ")?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}
