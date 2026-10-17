use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use floppa_foldfin::{
    node::{Node, ProcessConfig},
    tls::TlsIdentity,
};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

#[derive(Parser)]
#[command(about = "Floppa Foldfin node and explicit deployment registration")]
struct Args {
    #[arg(long, default_value = "/etc/floppa-foldfin/config.toml")]
    config: PathBuf,
    #[arg(long, default_value = "/etc/floppa-foldfin/secrets.toml")]
    secrets: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Print networkd and nftables configuration for explicit deployment.
    Network {
        #[arg(long)]
        uplink: String,
        #[arg(long, default_value = "floppa-foldfin")]
        user: String,
    },
    Serve,
    Register,
    Check,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Secrets {
    database_url: String,
}

fn read_secrets(path: &Path) -> Result<Secrets> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).context("cannot open node secrets")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            file.metadata()?.permissions().mode() & 0o027 == 0,
            "node secrets must not be group-writable or accessible to other users"
        );
    }
    let mut content = String::new();
    file.read_to_string(&mut content)
        .context("cannot read node secrets")?;
    // TOML parse diagnostics can contain the offending line, including a password.
    toml::from_str(&content).map_err(|_| anyhow::anyhow!("invalid node secrets TOML"))
}
fn read_identity(config: &ProcessConfig) -> Result<(Vec<u8>, Vec<u8>)> {
    Ok((
        std::fs::read(&config.certificate).context("read TLS certificate")?,
        std::fs::read(&config.private_key).context("read TLS private key")?,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let config: ProcessConfig =
        toml::from_str(&std::fs::read_to_string(&args.config).context("read node config")?)?;
    config.validate()?;
    if let Command::Network { uplink, user } = &args.command {
        print!(
            "{}",
            floppa_foldfin::network::render(&config, user, uplink)?
        );
        return Ok(());
    }
    let (certificate, key) = read_identity(&config)?;
    let identity = Arc::new(TlsIdentity::from_pem(&certificate, &key)?);
    drop(key);
    if matches!(args.command, Command::Check) {
        return Ok(());
    }
    let secrets = read_secrets(&args.secrets)?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&secrets.database_url)
        .await
        .map_err(|_| anyhow::anyhow!("cannot connect to node database"))?;
    if matches!(args.command, Command::Register) {
        floppa_core::foldfin::register_node(&pool, &config.region, &config.public).await?;
        tracing::info!("Foldfin node metadata registered");
        return Ok(());
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("Foldfin node TUN serving currently requires Linux");
    #[cfg(target_os = "linux")]
    {
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .with_http_listener(config.metrics)
            .install()?;
        let device = foldfin::adapters::tun::Tun::open(&config.interface)?.into_device();
        let tcp = tokio::net::TcpListener::bind(config.listen).await?;
        let udp = std::net::UdpSocket::bind(tcp.local_addr()?)?;
        let node = Node::new(
            pool,
            config.region.clone(),
            config.public.clone(),
            Duration::from_secs(config.lease_secs),
            device,
        )?;
        let (stop, mut stopped) = watch::channel(false);
        let running = node.serve(
            identity.server_config(),
            tcp,
            udp,
            Duration::from_secs(config.sync_interval_secs),
            async move {
                let _ = stopped.wait_for(|v| *v).await;
            },
        );
        tokio::pin!(running);
        // Register signal handlers before serving, including systemd ExecReload's SIGHUP.
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut reload = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        loop {
            tokio::select! {
                result = &mut running => return result,
                _ = term.recv() => break,
                _ = interrupt.recv() => break,
                _ = reload.recv() => {
                    match read_identity(&config).and_then(|(cert,key)| Ok(identity.replace_pem(&cert,&key)?)) {
                        Ok(()) => { metrics::counter!("floppa_foldfin_tls_reload_success_total").increment(1); tracing::info!("TLS identity reloaded"); }
                        Err(error) => { metrics::counter!("floppa_foldfin_tls_reload_failure_total").increment(1); tracing::warn!(%error,"TLS reload rejected; previous identity retained"); }
                    }
                }
            }
        }
        stop.send_replace(true);
        running.await
    }
}
