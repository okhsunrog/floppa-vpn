//! tarpc server for the `:vpn` process.
//!
//! Runs in the VPN service process, accepts connections from the UI process,
//! and delegates RPC calls to the local TunnelManager.

use super::rpc::{TunnelInfo, VpnRpc};
use super::tunnel::TunnelManager;
use futures::StreamExt;
use std::sync::Arc;
use tarpc::context::Context;
use tarpc::server::Channel;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, error, info, warn};

/// Handle to a running RPC server. Drop or call `shutdown()` to stop it.
pub struct RpcServerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl RpcServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

#[derive(Clone)]
struct VpnRpcServer {
    tunnel_manager: Arc<TunnelManager>,
}

impl VpnRpc for VpnRpcServer {
    async fn get_full_info(self, _ctx: Context) -> TunnelInfo {
        let is_running = self.tunnel_manager.is_running().await;
        let last_packet_received = self.tunnel_manager.get_last_packet_received().await;
        let connected_secs = self
            .tunnel_manager
            .get_connection_duration()
            .await
            .map(|d| d.as_secs());
        let stats = self.tunnel_manager.get_stats().await;
        TunnelInfo {
            is_running,
            last_packet_received,
            connected_secs,
            tx_bytes: stats.as_ref().map(|s| s.tx_bytes),
            rx_bytes: stats.as_ref().map(|s| s.rx_bytes),
        }
    }

    async fn stop(self, _ctx: Context) -> Result<(), String> {
        let result = self.tunnel_manager.stop().await;

        // Stop the Android VPN service (foreground notification, TUN, stopSelf)
        #[cfg(target_os = "android")]
        super::jni_entry::stop_vpn_service();

        result
    }

    async fn ping(self, _ctx: Context) -> Result<(), String> {
        self.tunnel_manager.ping().await
    }
}

/// Start the tarpc server on a Unix domain socket.
///
/// Returns a handle that can be used to shut down the server.
/// The server runs in a background tokio task.
pub fn start_server(
    socket_path: &str,
    tunnel_manager: Arc<TunnelManager>,
) -> Result<RpcServerHandle, String> {
    // Remove stale socket file if it exists
    match std::fs::remove_file(socket_path) {
        Ok(()) => debug!("Removed stale socket: {socket_path}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Failed to remove stale socket {socket_path}: {e}"),
    }

    let listener = UnixListener::bind(socket_path)
        .map_err(|e| format!("Failed to bind Unix socket at {socket_path}: {e}"))?;

    info!("tarpc server listening on {socket_path}");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let server = VpnRpcServer { tunnel_manager };
    let socket_path_owned = socket_path.to_owned();

    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _addr)) => {
                            debug!("UI process connected to RPC server");
                            let framed = LengthDelimitedCodec::builder().new_framed(stream);
                            let transport = tarpc::serde_transport::new(
                                framed,
                                tokio_serde::formats::Bincode::default(),
                            );
                            let channel = tarpc::server::BaseChannel::with_defaults(transport);
                            let server = server.clone();
                            tokio::spawn(async move {
                                channel.execute(server.serve())
                                    .for_each(|resp| async { tokio::spawn(resp); })
                                    .await;
                                debug!("UI process disconnected from RPC server");
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {e}");
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("tarpc server shutting down");
                        break;
                    }
                }
            }
        }
        // Clean up socket file
        let _ = std::fs::remove_file(&socket_path_owned);
    });

    Ok(RpcServerHandle { shutdown_tx })
}
