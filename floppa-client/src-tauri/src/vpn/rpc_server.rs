//! tarpc server for the `:vpn` process.
//!
//! Runs in the VPN service process, accepts connections from the UI process,
//! and delegates RPC calls to the local TunnelManager.

use super::rpc::{RunningInfo, TunnelInfo, VpnRpc};
use super::state::ProtocolConfig;
use super::tunnel::TunnelManager;
use futures::StreamExt;
use std::path::PathBuf;
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
    socket_path: PathBuf,
}

impl RpcServerHandle {
    /// Stop accepting connections. The socket file is left in place.
    ///
    /// Deliberately: the path is shared between service generations, and by the time the previous
    /// generation is shut down the next one has usually already bound the same path (see
    /// `SERVER_EPOCH` in `jni_entry.rs`). Unlinking here would remove *its* socket.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Stop accepting connections and unlink the socket file.
    ///
    /// Only for the generation that owns the path — i.e. a `nativeStop` whose epoch matched — and
    /// never from a handle that has been superseded.
    pub fn shutdown_and_unlink(self) {
        self.shutdown();
        match std::fs::remove_file(&self.socket_path) {
            Ok(()) => debug!("removed socket {}", self.socket_path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                "failed to remove socket {}: {e}",
                self.socket_path.display()
            ),
        }
    }
}

/// What the service is holding before any tunnel exists.
///
/// This only has anywhere to live because the RPC server now binds ahead of the tunnel. Before
/// that, "the service is coming up", "the service is up and idle" and "the service failed" were
/// all a socket that would not connect, and the caller could only wait and guess.
pub struct ServiceState {
    /// Generation of the service, taken from the request that started it.
    pub epoch: u64,
    /// The descriptor handed over by `VpnService.Builder.establish()`.
    tun_fd: std::sync::Mutex<Option<std::os::fd::RawFd>>,
    start_error: std::sync::Mutex<Option<String>>,
}

impl ServiceState {
    pub fn new(epoch: u64, tun_fd: std::os::fd::RawFd) -> Self {
        Self {
            epoch,
            tun_fd: std::sync::Mutex::new(Some(tun_fd)),
            start_error: std::sync::Mutex::new(None),
        }
    }

    fn take_fd(&self) -> Option<std::os::fd::RawFd> {
        self.tun_fd.lock().ok()?.take()
    }

    fn set_error(&self, error: String) {
        if let Ok(mut guard) = self.start_error.lock() {
            *guard = Some(error);
        }
    }

    fn error(&self) -> Option<String> {
        self.start_error.lock().ok()?.clone()
    }
}

#[derive(Clone)]
struct VpnRpcServer {
    tunnel_manager: Arc<TunnelManager>,
    service: Arc<ServiceState>,
}

impl VpnRpc for VpnRpcServer {
    async fn get_full_info(self, _ctx: Context) -> TunnelInfo {
        let connected_secs = self
            .tunnel_manager
            .get_connection_duration()
            .await
            .map(|d| d.as_secs());
        let stats = self.tunnel_manager.get_stats().await;
        // Running-ness and identity come out of the same Option, which is why they are one field.
        let running = self.tunnel_manager.meta().await.map(|m| RunningInfo {
            protocol: m.protocol,
            endpoint: m.endpoint,
            address: m.address,
            connected_secs,
        });
        let start_error = self.service.error();
        TunnelInfo {
            // Up and idle, with no tunnel asked for yet and nothing having gone wrong. Only
            // reachable because the socket is bound before the tunnel is started.
            starting: running.is_none() && start_error.is_none(),
            running,
            epoch: self.service.epoch,
            start_error,
            last_packet_received: self.tunnel_manager.get_last_packet_received().await,
            tx_bytes: stats.as_ref().map(|s| s.tx_bytes),
            rx_bytes: stats.as_ref().map(|s| s.rx_bytes),
        }
    }

    async fn start_tunnel(
        self,
        _ctx: Context,
        epoch: u64,
        config: crate::vpn::rpc::WireConfig,
        endpoint: String,
    ) -> Result<(), String> {
        // A request for a generation we have moved past is not ours to obey.
        if epoch != self.service.epoch {
            return Err(format!(
                "stale request: epoch {epoch}, this service is {}",
                self.service.epoch
            ));
        }

        let Some(tun_fd) = self.service.take_fd() else {
            let e = "the tunnel descriptor has already been used".to_string();
            self.service.set_error(e.clone());
            return Err(e);
        };

        // The protocol arrives with the config instead of being recovered by inspecting it, and
        // the endpoint arrives already resolved: by now this device's DNS points into a tunnel
        // that does not exist yet, so resolving here would fail.
        let mut config: ProtocolConfig = config.into();
        match &mut config {
            ProtocolConfig::WireGuard(wg) => wg.peer_endpoint = endpoint,
            ProtocolConfig::AmneziaWg(awg) => awg.wg.peer_endpoint = endpoint,
            // VLESS dials by address while taking its SNI from `server_name`, so substituting a
            // literal here does not disturb the REALITY handshake.
            ProtocolConfig::Vless(vless) => vless.server_addr = endpoint,
        }
        let result = match &config {
            ProtocolConfig::Vless(vless) => {
                self.tunnel_manager
                    .start_vless_with_fd(&vless.to_shoes_config(), tun_fd)
                    .await
            }
            ProtocolConfig::AmneziaWg(awg) => {
                self.tunnel_manager
                    .start_wireguard_with_fd(&awg.wg, tun_fd, Some(&awg.obfuscation))
                    .await
            }
            ProtocolConfig::WireGuard(wg) => {
                self.tunnel_manager
                    .start_wireguard_with_fd(wg, tun_fd, None)
                    .await
            }
        };

        match result {
            Ok(()) => {
                info!(protocol = %config.protocol(), "tunnel started");
                // The notification has been saying "connecting" since the service came up; this is
                // the first moment it is entitled to say anything else.
                #[cfg(target_os = "android")]
                super::jni_entry::set_service_connected(true);
                Ok(())
            }
            Err(e) => {
                // Recorded as well as returned: the caller may already have given up, and the next
                // observation should still say why rather than looking like an idle service.
                error!("failed to start the tunnel: {e}");
                self.service.set_error(e.clone());
                Err(e)
            }
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

    async fn set_log_config(self, _ctx: Context, config: crate::logging::LogConfig) {
        crate::logging::apply_log_config(&config);
    }

    async fn start_log_capture(self, _ctx: Context, capture_id: String) {
        let Some(log_dir) = crate::logging::get_log_dir() else {
            warn!("Cannot start VPN log capture: log directory not initialized");
            return;
        };
        if let Err(e) = crate::logging::start_file_capture(log_dir, "vpn", &capture_id) {
            warn!("Failed to start VPN log capture: {e}");
        }
    }

    async fn stop_log_capture(self, _ctx: Context) {
        let _ = crate::logging::stop_file_capture();
    }
}

/// Start the tarpc server on a Unix domain socket.
///
/// Returns a handle that can be used to shut down the server.
/// The server runs in a background tokio task.
pub fn start_server(
    socket_path: &str,
    tunnel_manager: Arc<TunnelManager>,
    service: Arc<ServiceState>,
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

    let server = VpnRpcServer {
        tunnel_manager,
        service,
    };
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
        // The socket file is *not* unlinked here. This task exits asynchronously after
        // `shutdown()`, and `shutdown()` is what `nativeStartServer` calls on the previous
        // generation right after binding the new one to the same path. An unlink from here raced
        // that bind and won often enough to remove the new generation's socket, leaving the UI
        // with `NotFound` until it gave up. Unlinking is done only before a bind (above) and by
        // the owning generation's `nativeStop` via [`RpcServerHandle::shutdown_and_unlink`].
    });

    Ok(RpcServerHandle {
        shutdown_tx,
        socket_path: PathBuf::from(socket_path),
    })
}
