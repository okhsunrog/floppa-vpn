use anyhow::{Result, anyhow};
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
