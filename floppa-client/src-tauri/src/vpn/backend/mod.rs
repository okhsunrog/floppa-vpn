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

use super::platform::TunParams;
use super::state::{ProtocolConfig, TrafficStats};
use crate::vpn::actor::types::Observation;
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// All tunnel info returned by [`VpnBackend::get_all_info`].
#[derive(Debug, Clone, Default)]
pub struct VpnFullInfo {
    pub is_running: bool,
    pub stats: Option<TrafficStats>,
    pub last_packet_received: Option<i64>,
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
    /// resolved once. `tun_params` carries platform-specific configuration
    /// (fwmark, wintun path, manage_device) from the platform layer.
    async fn start(
        &self,
        config: &ProtocolConfig,
        interface_name: &str,
        tun_params: &TunParams,
        endpoint: SocketAddr,
    ) -> Result<(), String>;

    /// Start tunnel from a file descriptor provided by the platform VPN service.
    ///
    /// Used on Android (fd from VpnService) and potentially iOS.
    async fn start_with_fd(&self, config: &ProtocolConfig, tun_fd: i32) -> Result<(), String>;

    /// Ask a already-running out-of-process service to bring up a tunnel on the descriptor it
    /// holds, and get back a reason if it cannot.
    ///
    /// `epoch` identifies the request, so a service instance that has been superseded can refuse
    /// it instead of obeying. Meaningless for an in-process backend, which has no service to ask.
    async fn start_tunnel(
        &self,
        _epoch: u64,
        _config: &ProtocolConfig,
        _endpoint: SocketAddr,
    ) -> Result<(), String> {
        Err("this backend has no out-of-process service to ask".to_string())
    }

    /// Stop the tunnel.
    async fn stop(&self) -> Result<(), String>;

    /// Get all tunnel info in a single call.
    ///
    /// Returns `None` if the backend is unreachable (e.g. `:vpn` service not running).
    /// This is a **normal state**, not an error — callers should treat `None` as
    /// "tunnel not available" without logging errors or retrying.
    async fn get_all_info(&self) -> Option<VpnFullInfo>;

    /// Look at the world.
    ///
    /// Distinct from [`Self::get_all_info`] in the one way that matters: it never collapses "there
    /// is no tunnel" and "I could not reach the thing that would know" into the same value. The
    /// first is authoritative, the second is not, and treating them alike is what let a transient
    /// IPC gap read as a dropped tunnel.
    async fn observe(&self) -> Observation;

    /// How long this backend may be unreachable before its tunnel is presumed lost.
    ///
    /// Zero for an in-process backend, which cannot fail to answer. Non-zero only where the tunnel
    /// lives in another process that can be restarted underneath us.
    fn liveness_grace(&self) -> Duration {
        Duration::ZERO
    }

    /// Ping the VLESS server through the proxy chain (bypasses TUN).
    /// Updates `last_packet_received` on success so the health dot reflects connectivity.
    async fn ping(&self) -> Result<(), String>;

    /// Propagate log config to the tunnel process.
    /// Default no-op for desktop (same-process, handled by logging module directly).
    async fn set_log_config(&self, _config: &crate::logging::LogConfig) {}

    /// Start file logging in the tunnel process for a diagnostic capture.
    async fn start_log_capture(&self, _capture_id: &str) {}

    /// Stop file logging in the tunnel process for a diagnostic capture.
    async fn stop_log_capture(&self) {}
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
