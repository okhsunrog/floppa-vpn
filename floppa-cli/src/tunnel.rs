use crate::net::{NetworkState, get_default_gateway, route_exists, run_ip};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use gotatun::device::{Device, DeviceBuilder, Peer as DevicePeer};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519;
use ipnetwork::IpNetwork;
use std::net::SocketAddr;
use std::str::FromStr;

pub const DEFAULT_INTERFACE_NAME: &str = "floppa0";

pub type FloppaDevice = Device<(UdpSocketFactory, TunDevice, TunDevice)>;

/// AmneziaWG 2.0 obfuscation params parsed from an AmneziaWG `.conf` `[Interface]`.
#[derive(Default)]
pub struct AwgObfuscation {
    pub jc: u32,
    pub jmin: u32,
    pub jmax: u32,
    pub s1: u32,
    pub s2: u32,
    pub s3: u32,
    pub s4: u32,
    pub h1: String,
    pub h2: String,
    pub h3: String,
    pub h4: String,
    pub i1: Option<String>,
    pub i2: Option<String>,
    pub i3: Option<String>,
    pub i4: Option<String>,
    pub i5: Option<String>,
}

/// Parsed WireGuard config. When `obfuscation` is set, it's an AmneziaWG config (same tunnel
/// path, with interface-wide obfuscation applied to the gotatun device).
pub struct WgConfig {
    pub private_key: String,
    pub address: String,
    pub dns: Option<String>,
    pub mtu: Option<u16>,
    pub peer_public_key: String,
    pub peer_preshared_key: Option<String>,
    pub peer_endpoint: String,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u16>,
    pub obfuscation: Option<AwgObfuscation>,
}

impl WgConfig {
    pub fn from_config_str(config: &str) -> Result<Self> {
        let mut private_key = None;
        let mut address = None;
        let mut dns = None;
        let mut mtu = None;
        let mut peer_public_key = None;
        let mut peer_preshared_key = None;
        let mut peer_endpoint = None;
        let mut allowed_ips = None;
        let mut persistent_keepalive = None;

        let mut obf = AwgObfuscation::default();
        let mut has_awg = false;

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
                        "mtu" => mtu = value.parse().ok(),
                        // AmneziaWG obfuscation params
                        "jc" => {
                            obf.jc = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param jc: {value}"))?;
                            has_awg = true;
                        }
                        "jmin" => {
                            obf.jmin = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param jmin: {value}"))?;
                            has_awg = true;
                        }
                        "jmax" => {
                            obf.jmax = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param jmax: {value}"))?;
                            has_awg = true;
                        }
                        "s1" => {
                            obf.s1 = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param s1: {value}"))?;
                            has_awg = true;
                        }
                        "s2" => {
                            obf.s2 = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param s2: {value}"))?;
                            has_awg = true;
                        }
                        "s3" => {
                            obf.s3 = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param s3: {value}"))?;
                            has_awg = true;
                        }
                        "s4" => {
                            obf.s4 = value
                                .parse()
                                .with_context(|| format!("Invalid AWG param s4: {value}"))?;
                            has_awg = true;
                        }
                        "h1" => {
                            obf.h1 = value;
                            has_awg = true;
                        }
                        "h2" => {
                            obf.h2 = value;
                            has_awg = true;
                        }
                        "h3" => {
                            obf.h3 = value;
                            has_awg = true;
                        }
                        "h4" => {
                            obf.h4 = value;
                            has_awg = true;
                        }
                        "i1" => {
                            obf.i1 = (!value.is_empty()).then_some(value);
                            has_awg = true;
                        }
                        "i2" => {
                            obf.i2 = (!value.is_empty()).then_some(value);
                            has_awg = true;
                        }
                        "i3" => {
                            obf.i3 = (!value.is_empty()).then_some(value);
                            has_awg = true;
                        }
                        "i4" => {
                            obf.i4 = (!value.is_empty()).then_some(value);
                            has_awg = true;
                        }
                        "i5" => {
                            obf.i5 = (!value.is_empty()).then_some(value);
                            has_awg = true;
                        }
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

        // AmneziaWG headers default to standard WireGuard (1..4) when only some are present.
        if has_awg {
            if obf.h1.is_empty() {
                obf.h1 = "1".into();
            }
            if obf.h2.is_empty() {
                obf.h2 = "2".into();
            }
            if obf.h3.is_empty() {
                obf.h3 = "3".into();
            }
            if obf.h4.is_empty() {
                obf.h4 = "4".into();
            }
        }

        Ok(Self {
            private_key: private_key.ok_or_else(|| anyhow!("Missing PrivateKey"))?,
            address: address.ok_or_else(|| anyhow!("Missing Address"))?,
            dns,
            mtu,
            peer_public_key: peer_public_key.ok_or_else(|| anyhow!("Missing Peer PublicKey"))?,
            peer_preshared_key,
            peer_endpoint: peer_endpoint.ok_or_else(|| anyhow!("Missing Peer Endpoint"))?,
            allowed_ips: allowed_ips.unwrap_or_else(|| "0.0.0.0/0, ::/0".to_string()),
            persistent_keepalive,
            obfuscation: has_awg.then_some(obf),
        })
    }

    /// MTU to use for the TUN device (AmneziaWG configs ship a lower MTU; default 1420).
    pub fn mtu(&self) -> u16 {
        self.mtu.unwrap_or(1420)
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

/// Build a gotatun `AwgConfig` from parsed obfuscation params.
#[allow(clippy::field_reassign_with_default)]
fn build_gotatun_awg(obf: &AwgObfuscation) -> Result<gotatun::noise::awg::AwgConfig> {
    use gotatun::noise::awg::{AwgConfig as GotaAwg, MagicHeader, ObfChain};

    let parse_h =
        |s: &str| MagicHeader::parse(s).map_err(|e| anyhow!("Invalid AWG header '{s}': {e}"));
    let parse_i = |o: &Option<String>| -> Result<Option<ObfChain>> {
        match o {
            Some(spec) => ObfChain::parse(spec)
                .map(Some)
                .map_err(|e| anyhow!("Invalid AWG signature packet '{spec}': {e}")),
            None => Ok(None),
        }
    };

    let mut a = GotaAwg::default();
    a.jc = obf.jc as usize;
    a.jmin = obf.jmin as usize;
    a.jmax = obf.jmax as usize;
    a.s1 = obf.s1 as usize;
    a.s2 = obf.s2 as usize;
    a.s3 = obf.s3 as usize;
    a.s4 = obf.s4 as usize;
    a.h1 = parse_h(&obf.h1)?;
    a.h2 = parse_h(&obf.h2)?;
    a.h3 = parse_h(&obf.h3)?;
    a.h4 = parse_h(&obf.h4)?;
    a.i_packets = [
        parse_i(&obf.i1)?,
        parse_i(&obf.i2)?,
        parse_i(&obf.i3)?,
        parse_i(&obf.i4)?,
        parse_i(&obf.i5)?,
    ];
    Ok(a)
}

pub async fn configure_networking(config: &WgConfig, interface: &str) -> Result<NetworkState> {
    let addr = config.address_network()?;

    run_ip(&["addr", "add", &addr.to_string(), "dev", interface])?;
    run_ip(&["link", "set", interface, "mtu", &config.mtu().to_string()])?;
    run_ip(&["link", "set", interface, "up"])?;

    // Add host route for WG endpoint via default gateway to prevent routing loop.
    // Use `replace` so repeated starts after a crash are idempotent.
    let endpoint = config.peer_socket_addr().await?;
    let endpoint_route = get_default_gateway()?
        .map(|gateway| {
            let ip = endpoint.ip();
            let prefix = if ip.is_ipv4() { 32 } else { 128 };
            let route = format!("{ip}/{prefix}");
            run_ip(&["route", "replace", &route, "via", &gateway])?;
            eprintln!("Endpoint route: {route} via {gateway}");
            Ok::<_, anyhow::Error>((route, gateway))
        })
        .transpose()?;

    // Add routes for allowed IPs. Use `replace` for idempotent restarts.
    for network in config.allowed_ips_networks() {
        if network.prefix() == 0 {
            if network.is_ipv4() {
                run_ip(&["route", "replace", "0.0.0.0/1", "dev", interface])?;
                run_ip(&["route", "replace", "128.0.0.0/1", "dev", interface])?;
            } else {
                if let Err(e) = run_ip(&["route", "replace", "::/1", "dev", interface]) {
                    eprintln!("Skipping IPv6 VPN route ::/1: {e}");
                }
                if let Err(e) = run_ip(&["route", "replace", "8000::/1", "dev", interface]) {
                    eprintln!("Skipping IPv6 VPN route 8000::/1: {e}");
                }
            }
        } else {
            run_ip(&["route", "replace", &network.to_string(), "dev", interface])?;
        }
    }

    let ip = addr.ip();
    eprintln!("VPN IP: {ip}");
    eprintln!("Endpoint: {}", config.peer_endpoint);

    Ok(NetworkState {
        interface: interface.to_string(),
        endpoint_route: endpoint_route.as_ref().map(|(route, _)| route.clone()),
        endpoint_gateway: endpoint_route.as_ref().map(|(_, gateway)| gateway.clone()),
    })
}

pub fn status(interface: &str) -> Result<()> {
    if !interface_exists(interface) {
        bail!("Floppa {interface}: not connected");
    }
    if !route_exists(&["route", "show", "0.0.0.0/1"])
        || !route_exists(&["route", "show", "128.0.0.0/1"])
    {
        bail!("Floppa {interface}: interface exists, but VPN routes are missing");
    }
    println!("Floppa {interface}: connected");
    Ok(())
}

pub fn interface_exists(interface: &str) -> bool {
    route_exists(&["link", "show", interface])
}

pub async fn create_tunnel(config: &WgConfig, interface: &str) -> Result<FloppaDevice> {
    let private_key = config.private_key_bytes()?;
    let peer_public_key = config.peer_public_key_bytes()?;
    let preshared_key = config.peer_preshared_key_bytes()?;
    let endpoint = config.peer_socket_addr().await?;
    let allowed_ips = config.allowed_ips_networks();

    let mut tun_config = tun::Configuration::default();
    tun_config.tun_name(interface).mtu(config.mtu());
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

    let mut builder = DeviceBuilder::new()
        .with_default_udp()
        .with_ip(gota_tun)
        .with_private_key(x25519::StaticSecret::from(private_key))
        .with_peer(peer);

    // AmneziaWG: apply interface-wide obfuscation. Absent → plain WireGuard.
    if let Some(obf) = &config.obfuscation {
        builder = builder.with_awg(build_gotatun_awg(obf)?);
    }

    let device = builder.build().await?;

    Ok(device)
}
