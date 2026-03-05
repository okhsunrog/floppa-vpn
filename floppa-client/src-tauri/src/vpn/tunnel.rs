//! GotatunTunnel - WireGuard tunnel using gotatun library

use super::state::{TrafficStats, WgConfig};
use gotatun::device::{Device, DeviceBuilder, Peer as DevicePeer};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
#[allow(unused_imports)]
use tracing::{error, info, warn};

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

#[cfg(target_os = "android")]
impl UdpTransportFactory for AndroidUdpSocketFactory {
    type SendV4 = <UdpSocketFactory as UdpTransportFactory>::SendV4;
    type SendV6 = <UdpSocketFactory as UdpTransportFactory>::SendV6;
    type RecvV4 = <UdpSocketFactory as UdpTransportFactory>::RecvV4;
    type RecvV6 = <UdpSocketFactory as UdpTransportFactory>::RecvV6;

    async fn bind(
        &mut self,
        params: &UdpTransportFactoryParams,
    ) -> std::io::Result<((Self::SendV4, Self::RecvV4), (Self::SendV6, Self::RecvV6))> {
        // First, create sockets using the standard factory
        let ((udp_v4_tx, udp_v4_rx), (udp_v6_tx, udp_v6_rx)) =
            UdpSocketFactory.bind(params).await?;

        // Protect sockets from VPN routing (prevents routing loop)
        if let Some(callback) = SOCKET_PROTECT_CALLBACK.get() {
            use std::os::fd::AsFd;
            use std::os::fd::AsRawFd;

            // Protect IPv4 socket
            if !callback(udp_v4_tx.as_fd().as_raw_fd()) {
                warn!("Failed to protect IPv4 UDP socket");
            }

            // Protect IPv6 socket
            if !callback(udp_v6_tx.as_fd().as_raw_fd()) {
                warn!("Failed to protect IPv6 UDP socket");
            }
        } else {
            error!("Socket protect callback not set! VPN may not work correctly.");
        }

        Ok(((udp_v4_tx, udp_v4_rx), (udp_v6_tx, udp_v6_rx)))
    }
}

/// GotatunTunnel manages a WireGuard tunnel using gotatun
pub struct GotatunTunnel {
    device: Option<FloppaDevice>,
    interface_name: String,
    connected_at: Option<std::time::Instant>,
}

impl GotatunTunnel {
    /// Build a DevicePeer from WgConfig using a pre-resolved endpoint address.
    fn build_peer(config: &WgConfig, endpoint: std::net::SocketAddr) -> Result<DevicePeer, String> {
        let peer_public_key = config.peer_public_key_bytes()?;
        let preshared_key = config.peer_preshared_key_bytes()?;
        let allowed_ips = config.allowed_ips_networks();

        let public_key = x25519::PublicKey::from(peer_public_key);
        let mut peer = DevicePeer::new(public_key)
            .with_endpoint(endpoint)
            .with_allowed_ips(allowed_ips);

        peer.keepalive = Some(config.persistent_keepalive.unwrap_or(25));

        if let Some(psk) = preshared_key {
            peer = peer.with_preshared_key(psk);
        }

        Ok(peer)
    }

    /// Resolve the endpoint hostname to a `SocketAddr`.
    #[cfg(target_os = "android")]
    async fn resolve_endpoint(config: &WgConfig) -> Result<std::net::SocketAddr, String> {
        tokio::net::lookup_host(&config.peer_endpoint)
            .await
            .map_err(|e| format!("Failed to resolve endpoint '{}': {e}", config.peer_endpoint))?
            .next()
            .ok_or_else(|| {
                format!(
                    "Endpoint '{}' resolved to no addresses",
                    config.peer_endpoint
                )
            })
    }

    /// Create a new tunnel from WireGuard config (desktop platforms).
    ///
    /// `endpoint` is the pre-resolved server address so the hostname is only
    /// resolved once (in `connect_desktop`).
    #[cfg(not(target_os = "android"))]
    #[allow(unused_variables, unused_mut)]
    pub async fn new(
        config: &WgConfig,
        interface_name: &str,
        fwmark: Option<u32>,
        endpoint: std::net::SocketAddr,
    ) -> Result<Self, String> {
        info!("Creating gotatun tunnel on interface {}", interface_name);

        let private_key = config.private_key_bytes()?;
        let peer = Self::build_peer(config, endpoint)?;

        // Create TUN device configuration
        let mut tun_config = tun::Configuration::default();
        tun_config.tun_name(interface_name);

        #[cfg(target_os = "windows")]
        {
            // Metric = 1 gives tunnel routes highest priority over physical adapter
            tun_config.metric(1);
            tun_config.platform_config(|cfg| {
                // Fixed GUID prevents Windows "new network detected" popup on every connect
                cfg.device_guid(0xF109_9A00_C1EE_40A0_B5EC_DE3A_F109_9A00);
                // Load wintun.dll from the exe's directory (cwd may differ, e.g. deep-link launches)
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()));
                if let Some(dir) = exe_dir {
                    cfg.wintun_file(dir.join("wintun.dll"));
                }
            });
        }

        #[cfg(target_os = "macos")]
        tun_config.platform_config(|p| {
            p.enable_routing(false);
        });

        // Create the TUN device
        let tun_device = tun::create_as_async(&tun_config)
            .map_err(|e| format!("Failed to create TUN device: {}", e))?;

        // Wrap in gotatun's TunDevice
        let gota_tun = TunDevice::from_tun_device(tun_device)
            .map_err(|e| format!("Failed to wrap TUN device: {}", e))?;

        // Build the device with all configuration
        let mut builder = DeviceBuilder::new()
            .with_udp(UdpSocketFactory)
            .with_ip(gota_tun)
            .with_private_key(x25519::StaticSecret::from(private_key))
            .with_peer(peer);

        #[cfg(target_os = "linux")]
        if let Some(mark) = fwmark {
            builder = builder.with_fwmark(mark);
        }

        let device = builder
            .build()
            .await
            .map_err(|e| format!("Failed to build gotatun device: {}", e))?;

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
    pub async fn from_fd(config: &WgConfig, tun_fd: RawFd) -> Result<Self, String> {
        use tun::AbstractDevice;

        info!("Creating gotatun tunnel from fd {}", tun_fd);

        let private_key = config.private_key_bytes()?;
        let endpoint = Self::resolve_endpoint(config).await?;
        let peer = Self::build_peer(config, endpoint)?;

        // Create TUN device from raw fd
        let mut tun_config = tun::Configuration::default();
        tun_config.raw_fd(tun_fd);
        tun_config.close_fd_on_drop(false); // VpnService owns the fd
        tun_config.up();

        // Create the TUN device from existing fd
        let mut tun_device = tun::create_as_async(&tun_config)
            .map_err(|e| format!("Failed to create TUN device from fd: {}", e))?;

        // HACK: the `tun` crate stubs out MTU on Android (it just stores the value).
        // gotatun reads MTU from this, so we need to set it here with the correct value.
        let mtu = config.get_mtu() as u16;
        tun_device
            .set_mtu(mtu)
            .map_err(|e| format!("Failed to set MTU: {}", e))?;
        info!("Set TUN MTU to {}", mtu);

        // Wrap in gotatun's TunDevice
        let gota_tun = TunDevice::from_tun_device(tun_device)
            .map_err(|e| format!("Failed to wrap TUN device: {}", e))?;

        // Build the device with Android socket factory and all configuration
        let device = DeviceBuilder::new()
            .with_udp(AndroidUdpSocketFactory)
            .with_ip(gota_tun)
            .with_private_key(x25519::StaticSecret::from(private_key))
            .with_peer(peer)
            .build()
            .await
            .map_err(|e| format!("Failed to build gotatun device: {}", e))?;

        info!("Tunnel configured successfully");

        Ok(Self {
            device: Some(device),
            interface_name: format!("tun_fd_{}", tun_fd),
            connected_at: Some(std::time::Instant::now()),
        })
    }

    /// Stub for Android - use from_fd instead
    #[cfg(target_os = "android")]
    pub async fn new(
        _config: &WgConfig,
        _interface_name: &str,
        _fwmark: Option<u32>,
    ) -> Result<Self, String> {
        Err("On Android, use from_fd() with the fd from VpnService".to_string())
    }

    /// Get traffic statistics
    pub async fn get_stats(&self) -> Result<TrafficStats, String> {
        let device = self.device.as_ref().ok_or("Device not initialized")?;
        let peers = device.peers().await;

        let mut stats = TrafficStats::default();
        for peer_stats in peers {
            stats.rx_bytes += peer_stats.stats.rx_bytes as u64;
            stats.tx_bytes += peer_stats.stats.tx_bytes as u64;
        }
        Ok(stats)
    }

    /// Get last handshake time (seconds ago)
    pub async fn get_last_handshake(&self) -> Option<i64> {
        let device = self.device.as_ref()?;
        let peers = device.peers().await;

        for peer_stats in peers {
            if let Some(duration) = peer_stats.stats.last_handshake {
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
    pub async fn stop(mut self) -> Result<(), String> {
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

/// Tunnel manager that owns the tunnel and provides thread-safe access
pub struct TunnelManager {
    tunnel: RwLock<Option<GotatunTunnel>>,
}

impl TunnelManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tunnel: RwLock::new(None),
        })
    }

    /// Start tunnel on desktop platforms (creates TUN device)
    ///
    /// `fwmark` is used on Linux for policy routing to ensure VPN packets bypass the VPN interface
    #[cfg(not(target_os = "android"))]
    pub async fn start(
        &self,
        config: &WgConfig,
        interface_name: &str,
        fwmark: Option<u32>,
        endpoint: std::net::SocketAddr,
    ) -> Result<(), String> {
        let mut tunnel_guard = self.tunnel.write().await;

        // Stop existing tunnel if any
        if let Some(tunnel) = tunnel_guard.take() {
            tunnel.stop().await?;
        }

        // Create new tunnel
        let tunnel = GotatunTunnel::new(config, interface_name, fwmark, endpoint).await?;
        *tunnel_guard = Some(tunnel);

        Ok(())
    }

    /// Start tunnel on Android using fd from VpnService
    #[cfg(target_os = "android")]
    pub async fn start(
        &self,
        _config: &WgConfig,
        _interface_name: &str,
        _fwmark: Option<u32>,
    ) -> Result<(), String> {
        // On Android, we need to wait for the fd from VpnService
        // Use start_with_fd instead after receiving the fd
        Err("On Android, call start_with_fd() after receiving fd from VpnService".to_string())
    }

    /// Start tunnel using a raw file descriptor (Android only)
    #[cfg(target_os = "android")]
    pub async fn start_with_fd(&self, config: &WgConfig, tun_fd: RawFd) -> Result<(), String> {
        let mut tunnel_guard = self.tunnel.write().await;

        // Stop existing tunnel if any
        if let Some(tunnel) = tunnel_guard.take() {
            tunnel.stop().await?;
        }

        // Create new tunnel from fd
        let tunnel = GotatunTunnel::from_fd(config, tun_fd).await?;
        *tunnel_guard = Some(tunnel);

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let mut tunnel_guard = self.tunnel.write().await;
        if let Some(tunnel) = tunnel_guard.take() {
            tunnel.stop().await?;
        }
        Ok(())
    }

    pub async fn is_running(&self) -> bool {
        self.tunnel.read().await.is_some()
    }

    pub async fn get_stats(&self) -> Option<TrafficStats> {
        let tunnel_guard = self.tunnel.read().await;
        if let Some(tunnel) = tunnel_guard.as_ref() {
            tunnel.get_stats().await.ok()
        } else {
            None
        }
    }

    pub async fn get_last_handshake(&self) -> Option<i64> {
        let tunnel_guard = self.tunnel.read().await;
        if let Some(tunnel) = tunnel_guard.as_ref() {
            tunnel.get_last_handshake().await
        } else {
            None
        }
    }

    pub async fn get_connection_duration(&self) -> Option<Duration> {
        let tunnel_guard = self.tunnel.read().await;
        tunnel_guard.as_ref().and_then(|t| t.connection_duration())
    }

    pub async fn get_interface_name(&self) -> Option<String> {
        let tunnel_guard = self.tunnel.read().await;
        tunnel_guard
            .as_ref()
            .map(|t| t.interface_name().to_string())
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self {
            tunnel: RwLock::new(None),
        }
    }
}
