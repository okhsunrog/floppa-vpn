mod admin;
mod bot;

use anyhow::Result;
use axum::Router;
use floppa_core::{Config, Secrets, db};
use std::net::SocketAddr;
use teloxide::{prelude::*, types::BotCommand, utils::command::BotCommands};
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

    // Start Prometheus metrics exporter
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(([127, 0, 0, 1], 9102))
        .install()
        .map_err(|e| anyhow::anyhow!("Failed to start metrics exporter: {e}"))?;
    info!("Metrics exporter listening on 127.0.0.1:9102");

    let config = Config::from_env()?;
    let secrets = Secrets::from_env()?;

    let bot_secrets = secrets
        .bot
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Bot secrets missing (bot.token)"))?;

    let pool = db::init_pool(&secrets.database_url).await?;
    info!("Connected to database");

    // Derive WG public key for client configs
    let wg_public_key = secrets.wg_public_key()?;

    // Build teloxide bot (shared between Axum and dispatcher)
    let bot = Bot::new(&bot_secrets.token);
    info!("Bot initialized");

    // Register bot commands so Telegram shows the menu button
    bot.set_my_commands(bot::handlers::Command::bot_commands())
        .await?;
    bot.set_my_commands(vec![
        BotCommand::new("start", "Запустить бота"),
        BotCommand::new("status", "Проверить подписку"),
        BotCommand::new("buy", "Купить тариф"),
        BotCommand::new("vless", "VLESS конфиг"),
        BotCommand::new("lang", "Сменить язык"),
    ])
    .language_code("ru")
    .await?;
    info!("Bot commands registered");

    // Build Axum router
    let api_router = admin::routes::create_router(
        pool.clone(),
        config.clone(),
        secrets.clone(),
        wg_public_key.clone(),
        bot.clone(),
    );

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
            ])
            .allow_credentials(true)
    };

    let app = Router::new()
        .nest("/api", api_router)
        .merge(static_routes)
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Listening on {}", addr);

    // Build teloxide dispatcher
    let handler = bot::handlers::schema();
    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![pool, config, secrets, wg_public_key])
        .enable_ctrlc_handler()
        .build();

    // Run both concurrently
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tokio::select! {
        result = axum::serve(listener, app) => {
            error!("Axum server exited: {:?}", result);
            result?;
        }
        () = dispatcher.dispatch() => {
            error!("Bot dispatcher exited");
        }
    }

    Ok(())
}
