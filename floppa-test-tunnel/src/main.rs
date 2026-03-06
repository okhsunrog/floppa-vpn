//! Minimal gotatun WireGuard tunnel for integration testing.
//!
//! Reads a standard WireGuard .conf file, creates a TUN device,
//! establishes a gotatun tunnel, configures IP/routes, prints "READY",
//! and waits for SIGTERM/SIGINT.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use gotatun::device::{Device, DeviceBuilder, Peer as DevicePeer};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519;
use ipnetwork::IpNetwork;
use std::net::SocketAddr;
use std::process::Command;
use std::str::FromStr;

const INTERFACE_NAME: &str = "floppa-test0";

type FloppaDevice = Device<(UdpSocketFactory, TunDevice, TunDevice)>;

/// Parsed WireGuard config (mirrors floppa-client's WgConfig).
struct WgConfig {
    private_key: String,
    address: String,
    peer_public_key: String,
    peer_preshared_key: Option<String>,
    peer_endpoint: String,
    allowed_ips: String,
    persistent_keepalive: Option<u16>,
}

impl WgConfig {
    fn from_config_str(config: &str) -> Result<Self, String> {
        let mut private_key = None;
        let mut address = None;
        let mut peer_public_key = None;
        let mut peer_preshared_key = None;
        let mut peer_endpoint = None;
        let mut allowed_ips = None;
        let mut persistent_keepalive = None;

        let mut in_interface = false;
        let mut in_peer = false;

        for line in config.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.eq_ignore_ascii_case("[Interface]") {
                in_interface = true;
                in_peer = false;
                continue;
            }
            if line.eq_ignore_ascii_case("[Peer]") {
                in_interface = false;
                in_peer = true;
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if in_interface {
                    match key.as_str() {
                        "privatekey" => private_key = Some(value),
                        "address" => address = Some(value),
                        _ => {}
                    }
                } else if in_peer {
                    match key.as_str() {
                        "publickey" => peer_public_key = Some(value),
                        "presharedkey" => peer_preshared_key = Some(value),
                        "endpoint" => peer_endpoint = Some(value),
                        "allowedips" => allowed_ips = Some(value),
                        "persistentkeepalive" => persistent_keepalive = value.parse().ok(),
                        _ => {}
                    }
                }
            }
        }

        Ok(Self {
            private_key: private_key.ok_or("Missing PrivateKey")?,
            address: address.ok_or("Missing Address")?,
            peer_public_key: peer_public_key.ok_or("Missing Peer PublicKey")?,
            peer_preshared_key,
            peer_endpoint: peer_endpoint.ok_or("Missing Peer Endpoint")?,
            allowed_ips: allowed_ips.unwrap_or_else(|| "0.0.0.0/0, ::/0".to_string()),
            persistent_keepalive,
        })
    }

    fn private_key_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = BASE64
            .decode(&self.private_key)
            .map_err(|e| format!("Invalid private key base64: {e}"))?;
        bytes
            .try_into()
            .map_err(|_| "Private key must be 32 bytes".to_string())
    }

    fn peer_public_key_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = BASE64
            .decode(&self.peer_public_key)
            .map_err(|e| format!("Invalid public key base64: {e}"))?;
        bytes
            .try_into()
            .map_err(|_| "Public key must be 32 bytes".to_string())
    }

    fn peer_preshared_key_bytes(&self) -> Result<Option<[u8; 32]>, String> {
        match &self.peer_preshared_key {
            Some(psk) => {
                let bytes = BASE64
                    .decode(psk)
                    .map_err(|e| format!("Invalid preshared key base64: {e}"))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| "Preshared key must be 32 bytes".to_string())?;
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    async fn peer_socket_addr(&self) -> Result<SocketAddr, String> {
        tokio::net::lookup_host(&self.peer_endpoint)
            .await
            .map_err(|e| format!("Failed to resolve endpoint '{}': {e}", self.peer_endpoint))?
            .next()
            .ok_or_else(|| format!("Endpoint '{}' resolved to no addresses", self.peer_endpoint))
    }

    fn address_network(&self) -> Result<IpNetwork, String> {
        IpNetwork::from_str(&self.address).map_err(|e| format!("Invalid address: {e}"))
    }

    fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }
}

fn run_ip(args: &[&str]) -> Result<(), String> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run ip {}: {e}", args.join(" ")))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("ip {} failed: {stderr}", args.join(" ")))
    }
}

fn get_default_gateway() -> Result<Option<String>, String> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .map_err(|e| format!("Failed to get default route: {e}"))?;
    let route_output = String::from_utf8_lossy(&output.stdout);
    // Parse "default via 172.17.0.1 dev eth0"
    Ok(route_output
        .split_whitespace()
        .skip_while(|&w| w != "via")
        .nth(1)
        .map(|s| s.to_string()))
}

async fn configure_networking(config: &WgConfig) -> Result<(), String> {
    let addr = config.address_network()?;

    // Add IP address
    run_ip(&["addr", "add", &addr.to_string(), "dev", INTERFACE_NAME])?;

    // Bring interface up
    run_ip(&["link", "set", INTERFACE_NAME, "up"])?;

    // Add host route for WG endpoint via default gateway to prevent routing loop.
    // Without this, the catch-all routes (0.0.0.0/1, 128.0.0.0/1) would capture
    // the WG endpoint UDP traffic itself, creating a loop.
    let endpoint = config.peer_socket_addr().await?;
    if let Some(gateway) = get_default_gateway()? {
        let endpoint_route = format!("{}/32", endpoint.ip());
        run_ip(&["route", "add", &endpoint_route, "via", &gateway])?;
        eprintln!("Added endpoint route: {endpoint_route} via {gateway}");
    }

    // Add routes
    for network in config.allowed_ips_networks() {
        if network.prefix() == 0 {
            if network.is_ipv4() {
                run_ip(&["route", "add", "0.0.0.0/1", "dev", INTERFACE_NAME])?;
                run_ip(&["route", "add", "128.0.0.0/1", "dev", INTERFACE_NAME])?;
            } else {
                run_ip(&["route", "add", "::/1", "dev", INTERFACE_NAME])?;
                run_ip(&["route", "add", "8000::/1", "dev", INTERFACE_NAME])?;
            }
        } else {
            run_ip(&["route", "add", &network.to_string(), "dev", INTERFACE_NAME])?;
        }
    }

    Ok(())
}

async fn create_tunnel(config: &WgConfig) -> Result<FloppaDevice, String> {
    let private_key = config.private_key_bytes()?;
    let peer_public_key = config.peer_public_key_bytes()?;
    let preshared_key = config.peer_preshared_key_bytes()?;
    let endpoint = config.peer_socket_addr().await?;
    let allowed_ips = config.allowed_ips_networks();

    // Create TUN device
    let mut tun_config = tun::Configuration::default();
    tun_config.tun_name(INTERFACE_NAME);
    let tun_device = tun::create_as_async(&tun_config)
        .map_err(|e| format!("Failed to create TUN device: {e}"))?;
    let gota_tun = TunDevice::from_tun_device(tun_device)
        .map_err(|e| format!("Failed to wrap TUN device: {e}"))?;

    // Build peer
    let public_key = x25519::PublicKey::from(peer_public_key);
    let mut peer = DevicePeer::new(public_key)
        .with_endpoint(endpoint)
        .with_allowed_ips(allowed_ips);

    peer.keepalive = Some(config.persistent_keepalive.unwrap_or(25));

    if let Some(psk) = preshared_key {
        peer = peer.with_preshared_key(psk);
    }

    // Build gotatun device with all configuration
    let device = DeviceBuilder::new()
        .with_default_udp()
        .with_ip(gota_tun)
        .with_private_key(x25519::StaticSecret::from(private_key))
        .with_peer(peer)
        .build()
        .await
        .map_err(|e| format!("Failed to build gotatun device: {e}"))?;

    Ok(device)
}

#[tokio::main]
async fn main() {
    let config_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("WG_CONFIG").ok())
        .expect("Usage: floppa-test-tunnel <config.conf> (or set WG_CONFIG env var)");

    let config_str = std::fs::read_to_string(&config_path).expect("Failed to read config file");
    let config = WgConfig::from_config_str(&config_str).expect("Failed to parse config");

    eprintln!("Creating gotatun tunnel on {INTERFACE_NAME}...");
    let device = create_tunnel(&config)
        .await
        .expect("Failed to create tunnel");

    eprintln!("Configuring networking...");
    configure_networking(&config)
        .await
        .expect("Failed to configure networking");

    // Signal readiness — pytest watches for this line on stdout
    println!("READY");

    // Wait for SIGTERM or Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl_c");

    eprintln!("Shutting down tunnel...");
    device.stop().await;
}
