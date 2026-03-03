//! tarpc service definition for VPN IPC.
//!
//! Used for communication between the UI process (tarpc client) and the
//! `:vpn` process (tarpc server) on Android.

/// All tunnel info returned in a single RPC call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelInfo {
    pub is_running: bool,
    pub last_handshake: Option<i64>,
    pub connected_secs: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
}

#[tarpc::service]
pub trait VpnRpc {
    /// Get all tunnel info in a single call.
    async fn get_full_info() -> TunnelInfo;

    /// Stop the tunnel and VPN service.
    async fn stop() -> Result<(), String>;
}
