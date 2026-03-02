//! VPN backend abstraction layer.
//!
//! Provides a unified interface for tunnel management across platforms:
//! - **Desktop** (Linux/Windows/macOS): in-process tunnel via gotatun
//! - **Android**: IPC to separate `:vpn` process via tarpc over Unix socket
//! - **iOS** (future): IPC to Network Extension via Apple's NE framework

#[cfg(not(target_os = "android"))]
mod in_process;

#[cfg(target_os = "android")]
mod android_ipc;

// iOS backend — stub for future implementation
mod ios;

use super::state::{TrafficStats, WgConfig};
use async_trait::async_trait;
use std::sync::Arc;

/// Backend for VPN tunnel management.
///
/// Each platform implements this trait differently:
/// - [`InProcessBackend`](in_process::InProcessBackend): tunnel runs in the current process (desktop, current Android)
/// - [`AndroidIpcBackend`](android_ipc::AndroidIpcBackend): tunnel in separate `:vpn` process via tarpc (future)
/// - [`IosBackend`](ios::IosBackend): tunnel in Network Extension via Apple IPC (future)
#[async_trait]
pub trait VpnBackend: Send + Sync {
    /// Start tunnel by creating a TUN device (desktop platforms).
    ///
    /// On Linux, `fwmark` is used for policy routing to prevent VPN packets
    /// from being routed back through the VPN interface.
    async fn start(
        &self,
        config: &WgConfig,
        interface_name: &str,
        fwmark: Option<u32>,
    ) -> Result<(), String>;

    /// Start tunnel from a file descriptor provided by the platform VPN service.
    ///
    /// Used on Android (fd from VpnService) and potentially iOS.
    async fn start_with_fd(&self, config: &WgConfig, tun_fd: i32) -> Result<(), String>;

    /// Stop the tunnel.
    async fn stop(&self) -> Result<(), String>;

    /// Check if the tunnel is currently running.
    async fn is_running(&self) -> bool;

    /// Get cumulative traffic statistics.
    async fn get_stats(&self) -> Option<TrafficStats>;

    /// Get time since last WireGuard handshake in seconds.
    async fn get_last_handshake(&self) -> Option<i64>;

    /// Get the tunnel interface name.
    async fn get_interface_name(&self) -> Option<String>;
}

/// Create the appropriate VPN backend for the current platform.
///
/// On Android, pass the socket path for tarpc IPC.
#[cfg(target_os = "android")]
pub fn create_backend(socket_path: String) -> Arc<dyn VpnBackend> {
    Arc::new(android_ipc::AndroidIpcBackend::new(socket_path))
}

/// Create the appropriate VPN backend for the current platform.
#[cfg(not(target_os = "android"))]
pub fn create_backend() -> Arc<dyn VpnBackend> {
    Arc::new(in_process::InProcessBackend::new())
}
