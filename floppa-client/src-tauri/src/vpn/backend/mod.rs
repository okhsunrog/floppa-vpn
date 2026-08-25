//! VPN backend abstraction layer.
//!
//! Provides a unified interface for tunnel management across platforms:
//! - **Desktop** (Linux/Windows/macOS): in-process tunnel via gotatun
//! - **Android**: IPC to separate `:vpn` process via tarpc over Unix socket
//!
//! iOS is not implemented. It had a stub here that returned `Err` from every method, was never
//! constructed, and compiled everywhere — so every change to the trait below had to be mirrored
//! into a type nothing calls. The design it carried is in `docs/IOS-BACKEND-PLAN.md`.

#[cfg(not(target_os = "android"))]
mod in_process;

#[cfg(target_os = "android")]
mod android_ipc;

use super::platform::TunParams;
use super::state::ProtocolConfig;
use crate::vpn::actor::types::{Observation, TunnelParams};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Why the backend could not do what it was asked.
///
/// Plain data, so it can travel inside an attempt's failure to the UI. The variants that matter
/// to a caller's *decision* — as opposed to its log line — are the ones a policy hangs off:
/// [`Self::PermissionDenied`] is what makes the desktop ladder retry once without the socket
/// mark, and it is derived from the OS error kind, never from the wording of a message that a
/// `setlocale` may have translated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendError {
    /// The OS refused a privileged operation. On Linux that is `SO_MARK`, which needs
    /// `CAP_NET_ADMIN`.
    #[error("permission denied: {detail}")]
    PermissionDenied { detail: String },
    /// The config cannot be turned into a tunnel: a malformed key, address or AWG parameter.
    #[error("config is not usable: {detail}")]
    InvalidConfig { detail: String },
    /// The tunnel engine or its device failed to start or stop.
    #[error("tunnel engine: {detail}")]
    Engine { detail: String },
    /// Nothing is running to be pinged.
    #[error("no tunnel is running")]
    NotRunning,
    /// Cross-process only: the service could not be reached, or the call to it failed in
    /// transit. Says nothing about whether a tunnel exists.
    #[error("VPN service unreachable: {detail}")]
    ServiceUnreachable { detail: String },
    /// Cross-process only: the service answered, and the answer was a refusal.
    #[error("VPN service refused: {detail}")]
    ServiceRefused { detail: String },
    /// The operation does not exist on this backend: an in-process backend has no service to
    /// ask, and a cross-process one starts nothing itself.
    #[error("not supported by this backend")]
    Unsupported,
}

/// Backend for VPN tunnel management.
///
/// Each platform implements this trait differently:
/// - `InProcessBackend`: the tunnel runs in the current process (desktop)
/// - `AndroidIpcBackend`: the tunnel lives in the separate `:vpn` process, reached over tarpc
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
    ) -> Result<(), BackendError>;

    /// Ask a already-running out-of-process service to bring up a tunnel on the descriptor it
    /// holds, and get back a reason if it cannot.
    ///
    /// `generation` identifies the service instance the request is for, so one that has been
    /// superseded refuses it instead of obeying. It is minted per service start — never the
    /// intent's epoch, which every protocol and pass of one cycle shares. Meaningless for an
    /// in-process backend, which has no service to ask.
    ///
    /// `params` are the split rules the service's descriptor was built with. They travel so the
    /// service can report them back to whoever finds the tunnel later.
    async fn start_tunnel(
        &self,
        _generation: u64,
        _config: &ProtocolConfig,
        _endpoint: SocketAddr,
        _params: &TunnelParams,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Stop the tunnel.
    async fn stop(&self) -> Result<(), BackendError>;

    /// "The next answer should come from this service generation."
    ///
    /// Set once, right after a service start is asked for. Until that generation answers, a reply
    /// from any other one means the cached connection still points at the instance being replaced,
    /// and it is dropped so the next look reconnects. Without it a cached connection to a dying
    /// instance kept answering `OtherGeneration` for the whole attempt budget, and the protocol
    /// was reported as failed to start.
    ///
    /// Meaningless for an in-process backend, which has no service and no connection.
    fn expect_generation(&self, _generation: u64) {}

    /// Look at the world.
    ///
    /// The single way to learn anything about the tunnel, and the reason there is no longer an
    /// `Option`-returning variant beside it: this one never collapses "there is no tunnel" and "I
    /// could not reach the thing that would know" into the same value. The first is authoritative,
    /// the second is not, and treating them alike is what let a transient IPC gap read as a
    /// dropped tunnel.
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
    async fn ping(&self) -> Result<(), BackendError>;

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
/// On Android, pass the socket path for tarpc IPC and the app handle for the plugin's intent path.
#[cfg(target_os = "android")]
pub fn create_backend(socket_path: String, app: tauri::AppHandle) -> Arc<dyn VpnBackend> {
    Arc::new(android_ipc::AndroidIpcBackend::new(socket_path, app))
}

/// Create the appropriate VPN backend for the current platform.
#[cfg(not(target_os = "android"))]
pub fn create_backend() -> Arc<dyn VpnBackend> {
    Arc::new(in_process::InProcessBackend::new())
}
