mod api;
mod auth;
mod dns;
mod paths;
mod service;
mod stop;
mod tunnel;
mod vless;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[cfg(unix)]
use tokio::signal::unix::SignalKind;

const DEFAULT_API_URL: &str = "https://floppa.okhsunrog.dev/api";

#[derive(Parser)]
#[command(name = "floppa-cli", about = "CLI client for Floppa VPN", version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Write debug logs to a file, for example `floppa-cli.log`
    #[arg(long, global = true)]
    log_file: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Choose login method or prompt interactively.
    Login {
        /// Login method. If omitted, the CLI asks interactively.
        #[arg(long, value_enum)]
        method: Option<auth::LoginMethod>,
        /// Account login for `--method account`. If omitted, it is prompted interactively.
        #[arg(long, env = "FLOPPA_ACCOUNT_LOGIN")]
        login: Option<String>,
        /// Environment variable that contains the password for `--method account`.
        /// If unset, password is prompted hidden.
        #[arg(
            long,
            env = "FLOPPA_ACCOUNT_PASSWORD_ENV",
            default_value = "FLOPPA_ACCOUNT_PASSWORD"
        )]
        password_env: String,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Log in with a Floppa account login + password without method selection.
    LoginAccount {
        /// Account login. If omitted, it is prompted interactively.
        #[arg(long, env = "FLOPPA_ACCOUNT_LOGIN")]
        login: Option<String>,
        /// Environment variable that contains the password. If unset, password is prompted hidden.
        #[arg(
            long,
            env = "FLOPPA_ACCOUNT_PASSWORD_ENV",
            default_value = "FLOPPA_ACCOUNT_PASSWORD"
        )]
        password_env: String,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Connect to VPN (auto-detects WireGuard/AmneziaWG .conf or VLESS URI)
    Connect {
        /// Config file (.conf) or VLESS URI file
        #[arg(long)]
        config: Option<String>,
        /// Protocol: wireguard (default), amneziawg, or vless
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
    /// Manage peers: delete stale or device-specific peers
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Manage VLESS config
    Vless {
        #[command(subcommand)]
        command: VlessCommand,
    },
    /// Manage local CLI device identity
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    /// Fetch and print config (WireGuard/AmneziaWG .conf or VLESS URI)
    Config {
        /// Protocol: wireguard (default), amneziawg, or vless
        #[arg(long, default_value = "wireguard")]
        protocol: String,
        /// Peer ID (WireGuard/AmneziaWG only; uses first active peer of that protocol if omitted)
        #[arg(long)]
        peer_id: Option<i64>,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
    /// Show local tunnel status without contacting the API
    Status {
        /// TUN interface name
        #[arg(long, default_value = tunnel::DEFAULT_INTERFACE_NAME)]
        interface: String,
    },
    /// Safely stop a running floppa-cli connect process
    Stop {
        /// TUN interface name
        #[arg(long, default_value = tunnel::DEFAULT_INTERFACE_NAME)]
        interface: String,
        /// Target a specific floppa-cli connect PID when multiple are running
        #[arg(long)]
        pid: Option<u32>,
        /// Send SIGKILL if graceful SIGTERM stop times out
        #[arg(long)]
        force: bool,
    },
    /// Install and manage a systemd service for the VPN tunnel
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Remove saved login token
    Logout,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Install a systemd unit for `floppa-cli connect`
    Install {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
        /// Absolute path to the floppa-cli binary
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Protocol passed to `connect`
        #[arg(long, default_value = "amneziawg")]
        protocol: String,
        /// TUN interface name
        #[arg(long, default_value = tunnel::DEFAULT_INTERFACE_NAME)]
        interface: String,
        /// Skip DNS configuration
        #[arg(long)]
        no_dns: bool,
        /// API URL passed to `connect`
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
        /// Unix user that should run the service
        #[arg(long, env = "USER")]
        user: Option<String>,
        /// Home directory for the service user
        #[arg(long, env = "HOME")]
        home: Option<PathBuf>,
        /// Absolute path to the service log file
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Remove an installed systemd unit
    Uninstall {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Start the systemd service
    Start {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Stop the systemd service
    Stop {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Restart the systemd service
    Restart {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Show systemd service status
    Status {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Enable the systemd service at boot
    Enable {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
    /// Disable the systemd service at boot
    Disable {
        /// Service scope: system (`sudo systemctl`) or user (`systemctl --user`)
        #[arg(long, value_enum, default_value_t = service::ServiceScope::System)]
        scope: service::ServiceScope,
        /// Service name without `.service`
        #[arg(long, default_value = "floppa-cli")]
        name: String,
    },
}

#[derive(Subcommand)]
enum PeerCommand {
    /// Delete one peer, all peers for this device/protocol, or all peers
    Delete {
        /// Exact peer ID to delete
        #[arg(long)]
        peer_id: Option<i64>,
        /// Delete all active peers for this protocol and this CLI device
        #[arg(long)]
        protocol: Option<String>,
        /// Delete all peers for the current account. Use with care.
        #[arg(long)]
        all: bool,
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
}

#[derive(Subcommand)]
enum VlessCommand {
    /// Regenerate VLESS UUID and print the new URI
    Regenerate {
        #[arg(long, env = "FLOPPA_API_URL", default_value = DEFAULT_API_URL)]
        api_url: String,
    },
}

#[derive(Subcommand)]
enum DeviceCommand {
    /// Print local device_id/device_name
    Show,
    /// Generate a new local device identity
    Reset,
}

fn is_vless(config_str: &str) -> bool {
    config_str.trim().starts_with("vless://")
}

#[tokio::main]
async fn main() -> Result<()> {
    paths::configure_process_path();

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
        Command::Login {
            method,
            login,
            password_env,
            api_url,
        } => {
            auth::login(&api_url, method, login.as_deref(), &password_env).await?;
        }
        Command::Connect {
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
                    if protocol == "vless" {
                        client.get_vless_config().await?
                    } else {
                        client.find_or_create_peer(&protocol).await?
                    }
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
                    "{:<6} {:<18} {:<14} {:<32} Device",
                    "ID", "IP", "Status", "Device ID"
                );
                for p in &peers {
                    println!(
                        "{:<6} {:<18} {:<14} {:<32} {}",
                        p.id,
                        p.assigned_ip,
                        p.sync_status,
                        p.device_id.as_deref().unwrap_or("-"),
                        p.device_name.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Command::Peer {
            command:
                PeerCommand::Delete {
                    peer_id,
                    protocol,
                    all,
                    api_url,
                },
        } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);

            let identity = if protocol.is_some() || all {
                Some(api::get_or_create_device_identity()?)
            } else {
                None
            };
            let peers = if protocol.is_some() || all {
                Some(client.list_peers().await?)
            } else {
                None
            };

            let mut ids = Vec::new();
            if let Some(id) = peer_id {
                ids.push(id);
            } else if let Some(protocol) = protocol {
                let identity = identity.as_ref().expect("identity loaded above");
                ids.extend(
                    peers
                        .as_ref()
                        .expect("peers loaded above")
                        .iter()
                        .filter(|p| {
                            p.protocol == protocol
                                && p.device_id.as_deref() == Some(identity.device_id.as_str())
                        })
                        .map(|p| p.id),
                );
            } else if all {
                ids.extend(
                    peers
                        .as_ref()
                        .expect("peers loaded above")
                        .iter()
                        .map(|p| p.id),
                );
            } else {
                bail!("Provide --peer-id, --protocol, or --all");
            }

            if ids.is_empty() {
                eprintln!("No matching peers found.");
            }
            for id in ids {
                client.delete_peer(id).await?;
                println!("Deleted peer {id}.");
            }
        }
        Command::Vless {
            command: VlessCommand::Regenerate { api_url },
        } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let uri = client.regenerate_vless_config().await?;
            println!("{uri}");
        }
        Command::Device {
            command: DeviceCommand::Show,
        } => {
            let identity = api::get_or_create_device_identity()?;
            println!("{}", serde_json::to_string_pretty(&identity)?);
        }
        Command::Device {
            command: DeviceCommand::Reset,
        } => {
            let identity = api::reset_device_identity()?;
            println!("{}", serde_json::to_string_pretty(&identity)?);
        }
        Command::Config {
            protocol,
            peer_id,
            api_url,
        } => {
            let token =
                auth::load_token()?.context("Not logged in. Run `floppa-cli login` first.")?;
            let client = api::ApiClient::new(&api_url, &token);
            let config = if protocol == "vless" {
                client.get_vless_config().await?
            } else {
                match peer_id {
                    Some(id) => client.get_peer_config(id).await?,
                    None => client.find_or_create_peer(&protocol).await?,
                }
            };
            print!("{config}");
        }
        Command::Status { interface } => {
            tunnel::status(&interface)?;
        }
        Command::Stop {
            interface,
            pid,
            force,
        } => {
            stop::stop(&interface, pid, force)?;
        }
        Command::Service { command } => {
            handle_service_command(command)?;
        }
        Command::Logout => {
            auth::logout()?;
            eprintln!("Logged out.");
        }
    }

    Ok(())
}

fn handle_service_command(command: ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install {
            scope,
            name,
            binary,
            protocol,
            interface,
            no_dns,
            api_url,
            user,
            home,
            log_file,
        } => {
            let home = home.unwrap_or_else(default_home);
            let user = user
                .or_else(|| std::env::var("USER").ok())
                .unwrap_or_default();
            let log_file = log_file.unwrap_or_else(|| {
                home.join(".local")
                    .join("state")
                    .join("floppa-cli")
                    .join("floppa-cli.log")
            });
            let binary = binary.unwrap_or_else(|| {
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("floppa-cli"))
            });
            service::install(&service::ServiceInstallOptions {
                scope,
                name,
                binary,
                protocol,
                interface,
                no_dns,
                api_url,
                user,
                home,
                log_file,
            })
        }
        ServiceCommand::Uninstall { scope, name } => {
            service::uninstall(&service::ServiceUninstallOptions { scope, name })
        }
        ServiceCommand::Start { scope, name } => {
            service::control(&service::ServiceControlOptions {
                scope,
                name,
                action: service::ServiceAction::Start,
            })
        }
        ServiceCommand::Stop { scope, name } => service::control(&service::ServiceControlOptions {
            scope,
            name,
            action: service::ServiceAction::Stop,
        }),
        ServiceCommand::Restart { scope, name } => {
            service::control(&service::ServiceControlOptions {
                scope,
                name,
                action: service::ServiceAction::Restart,
            })
        }
        ServiceCommand::Status { scope, name } => {
            service::control(&service::ServiceControlOptions {
                scope,
                name,
                action: service::ServiceAction::Status,
            })
        }
        ServiceCommand::Enable { scope, name } => {
            service::control(&service::ServiceControlOptions {
                scope,
                name,
                action: service::ServiceAction::Enable,
            })
        }
        ServiceCommand::Disable { scope, name } => {
            service::control(&service::ServiceControlOptions {
                scope,
                name,
                action: service::ServiceAction::Disable,
            })
        }
    }
}

fn default_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

struct CleanupKind {
    dns: bool,
    tunnel: CleanupTunnel,
}

enum CleanupTunnel {
    WireGuard(tunnel::NetworkState),
    Vless(vless::NetworkState),
}

impl CleanupKind {
    fn wireguard(state: tunnel::NetworkState, dns: bool) -> Self {
        Self {
            dns,
            tunnel: CleanupTunnel::WireGuard(state),
        }
    }

    fn vless(state: vless::NetworkState, dns: bool) -> Self {
        Self {
            dns,
            tunnel: CleanupTunnel::Vless(state),
        }
    }

    fn cleanup(&mut self) {
        if self.dns
            && let Err(e) = dns::restore_dns()
        {
            eprintln!("DNS restore failed: {e}");
        }

        match &self.tunnel {
            CleanupTunnel::WireGuard(state) => {
                if let Err(e) = tunnel::cleanup_networking(state) {
                    eprintln!("Tunnel cleanup failed: {e}");
                }
            }
            CleanupTunnel::Vless(state) => {
                if let Err(e) = vless::cleanup_networking(state) {
                    eprintln!("VLESS cleanup failed: {e}");
                }
            }
        }
    }
}

async fn connect_wireguard(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let wg_config = tunnel::WgConfig::from_config_str(config_str)?;
    eprintln!("Creating WireGuard tunnel on {interface}...");
    let device = tunnel::create_tunnel(&wg_config, interface).await?;
    eprintln!("Configuring networking...");
    let network_state = tunnel::configure_networking(&wg_config, interface).await?;
    tunnel::verify_networking(&network_state)?;

    let mut cleanup = CleanupKind::wireguard(network_state, !no_dns);
    if !no_dns {
        dns::set_dns(&wg_config)?;
    }

    println!("READY");
    eprintln!("Connected! Press Ctrl+C or send SIGTERM to disconnect.");
    wait_for_shutdown().await?;

    eprintln!("\nDisconnecting...");
    cleanup.cleanup();
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
    let network_state = vless::configure_networking(&config, interface).await?;
    vless::verify_networking(&network_state)?;

    let mut cleanup = CleanupKind::vless(network_state, !no_dns);
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
    eprintln!("Connected! Press Ctrl+C or send SIGTERM to disconnect.");
    wait_for_shutdown().await?;

    eprintln!("\nDisconnecting...");
    cleanup.cleanup();
    tunnel.stop().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("Disconnected.");
    Ok(())
}

async fn wait_for_shutdown() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
}
