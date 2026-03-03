//! Android IPC backend.
//!
//! Communicates with a separate `:vpn` process via tarpc over Unix domain socket.
//! The VPN process runs as an Android foreground Service, keeping the tunnel alive
//! even when the UI (Tauri) process is killed by the system or user swipe.
//!
//! ```text
//! UI Process (Tauri)              :vpn Process (Android Service)
//! ┌──────────────────┐           ┌─────────────────────────────┐
//! │ AndroidIpcBackend│──tarpc──→ │ tarpc server                │
//! │ (this file)      │  (UDS)    │    └─ GotatunTunnel         │
//! └──────────────────┘           └─────────────────────────────┘
//! ```

use super::VpnBackend;
use crate::vpn::rpc::VpnRpcClient;
use crate::vpn::state::{TrafficStats, WgConfig};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, warn};

pub struct AndroidIpcBackend {
    socket_path: String,
    client: Mutex<Option<VpnRpcClient>>,
}

impl AndroidIpcBackend {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            client: Mutex::new(None),
        }
    }

    /// Get or create a tarpc client connection.
    /// Lazily connects on first use, reconnects on error.
    async fn get_client(&self) -> Result<VpnRpcClient, String> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        debug!("Connecting to VPN service at {}", self.socket_path);

        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| format!("Failed to connect to VPN socket: {e}"))?;

        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport = tarpc::serde_transport::new(
            framed,
            tokio_serde::formats::Bincode::default(),
        );

        let client = VpnRpcClient::new(tarpc::client::Config::default(), transport).spawn();

        *guard = Some(client.clone());
        Ok(client)
    }

    /// Invalidate the cached client (e.g. after an error).
    async fn invalidate_client(&self) {
        *self.client.lock().await = None;
    }
}

#[async_trait]
impl VpnBackend for AndroidIpcBackend {
    async fn start(
        &self,
        _config: &WgConfig,
        _interface_name: &str,
        _fwmark: Option<u32>,
    ) -> Result<(), String> {
        // On Android, the tunnel starts via JNI in the :vpn process.
        // The Kotlin VpnPlugin.startVpn() launches FloppaVpnService which
        // calls nativeStartTunnel() with the TUN fd and WG config.
        Err("On Android, tunnel starts via VpnService JNI, not through backend.start()".into())
    }

    async fn start_with_fd(&self, _config: &WgConfig, _tun_fd: i32) -> Result<(), String> {
        // Same as start() — not used in two-process architecture.
        Err("On Android, tunnel starts via VpnService JNI, not through backend.start_with_fd()".into())
    }

    async fn stop(&self) -> Result<(), String> {
        let client = self.get_client().await?;
        match client.stop(tarpc::context::current()).await {
            Ok(result) => {
                self.invalidate_client().await;
                result
            }
            Err(e) => {
                self.invalidate_client().await;
                Err(format!("RPC error: {e}"))
            }
        }
    }

    async fn is_running(&self) -> bool {
        let client = match self.get_client().await {
            Ok(c) => c,
            Err(e) => {
                debug!("Cannot reach :vpn process: {e}");
                return false;
            }
        };
        match client.get_status(tarpc::context::current()).await {
            Ok((is_running, _, _)) => is_running,
            Err(e) => {
                warn!("Failed to get VPN status via tarpc: {e}");
                self.invalidate_client().await;
                false
            }
        }
    }

    async fn get_stats(&self) -> Option<TrafficStats> {
        let client = match self.get_client().await {
            Ok(c) => c,
            Err(e) => {
                debug!("Cannot reach :vpn process for stats: {e}");
                return None;
            }
        };
        match client.get_stats(tarpc::context::current()).await {
            Ok(Some((tx_bytes, rx_bytes))) => Some(TrafficStats {
                tx_bytes,
                rx_bytes,
                ..Default::default()
            }),
            Ok(None) => None,
            Err(e) => {
                warn!("Failed to get VPN stats via tarpc: {e}");
                self.invalidate_client().await;
                None
            }
        }
    }

    async fn get_last_handshake(&self) -> Option<i64> {
        let client = match self.get_client().await {
            Ok(c) => c,
            Err(e) => {
                debug!("Cannot reach :vpn process for handshake: {e}");
                return None;
            }
        };
        match client.get_status(tarpc::context::current()).await {
            Ok((_, last_handshake, _)) => last_handshake,
            Err(e) => {
                warn!("Failed to get VPN status via tarpc: {e}");
                self.invalidate_client().await;
                None
            }
        }
    }

    async fn get_interface_name(&self) -> Option<String> {
        // On Android, the interface name is always "tun0" (assigned by VpnService).
        if self.is_running().await {
            Some("tun0".to_string())
        } else {
            None
        }
    }

    async fn get_connected_secs(&self) -> Option<u64> {
        let client = match self.get_client().await {
            Ok(c) => c,
            Err(_) => return None,
        };
        match client.get_status(tarpc::context::current()).await {
            Ok((_, _, connected_secs)) => connected_secs,
            Err(e) => {
                warn!("Failed to get connected_secs via tarpc: {e}");
                self.invalidate_client().await;
                None
            }
        }
    }
}
