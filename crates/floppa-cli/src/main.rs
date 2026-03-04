mod api;
mod auth;
mod dns;
mod tunnel;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[derive(Parser)]
#[command(name = "floppa-cli", about = "CLI client for Floppa VPN")]
struct Cli {
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
    /// Connect to VPN
    Connect {
        /// Use a WireGuard config file instead of fetching from API
        #[arg(long)]
        config: Option<String>,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// List your peers
    Peers {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Fetch and print WireGuard config
    Config {
        /// Peer ID (uses first active peer if omitted)
        #[arg(long)]
        peer_id: Option<i64>,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Remove saved login token
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Login { api_url } => {
            auth::login(&api_url).await?;
        }
        Command::Connect { config, api_url } => {
            let config_str = match config {
                Some(path) => {
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read config file: {path}"))?
                }
                None => {
                    let token = auth::load_token()?.context(
                        "Not logged in. Run `floppa-cli login` first.",
                    )?;
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
                    client.find_or_create_peer().await?
                }
            };

            let wg_config = tunnel::WgConfig::from_config_str(&config_str)?;
            eprintln!("Creating tunnel...");
            let device = tunnel::create_tunnel(&wg_config).await?;
            eprintln!("Configuring networking...");
            tunnel::configure_networking(&wg_config).await?;
            dns::set_dns(&wg_config)?;

            eprintln!("Connected! Press Ctrl+C to disconnect.");
            tokio::signal::ctrl_c().await?;

            eprintln!("\nDisconnecting...");
            dns::restore_dns()?;
            device.stop().await;
            eprintln!("Disconnected.");
        }
        Command::Peers { api_url } => {
            let token = auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let peers = client.list_peers().await?;
            if peers.is_empty() {
                eprintln!("No peers found.");
            } else {
                println!("{:<6} {:<18} {:<14} {}", "ID", "IP", "Status", "Device");
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
        Command::Config { peer_id, api_url } => {
            let token = auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let config = match peer_id {
                Some(id) => client.get_peer_config(id).await?,
                None => client.find_or_create_peer().await?,
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
