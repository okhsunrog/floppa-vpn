use anyhow::{Result, anyhow};
use floppa_tunnel_config::conf::{Endpoint, comma_list};
use floppa_tunnel_config::{route, vless};
use ipnetwork::IpNetwork;
use shoes_lite::api::{VlessConfig, VlessTunnel};
use std::net::IpAddr;

/// Parse a VLESS URI and create a VlessConfig with VPN defaults.
pub fn parse_uri(uri: &str) -> Result<VlessConfig> {
    let mut config = VlessConfig::from_uri(uri).map_err(|e| anyhow!("{e}"))?;

    // Set VPN defaults if not specified in URI
    if config.address.is_none() {
        config.address = Some(vless::ADDRESS.to_string());
    }
    if config.dns.is_none() {
        config.dns = Some(vless::DNS.to_string());
    }
    if config.mtu.is_none() {
        config.mtu = Some(vless::MTU);
    }
    if config.allowed_ips.is_none() {
        config.allowed_ips = Some(comma_list(route::CATCH_ALL));
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
    let endpoint: Endpoint = config
        .server_addr
        .parse()
        .map_err(|e| anyhow!("Invalid server address '{}': {e}", config.server_addr))?;
    if let Some(ip) = endpoint.ip() {
        return Ok(ip);
    }
    let addrs = tokio::net::lookup_host(endpoint.to_string())
        .await
        .map_err(|e| anyhow!("Failed to resolve {endpoint}: {e}"))?;
    Ok(route::pick_endpoint(addrs)
        .ok_or_else(|| anyhow!("{endpoint} resolved to no addresses"))?
        .ip())
}

pub fn allowed_ips_networks(config: &VlessConfig) -> Result<Vec<IpNetwork>> {
    let Some(allowed_ips) = config.allowed_ips.as_deref() else {
        return Ok(route::CATCH_ALL.to_vec());
    };
    allowed_ips
        .split(',')
        .map(|s| {
            let s = s.trim();
            s.parse()
                .map_err(|_| anyhow!("Invalid allowed IPs entry '{s}'"))
        })
        .collect()
}
