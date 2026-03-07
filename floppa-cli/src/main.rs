mod api;
mod auth;
mod dns;
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
    /// Connect to VPN (auto-detects WireGuard .conf or VLESS URI)
    Connect {
        /// Config file (.conf) or VLESS URI file
        #[arg(long)]
        config: Option<String>,
        /// Protocol to use when creating a new peer (wireguard or vless)
        #[arg(long, default_value = "wireguard")]
        protocol: String,
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
    /// Fetch and print peer config (WireGuard .conf or VLESS URI)
    Config {
        /// Peer ID (uses first active peer if omitted)
        #[arg(long)]
        peer_id: Option<i64>,
        /// Protocol to use when creating a new peer (wireguard or vless)
        #[arg(long, default_value = "wireguard")]
        protocol: String,
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
                    client.find_or_create_peer(&protocol).await?
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
                println!(
                    "{:<6} {:<10} {:<18} {:<14} Device",
                    "ID", "Protocol", "IP", "Status"
                );
                for p in &peers {
                    println!(
                        "{:<6} {:<10} {:<18} {:<14} {}",
                        p.id,
                        p.protocol,
                        p.assigned_ip.as_deref().unwrap_or("n/a"),
                        p.sync_status,
                        p.device_name.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Command::Config {
            peer_id,
            protocol,
            api_url,
        } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let config = match peer_id {
                Some(id) => client.get_peer_config(id).await?,
                None => client.find_or_create_peer(&protocol).await?,
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

async fn connect_wireguard(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let wg_config = tunnel::WgConfig::from_config_str(config_str)?;
    eprintln!("Creating WireGuard tunnel on {interface}...");
    let device = tunnel::create_tunnel(&wg_config, interface).await?;
    eprintln!("Configuring networking...");
    tunnel::configure_networking(&wg_config, interface).await?;

    if !no_dns {
        dns::set_dns(&wg_config)?;
    }

    println!("READY");
    eprintln!("Connected! Press Ctrl+C to disconnect.");
    tokio::signal::ctrl_c().await?;

    eprintln!("\nDisconnecting...");
    if !no_dns {
        dns::restore_dns()?;
    }
    device.stop().await;
    eprintln!("Disconnected.");
    Ok(())
}

async fn connect_vless(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let config = vless::parse_uri(config_str.trim())?;

    eprintln!("Creating VLESS+REALITY tunnel on {interface}...");
    eprintln!("Server: {}", config.server_addr);
    eprintln!("SNI: {}", config.server_name);

    let tunnel = vless::create_tunnel(&config, interface).await?;

    eprintln!("Configuring networking...");
    vless::configure_networking(&config, interface).await?;

    if !no_dns {
        // Write DNS servers from config
        if let Some(ref dns) = config.dns {
            let servers: Vec<String> = dns.split(',').map(|s| s.trim().to_string()).collect();
            if !servers.is_empty() {
                dns::write_dns(&servers)?;
            }
        }
    }

    println!("READY");
    eprintln!("Connected! Press Ctrl+C to disconnect.");
    tokio::signal::ctrl_c().await?;

    eprintln!("\nDisconnecting...");
    if !no_dns {
        dns::restore_dns()?;
    }
    tunnel.stop().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("Disconnected.");
    Ok(())
}
