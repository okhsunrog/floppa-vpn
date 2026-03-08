//! Platform-specific VPN operations
//!
//! This module provides platform-specific implementations for:
//! - TUN interface IP configuration
//! - Routing table management
//! - DNS configuration

use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::path::PathBuf;

/// Platform-specific parameters for TUN device creation.
///
/// Each platform provides these via [`Platform::tun_params()`], centralizing
/// OS-specific decisions (fwmark, wintun path, device management) so callers
/// don't need scattered `#[cfg]` blocks.
#[derive(Debug, Clone, Default)]
pub struct TunParams {
    /// Whether the `tun` crate should manage the device (create, configure, bring up).
    ///
    /// - Linux: `false` — pkexec helper pre-creates a persistent TUN
    /// - Windows: `true` — Wintun creates the adapter in-process
    pub manage_device: bool,

    /// Firewall mark for policy routing (Linux only).
    ///
    /// Marks WireGuard UDP packets so they bypass the VPN routing table,
    /// preventing routing loops. `None` if CAP_NET_ADMIN is unavailable.
    pub fwmark: Option<u32>,

    /// Path to `wintun.dll` (Windows only).
    pub wintun_file: Option<PathBuf>,
}

/// Platform-specific VPN operations
#[async_trait]
pub trait Platform: Send + Sync {
    /// Return platform-specific TUN creation parameters.
    ///
    /// Called once before each connection to determine how the TUN device
    /// should be created (manage_device, fwmark, wintun path, etc.).
    fn tun_params(&self) -> TunParams;

    /// Prepare TUN interface before tunnel startup.
    ///
    /// On Linux, this may invoke a privileged helper to create a persistent
    /// TUN owned by the current user. Other platforms may no-op.
    async fn prepare_tun(&self, iface: &str) -> Result<(), String>;

    /// Configure IP address on TUN interface
    async fn configure_address(&self, iface: &str, addr: IpNetwork) -> Result<(), String>;

    /// Add a host route for the VPN endpoint through the original default gateway.
    /// This prevents routing loops when split routes (0.0.0.0/1, 128.0.0.0/1) are active.
    /// Must be called BEFORE add_routes().
    async fn add_endpoint_route(&self, endpoint_ip: IpAddr) -> Result<(), String>;

    /// Remove the endpoint host route added by add_endpoint_route().
    async fn remove_endpoint_route(&self) -> Result<(), String>;

    /// Add routes through VPN interface
    async fn add_routes(&self, iface: &str, allowed_ips: &[IpNetwork]) -> Result<(), String>;

    /// Remove VPN routes
    async fn remove_routes(&self, iface: &str) -> Result<(), String>;

    /// Configure DNS servers (saves original config for restore)
    async fn configure_dns(&self, iface: &str, servers: &[IpAddr]) -> Result<(), String>;

    /// Restore original DNS configuration
    async fn restore_dns(&self, iface: &str) -> Result<(), String>;

    /// Cleanup interface (remove address, routes, DNS, endpoint route)
    async fn cleanup(&self, iface: &str) -> Result<(), String>;
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform as PlatformImpl;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::WindowsPlatform as PlatformImpl;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "android")]
pub use android::AndroidPlatform as PlatformImpl;

// Stub for unsupported platforms (macOS, iOS, etc.)
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "android")))]
pub struct PlatformImpl;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "android")))]
impl PlatformImpl {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "android")))]
#[async_trait]
impl Platform for PlatformImpl {
    fn tun_params(&self) -> TunParams {
        TunParams::default()
    }

    async fn prepare_tun(&self, _iface: &str) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn configure_address(&self, _iface: &str, _addr: IpNetwork) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn add_endpoint_route(&self, _endpoint_ip: IpAddr) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn remove_endpoint_route(&self) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn add_routes(&self, _iface: &str, _allowed_ips: &[IpNetwork]) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn remove_routes(&self, _iface: &str) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn configure_dns(&self, _iface: &str, _servers: &[IpAddr]) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn restore_dns(&self, _iface: &str) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }

    async fn cleanup(&self, _iface: &str) -> Result<(), String> {
        Err("Platform not supported".to_string())
    }
}

/// Get the platform implementation
pub fn get_platform() -> PlatformImpl {
    PlatformImpl::new()
}
