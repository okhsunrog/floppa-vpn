//! GotatunTunnel - WireGuard tunnel using gotatun library

use super::backend::BackendError;
#[cfg(not(target_os = "android"))]
use super::platform::TunParams;
use super::protocol::Protocol;
use crate::vpn::actor::types::RawStats;
use floppa_tunnel_config::{TunnelConfig, device};
use gotatun::device::{Device, DeviceBuilder};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
#[cfg(not(target_os = "android"))]
use shoes_lite::tun::TunServerConfig;
#[cfg(not(target_os = "android"))]
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
#[allow(unused_imports)]
use tracing::{error, info, warn};

/// Fixed Wintun adapter GUID for WireGuard tunnels.
/// Different from VLESS to avoid adapter conflicts, but fixed to prevent
/// Windows "new network detected" popups on every connect.
#[cfg(target_os = "windows")]
const WG_DEVICE_GUID: u128 = 0xF109_9A00_C1EE_40A0_B5EC_DE3A_F109_9A00;

/// Fixed Wintun adapter GUID for VLESS tunnels.
#[cfg(target_os = "windows")]
const VLESS_DEVICE_GUID: u128 = 0xF109_9A00_C1EE_40A0_B5EC_DE3A_F109_9A01;

#[cfg(target_os = "android")]
use std::os::fd::RawFd;

#[cfg(target_os = "android")]
use gotatun::udp::{UdpTransportFactory, UdpTransportFactoryParams};

/// Type alias for our device configuration (desktop)
#[cfg(not(target_os = "android"))]
type FloppaDevice = Device<(UdpSocketFactory, TunDevice, TunDevice)>;

/// Type alias for our device configuration (Android with socket protection)
#[cfg(target_os = "android")]
type FloppaDevice = Device<(AndroidUdpSocketFactory, TunDevice, TunDevice)>;

/// Global socket protection callback for Android
/// This is set by the Tauri plugin and called when sockets need to be protected
#[cfg(target_os = "android")]
static SOCKET_PROTECT_CALLBACK: std::sync::OnceLock<Box<dyn Fn(RawFd) -> bool + Send + Sync>> =
    std::sync::OnceLock::new();

/// Set the socket protection callback (called from Tauri plugin initialization)
#[cfg(target_os = "android")]
pub fn set_socket_protect_callback<F>(callback: F)
where
    F: Fn(RawFd) -> bool + Send + Sync + 'static,
{
    let _ = SOCKET_PROTECT_CALLBACK.set(Box::new(callback));
}

/// Android UDP socket factory that protects sockets from VPN routing
#[cfg(target_os = "android")]
pub struct AndroidUdpSocketFactory;

/// Binds through the standard factory, then hands the descriptor to `VpnService.protect()` so the
/// tunnel's own UDP traffic bypasses the tunnel.
///
/// gotatun 0.9 replaced the separate IPv4 and IPv6 sockets with one dual-stack socket, which is
/// why there is a single descriptor to protect here rather than two. That removes a state this
/// used to be able to reach: one family protected and the other not, which would have routed half
/// the handshake traffic back into the tunnel it was trying to establish.
#[cfg(target_os = "android")]
impl UdpTransportFactory for AndroidUdpSocketFactory {
    type Send = <UdpSocketFactory as UdpTransportFactory>::Send;
    type Recv = <UdpSocketFactory as UdpTransportFactory>::Recv;

    async fn bind(
        &mut self,
        params: &UdpTransportFactoryParams,
    ) -> std::io::Result<(Self::Send, Self::Recv)> {
        // UdpSocketFactory carries buffer-size fields, so it is constructed via ::default()
        // rather than as a unit struct.
        let (udp_tx, udp_rx) = UdpSocketFactory::default().bind(params).await?;

        if let Some(callback) = SOCKET_PROTECT_CALLBACK.get() {
            use std::os::fd::AsFd;
            use std::os::fd::AsRawFd;

            if !callback(udp_tx.as_fd().as_raw_fd()) {
                warn!("Failed to protect the UDP socket");
            }
        } else {
            error!("Socket protect callback not set! VPN may not work correctly.");
        }

        Ok((udp_tx, udp_rx))
    }
}

/// Classify a failure of the engine or its device.
///
/// Walks the error's source chain for an `io::Error`, so a refused privilege — the socket mark
/// without `CAP_NET_ADMIN` — is recognised by its kind. It used to be recognised by the text
/// "Operation not permitted", which a `setlocale` from the GTK side turns into something else.
fn engine_error(what: &str, e: impl std::error::Error + 'static) -> BackendError {
    let detail = format!("{what}: {e}");
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return BackendError::PermissionDenied { detail };
        }
        source = err.source();
    }
    BackendError::Engine { detail }
}

fn invalid_config(detail: impl std::fmt::Display) -> BackendError {
    BackendError::InvalidConfig {
        detail: detail.to_string(),
    }
}

/// GotatunTunnel manages a WireGuard / AmneziaWG tunnel using gotatun
pub struct GotatunTunnel {
    device: Option<FloppaDevice>,
    interface_name: String,
    connected_at: Option<std::time::Instant>,
}

impl GotatunTunnel {
    /// Resolve the endpoint hostname to a `SocketAddr`.
    #[cfg(target_os = "android")]
    async fn resolve_endpoint(config: &TunnelConfig) -> Result<std::net::SocketAddr, BackendError> {
        let endpoint = &config.peer.endpoint;
        tokio::net::lookup_host(endpoint.to_string())
            .await
            .map_err(|e| BackendError::Engine {
                detail: format!("Failed to resolve endpoint '{endpoint}': {e}"),
            })?
            .next()
            .ok_or_else(|| BackendError::Engine {
                detail: format!("Endpoint '{endpoint}' resolved to no addresses"),
            })
    }

    /// Create a new tunnel from a WireGuard or AmneziaWG config (desktop platforms).
    ///
    /// `endpoint` is the pre-resolved server address so the hostname is only resolved once, by the
    /// attempt that is bringing the tunnel up. `tun_params` carries platform-specific
    /// configuration from the platform layer.
    #[cfg(not(target_os = "android"))]
    #[allow(unused_variables, unused_mut)]
    pub async fn new(
        config: &TunnelConfig,
        interface_name: &str,
        tun_params: &TunParams,
        endpoint: std::net::SocketAddr,
    ) -> Result<Self, BackendError> {
        info!("Creating gotatun tunnel on interface {}", interface_name);

        // Create TUN device configuration
        let mut tun_config = tun::Configuration::default();
        tun_config.tun_name(interface_name);

        #[cfg(target_os = "windows")]
        {
            tun_config.metric(1);
            let wintun_file = tun_params.wintun_file.clone();
            tun_config.platform_config(|cfg| {
                cfg.device_guid(WG_DEVICE_GUID);
                if let Some(ref path) = wintun_file {
                    cfg.wintun_file(path);
                }
            });
        }

        #[cfg(target_os = "macos")]
        tun_config.platform_config(|p| {
            p.enable_routing(false);
        });

        // Create the TUN device
        let tun_device = tun::create_as_async(&tun_config)
            .map_err(|e| engine_error("creating the TUN device", e))?;

        // Wrap in gotatun's TunDevice
        let gota_tun = TunDevice::from_tun_device(tun_device)
            .map_err(|e| engine_error("wrapping the TUN device", e))?;

        // Build the device with all configuration
        let mut builder = DeviceBuilder::new()
            .with_udp(UdpSocketFactory::default())
            .with_ip(gota_tun);

        #[cfg(target_os = "linux")]
        if let Some(mark) = tun_params.fwmark {
            builder = builder.with_fwmark(mark);
        }

        // Key, peer, and AmneziaWG obfuscation when the config carries it.
        let builder = device::configure(builder, config, endpoint).map_err(invalid_config)?;

        let device = builder
            .build()
            .await
            .map_err(|e| engine_error("building the tunnel device", e))?;

        info!("Tunnel configured successfully");

        Ok(Self {
            device: Some(device),
            interface_name: interface_name.to_string(),
            connected_at: Some(std::time::Instant::now()),
        })
    }

    /// Create a new tunnel from a raw file descriptor (Android)
    ///
    /// On Android, the VpnService creates the TUN interface and provides us
    /// with the file descriptor. We just wrap it and use it with gotatun.
    #[cfg(target_os = "android")]
    pub async fn from_fd(config: &TunnelConfig, tun_fd: RawFd) -> Result<Self, BackendError> {
        use tun::AbstractDevice;

        info!("Creating gotatun tunnel from fd {}", tun_fd);

        let endpoint = Self::resolve_endpoint(config).await?;

        // Create TUN device from raw fd
        let mut tun_config = tun::Configuration::default();
        tun_config.raw_fd(tun_fd);
        tun_config.close_fd_on_drop(false); // VpnService owns the fd
        tun_config.up();

        // Create the TUN device from existing fd
        let mut tun_device = tun::create_as_async(&tun_config)
            .map_err(|e| engine_error("creating the TUN device from the descriptor", e))?;

        // HACK: the `tun` crate stubs out MTU on Android (it just stores the value).
        // gotatun reads MTU from this, so we need to set it here with the correct value.
        let mtu = config.mtu();
        tun_device
            .set_mtu(mtu)
            .map_err(|e| engine_error("setting the TUN MTU", e))?;
        info!("Set TUN MTU to {}", mtu);

        // Wrap in gotatun's TunDevice
        let gota_tun = TunDevice::from_tun_device(tun_device)
            .map_err(|e| engine_error("wrapping the TUN device", e))?;

        // Build the device with the Android socket factory, then the key, peer, and AmneziaWG
        // obfuscation when the config carries it.
        let builder = DeviceBuilder::new()
            .with_udp(AndroidUdpSocketFactory)
            .with_ip(gota_tun);
        let builder = device::configure(builder, config, endpoint).map_err(invalid_config)?;

        let device = builder
            .build()
            .await
            .map_err(|e| engine_error("building the tunnel device", e))?;

        info!("Tunnel configured successfully");

        Ok(Self {
            device: Some(device),
            interface_name: format!("tun_fd_{}", tun_fd),
            connected_at: Some(std::time::Instant::now()),
        })
    }

    /// Get traffic statistics
    pub async fn get_stats(&self) -> Result<RawStats, String> {
        let device = self.device.as_ref().ok_or("Device not initialized")?;
        let peers = device.peers().await;

        let mut stats = RawStats::default();
        for peer_stats in peers {
            stats.rx_bytes += peer_stats.stats.rx_bytes as u64;
            stats.tx_bytes += peer_stats.stats.tx_bytes as u64;
        }
        Ok(stats)
    }

    /// Get time since last packet was received (seconds ago)
    pub async fn get_last_packet_received(&self) -> Option<i64> {
        let device = self.device.as_ref()?;
        let peers = device.peers().await;

        for peer_stats in peers {
            if let Some(duration) = peer_stats.stats.last_packet_received {
                return Some(duration.as_secs() as i64);
            }
        }
        None
    }

    /// Get connection duration
    pub fn connection_duration(&self) -> Option<Duration> {
        self.connected_at.map(|t| t.elapsed())
    }

    /// Get interface name
    pub fn interface_name(&self) -> &str {
        &self.interface_name
    }

    /// Stop the tunnel
    pub async fn stop(mut self) -> Result<(), BackendError> {
        info!("Stopping gotatun tunnel");
        if let Some(device) = self.device.take() {
            device.stop().await;
            info!("Gotatun tunnel stopped");
        }
        Ok(())
    }
}

impl Drop for GotatunTunnel {
    fn drop(&mut self) {
        if self.device.is_some() {
            error!("GotatunTunnel dropped without calling stop()");
        }
    }
}

/// Active tunnel — wraps either a WireGuard (gotatun) or VLESS tunnel.
enum ActiveTunnel {
    WireGuard(GotatunTunnel),
    Vless(shoes_lite::api::VlessTunnel),
}

impl ActiveTunnel {
    async fn get_stats(&self) -> Option<RawStats> {
        match self {
            Self::WireGuard(t) => t.get_stats().await.ok(),
            Self::Vless(t) => {
                let stats = t.get_stats();
                Some(RawStats {
                    tx_bytes: stats.tx_bytes,
                    rx_bytes: stats.rx_bytes,
                })
            }
        }
    }

    async fn get_last_packet_received(&self) -> Option<i64> {
        match self {
            Self::WireGuard(t) => t.get_last_packet_received().await,
            Self::Vless(t) => t
                .time_since_last_packet_received()
                .map(|d| d.as_secs() as i64),
        }
    }

    fn connection_duration(&self) -> Option<Duration> {
        match self {
            Self::WireGuard(t) => t.connection_duration(),
            Self::Vless(t) => Some(t.connection_duration()),
        }
    }

    async fn ping(&self) -> Result<(), BackendError> {
        match self {
            Self::Vless(t) => t
                .ping(Duration::from_secs(10))
                .await
                .map_err(|detail| BackendError::Engine { detail }),
            Self::WireGuard(_) => Ok(()), // WireGuard has handshake-based health
        }
    }

    async fn stop(self) -> Result<(), BackendError> {
        match self {
            Self::WireGuard(t) => t.stop().await,
            Self::Vless(t) => t
                .stop()
                .await
                .map_err(|detail| BackendError::Engine { detail }),
        }
    }
}

/// What is running, reported by the process that owns it.
///
/// Recorded at start rather than inferred later: the protocol used to be guessed from whichever
/// config the settings happened to name, so an adopted tunnel could confidently report the wrong
/// one after a failed probe cycle had rewritten that setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelMeta {
    pub protocol: Protocol,
    pub endpoint: String,
    pub address: String,
}

/// Tunnel manager that owns the tunnel and provides thread-safe access
pub struct TunnelManager {
    tunnel: RwLock<Option<(ActiveTunnel, TunnelMeta)>>,
}

impl TunnelManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Stop existing tunnel if any (helper for start methods).
    async fn stop_existing(
        tunnel_guard: &mut Option<(ActiveTunnel, TunnelMeta)>,
    ) -> Result<(), BackendError> {
        if let Some((tunnel, _)) = tunnel_guard.take() {
            tunnel.stop().await?;
        }
        Ok(())
    }

    /// Identity of the running tunnel, if there is one.
    pub async fn meta(&self) -> Option<TunnelMeta> {
        self.tunnel.read().await.as_ref().map(|(_, m)| m.clone())
    }

    /// Start a WireGuard / AmneziaWG tunnel on desktop platforms (creates TUN device)
    #[cfg(not(target_os = "android"))]
    pub async fn start_wireguard(
        &self,
        config: &TunnelConfig,
        interface_name: &str,
        tun_params: &TunParams,
        endpoint: std::net::SocketAddr,
    ) -> Result<(), BackendError> {
        let mut tunnel_guard = self.tunnel.write().await;
        Self::stop_existing(&mut tunnel_guard).await?;

        let tunnel = GotatunTunnel::new(config, interface_name, tun_params, endpoint).await?;
        *tunnel_guard = Some((
            ActiveTunnel::WireGuard(tunnel),
            TunnelMeta {
                protocol: wireguard_family(config),
                endpoint: endpoint.to_string(),
                address: config.interface.address.to_string(),
            },
        ));

        Ok(())
    }

    /// Start a WireGuard / AmneziaWG tunnel using a raw file descriptor (Android only)
    #[cfg(target_os = "android")]
    pub async fn start_wireguard_with_fd(
        &self,
        config: &TunnelConfig,
        tun_fd: RawFd,
    ) -> Result<(), BackendError> {
        let mut tunnel_guard = self.tunnel.write().await;
        Self::stop_existing(&mut tunnel_guard).await?;

        let tunnel = GotatunTunnel::from_fd(config, tun_fd).await?;
        *tunnel_guard = Some((
            ActiveTunnel::WireGuard(tunnel),
            TunnelMeta {
                protocol: wireguard_family(config),
                endpoint: config.peer.endpoint.to_string(),
                address: config.interface.address.to_string(),
            },
        ));

        Ok(())
    }

    /// Start VLESS tunnel on desktop platforms
    #[cfg(not(target_os = "android"))]
    #[allow(unused_variables)]
    pub async fn start_vless(
        &self,
        config: &shoes_lite::api::VlessConfig,
        interface_name: &str,
        tun_params: &TunParams,
    ) -> Result<(), BackendError> {
        let mut tunnel_guard = self.tunnel.write().await;
        Self::stop_existing(&mut tunnel_guard).await?;

        let mut tun_config = TunServerConfig::new()
            .tun_name(interface_name.to_string())
            .manage_device(tun_params.manage_device);

        #[cfg(target_os = "windows")]
        {
            tun_config = tun_config.device_guid(VLESS_DEVICE_GUID);
            if let Some(ref path) = tun_params.wintun_file {
                tun_config = tun_config.wintun_file(path);
            }
        }

        if let Some(ref addr_str) = config.address {
            let addr: IpAddr = addr_str
                .parse()
                .map_err(|e| invalid_config(format!("tunnel address '{addr_str}': {e}")))?;
            tun_config = tun_config.address(addr);
        }
        if let Some(mtu) = config.mtu {
            tun_config = tun_config.mtu(mtu);
        }

        let tunnel = shoes_lite::api::VlessTunnel::start(config, tun_config)
            .await
            .map_err(|detail| BackendError::Engine { detail })?;
        *tunnel_guard = Some((ActiveTunnel::Vless(tunnel), vless_meta(config)));
        Ok(())
    }

    /// Start VLESS tunnel using a raw file descriptor (Android/iOS)
    pub async fn start_vless_with_fd(
        &self,
        config: &shoes_lite::api::VlessConfig,
        tun_fd: i32,
    ) -> Result<(), BackendError> {
        info!(
            "Starting VLESS tunnel from fd={}, server={}, sni={}, mtu={:?}",
            tun_fd, config.server_addr, config.server_name, config.mtu
        );
        let mut tunnel_guard = self.tunnel.write().await;
        Self::stop_existing(&mut tunnel_guard).await?;

        let tunnel = shoes_lite::api::VlessTunnel::from_fd(config, tun_fd)
            .await
            .map_err(|detail| BackendError::Engine { detail })?;
        info!("VLESS tunnel started successfully from fd={}", tun_fd);
        *tunnel_guard = Some((ActiveTunnel::Vless(tunnel), vless_meta(config)));
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), BackendError> {
        let mut tunnel_guard = self.tunnel.write().await;
        if let Some((tunnel, _)) = tunnel_guard.take() {
            tunnel.stop().await?;
        }
        Ok(())
    }

    pub async fn get_stats(&self) -> Option<RawStats> {
        let tunnel_guard = self.tunnel.read().await;
        if let Some((tunnel, _)) = tunnel_guard.as_ref() {
            tunnel.get_stats().await
        } else {
            None
        }
    }

    pub async fn get_last_packet_received(&self) -> Option<i64> {
        let tunnel_guard = self.tunnel.read().await;
        if let Some((tunnel, _)) = tunnel_guard.as_ref() {
            tunnel.get_last_packet_received().await
        } else {
            None
        }
    }

    pub async fn get_connection_duration(&self) -> Option<Duration> {
        let tunnel_guard = self.tunnel.read().await;
        tunnel_guard
            .as_ref()
            .and_then(|(t, _)| t.connection_duration())
    }

    pub async fn ping(&self) -> Result<(), BackendError> {
        let tunnel_guard = self.tunnel.read().await;
        match tunnel_guard.as_ref() {
            Some((tunnel, _)) => tunnel.ping().await,
            None => Err(BackendError::NotRunning),
        }
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self {
            tunnel: RwLock::new(None),
        }
    }
}

/// Which of the two gotatun-backed protocols a config is.
fn wireguard_family(config: &TunnelConfig) -> Protocol {
    if config.is_amneziawg() {
        Protocol::AmneziaWg
    } else {
        Protocol::WireGuard
    }
}

fn vless_meta(config: &shoes_lite::api::VlessConfig) -> TunnelMeta {
    TunnelMeta {
        protocol: Protocol::Vless,
        endpoint: config.server_addr.to_string(),
        address: config.address.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrapper shaped like the engine's own errors: the `io::Error` sits one or two sources
    /// deep, never at the top.
    #[derive(Debug, thiserror::Error)]
    enum Wrapped {
        #[error("bind failed")]
        Bind(#[source] std::io::Error),
        #[error("outer")]
        Outer(#[source] Box<Wrapped>),
    }

    #[test]
    fn a_refused_privilege_is_recognised_by_kind_wherever_it_sits_in_the_chain() {
        let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            engine_error("x", Wrapped::Bind(denied())),
            BackendError::PermissionDenied { .. }
        ));
        assert!(matches!(
            engine_error("x", Wrapped::Outer(Box::new(Wrapped::Bind(denied())))),
            BackendError::PermissionDenied { .. }
        ));
    }

    #[test]
    fn any_other_failure_is_an_engine_error_with_the_full_message() {
        let e = engine_error(
            "building",
            Wrapped::Bind(std::io::Error::from(std::io::ErrorKind::AddrInUse)),
        );
        assert_eq!(
            e,
            BackendError::Engine {
                detail: "building: bind failed".into()
            }
        );
    }
}
