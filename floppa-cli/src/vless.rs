use crate::net::{NetworkState, get_default_gateway, run_ip};
use anyhow::{Result, anyhow};
use ipnetwork::IpNetwork;
use shoes_lite::api::{VlessConfig, VlessTunnel};

/// Parse a VLESS URI and create a VlessConfig with VPN defaults.
pub fn parse_uri(uri: &str) -> Result<VlessConfig> {
    let mut config = VlessConfig::from_uri(uri).map_err(|e| anyhow!("{e}"))?;

    // Set VPN defaults if not specified in URI
    if config.address.is_none() {
        config.address = Some("10.0.0.2".to_string());
    }
    if config.dns.is_none() {
        config.dns = Some("1.1.1.1".to_string());
    }
    if config.mtu.is_none() {
        config.mtu = Some(1500);
    }
    if config.allowed_ips.is_none() {
        config.allowed_ips = Some("0.0.0.0/0, ::/0".to_string());
    }

    Ok(config)
}

/// Create and start a VLESS+REALITY tunnel.
pub async fn create_tunnel(config: &VlessConfig, interface: &str) -> Result<VlessTunnel> {
    VlessTunnel::new(config, interface)
        .await
        .map_err(|e| anyhow!("{e}"))
}

/// Configure routes for the VLESS tunnel (endpoint bypass + allowed IPs).
pub async fn configure_networking(config: &VlessConfig, interface: &str) -> Result<NetworkState> {
    // Resolve endpoint IP. server_addr is "host:port" which may be an IPv4 literal, an
    // IPv6 bracket-literal ([::1]:443), or a hostname. Parse as SocketAddr first so
    // bracket-literals don't produce a mangled host when split on ':'.
    let endpoint_ip: std::net::IpAddr = match config.server_addr.parse::<std::net::SocketAddr>() {
        Ok(sa) => sa.ip(),
        Err(_) => match config.server_addr.parse::<std::net::IpAddr>() {
            Ok(ip) => ip,
            Err(_) => tokio::net::lookup_host(&config.server_addr)
                .await?
                .next()
                .ok_or_else(|| anyhow!("Cannot resolve {}", config.server_addr))?
                .ip(),
        },
    };

    let endpoint_route = get_default_gateway()?
        .map(|gateway| {
            let prefix = if endpoint_ip.is_ipv4() { 32 } else { 128 };
            let route = format!("{endpoint_ip}/{prefix}");
            run_ip(&["route", "replace", &route, "via", &gateway])?;
            eprintln!("Endpoint route: {route} via {gateway}");
            Ok::<_, anyhow::Error>((route, gateway))
        })
        .transpose()?;

    // Parse allowed IPs and add routes through TUN. Use `replace` for idempotent restarts.
    let allowed_ips_str = config.allowed_ips.as_deref().unwrap_or("0.0.0.0/0, ::/0");
    let networks: Vec<IpNetwork> = allowed_ips_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    for network in &networks {
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

    let addr = config.address.as_deref().unwrap_or("unknown");
    eprintln!("VPN IP: {addr}");
    eprintln!("Endpoint: {}", config.server_addr);

    Ok(NetworkState {
        interface: interface.to_string(),
        endpoint_route: endpoint_route.as_ref().map(|(route, _)| route.clone()),
        endpoint_gateway: endpoint_route.as_ref().map(|(_, gateway)| gateway.clone()),
    })
}
