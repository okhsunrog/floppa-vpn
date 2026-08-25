mod auth;
mod connect;
mod protocol;
mod provision;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use floppa_api_client::{ApiClient, ProvisionApi};

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[derive(Parser)]
#[command(name = "floppa-cli", about = "CLI client for Floppa VPN")]
struct Cli {
    /// Write debug logs to a file (e.g. /tmp/floppa-cli.log)
    #[arg(long, global = true)]
    log_file: Option<String>,

    /// Login token file (default: <config dir>/floppa-cli/token; under sudo, the invoking
    /// user's config dir)
    #[arg(long, global = true, env = "FLOPPA_TOKEN_FILE")]
    token_file: Option<std::path::PathBuf>,

    /// Login token, bypassing the token file (prefer the env var over the flag)
    #[arg(long, global = true, env = "FLOPPA_TOKEN", hide_env_values = true)]
    token: Option<String>,

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
        /// Tunnel protocol (AmneziaWG by default, like the app)
        #[arg(long, value_enum, default_value_t = protocol::Protocol::AmneziaWg)]
        protocol: protocol::Protocol,
        /// TUN interface name
        #[arg(long, default_value = floppa_vpn_core::protocol::InterfaceName::DEFAULT)]
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
        /// Tunnel protocol (AmneziaWG by default, like the app)
        #[arg(long, value_enum, default_value_t = protocol::Protocol::AmneziaWg)]
        protocol: protocol::Protocol,
        /// Peer ID (WireGuard/AmneziaWG only; uses first active peer of that protocol if omitted)
        #[arg(long)]
        peer_id: Option<i64>,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Sign out: end this login's session on the server and remove the saved token
    Logout {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
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

    let tokens = auth::TokenSource::new(cli.token, cli.token_file);

    match cli.command {
        Command::Login { api_url } => {
            auth::login(&api_url, &tokens).await?;
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
                    let token = tokens.require()?;
                    let client = ApiClient::new(&api_url, &token)?;
                    let me = client.me().await?;
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
                    provision::config_for(&client, protocol, &auth::device_identity()?).await?
                }
            };

            connect::run(&config_str, &interface, no_dns, &auth::config_dir()?).await?;
        }
        Command::Peers { api_url } => {
            let token = tokens.require()?;
            let client = ApiClient::new(&api_url, &token)?;
            let peers = client.list_peers().await?;
            if peers.is_empty() {
                eprintln!("No peers found.");
            } else {
                println!(
                    "{:<6} {:<18} {:<14} {:<10} Device",
                    "ID", "IP", "Status", "Protocol"
                );
                for p in &peers {
                    println!(
                        "{:<6} {:<18} {:<14} {:<10} {}",
                        p.id,
                        p.assigned_ip,
                        p.sync_status,
                        p.protocol,
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
            let token = tokens.require()?;
            let client = ApiClient::new(&api_url, &token)?;
            let config = match (protocol, peer_id) {
                (protocol::Protocol::WireGuard | protocol::Protocol::AmneziaWg, Some(id)) => {
                    client.peer_config(id).await?
                }
                (protocol::Protocol::Vless, Some(_)) => bail!("--peer-id does not apply to VLESS"),
                (protocol, None) => {
                    provision::config_for(&client, protocol, &auth::device_identity()?).await?
                }
            };
            print!("{config}");
        }
        Command::Logout { api_url } => {
            // Best effort on the server side: a token the server no longer accepts (expired,
            // already signed out elsewhere) is exactly the one that must still go locally.
            if let Some(token) = tokens.load()? {
                match auth::session_id(&token) {
                    Some(session_id) => {
                        match ApiClient::new(&api_url, &token)?
                            .delete_session(session_id)
                            .await
                        {
                            Ok(()) => eprintln!("Session ended on the server."),
                            // Already gone, one way or another: an expired token, or a session
                            // signed out from somewhere else. Exactly the one that must still go
                            // locally.
                            Err(e)
                                if e.is_unauthorized()
                                    || e.refusal().is_some_and(|r| r.status == 404) =>
                            {
                                eprintln!("The server had already ended this session.")
                            }
                            Err(e) => eprintln!("Could not end the session on the server: {e}"),
                        }
                    }
                    None => {
                        eprintln!("Token has no session to end on the server; removing it locally.")
                    }
                }
            }
            tokens.remove()?;
            eprintln!("Logged out.");
        }
    }

    Ok(())
}
