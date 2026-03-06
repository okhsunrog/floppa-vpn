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

use super::state::{ProtocolConfig, TrafficStats};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;

/// All tunnel info returned by [`VpnBackend::get_all_info`].
#[derive(Debug, Clone, Default)]
pub struct VpnFullInfo {
    pub is_running: bool,
    pub stats: Option<TrafficStats>,
    pub last_handshake: Option<i64>,
    pub connected_secs: Option<u64>,
}

/// Backend for VPN tunnel management.
///
/// Each platform implements this trait differently:
/// - [`InProcessBackend`](in_process::InProcessBackend): tunnel runs in the current process (desktop)
/// - [`AndroidIpcBackend`](android_ipc::AndroidIpcBackend): tunnel in separate `:vpn` process via tarpc
/// - [`IosBackend`](ios::IosBackend): tunnel in Network Extension via Apple IPC (future)
#[async_trait]
pub trait VpnBackend: Send + Sync {
    /// Start tunnel by creating a TUN device (desktop platforms).
    ///
    /// `endpoint` is the pre-resolved server address so the hostname is only
    /// resolved once. On Linux, `fwmark` is used for policy routing to prevent
    /// VPN packets from being routed back through the VPN interface.
    async fn start(
        &self,
        config: &ProtocolConfig,
        interface_name: &str,
        fwmark: Option<u32>,
        endpoint: SocketAddr,
    ) -> Result<(), String>;

    /// Start tunnel from a file descriptor provided by the platform VPN service.
    ///
    /// Used on Android (fd from VpnService) and potentially iOS.
    async fn start_with_fd(&self, config: &ProtocolConfig, tun_fd: i32) -> Result<(), String>;

    /// Stop the tunnel.
    async fn stop(&self) -> Result<(), String>;

    /// Get all tunnel info in a single call.
    ///
    /// Returns `None` if the backend is unreachable (e.g. `:vpn` service not running).
    /// This is a **normal state**, not an error — callers should treat `None` as
    /// "tunnel not available" without logging errors or retrying.
    async fn get_all_info(&self) -> Option<VpnFullInfo>;
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
