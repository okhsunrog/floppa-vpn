mod sync;
mod tc;
mod wg;

use anyhow::Result;
use floppa_core::{Config, Secrets, db};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("floppa-daemon starting");

    // Load configuration
    let config = Config::from_env()?;
    let secrets = Secrets::from_env()?;
    info!(interface = %config.wireguard.interface, "Loaded config");

    // Derive public key from private key
    let wg_public_key = secrets.wg_public_key()?;
    info!(public_key = %wg_public_key, "Derived WireGuard public key");

    // Ensure WireGuard interface exists
    wg::ensure_interface(
        &config.wireguard.interface,
        &secrets.wg_private_key,
        config.wireguard.get_listen_port(),
        &config.wireguard.get_server_ip(),
        &config.wireguard.client_subnet,
    )?;
    info!(
        interface = %config.wireguard.interface,
        port = config.wireguard.get_listen_port(),
        "WireGuard interface ready"
    );

    // Connect to database
    let pool = db::init_pool(&secrets.database_url).await?;
    info!("Connected to database");

    // Run migrations
    db::run_migrations(&pool).await?;
    info!("Migrations complete");

    // Main sync loop with graceful shutdown on SIGTERM/SIGINT
    let config_for_shutdown = config.clone();
    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        result = sync::run_sync_loop(&pool, &config) => {
            if let Err(e) = result {
                error!(error = %e, "Sync loop failed");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down");
        }
        _ = sigterm.recv() => {
            info!("Received SIGTERM, shutting down");
        }
    }

    // Clean up tc rules on exit
    if let Some(ref rate_limit) = config_for_shutdown.wireguard.rate_limit
        && rate_limit.enabled
    {
        info!("Cleaning up traffic control rules");
        if let Err(e) = tc::cleanup_tc(&config_for_shutdown.wireguard.interface) {
            error!(error = %e, "Failed to clean up traffic control");
        }
    }

    info!("floppa-daemon stopped");
    Ok(())
}
