mod api;
mod auth;
mod dns;
mod net;
mod rollback;
mod tunnel;
mod vless;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[derive(Parser)]
#[command(name = "floppa-cli", about = "CLI client for Floppa VPN")]
struct Cli {
    /// Write debug logs to a file (e.g. /tmp/floppa-cli.log)
    #[arg(long, global = true)]
    log_file: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Log in via Telegram (opens browser)
    Login {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Connect to VPN (auto-detects WireGuard/AmneziaWG .conf or VLESS URI)
    Connect {
        /// Config file (.conf) or VLESS URI file
        #[arg(long)]
        config: Option<String>,
        /// Tunnel protocol
        #[arg(long, value_enum, default_value_t = api::Protocol::WireGuard)]
        protocol: api::Protocol,
        /// TUN interface name
        #[arg(long, default_value = tunnel::DEFAULT_INTERFACE_NAME)]
        interface: String,
        /// Skip DNS configuration
        #[arg(long)]
        no_dns: bool,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// List your peers
    Peers {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Fetch and print config (WireGuard/AmneziaWG .conf or VLESS URI)
    Config {
        /// Tunnel protocol
        #[arg(long, value_enum, default_value_t = api::Protocol::WireGuard)]
        protocol: api::Protocol,
        /// Peer ID (WireGuard/AmneziaWG only; uses first active peer of that protocol if omitted)
        #[arg(long)]
        peer_id: Option<i64>,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Remove saved login token
    Logout,
}

fn is_vless(config_str: &str) -> bool {
    config_str.trim().starts_with("vless://")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // _guard must live until main() returns to flush the file appender
    let _guard = if let Some(ref log_path) = cli.log_file {
        let path = std::path::Path::new(log_path);
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        let filename = path
            .file_name()
            .context("Invalid log file path")?
            .to_str()
            .context("Invalid log file name")?;
        let file_appender = tracing_appender::rolling::never(dir, filename);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_env_filter(env_filter)
            .init();
        Some(guard)
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(env_filter)
            .init();
        None
    };
    tracing_log::LogTracer::init().ok();

    match cli.command {
        Command::Login { api_url } => {
            auth::login(&api_url).await?;
        }
        Command::Connect {
            config,
            protocol,
            interface,
            no_dns,
            api_url,
        } => {
            let config_str = match config {
                Some(path) => std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read config file: {path}"))?,
                None => {
                    let token = auth::load_token()?
                        .context("Not logged in. Run `floppa-cli login` first.")?;
                    let client = api::ApiClient::new(&api_url, &token);
                    let me = client.get_me().await?;
                    if let Some(ref sub) = me.subscription {
                        eprintln!(
                            "Plan: {} (speed limit: {})",
                            sub.plan_name,
                            sub.speed_limit_mbps
                                .map(|s| format!("{s} Mbps"))
                                .unwrap_or_else(|| "unlimited".into())
                        );
                    } else {
                        bail!("No active subscription");
                    }
                    client.config_for(protocol).await?
                }
            };

            if is_vless(&config_str) {
                connect_vless(&config_str, &interface, no_dns).await?;
            } else {
                connect_wireguard(&config_str, &interface, no_dns).await?;
            }
        }
        Command::Peers { api_url } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let peers = client.list_peers().await?;
            if peers.is_empty() {
                eprintln!("No peers found.");
            } else {
                println!("{:<6} {:<18} {:<14} Device", "ID", "IP", "Status");
                for p in &peers {
                    println!(
                        "{:<6} {:<18} {:<14} {}",
                        p.id,
                        p.assigned_ip,
                        p.sync_status,
                        p.device_name.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Command::Config {
            protocol,
            peer_id,
            api_url,
        } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let config = match (protocol, peer_id) {
                (api::Protocol::WireGuard | api::Protocol::AmneziaWg, Some(id)) => {
                    client.get_peer_config(id).await?
                }
                (api::Protocol::Vless, Some(_)) => bail!("--peer-id does not apply to VLESS"),
                (protocol, None) => client.config_for(protocol).await?,
            };
            print!("{config}");
        }
        Command::Logout => {
            auth::logout()?;
            eprintln!("Logged out.");
        }
    }

    Ok(())
}

/// The running tunnel, whichever protocol backs it.
enum Tunnel {
    WireGuard(tunnel::FloppaDevice),
    Vless(shoes_lite::api::VlessTunnel),
}

impl Tunnel {
    async fn stop(self) -> Result<()> {
        match self {
            Tunnel::WireGuard(device) => {
                device.stop().await;
                Ok(())
            }
            Tunnel::Vless(tunnel) => tunnel
                .stop()
                .await
                .map_err(|e| anyhow::anyhow!("VLESS tunnel stop failed: {e}")),
        }
    }
}

async fn connect_wireguard(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let wg_config = tunnel::WgConfig::from_config_str(config_str)?;
    let endpoint = wg_config.resolve_endpoint().await?;
    eprintln!("Creating WireGuard tunnel on {interface}...");
    let device = tunnel::create_tunnel(&wg_config, endpoint, interface).await?;
    eprintln!("Configuring networking...");
    let addr = tunnel::bring_up_interface(&wg_config, interface)?;
    let mut rollback = rollback::Rollback::new(net::configure_routes(
        endpoint.ip(),
        &wg_config.allowed_ips_networks(),
        interface,
    )?);
    eprintln!("VPN IP: {}", addr.ip());
    eprintln!("Endpoint: {} ({endpoint})", wg_config.peer_endpoint);

    let dns_servers = wg_config.dns_servers();
    if !no_dns && !dns_servers.is_empty() {
        rollback.set_dns(dns::apply(interface, &dns_servers)?);
    }

    wait_then_disconnect(rollback, Tunnel::WireGuard(device)).await
}

async fn connect_vless(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let config = vless::parse_uri(config_str.trim())?;

    eprintln!("Creating VLESS+REALITY tunnel on {interface}...");
    eprintln!("Server: {}", config.server_addr);
    eprintln!("SNI: {}", config.server_name);

    let tunnel = vless::create_tunnel(&config, interface).await?;

    eprintln!("Configuring networking...");
    let endpoint = vless::endpoint_ip(&config).await?;
    let mut rollback = rollback::Rollback::new(net::configure_routes(
        endpoint,
        &vless::allowed_ips_networks(&config),
        interface,
    )?);
    eprintln!("VPN IP: {}", config.address.as_deref().unwrap_or("unknown"));
    eprintln!("Endpoint: {}", config.server_addr);

    if !no_dns && let Some(ref dns) = config.dns {
        let servers: Vec<String> = dns.split(',').map(|s| s.trim().to_string()).collect();
        if !servers.is_empty() {
            rollback.set_dns(dns::apply(interface, &servers)?);
        }
    }

    wait_then_disconnect(rollback, Tunnel::Vless(tunnel)).await
}

/// Announce readiness, block until asked to stop, then tear everything down.
async fn wait_then_disconnect(rollback: rollback::Rollback, tunnel: Tunnel) -> Result<()> {
    println!("READY");
    eprintln!("Connected! Press Ctrl+C to disconnect.");
    let signal = wait_for_shutdown_signal().await?;

    eprintln!("\n{signal} received, disconnecting...");
    disconnect(rollback, tunnel).await?;
    eprintln!("Disconnected.");
    Ok(())
}

/// Block until SIGINT (Ctrl+C), SIGTERM (systemd/docker stop) or SIGHUP arrives.
async fn wait_for_shutdown_signal() -> Result<&'static str> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let name = tokio::select! {
        r = tokio::signal::ctrl_c() => { r?; "SIGINT" }
        _ = terminate.recv() => "SIGTERM",
        _ = hangup.recv() => "SIGHUP",
    };
    Ok(name)
}

/// Undo the connection. The host rollback (DNS, routes) and the tunnel stop are independent:
/// each runs even if the other failed, and the failures are reported together.
async fn disconnect(mut rollback: rollback::Rollback, tunnel: Tunnel) -> Result<()> {
    let mut errors = Vec::new();
    if let Err(e) = rollback.run() {
        errors.push(e);
    }
    if let Err(e) = tunnel.stop().await {
        errors.push(e);
    }
    net::collect_errors(errors)
}
