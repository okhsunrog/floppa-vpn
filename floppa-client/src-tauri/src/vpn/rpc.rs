//! tarpc service definition for VPN IPC.
//!
//! Used for communication between the UI process (tarpc client) and the
//! `:vpn` process (tarpc server) on Android.

/// All tunnel info returned in a single RPC call.
///
/// `protocol`, `endpoint` and `address` come from the process that actually owns the tunnel. They
/// exist so the UI process never has to infer what is running from its own settings — a guess that
/// is wrong precisely when it matters, after a failed probe cycle rewrote them.
///
/// The wire format is bincode, which is not self-describing: adding a field is not backward
/// compatible, and `#[serde(default)]` cannot help because there is no field name to be missing.
/// A new UI meeting an older surviving `:vpn` would decode garbage rather than fail. That is why
/// the socket is versioned by name (see `SOCKET_NAME`): an incompatible peer is simply unreachable,
/// which the actor already knows how to handle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelInfo {
    pub is_running: bool,
    pub protocol: Option<crate::vpn::protocol::Protocol>,
    pub endpoint: Option<String>,
    pub address: Option<String>,
    pub last_packet_received: Option<i64>,
    pub connected_secs: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
}

/// Versioned by name. Bumping this is the supported way to make an incompatible wire change:
/// an old peer keeps listening on the old path, the new UI never finds it, and the tunnel is
/// treated as unreachable instead of being misread.
pub const SOCKET_NAME: &str = "vpn-v2.sock";

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
