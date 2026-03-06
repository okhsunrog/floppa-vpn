use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use gotatun::device::{Device, DeviceBuilder, Peer as DevicePeer};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519;
use ipnetwork::IpNetwork;
use std::net::SocketAddr;
use std::process::Command;
use std::str::FromStr;

pub const INTERFACE_NAME: &str = "floppa0";

pub type FloppaDevice = Device<(UdpSocketFactory, TunDevice, TunDevice)>;

/// Parsed WireGuard config.
pub struct WgConfig {
    pub private_key: String,
    pub address: String,
    pub dns: Option<String>,
    pub peer_public_key: String,
    pub peer_preshared_key: Option<String>,
    pub peer_endpoint: String,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u16>,
}

impl WgConfig {
    pub fn from_config_str(config: &str) -> Result<Self> {
        let mut private_key = None;
        let mut address = None;
        let mut dns = None;
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
                        "dns" => dns = Some(value),
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
            private_key: private_key.ok_or_else(|| anyhow!("Missing PrivateKey"))?,
            address: address.ok_or_else(|| anyhow!("Missing Address"))?,
            dns,
            peer_public_key: peer_public_key.ok_or_else(|| anyhow!("Missing Peer PublicKey"))?,
            peer_preshared_key,
            peer_endpoint: peer_endpoint.ok_or_else(|| anyhow!("Missing Peer Endpoint"))?,
            allowed_ips: allowed_ips.unwrap_or_else(|| "0.0.0.0/0, ::/0".to_string()),
            persistent_keepalive,
        })
    }

    fn private_key_bytes(&self) -> Result<[u8; 32]> {
        let bytes = BASE64.decode(&self.private_key)?;
        bytes
            .try_into()
            .map_err(|_| anyhow!("Private key must be 32 bytes"))
    }

    fn peer_public_key_bytes(&self) -> Result<[u8; 32]> {
        let bytes = BASE64.decode(&self.peer_public_key)?;
        bytes
            .try_into()
            .map_err(|_| anyhow!("Public key must be 32 bytes"))
    }

    fn peer_preshared_key_bytes(&self) -> Result<Option<[u8; 32]>> {
        match &self.peer_preshared_key {
            Some(psk) => {
                let bytes = BASE64.decode(psk)?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow!("PSK must be 32 bytes"))?;
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    async fn peer_socket_addr(&self) -> Result<SocketAddr> {
        tokio::net::lookup_host(&self.peer_endpoint)
            .await?
            .next()
            .ok_or_else(|| anyhow!("Endpoint '{}' resolved to no addresses", self.peer_endpoint))
    }

    fn address_network(&self) -> Result<IpNetwork> {
        Ok(IpNetwork::from_str(&self.address)?)
    }

    fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    pub fn dns_servers(&self) -> Vec<String> {
        self.dns
            .as_deref()
            .map(|d| d.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default()
    }
}

fn run_ip(args: &[&str]) -> Result<()> {
    let output = Command::new("ip").args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!("ip {} failed: {}", args.join(" "), stderr.trim()))
    }
}

fn get_default_gateway() -> Result<Option<String>> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()?;
    let route_output = String::from_utf8_lossy(&output.stdout);
    Ok(route_output
        .split_whitespace()
        .skip_while(|&w| w != "via")
        .nth(1)
        .map(|s| s.to_string()))
}

pub async fn configure_networking(config: &WgConfig) -> Result<()> {
    let addr = config.address_network()?;

    run_ip(&["addr", "add", &addr.to_string(), "dev", INTERFACE_NAME])?;
    run_ip(&["link", "set", INTERFACE_NAME, "mtu", "1420"])?;
    run_ip(&["link", "set", INTERFACE_NAME, "up"])?;

    // Add host route for WG endpoint via default gateway to prevent routing loop
    let endpoint = config.peer_socket_addr().await?;
    if let Some(gateway) = get_default_gateway()? {
        let endpoint_route = format!("{}/32", endpoint.ip());
        run_ip(&["route", "add", &endpoint_route, "via", &gateway])?;
        eprintln!("Endpoint route: {} via {}", endpoint_route, gateway);
    }

    // Add routes for allowed IPs
    for network in config.allowed_ips_networks() {
        if network.prefix() == 0 {
            if network.is_ipv4() {
                run_ip(&["route", "add", "0.0.0.0/1", "dev", INTERFACE_NAME])?;
                run_ip(&["route", "add", "128.0.0.0/1", "dev", INTERFACE_NAME])?;
            } else {
                let _ = run_ip(&["route", "add", "::/1", "dev", INTERFACE_NAME]);
                let _ = run_ip(&["route", "add", "8000::/1", "dev", INTERFACE_NAME]);
            }
        } else {
            run_ip(&["route", "add", &network.to_string(), "dev", INTERFACE_NAME])?;
        }
    }

    let ip = addr.ip();
    eprintln!("VPN IP: {ip}");
    eprintln!("Endpoint: {}", config.peer_endpoint);

    Ok(())
}

pub async fn create_tunnel(config: &WgConfig) -> Result<FloppaDevice> {
    let private_key = config.private_key_bytes()?;
    let peer_public_key = config.peer_public_key_bytes()?;
    let preshared_key = config.peer_preshared_key_bytes()?;
    let endpoint = config.peer_socket_addr().await?;
    let allowed_ips = config.allowed_ips_networks();

    let mut tun_config = tun::Configuration::default();
    tun_config.tun_name(INTERFACE_NAME).mtu(1420);
    let tun_device = tun::create_as_async(&tun_config)?;
    let gota_tun = TunDevice::from_tun_device(tun_device)?;

    let public_key = x25519::PublicKey::from(peer_public_key);
    let mut peer = DevicePeer::new(public_key)
        .with_endpoint(endpoint)
        .with_allowed_ips(allowed_ips);

    peer.keepalive = Some(config.persistent_keepalive.unwrap_or(25));

    if let Some(psk) = preshared_key {
        peer = peer.with_preshared_key(psk);
    }

    let device = DeviceBuilder::new()
        .with_default_udp()
        .with_ip(gota_tun)
        .with_private_key(x25519::StaticSecret::from(private_key))
        .with_peer(peer)
        .build()
        .await?;

    Ok(device)
}
