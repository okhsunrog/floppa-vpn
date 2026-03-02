//! tarpc service definition for VPN IPC.
//!
//! Used for communication between the UI process (tarpc client) and the
//! `:vpn` process (tarpc server) on Android.

#[tarpc::service]
pub trait VpnRpc {
    /// Get traffic statistics: (tx_bytes, rx_bytes)
    async fn get_stats() -> Option<(u64, u64)>;

    /// Get tunnel status: (is_running, last_handshake_secs)
    async fn get_status() -> (bool, Option<i64>);

    /// Stop the tunnel and VPN service
    async fn stop() -> Result<(), String>;
}
