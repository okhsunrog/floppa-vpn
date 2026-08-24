//! The IPC wire format between the UI process and the `:vpn` process on Android.
//!
//! The service trait is Android-only, because tarpc is an Android-only dependency. The payload type
//! is not gated: it is plain data, and gating it would mean its tests compile on one target only.

use crate::vpn::protocol::Protocol;
use serde::{Deserialize, Serialize};

/// The identity of a running tunnel, as reported by the process that owns it.
///
/// The UI process never infers this from its own settings. That guess is wrong precisely when it
/// matters — after a failed probe cycle has rewritten them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInfo {
    pub protocol: Protocol,
    pub endpoint: String,
    pub address: String,
    pub connected_secs: Option<u64>,
}

/// All tunnel info returned in a single RPC call.
///
/// `running` is one field rather than a `bool` beside a set of `Option`s, so "a tunnel is up but we
/// do not know which" cannot be written down. The owning process reads both halves out of the same
/// `Option`, so they could never disagree — encoding that in the type means nothing downstream has
/// to check for a combination that cannot occur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub running: Option<RunningInfo>,
    pub last_packet_received: Option<i64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
}

impl TunnelInfo {
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

/// The IPC socket name. Keep in sync with `FloppaVpnService.kt`.
///
/// This never needs versioning, and the wire format never needs to stay backward compatible.
/// Both ends ship in the same APK and are always the same build: installing one replaces the
/// other, and installing force-stops every process of the package, so two builds cannot be live
/// at once. Change the format freely — including the method set, which shifts tarpc's dispatch
/// indices.
pub const SOCKET_NAME: &str = "vpn.sock";

#[cfg(target_os = "android")]
#[tarpc::service]
pub trait VpnRpc {
    /// Get all tunnel info in a single call.
    async fn get_full_info() -> TunnelInfo;

    /// Stop the tunnel and VPN service.
    async fn stop() -> Result<(), String>;

    /// Ping the VLESS server through the proxy chain.
    /// Updates last_packet_received on success.
    async fn ping() -> Result<(), String>;

    /// Apply a new log configuration in the VPN process.
    async fn set_log_config(config: crate::logging::LogConfig);

    /// Start writing VPN process logs into a diagnostic capture.
    async fn start_log_capture(capture_id: String);

    /// Stop writing VPN process logs into a diagnostic capture.
    async fn stop_log_capture();
}
