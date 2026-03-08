//! Android platform implementation for VPN operations
//!
//! On Android, the VpnService handles all platform-specific operations:
//! - TUN interface creation
//! - IP address configuration
//! - Routing
//! - DNS configuration
//!
//! These operations are performed in Kotlin code (FloppaVpnService) when
//! starting the VPN via the tauri-plugin-vpn. The Rust side just receives
//! the TUN file descriptor and uses it with gotatun.

use super::{Platform, TunParams};
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use tracing::debug;

/// Android platform implementation
///
/// On Android, most operations are no-ops because VpnService handles
/// address, routing, and DNS configuration automatically.
pub struct AndroidPlatform;

impl AndroidPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AndroidPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Platform for AndroidPlatform {
    fn tun_params(&self) -> TunParams {
        TunParams::default()
    }

    async fn prepare_tun(&self, iface: &str) -> Result<(), String> {
        // On Android, TUN is created by VpnService before Rust gets the fd.
        debug!("Android: TUN prepared by VpnService for {}", iface);
        Ok(())
    }

    async fn configure_address(&self, iface: &str, addr: IpNetwork) -> Result<(), String> {
        // On Android, address is configured by VpnService.Builder.addAddress()
        // in the Kotlin code before we receive the TUN fd
        debug!(
            "Android: address {} configured by VpnService for {}",
            addr, iface
        );
        Ok(())
    }

    async fn add_endpoint_route(&self, endpoint_ip: IpAddr) -> Result<(), String> {
        // On Android, routing is handled by VpnService
        debug!(
            "Android: endpoint routing handled by VpnService for {}",
            endpoint_ip
        );
        Ok(())
    }

    async fn remove_endpoint_route(&self) -> Result<(), String> {
        debug!("Android: endpoint routing handled by VpnService");
        Ok(())
    }

    async fn add_routes(&self, iface: &str, allowed_ips: &[IpNetwork]) -> Result<(), String> {
        // On Android, routes are configured by VpnService.Builder.addRoute()
        // in the Kotlin code before we receive the TUN fd
        debug!(
            "Android: {} routes configured by VpnService for {}",
            allowed_ips.len(),
            iface
        );
        Ok(())
    }

    async fn remove_routes(&self, iface: &str) -> Result<(), String> {
        // Routes are automatically removed when VpnService stops
        debug!("Android: routes removed with VpnService for {}", iface);
        Ok(())
    }

    async fn configure_dns(&self, iface: &str, servers: &[IpAddr]) -> Result<(), String> {
        // On Android, DNS is configured by VpnService.Builder.addDnsServer()
        // in the Kotlin code before we receive the TUN fd
        debug!(
            "Android: {} DNS servers configured by VpnService for {}",
            servers.len(),
            iface
        );
        Ok(())
    }

    async fn restore_dns(&self, iface: &str) -> Result<(), String> {
        // DNS is automatically restored when VpnService stops
        debug!("Android: DNS restored with VpnService for {}", iface);
        Ok(())
    }

    async fn cleanup(&self, iface: &str) -> Result<(), String> {
        // Cleanup is handled by stopping the VpnService
        debug!("Android: cleanup handled by VpnService for {}", iface);
        Ok(())
    }
}
