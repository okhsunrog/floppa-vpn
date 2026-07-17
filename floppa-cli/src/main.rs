mod api;
mod auth;
mod dns;
mod reconnect;
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
        Command::Logout => {
            auth::logout()?;
            eprintln!("Logged out.");
        }
    }

    Ok(())
}

async fn connect_wireguard(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let interface = interface.to_string();
    let config_str = config_str.to_string();

    // Shared, rebuildable tunnel state. `Device` is not `Clone` and is torn
    // down via `stop(self)`, so it lives inside a RefCell we swap on rebuild.
    let device: std::rc::Rc<std::cell::RefCell<Option<tunnel::FloppaDevice>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    let rebuild = {
        let device = device.clone();
        let config_str = config_str.clone();
        let interface = interface.clone();
        move || -> reconnect::BoxFutureLocal<Result<()>> {
            let device = device.clone();
            let config_str = config_str.clone();
            let interface = interface.clone();
            Box::pin(async move {
                // Tear down any previous instance before rebuilding.
                let prev = device.borrow_mut().take();
                if let Some(d) = prev {
                    d.stop().await;
                }
                if !no_dns {
                    let _ = dns::restore_dns();
                }

                let wg_config = tunnel::WgConfig::from_config_str(&config_str)?;
                eprintln!("Creating WireGuard tunnel on {interface}...");
                let dev = tunnel::create_tunnel(&wg_config, &interface).await?;
                eprintln!("Configuring networking...");
                tunnel::configure_networking(&wg_config, &interface).await?;
                if !no_dns {
                    dns::set_dns(&wg_config)?;
                }
                *device.borrow_mut() = Some(dev);
                Ok(())
            })
        }
    };

    let health = {
        let device = device.clone();
        let stale_after = reconnect::ReconnectConfig::default().handshake_stale_after;
        move || -> reconnect::BoxFutureLocal<Result<bool>> {
            let device = device.clone();
            Box::pin(async move {
                // Take the device out of the shared cell for the duration of the
                // read so we don't hold a RefCell borrow across an await point.
                let held = device.borrow_mut().take();
                let Some(d) = held else {
                    *device.borrow_mut() = None;
                    return Ok(false);
                };
                // Find the newest handshake across peers; if it is older than the
                // stale threshold (or missing), the tunnel is considered down.
                let result = d
                    .read(async |dr| {
                        dr.peers()
                            .await
                            .iter()
                            .filter_map(|p| p.stats.last_handshake)
                            .max()
                            .map(|hs| {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default();
                                now.saturating_sub(hs) <= stale_after
                            })
                            .unwrap_or(false)
                    })
                    .await;
                *device.borrow_mut() = Some(d);
                Ok(result)
            })
        }
    };

    let signal = reconnect::ReconnectSignal::default();
    let _watcher = reconnect::spawn_resume_watcher(signal.clone());

    // The reconnect loop owns the lifecycle from here; it drives rebuild/health
    // until a shutdown signal arrives. We reuse Ctrl+C / SIGTERM as the abort.
    let shutdown = Box::pin(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = reconnect::run(
        reconnect::ReconnectConfig::default(),
        Box::new(health),
        Box::new(rebuild),
        &signal,
        shutdown,
    )
    .await;

    eprintln!("\nDisconnecting...");
    if !no_dns {
        let _ = dns::restore_dns();
    }
    // Extract the device from the shared cell before awaiting stop().
    let dev = device.borrow_mut().take();
    if let Some(d) = dev {
        d.stop().await;
    }
    result?;
    eprintln!("Disconnected.");
    Ok(())
}

async fn connect_vless(config_str: &str, interface: &str, no_dns: bool) -> Result<()> {
    let interface = interface.to_string();
    let config_str = config_str.trim().to_string();

    let tunnel: std::rc::Rc<std::cell::RefCell<Option<shoes_lite::api::VlessTunnel>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    let rebuild = {
        let tunnel = tunnel.clone();
        let config_str = config_str.clone();
        let interface = interface.clone();
        move || -> reconnect::BoxFutureLocal<Result<()>> {
            let tunnel = tunnel.clone();
            let config_str = config_str.clone();
            let interface = interface.clone();
            Box::pin(async move {
                // Tear down any previous instance before rebuilding.
                let prev = tunnel.borrow_mut().take();
                if let Some(t) = prev {
                    let _ = t.stop().await;
                }
                if !no_dns {
                    let _ = dns::restore_dns();
                }

                let config = vless::parse_uri(config_str.as_str())?;
                eprintln!("Creating VLESS+REALITY tunnel on {interface}...");
                eprintln!("Server: {}", config.server_addr);
                eprintln!("SNI: {}", config.server_name);
                let t = vless::create_tunnel(&config, &interface).await?;
                eprintln!("Configuring networking...");
                vless::configure_networking(&config, &interface).await?;
                if !no_dns && let Some(ref dns) = config.dns {
                    let servers: Vec<String> =
                        dns.split(',').map(|s| s.trim().to_string()).collect();
                    if !servers.is_empty() {
                        dns::write_dns(&servers)?;
                    }
                }
                *tunnel.borrow_mut() = Some(t);
                Ok(())
            })
        }
    };

    let health = {
        let config_str = config_str.clone();
        move || -> reconnect::BoxFutureLocal<Result<bool>> {
            let config_str = config_str.clone();
            Box::pin(async move {
                let cfg = match vless::parse_uri(config_str.as_str()) {
                    Ok(c) => c,
                    Err(_) => return Ok(false),
                };
                let reachable = std::net::TcpStream::connect_timeout(
                    &cfg.server_addr
                        .parse()
                        .unwrap_or_else(|_| "127.0.0.1:443".parse().unwrap()),
                    std::time::Duration::from_secs(3),
                )
                .is_ok();
                Ok(reachable)
            })
        }
    };

    let signal = reconnect::ReconnectSignal::default();
    let _watcher = reconnect::spawn_resume_watcher(signal.clone());

    let shutdown = Box::pin(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    let result = reconnect::run(
        reconnect::ReconnectConfig::default(),
        Box::new(health),
        Box::new(rebuild),
        &signal,
        shutdown,
    )
    .await;

    eprintln!("\nDisconnecting...");
    if !no_dns {
        let _ = dns::restore_dns();
    }
    let tunnel_dev = tunnel.borrow_mut().take();
    if let Some(t) = tunnel_dev {
        let _ = t.stop().await;
    }
    result?;
    eprintln!("Disconnected.");
    Ok(())
}
