#![allow(dead_code)]

mod auth;
mod config;
mod registry;
mod stats;

use std::sync::Arc;

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};

use shoes_lite::address::{NetLocation, NetLocationMask};
use shoes_lite::client_proxy_selector::{ClientProxySelector, ConnectAction, ConnectRule};
use shoes_lite::reality::{self, RealityServerTarget};
use shoes_lite::resolver::CachingNativeResolver;
use shoes_lite::socket_util::{new_tcp_listener, set_tcp_keepalive};
use shoes_lite::tcp::chain_builder::build_direct_chain_group;
use shoes_lite::tcp::tcp_handler::TcpServerHandler;
use shoes_lite::tcp::tcp_server::process_stream;
use shoes_lite::tls_server_handler::{
    InnerProtocol, TlsServerHandler, TlsServerTarget, VisionVlessConfig,
};

use crate::auth::MultiUserAuthenticator;
use crate::config::{VlessServerConfig, VlessServerSecrets};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,shoes_lite=info".parse().unwrap()),
        )
        .init();

    let config = VlessServerConfig::from_env().context("Failed to load config")?;
    let secrets = VlessServerSecrets::from_env().context("Failed to load secrets")?;

    info!("floppa-vless starting on {}", config.server.listen_addr);

    // Database connection
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&secrets.database_url)
        .await
        .context("Failed to connect to database")?;
    info!("Connected to database");

    // Initialize UUID registry
    let authenticator = Arc::new(MultiUserAuthenticator::new());
    registry::full_sync(&pool, &authenticator)
        .await
        .context("Initial registry sync failed")?;

    // Parse REALITY private key
    let private_key = reality::decode_private_key(&secrets.reality_private_key)
        .map_err(|e| anyhow::anyhow!("Invalid REALITY private key: {e}"))?;

    // Parse short IDs
    let short_ids: Vec<[u8; 8]> = config
        .reality
        .short_ids
        .iter()
        .map(|s| {
            reality::decode_short_id(s)
                .unwrap_or_else(|e| panic!("Invalid REALITY short_id '{s}': {e}"))
        })
        .collect();

    // Create resolver and proxy selector (direct connections to internet)
    let resolver: Arc<dyn shoes_lite::resolver::Resolver> = Arc::new(CachingNativeResolver::new());

    let direct_chain_group = build_direct_chain_group(resolver.clone());
    // NOTE: masks must contain NetLocationMask::ANY to match all destinations.
    // An empty vec![] means "match nothing" — all connections would be silently blocked.
    let allow_all_rule = ConnectRule::new(
        vec![NetLocationMask::ANY],
        ConnectAction::new_allow(None, direct_chain_group),
    );
    let proxy_selector = Arc::new(ClientProxySelector::new(vec![allow_all_rule]));

    // Parse dest location
    let dest = NetLocation::from_str(&config.reality.dest, Some(443))
        .map_err(|e| anyhow::anyhow!("Invalid REALITY dest '{}': {e}", config.reality.dest))?;

    // Build dest client chain (for connecting to camouflage destination)
    let dest_chain = shoes_lite::tcp::chain_builder::build_client_proxy_chain(
        shoes_lite::option_util::OneOrSome::One(shoes_lite::config::ClientChainHop::Single(
            shoes_lite::config::ConfigSelection::Config(shoes_lite::config::ClientConfig::default()),
        )),
        resolver.clone(),
    );

    // Build REALITY server target with Vision+VLESS
    let reality_target = RealityServerTarget {
        private_key,
        short_ids,
        dest: dest.clone(),
        max_time_diff: Some(120_000), // 2 minutes
        min_client_version: None,
        max_client_version: None,
        cipher_suites: reality::DEFAULT_CIPHER_SUITES.to_vec(),
        effective_selector: proxy_selector.clone(),
        inner_protocol: InnerProtocol::VisionVless(VisionVlessConfig {
            authenticator: authenticator.clone(),
            udp_enabled: false,
            fallback: None,
        }),
        dest_client_chain: dest_chain,
    };

    // Build TLS server handler with REALITY target mapped to the configured SNI
    let mut sni_targets = rustc_hash::FxHashMap::default();
    sni_targets.insert(
        config.reality.sni.clone(),
        TlsServerTarget::Reality(reality_target),
    );

    let server_handler: Arc<dyn TcpServerHandler> = Arc::new(TlsServerHandler::new(
        sni_targets,
        None, // no default target — unknown SNI gets rejected
        None, // no TLS buffer size override
        resolver.clone(),
    ));

    // Spawn background tasks

    // 1. LISTEN/NOTIFY for real-time DB sync
    let listen_pool = pool.clone();
    let listen_auth = authenticator.clone();
    let listener_handle = tokio::spawn(async move {
        registry::listen_for_changes(listen_pool, listen_auth).await;
    });

    // 2. Periodic full sync as safety net
    let sync_pool = pool.clone();
    let sync_auth = authenticator.clone();
    let sync_interval = config.traffic.sync_interval_secs;
    let periodic_handle = tokio::spawn(async move {
        registry::periodic_sync_loop(sync_pool, sync_auth, sync_interval).await;
    });

    // 3. Traffic stats flush
    let flush_pool = pool.clone();
    let flush_auth = authenticator.clone();
    let flush_interval = config.traffic.flush_interval_secs;
    let flush_handle = tokio::spawn(async move {
        stats::flush_loop(flush_pool, flush_auth, flush_interval).await;
    });

    // Start TCP listener
    let listen_addr: std::net::SocketAddr = config
        .server
        .listen_addr
        .parse()
        .context("Invalid listen_addr")?;

    let listener = new_tcp_listener(listen_addr, 4096, None)
        .map_err(|e| anyhow::anyhow!("Failed to bind {listen_addr}: {e}"))?;

    info!("Listening on {listen_addr}");

    // Accept loop with graceful shutdown
    let accept_loop = async {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    error!("Accept failed: {e}");
                    continue;
                }
            };

            if let Err(e) = set_tcp_keepalive(
                &stream,
                std::time::Duration::from_secs(300),
                std::time::Duration::from_secs(60),
            ) {
                error!("Failed to set TCP keepalive: {e}");
            }

            let handler = server_handler.clone();
            let resolver = resolver.clone();
            tokio::spawn(async move {
                if let Err(e) = process_stream(stream, handler, resolver).await {
                    tracing::debug!("{addr}: {e}");
                }
            });
        }
    };

    // Run until shutdown signal or a background task panics
    tokio::select! {
        _ = accept_loop => {},
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down...");
        }
        r = listener_handle => {
            error!("LISTEN/NOTIFY task exited unexpectedly: {r:?}");
        }
        r = periodic_handle => {
            error!("Periodic sync task exited unexpectedly: {r:?}");
        }
        r = flush_handle => {
            error!("Traffic flush task exited unexpectedly: {r:?}");
        }
    }

    // Flush remaining traffic stats before exit
    if let Err(e) = stats::flush_traffic(&authenticator, &pool).await {
        error!("Final traffic flush failed: {e:#}");
    }

    info!("floppa-vless stopped");
    Ok(())
}
