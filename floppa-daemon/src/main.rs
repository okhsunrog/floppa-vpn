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

    // Ensure WireGuard interface exists and matches config
    wg::ensure_interface(wg::WgTool::Wg, &config.wireguard, &secrets.wg_private_key)?;

    // Ensure AmneziaWG interface exists and matches config (if configured)
    if let Some(ref awg) = config.amneziawg {
        let awg_private_key = secrets.awg_private_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("amneziawg configured but awg_private_key secret is missing")
        })?;
        let awg_public_key = secrets.awg_public_key()?;
        wg::ensure_interface(wg::WgTool::Awg, awg, awg_private_key)?;
        info!(public_key = %awg_public_key, "AmneziaWG interface ready");
    }

    // Start Prometheus metrics exporter
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(([127, 0, 0, 1], 9101))
        .install()
        .map_err(|e| anyhow::anyhow!("Failed to start metrics exporter: {e}"))?;
    info!("Metrics exporter listening on 127.0.0.1:9101");

    // Connect to database
    let pool = db::init_pool(&secrets.database_url).await?;
    info!("Connected to database");

    // Run migrations
    db::run_migrations(&pool).await?;
    info!("Migrations complete");

    // Main sync loop with graceful shutdown on SIGTERM/SIGINT
    let config_for_shutdown = config.clone();
    let mut sigterm = signal(SignalKind::terminate())?;
    let outcome = tokio::select! {
        result = sync::run_sync_loop(&pool, &config) => {
            result.inspect_err(|e| error!(error = %e, "Sync loop failed"))
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down");
            Ok(())
        }
        _ = sigterm.recv() => {
            info!("Received SIGTERM, shutting down");
            Ok(())
        }
    };

    // Clean up tc rules on exit (per protocol interface)
    if let Some(ref rate_limit) = config_for_shutdown.wireguard.rate_limit
        && rate_limit.enabled
    {
        info!("Cleaning up traffic control rules");
        if let Err(e) = tc::cleanup_tc(&config_for_shutdown.wireguard.interface) {
            error!(error = %e, "Failed to clean up traffic control");
        }
    }
    if let Some(ref awg) = config_for_shutdown.amneziawg
        && awg.rate_limit.as_ref().map(|r| r.enabled).unwrap_or(false)
        && let Err(e) = tc::cleanup_tc(&awg.interface)
    {
        error!(error = %e, "Failed to clean up AmneziaWG traffic control");
    }

    info!("floppa-daemon stopped");
    // A failed sync loop must exit non-zero: tc rules are gone and nothing is
    // processing pending peers, so systemd has to restart us.
    outcome
}
