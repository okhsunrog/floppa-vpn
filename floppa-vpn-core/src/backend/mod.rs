//! What the actor does tunnels through.
//!
//! Both implementations are in-process, because the actor is always in the same process as the
//! tunnel now: `in_process` on desktop, and `android_service` inside `:vpn`, where the only thing
//! that still crosses a boundary is the descriptor — Kotlin makes it, Rust runs on it. There used
//! to be a third, an IPC backend for a UI process that owned the decisions but not the tunnel; the
//! move made it unnecessary, and with it the whole notion of an observation that could fail to
//! arrive.
//!
//! iOS is not implemented. It had a stub here that returned `Err` from every method, was never
//! constructed, and compiled everywhere — so every change to the trait below had to be mirrored
//! into a type nothing calls. The design it carried is in `docs/IOS-BACKEND-PLAN.md`.

#[cfg(not(target_os = "android"))]
mod in_process;

#[cfg(target_os = "android")]
mod android_service;

#[cfg(target_os = "android")]
pub use android_service::AndroidServiceBackend;

use super::platform::TunParams;
use super::state::ProtocolConfig;
use crate::actor::types::{Observation, TunnelParams};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::net::SocketAddr;
#[cfg(not(target_os = "android"))]
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

    /// Look at the world.
    ///
    /// The single way to learn anything about the tunnel, and the reason there is no longer an
    /// `Option`-returning variant beside it: this one never collapses "there is no tunnel" and "I
    /// could not reach the thing that would know" into the same value. The first is authoritative,
    /// the second is not, and treating them alike is what let a transient IPC gap read as a
    /// dropped tunnel. Every backend is now in the tunnel's own process and so always answers, but
    /// the distinction stays in the type: it is what stopped that bug from being writable.
    async fn observe(&self) -> Observation;

    /// How long this backend may be unreachable before its tunnel is presumed lost.
    ///
    /// Zero everywhere now: the actor and the tunnel are in one process, so an answer cannot fail
    /// to arrive. It is still a knob rather than a constant because the darkness clock it feeds is
    /// what a second process would need again.
    fn liveness_grace(&self) -> Duration {
        Duration::ZERO
    }

    /// Ping the VLESS server through the proxy chain (bypasses TUN).
    /// Updates `last_packet_received` on success so the health dot reflects connectivity.
    async fn ping(&self) -> Result<(), BackendError>;

    /// Make the far side prove it is there, and say whether it did.
    ///
    /// The counterpart to the passive silence an observation reports: that only says nothing has
    /// arrived, which a sleeping phone and a keepalive-less config both produce without anything
    /// being wrong. This costs a round trip — a forced rehandshake for the WireGuard family, a
    /// ping for VLESS — and is what the actor waits on before calling a tunnel lost.
    async fn probe(&self) -> Result<(), BackendError>;

    /// Propagate log config to the tunnel process.
    /// Default no-op for desktop (same-process, handled by logging module directly).
    async fn set_log_config(&self, _config: &crate::logging::LogConfig) {}

    /// Start file logging in the tunnel process for a diagnostic capture.
    async fn start_log_capture(&self, _capture_id: &str) {}

    /// Stop file logging in the tunnel process for a diagnostic capture.
    async fn stop_log_capture(&self) {}
}

/// The desktop backend. Android builds its own inside `:vpn`, where the service registry it needs
/// lives — see `jni_entry` in the app.
#[cfg(not(target_os = "android"))]
pub fn create_backend() -> Arc<dyn VpnBackend> {
    Arc::new(in_process::InProcessBackend::new())
}
