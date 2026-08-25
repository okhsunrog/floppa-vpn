//! Android platform implementation for VPN operations
//!
//! On Android the VpnService owns everything this trait describes: the TUN interface, its address,
//! its routes and its DNS servers are all set atomically by `VpnService.Builder` in Kotlin, before
//! the Rust side ever receives the file descriptor.
//!
//! So every method here is a no-op — and, importantly, so is every *undo*: the whole configuration
//! is torn down as one unit when the service stops. That is why no Android rollback step is
//! `durable()`: there is nothing that can outlive the process and need recovering at next start.

use super::{DnsSnapshot, Gateway, IpFamily, Platform, PlatformError, TunParams};
use crate::vpn::protocol::InterfaceName;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use tracing::debug;

/// Android platform implementation.
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

    async fn preflight(&self) -> Result<(), PlatformError> {
        // VPN consent is checked by the plugin as part of starting the service, not here.
        Ok(())
    }

    async fn prepare_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        debug!("Android: TUN prepared by VpnService for {iface}");
        Ok(())
    }

    async fn release_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        debug!("Android: TUN released with VpnService for {iface}");
        Ok(())
    }

    async fn configure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        debug!("Android: address {addr} configured by VpnService for {iface}");
        Ok(())
    }

    async fn deconfigure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        debug!("Android: address {addr} released with VpnService for {iface}");
        Ok(())
    }

    async fn default_gateway(&self, _family: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        // The endpoint is protected with VpnService.protect(), not with a host route.
        Ok(None)
    }

    async fn interface_index(&self, _iface: &InterfaceName) -> Option<u32> {
        None
    }

    async fn add_endpoint_route(
        &self,
        endpoint: IpAddr,
        _gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        debug!("Android: endpoint routing handled by VpnService for {endpoint}");
        Ok(())
    }

    async fn remove_endpoint_route(
        &self,
        endpoint: IpAddr,
        _gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        debug!("Android: endpoint routing released with VpnService for {endpoint}");
        Ok(())
    }

    async fn add_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        debug!(
            "Android: {} routes configured by VpnService for {iface}",
            routes.len()
        );
        Ok(())
    }

    async fn remove_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        debug!(
            "Android: {} routes removed with VpnService for {iface}",
            routes.len()
        );
        Ok(())
    }

    async fn capture_dns(
        &self,
        _iface: &InterfaceName,
        _if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        Ok(DnsSnapshot::OwnedByLink)
    }

    async fn configure_dns(
        &self,
        iface: &InterfaceName,
        servers: &[IpAddr],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        debug!(
            "Android: {} DNS servers configured by VpnService for {iface}",
            servers.len()
        );
        Ok(())
    }

    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        _snapshot: &DnsSnapshot,
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        debug!("Android: DNS restored with VpnService for {iface}");
        Ok(())
    }

    async fn ipv6_enabled(&self) -> bool {
        // The VpnService decides which families it accepts; routes are advisory here.
        true
    }
}
