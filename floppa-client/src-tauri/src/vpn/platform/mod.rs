//! Platform-specific VPN operations
//!
//! This module provides platform-specific implementations for:
//! - TUN interface IP configuration
//! - Routing table management
//! - DNS configuration

use crate::vpn::protocol::InterfaceName;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;

/// Why a platform operation failed.
///
/// The distinction is load-bearing for the connect cycle: [`Self::PermissionDenied`] and
/// [`Self::Unavailable`] mean trying the next protocol is pointless and will just produce another
/// consent prompt, while [`Self::Failed`] is worth another attempt. Previously this was recovered
/// by substring-matching the error text (`e.contains("Operation not permitted")`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlatformError {
    /// The privileged helper is missing, outdated, or refused to install.
    #[error("privileged helper unavailable: {0}")]
    Unavailable(String),
    /// The operation needs privileges this process does not have and cannot obtain.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// Everything else.
    #[error("{0}")]
    Failed(String),
}

impl PlatformError {
    /// Should the connect cycle stop entirely rather than try the next protocol?
    pub const fn is_fatal_for_cycle(&self) -> bool {
        matches!(self, Self::PermissionDenied(_) | Self::Unavailable(_))
    }
}

/// The default gateway a host route was pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gateway(pub String);

/// DNS state captured *before* it was changed, owned by the rollback step that changed it.
///
/// Keeping the snapshot in the step rather than in the platform object is what makes a
/// double-capture unrepresentable: previously a second `configure_dns` without an intervening
/// restore saved floppa's own generated `/etc/resolv.conf` as "the original", so restoring made
/// the overwrite permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DnsSnapshot {
    /// systemd-resolved: `resolvectl revert <iface>` is stateless, nothing to carry.
    Resolvectl,
    /// Direct `/etc/resolv.conf`. `symlink_target` is `Some` when the path *was* a symlink, so the
    /// restore recreates the link instead of writing through it into the resolver's own stub file.
    ResolvConf {
        content: Option<String>,
        symlink_target: Option<PathBuf>,
    },
    /// Windows: restoring means handing the interface back to DHCP.
    Dhcp,
    /// Android: DNS is set atomically as part of `VpnService.Builder`, and dies with the link.
    OwnedByLink,
}

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

/// Platform-specific VPN operations.
///
/// **Stateless by contract.** An implementation must not remember what it applied — every undo
/// receives back exactly what the corresponding apply recorded, carried in a
/// [`Step`](crate::vpn::rollback::Step). This is what allows a partially applied connect to be
/// unwound precisely, and what makes "restore something that was never saved" impossible to
/// express.
#[async_trait]
pub trait Platform: Send + Sync {
    /// Return platform-specific TUN creation parameters.
    fn tun_params(&self) -> TunParams;

    /// Check that this platform can currently configure networking at all.
    ///
    /// Runs before any mutation, so a missing privileged helper surfaces as a typed
    /// [`PlatformError::Unavailable`] up front rather than as an opaque failure midway through a
    /// connect (previously it was only `warn!`-logged at startup and then rediscovered later).
    async fn preflight(&self) -> Result<(), PlatformError>;

    /// Prepare the tunnel link before tunnel startup. Undone by [`Self::release_link`].
    async fn prepare_link(&self, iface: &InterfaceName) -> Result<(), PlatformError>;

    /// Undo [`Self::prepare_link`]. Must be safe to call when nothing was prepared.
    async fn release_link(&self, iface: &InterfaceName) -> Result<(), PlatformError>;

    /// Configure the tunnel address. Undone by [`Self::deconfigure_address`] with the same value.
    async fn configure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError>;

    /// Remove exactly the address that was configured.
    async fn deconfigure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError>;

    /// The current default gateway, read before anything is mutated so the endpoint route's undo
    /// can match on gateway as well as destination.
    async fn default_gateway(&self) -> Result<Option<Gateway>, PlatformError>;

    /// The OS index of an interface, where the platform needs it to scope routes and DNS.
    /// Captured fresh per attempt: Wintun assigns a new index every time an adapter is created,
    /// and a stale one points at an unrelated system interface.
    async fn interface_index(&self, iface: &InterfaceName) -> Option<u32>;

    /// Add a host route for the VPN endpoint through the original default gateway, so tunnelled
    /// traffic does not loop. Must be called BEFORE [`Self::add_routes`].
    async fn add_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError>;

    /// Remove exactly the endpoint route that was added.
    async fn remove_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError>;

    /// Add the given routes, which are already split (see
    /// [`split_default`](crate::vpn::rollback::split_default)).
    async fn add_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError>;

    /// Remove exactly the routes that were added.
    async fn remove_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError>;

    /// Capture the current DNS configuration. Called BEFORE [`Self::configure_dns`]; the result is
    /// stored in the rollback step, not in `self`.
    async fn capture_dns(
        &self,
        iface: &InterfaceName,
        if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError>;

    /// Point DNS at the tunnel's servers.
    async fn configure_dns(
        &self,
        iface: &InterfaceName,
        servers: &[IpAddr],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError>;

    /// Restore the DNS configuration captured by [`Self::capture_dns`].
    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        snapshot: &DnsSnapshot,
        if_index: Option<u32>,
    ) -> Result<(), PlatformError>;

    /// Whether IPv6 routes should be installed on this host.
    async fn ipv6_enabled(&self) -> bool;
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

    async fn preflight(&self) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn prepare_link(&self, _iface: &InterfaceName) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn release_link(&self, _iface: &InterfaceName) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn configure_address(
        &self,
        _iface: &InterfaceName,
        _addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn deconfigure_address(
        &self,
        _iface: &InterfaceName,
        _addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn default_gateway(&self) -> Result<Option<Gateway>, PlatformError> {
        Err(unsupported())
    }

    async fn interface_index(&self, _iface: &InterfaceName) -> Option<u32> {
        None
    }

    async fn add_endpoint_route(
        &self,
        _endpoint: IpAddr,
        _gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn remove_endpoint_route(
        &self,
        _endpoint: IpAddr,
        _gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn add_routes(
        &self,
        _iface: &InterfaceName,
        _routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn remove_routes(
        &self,
        _iface: &InterfaceName,
        _routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn capture_dns(
        &self,
        _iface: &InterfaceName,
        _if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        Err(unsupported())
    }

    async fn configure_dns(
        &self,
        _iface: &InterfaceName,
        _servers: &[IpAddr],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        Err(unsupported())
    }

    async fn restore_dns(
        &self,
        _iface: &InterfaceName,
        _snapshot: &DnsSnapshot,
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn ipv6_enabled(&self) -> bool {
        false
    }
}

/// Undo paths return `Ok` on the unsupported stub: there is nothing to undo, and a rollback must
/// never wedge on a platform that could not have applied anything in the first place.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "android")))]
fn unsupported() -> PlatformError {
    PlatformError::Unavailable("platform not supported".to_string())
}

/// Get the platform implementation
pub fn get_platform() -> PlatformImpl {
    PlatformImpl::new()
}
