use anyhow::{Result, anyhow};
use ipnetwork::IpNetwork;
use shoes_lite::api::{VlessConfig, VlessTunnel};
use std::net::IpAddr;

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

/// Resolve the VLESS server address to the IP the endpoint route must pin.
pub async fn endpoint_ip(config: &VlessConfig) -> Result<IpAddr> {
    let endpoint_host = config
        .server_addr
        .split(':')
        .next()
        .unwrap_or(&config.server_addr);
    if let Ok(ip) = endpoint_host.parse::<IpAddr>() {
        return Ok(ip);
    }
    Ok(tokio::net::lookup_host(&config.server_addr)
        .await?
        .next()
        .ok_or_else(|| anyhow!("Cannot resolve {}", config.server_addr))?
        .ip())
}

pub fn allowed_ips_networks(config: &VlessConfig) -> Vec<IpNetwork> {
    config
        .allowed_ips
        .as_deref()
        .unwrap_or("0.0.0.0/0, ::/0")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}
