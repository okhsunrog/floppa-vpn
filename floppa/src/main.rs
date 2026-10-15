mod auth;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod client;
mod connect;
mod protocol;
mod provision;
#[cfg(target_os = "linux")]
mod service;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use floppa_api_client::{ApiClient, ProvisionApi};
use floppa_vpn_core::protocol::Protocol;

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[derive(Parser)]
#[command(name = "floppa", about = "Command-line client for Floppa VPN")]
struct Cli {
    /// Write debug logs to a file (e.g. /tmp/floppa.log)
    #[arg(long, global = true)]
    log_file: Option<String>,

    /// Login token file (default: <config dir>/floppa/token; under sudo, the invoking
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
    /// Hold the tunnel as a system service (root; normally started by systemd, not by hand)
    ///
    /// The tunnel then outlives every client: closing the app, logging out, or never logging in
    /// stops taking it down, and `systemctl enable` brings it back after a reboot.
    #[cfg(target_os = "linux")]
    Service,
    /// Read or replace preferences through the selected tunnel owner
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Settings {
        #[command(subcommand)]
        action: SettingsAction,
    },
    /// Log in via Telegram (opens browser)
    Login {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Connect to VPN (auto-detects WireGuard/AmneziaWG .conf or VLESS URI)
    ///
    /// Uses the system service when available, otherwise holds a standalone tunnel until Ctrl+C.
    /// --config is temporary input to the selected owner; it does not change ownership.
    Connect {
        /// JSON file containing {"excluded_routes": ["194.186.63.0/24"]}
        #[arg(long)]
        routes: Option<std::path::PathBuf>,
        /// Temporary config file (.conf) or VLESS URI file; not saved for future connections
        #[arg(long)]
        config: Option<String>,
        /// Tunnel protocol (AmneziaWG by default, like the app)
        #[arg(long, default_value = "amneziawg", value_parser = protocol::parser())]
        protocol: Protocol,
        /// TUN interface name
        #[arg(long, default_value = floppa_vpn_core::protocol::InterfaceName::DEFAULT)]
        interface: String,
        /// Skip DNS configuration
        #[arg(long)]
        no_dns: bool,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Take the tunnel the system service is holding down
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Disconnect,
    /// Bring back whatever tunnel the system service last had up
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Resume {
        /// Do nothing unless `floppa autostart on` was set. What the boot unit passes.
        #[arg(long)]
        if_enabled: bool,
    },
    /// Whether to reconnect the tunnel on boot; with no argument, prints the current setting
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Autostart {
        /// `on` or `off`
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
    /// Persist a config in the selected owner without connecting.
    /// Unlike temporary `connect --config`, this saves credentials for future connections.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Import {
        /// Config file (.conf) or VLESS URI file
        config: String,
    },
    /// What the system service says the tunnel is doing
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Status,
    /// List your peers
    Peers {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Fetch and print config (WireGuard/AmneziaWG .conf or VLESS URI)
    Config {
        /// Tunnel protocol (AmneziaWG by default, like the app)
        #[arg(long, default_value = "amneziawg", value_parser = protocol::parser())]
        protocol: Protocol,
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

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[derive(Subcommand)]
enum SettingsAction {
    Show,
    /// Set a dotted settings field to a JSON value (bare strings are accepted)
    Set {
        field: String,
        value: String,
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
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Settings { action } => {
            let owner = client::reach().await?;
            let state = owner.get_settings().await?;
            match action {
                SettingsAction::Show => println!("{}", serde_json::to_string_pretty(&state)?),
                SettingsAction::Set { field, value } => {
                    let mut json = serde_json::to_value(&state.document.settings)?;
                    let mut current = &mut json;
                    let parts: Vec<_> = field.split('.').collect();
                    for part in parts {
                        current = current
                            .get_mut(part)
                            .with_context(|| format!("unknown settings field: {field}"))?;
                    }
                    *current =
                        serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
                    let settings = serde_json::from_value(json)?;
                    let saved = owner
                        .update_settings(state.document.revision, settings)
                        .await?;
                    println!(
                        "Saved revision {}. Connection changes apply on the next connect.",
                        saved.document.revision
                    );
                }
            }
        }
        #[cfg(target_os = "linux")]
        Command::Service => {
            service::run().await?;
        }
        Command::Login { api_url } => {
            auth::login(&api_url, &tokens).await?;
        }
        Command::Connect {
            routes,
            config,
            protocol,
            interface,
            no_dns,
            api_url,
        } => {
            let excluded_routes = routes
                .as_ref()
                .map(|path| -> Result<_> {
                    let config: floppa_vpn_core::routing::RoutingConfig =
                        serde_json::from_slice(&std::fs::read(path)?)?;
                    floppa_vpn_core::routing::validate_exclusions(&config.excluded_routes)
                        .map_err(anyhow::Error::msg)
                })
                .transpose()?;
            // Select ownership independently of credentials. An explicit config is temporary
            // input to that same owner, not an implicit request to bypass the service.
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            let owner = client::reach_for_connect(&interface, no_dns).await?;
            if let Some(path) = config {
                let config_str = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read config file: {path}"))?;
                #[cfg(any(target_os = "linux", target_os = "windows"))]
                {
                    if routes.is_some() {
                        bail!("use `floppa settings` for routing preferences");
                    }
                    return client::connect_temporary(&owner, config_str).await;
                }
                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                connect::run(
                    &config_str,
                    &interface,
                    no_dns,
                    &auth::config_dir()?,
                    Vec::new(),
                    excluded_routes.unwrap_or_default(),
                )
                .await?;
                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                return Ok(());
            }

            // A service that already holds a config and a shell with no login is a real
            // combination — `floppa import` puts a `.conf` there, and nothing about running a
            // WireGuard tunnel requires a Floppa account. Sending that person to the server for a
            // config they have would fail for a reason that is not their problem.
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            if tokens.load()?.is_none() {
                let remote = &owner;
                if client::has_config(remote).await {
                    return client::connect_stored(remote, excluded_routes).await;
                }
                return Err(anyhow::anyhow!(
                    "Not logged in, and the tunnel service holds no config.\n\
                     Run `floppa login`, or hand it one with `floppa import <file>`."
                ));
            }

            let token = tokens.require()?;
            let api = ApiClient::new(&api_url, &token)?;
            let me = api.me().await?;
            let Some(sub) = me.subscription.as_ref() else {
                bail!("No active subscription");
            };
            eprintln!(
                "Plan: {} (speed limit: {})",
                sub.plan_name,
                sub.speed_limit_mbps
                    .map(|s| format!("{s} Mbps"))
                    .unwrap_or_else(|| "unlimited".into())
            );
            let identity = auth::device_identity()?;
            let config_str = provision::config_for(&api, protocol, &identity).await?;
            // Routes only: the server enforces the grant either way, so failing to ask costs this
            // run the private networks and nothing else.
            let private_routes = match api.private_routes().await {
                Ok(routes) => {
                    if !routes.is_empty() {
                        eprintln!("Private networks: {}", routes.join(", "));
                    }
                    routes
                }
                Err(e) => {
                    eprintln!("Could not fetch private networks ({e}); connecting without them");
                    Vec::new()
                }
            };

            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                let remote = &owner;
                // The service is what will need to reach the server later, with nobody at this
                // terminal, to replace a peer that has been deleted. Handing the session over is
                // how it can: the credentials are this user's and the store is root's.
                client::seed_session(remote, &api_url, &token, &identity).await;
                client::connect(remote, &config_str, private_routes, excluded_routes).await?;
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            connect::run(
                &config_str,
                &interface,
                no_dns,
                &auth::config_dir()?,
                private_routes,
                excluded_routes.unwrap_or_default(),
            )
            .await?;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Disconnect => {
            let remote = client::reach().await?;
            client::disconnect(&remote).await?;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Resume { if_enabled } => {
            let remote = client::reach().await?;
            client::resume(&remote, if_enabled).await?;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Autostart { state } => {
            let remote = client::reach().await?;
            client::autostart(&remote, state.map(|s| s == "on")).await?;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Import { config } => {
            let raw = std::fs::read_to_string(&config)
                .with_context(|| format!("Failed to read config file: {config}"))?;
            let remote = client::reach().await?;
            client::import(&remote, &raw).await?;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        Command::Status => {
            let remote = client::reach().await?;
            client::status(&remote).await?;
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
                (Protocol::WireGuard | Protocol::AmneziaWg, Some(id)) => {
                    client.peer_config(id).await?
                }
                (Protocol::Vless, Some(_)) => bail!("--peer-id does not apply to VLESS"),
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
