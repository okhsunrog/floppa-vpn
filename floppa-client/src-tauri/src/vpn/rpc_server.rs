//! tarpc server for the `:vpn` process.
//!
//! Runs in the VPN service process, accepts connections from the UI process,
//! and delegates RPC calls to the local TunnelManager.

use super::rpc::{RunningInfo, TunnelInfo, VpnRpc};
pub use super::rpc_listener::RpcServerHandle;
use super::state::ProtocolConfig;
use super::tunnel::TunnelManager;
use crate::vpn::actor::types::TunnelParams;
use futures::StreamExt;
use std::net::SocketAddr;
use std::sync::Arc;
use tarpc::context::Context;
use tarpc::server::Channel;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, error, info, warn};

/// How the tunnel on this service's descriptor was started, reported back with every
/// observation so the UI process learns it from the owner rather than guessing.
#[derive(Debug, Clone)]
pub struct Started {
    pub params: TunnelParams,
    pub autonomous: bool,
}

/// The descriptor's life in this service: not yet established, held, or already handed to a
/// tunnel. Three states rather than an `Option`, because "not yet" and "already used" both read
/// as `None` and mean opposite things to a start request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FdSlot {
    NotEstablished,
    Ready(std::os::fd::RawFd),
    Taken,
}

/// What the service is holding before any tunnel exists.
///
/// This only has anywhere to live because the RPC server binds ahead of everything else — ahead
/// of `establish()` too. Before that, "the service is coming up", "the service is up and idle",
/// "the TUN could not be established" and "the service failed" were all a socket that would not
/// connect, and the caller could only wait and guess.
pub struct ServiceState {
    /// Generation of the service, taken from the request that started it.
    pub epoch: u64,
    /// The descriptor handed over by `VpnService.Builder.establish()`, once it has been.
    tun_fd: std::sync::Mutex<FdSlot>,
    start_error: std::sync::Mutex<Option<String>>,
    /// Set once a tunnel is up on the descriptor.
    started: std::sync::Mutex<Option<Started>>,
}

impl ServiceState {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            tun_fd: std::sync::Mutex::new(FdSlot::NotEstablished),
            start_error: std::sync::Mutex::new(None),
            started: std::sync::Mutex::new(None),
        }
    }

    /// The service has established its TUN; from now on a tunnel can be started on it.
    pub fn set_fd(&self, fd: std::os::fd::RawFd) {
        if let Ok(mut guard) = self.tun_fd.lock() {
            *guard = FdSlot::Ready(fd);
        }
    }

    /// Established, or already running a tunnel — either way, not "still waiting for the TUN".
    pub fn tun_ready(&self) -> bool {
        self.tun_fd
            .lock()
            .map(|g| !matches!(*g, FdSlot::NotEstablished))
            .unwrap_or(false)
    }

    fn take_fd(&self) -> Result<std::os::fd::RawFd, &'static str> {
        let mut guard = self
            .tun_fd
            .lock()
            .map_err(|_| "the descriptor lock is poisoned")?;
        match *guard {
            FdSlot::Ready(fd) => {
                *guard = FdSlot::Taken;
                Ok(fd)
            }
            FdSlot::NotEstablished => Err("the tunnel descriptor has not been established yet"),
            FdSlot::Taken => Err("the tunnel descriptor has already been used"),
        }
    }

    /// Record why this generation could not start, for whoever observes next. Public because the
    /// one failure the Rust side cannot see itself — `establish()` — is reported from Kotlin.
    pub fn set_error(&self, error: String) {
        if let Ok(mut guard) = self.start_error.lock() {
            *guard = Some(error);
        }
    }

    fn error(&self) -> Option<String> {
        self.start_error.lock().ok()?.clone()
    }

    fn set_started(&self, started: Started) {
        if let Ok(mut guard) = self.started.lock() {
            *guard = Some(started);
        }
    }

    fn started(&self) -> Option<Started> {
        self.started.lock().ok()?.clone()
    }
}

/// Bring a tunnel up on the descriptor this service holds.
///
/// The one start path. The RPC `start_tunnel` reaches it after checking the request's epoch; an
/// autonomous start (`nativeStartTunnelFromBundle`) reaches it with what the bundle held. Whatever
/// the route in, what is recorded about the running tunnel — its params, whether anyone asked for
/// it — and what the notification says come out of this function alone.
///
/// The protocol arrives with the config instead of being recovered by inspecting it, and the
/// endpoint arrives already resolved: by now this device's DNS points into a tunnel that does not
/// exist yet, so resolving here would fail.
pub async fn bring_up(
    service: &ServiceState,
    tunnel_manager: &TunnelManager,
    mut config: ProtocolConfig,
    endpoint: SocketAddr,
    started: Started,
) -> Result<(), String> {
    let tun_fd = match service.take_fd() {
        Ok(fd) => fd,
        Err(why) => {
            let e = why.to_string();
            service.set_error(e.clone());
            return Err(e);
        }
    };

    // Through the parser rather than by field, so an IPv6 literal is bracketed the way the
    // `.conf` grammar wants it.
    let literal = || {
        endpoint
            .to_string()
            .parse::<floppa_tunnel_config::Endpoint>()
            .map_err(|e| format!("endpoint `{endpoint}`: {e}"))
    };
    match &mut config {
        ProtocolConfig::WireGuard(wg) => wg.set_endpoint(literal()?),
        ProtocolConfig::AmneziaWg(awg) => awg.wg.set_endpoint(literal()?),
        // VLESS dials by address while taking its SNI from `server_name`, so substituting a
        // literal here does not disturb the REALITY handshake.
        ProtocolConfig::Vless(vless) => vless.server_addr = endpoint.to_string(),
    }
    let result = match &config {
        ProtocolConfig::Vless(vless) => {
            tunnel_manager
                .start_vless_with_fd(&vless.to_shoes_config(), tun_fd)
                .await
        }
        ProtocolConfig::AmneziaWg(awg) => {
            tunnel_manager
                .start_wireguard_with_fd(&awg.tunnel(), tun_fd)
                .await
        }
        ProtocolConfig::WireGuard(wg) => {
            tunnel_manager
                .start_wireguard_with_fd(wg.tunnel(), tun_fd)
                .await
        }
    };

    match result.map_err(|e| e.to_string()) {
        Ok(()) => {
            info!(
                protocol = %config.protocol(),
                autonomous = started.autonomous,
                "tunnel started"
            );
            service.set_started(started);
            // The notification has been saying "connecting" since the service came up; this is
            // the first moment it is entitled to say anything else.
            super::jni_entry::set_service_connected(true);
            Ok(())
        }
        Err(e) => {
            // Recorded as well as returned: the caller may already have given up, and the next
            // observation should still say why rather than looking like an idle service.
            error!("failed to start the tunnel: {e}");
            service.set_error(e.clone());
            Err(e)
        }
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
        // `started` is written before the tunnel is observable and read after, so a running tunnel
        // without it cannot occur; the default is there to keep the match total, not to be seen.
        let running = self.tunnel_manager.meta().await.map(|m| {
            let started = self.service.started().unwrap_or_else(|| {
                warn!("a tunnel is running but nothing recorded how it was started");
                Started {
                    params: TunnelParams::default(),
                    autonomous: false,
                }
            });
            RunningInfo {
                protocol: m.protocol,
                endpoint: m.endpoint,
                address: m.address,
                connected_secs,
                params: started.params,
                autonomous: started.autonomous,
            }
        });
        let start_error = self.service.error();
        TunnelInfo {
            // Up and idle, with no tunnel asked for yet and nothing having gone wrong. Only
            // reachable because the socket is bound before the tunnel is started.
            starting: running.is_none() && start_error.is_none(),
            tun_ready: self.service.tun_ready(),
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
        params: TunnelParams,
    ) -> Result<(), String> {
        // A request for a generation we have moved past is not ours to obey.
        if epoch != self.service.epoch {
            return Err(format!(
                "stale request: epoch {epoch}, this service is {}",
                self.service.epoch
            ));
        }
        let endpoint: SocketAddr = endpoint
            .parse()
            .map_err(|e| format!("endpoint `{endpoint}`: {e}"))?;
        bring_up(
            &self.service,
            &self.tunnel_manager,
            config.into(),
            endpoint,
            Started {
                params,
                autonomous: false,
            },
        )
        .await
    }

    async fn stop(self, _ctx: Context) -> Result<(), String> {
        let result = self.tunnel_manager.stop().await.map_err(|e| e.to_string());

        // Stop the Android VPN service (foreground notification, TUN, stopSelf)
        #[cfg(target_os = "android")]
        super::jni_entry::stop_vpn_service();

        result
    }

    async fn ping(self, _ctx: Context) -> Result<(), String> {
        self.tunnel_manager.ping().await.map_err(|e| e.to_string())
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
/// Returns a handle that stops the server when shut down or dropped. The accept loop runs in a
/// background tokio task (see [`super::rpc_listener`]); each connection gets a tarpc channel here.
pub fn start_server(
    socket_path: &str,
    tunnel_manager: Arc<TunnelManager>,
    service: Arc<ServiceState>,
) -> Result<RpcServerHandle, String> {
    let server = VpnRpcServer {
        tunnel_manager,
        service,
    };
    super::rpc_listener::listen(std::path::Path::new(socket_path), move |stream| {
        debug!("UI process connected to RPC server");
        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport =
            tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());
        let channel = tarpc::server::BaseChannel::with_defaults(transport);
        let server = server.clone();
        tokio::spawn(async move {
            channel
                .execute(server.serve())
                .for_each(|resp| async {
                    tokio::spawn(resp);
                })
                .await;
            debug!("UI process disconnected from RPC server");
        });
    })
    .map_err(|e| format!("Failed to bind Unix socket at {socket_path}: {e}"))
}
