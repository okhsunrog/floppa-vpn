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

use super::{VpnBackend, VpnFullInfo};
use crate::vpn::rpc::VpnRpcClient;
use crate::vpn::state::{ProtocolConfig, TrafficStats};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, warn};

pub struct AndroidIpcBackend {
    socket_path: String,
    client: Mutex<Option<VpnRpcClient>>,
    /// Tracks whether the last connection attempt failed, to suppress repeated log messages.
    /// Only the first failure and recovery are logged.
    last_connect_failed: AtomicBool,
}

impl AndroidIpcBackend {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            client: Mutex::new(None),
            last_connect_failed: AtomicBool::new(false),
        }
    }

    /// Get or create a tarpc client connection.
    /// Lazily connects on first use, reconnects on error.
    async fn get_client(&self) -> Result<VpnRpcClient, String> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| format!("Failed to connect to VPN socket: {e}"))?;

        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport =
            tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());

        let client = VpnRpcClient::new(tarpc::client::Config::default(), transport).spawn();

        *guard = Some(client.clone());
        Ok(client)
    }

    /// Invalidate the cached client (e.g. after an RPC error).
    async fn invalidate_client(&self) {
        *self.client.lock().await = None;
    }
}

#[async_trait]
impl VpnBackend for AndroidIpcBackend {
    async fn start(
        &self,
        _config: &ProtocolConfig,
        _interface_name: &str,
        _fwmark: Option<u32>,
        _endpoint: std::net::SocketAddr,
    ) -> Result<(), String> {
        // On Android, the tunnel starts via JNI in the :vpn process.
        // The Kotlin VpnPlugin.startVpn() launches FloppaVpnService which
        // calls nativeStartTunnel() with the TUN fd and WG config.
        Err("On Android, tunnel starts via VpnService JNI, not through backend.start()".into())
    }

    async fn start_with_fd(&self, _config: &ProtocolConfig, _tun_fd: i32) -> Result<(), String> {
        // Same as start() — not used in two-process architecture.
        Err(
            "On Android, tunnel starts via VpnService JNI, not through backend.start_with_fd()"
                .into(),
        )
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

    async fn get_all_info(&self) -> Option<VpnFullInfo> {
        let client = match self.get_client().await {
            Ok(c) => {
                // Log recovery if previously failing
                if self.last_connect_failed.swap(false, Ordering::Relaxed) {
                    debug!("Reconnected to :vpn process");
                }
                c
            }
            Err(_) => {
                // Log only the first failure in a streak
                if !self.last_connect_failed.swap(true, Ordering::Relaxed) {
                    debug!("VPN service not running");
                }
                return None;
            }
        };

        match client.get_full_info(tarpc::context::current()).await {
            Ok(info) => Some(VpnFullInfo {
                is_running: info.is_running,
                stats: match (info.tx_bytes, info.rx_bytes) {
                    (Some(tx), Some(rx)) => Some(TrafficStats {
                        tx_bytes: tx,
                        rx_bytes: rx,
                        ..Default::default()
                    }),
                    _ => None,
                },
                last_handshake: info.last_handshake,
                connected_secs: info.connected_secs,
            }),
            Err(e) => {
                warn!("RPC get_full_info failed: {e}");
                self.invalidate_client().await;
                None
            }
        }
    }
}
