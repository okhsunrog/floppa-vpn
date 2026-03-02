//! IPC protocol types for cross-process VPN communication.
//!
//! Used by:
//! - Android: serialized over tarpc (Unix domain socket between UI and :vpn process)
//! - iOS: serialized via bincode, sent through Apple's NETunnelProviderManager.sendProviderMessage()
//!
//! Desktop platforms don't use IPC — the tunnel runs in-process.

use serde::{Deserialize, Serialize};

/// Command sent from UI process to VPN tunnel process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VpnCommand {
    /// Start the tunnel with the given WireGuard config.
    /// `tun_fd` is provided by the platform VPN service (Android) or None (iOS — extension creates it).
    Connect {
        config_str: String,
        tun_fd: Option<i32>,
    },

    /// Stop the tunnel.
    Disconnect,

    /// Request traffic statistics.
    GetStats,

    /// Request current tunnel status.
    GetStatus,
}

/// Response from VPN tunnel process to UI process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VpnResponse {
    /// Tunnel started successfully.
    Connected,

    /// Tunnel stopped.
    Disconnected,

    /// Traffic statistics.
    Stats { tx_bytes: u64, rx_bytes: u64 },

    /// Current tunnel status.
    Status {
        is_running: bool,
        last_handshake_secs: Option<i64>,
    },

    /// Error occurred while processing command.
    Error(String),
}
