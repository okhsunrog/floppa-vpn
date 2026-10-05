mod admin;
mod bot;
mod metrics;

use anyhow::Result;
use axum::Router;
use floppa_core::{Config, Secrets, db};
use std::net::SocketAddr;
use teloxide::prelude::*;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_HASH: &str = env!("GIT_HASH");
pub const BUILD_TIME: &str = env!("BUILD_TIME");

#[tokio::main]
async fn main() -> Result<()> {
    // Dump OpenAPI spec and exit (no DB/config needed)
    if std::env::args().any(|a| a == "--openapi") {
        let openapi = admin::routes::build_openapi();
        println!("{}", openapi.to_pretty_json()?);
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!(
        "Starting floppa-server v{}-{} (built {})",
        VERSION, GIT_HASH, BUILD_TIME
    );

    let api_only = std::env::args().any(|a| a == "--api-only");
    let mut config = Config::from_env()?;
    let mut secrets = Secrets::from_env()?;
    let pool = db::init_pool(&secrets.database_url).await?;
    info!("Connected to database");
    if std::env::args().any(|a| a == "--migrate") {
        db::run_migrations(&pool).await?;
        return Ok(());
    }
    // API-only mode retains real credential auth and all database operations. It neither
    // needs a Telegram token nor starts polling/notifications or advertises Telegram login.
    let bot = if api_only {
        config.bot = None;
        secrets.bot = None;
        None
    } else {
        let bot_secrets = secrets.bot.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Bot secrets missing (bot.token); use --api-only to serve without Telegram"
            )
        })?;
        Some(Bot::new(&bot_secrets.token))
    };

    metrics::install_exporter()
        .map_err(|e| anyhow::anyhow!("Failed to start metrics exporter: {e}"))?;

    // Derive the server public keys for client configs. A configured AmneziaWG section with
    // a bad/missing awg_private_key is a startup error, not a silently disabled protocol.
    let wg_public_key = secrets.wg_public_key()?;
    let awg_public_key = config
        .amneziawg
        .as_ref()
        .map(|_| secrets.awg_public_key())
        .transpose()?;

    let web_app_url = config.bot.as_ref().and_then(|b| b.web_app_url.clone());

    // Build Axum router
    let state = admin::routes::AppState::new(
        pool.clone(),
        config.clone(),
        secrets.clone(),
        wg_public_key.clone(),
        awg_public_key,
        bot.clone(),
    )?;
    let api_router = admin::routes::create_router(state);

    let static_routes = memory_serve::load!()
        .index_file(Some("/index.html"))
        .fallback(Some("/index.html"))
        .into_router();

    let cors = if config.allowed_origins.is_empty() {
        warn!("No allowed_origins configured, using permissive CORS policy");
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = config
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::DELETE,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderName::from_static(admin::routes::CLIENT_VERSION_HEADER),
            ])
            .expose_headers([axum::http::HeaderName::from_static(
                admin::routes::REFRESHED_TOKEN_HEADER,
            )])
            .allow_credentials(true)
    };

    // The request span records the path only: query strings carry Telegram login signatures
    // and one-time codes, which must not end up in the logs.
    let trace = TraceLayer::new_for_http().make_span_with(|req: &axum::extract::Request| {
        tracing::info_span!(
            "request",
            method = %req.method(),
            path = req.uri().path(),
            version = ?req.version(),
        )
    });

    let app = Router::new()
        .nest("/api", api_router)
        .merge(static_routes)
        .layer(trace)
        .layer(cors);

    let addr: SocketAddr = std::env::var("FLOPPA_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:3000".into())
        .parse()?;
    info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let Some(bot) = bot else {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
        return Ok(());
    };

    // Spawn background notification checker (every 30 min)
    bot::notifications::spawn(pool.clone(), bot.clone(), config.clone());
    info!("Notification checker started");

    // Build teloxide dispatcher
    // Handlers answer the user themselves before an error reaches this point (see
    // `bot::handlers::report_errors`); all that is left to do here is log it.
    let handler = bot::handlers::schema();
    let mut dispatcher = Dispatcher::builder(bot.clone(), handler)
        .dependencies(dptree::deps![pool, config, secrets, wg_public_key])
        .error_handler(std::sync::Arc::new(
            |err: bot::handlers::BotError| async move {
                metrics::bot_update(metrics::BotOutcome::Error);
                error!("Bot handler failed: {err}");
            },
        ))
        .enable_ctrlc_handler()
        .build();

    // Run both concurrently
    // Telegram-side cosmetics (command menu, Mini App button) are best-effort and must not
    // hold up serving: register them once we are listening.
    tokio::spawn(bot::configure_menu(bot, web_app_url));

    tokio::select! {
        result = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()) => {
            error!("Axum server exited: {:?}", result);
            result?;
        }
        () = dispatcher.dispatch() => {
            error!("Bot dispatcher exited");
        }
    }

    Ok(())
}
