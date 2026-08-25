//! tarpc server for the `:vpn` process.
//!
//! Runs in the VPN service process, accepts connections from the UI process,
//! and delegates RPC calls to the local TunnelManager.

use super::rpc::{RunningInfo, TunnelInfo, VpnRpc};
pub use super::rpc_listener::RpcServerHandle;
pub use super::service_state::{GenerationPhase, ServiceState, Started};
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

/// Bring a tunnel up on the descriptor this service holds.
///
/// The one start path. The RPC `start_tunnel` reaches it after checking the request's generation; an
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
            service.advance_to(GenerationPhase::Started);
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
            // Read off the generation's own phase rather than inferred from the absence of a
            // tunnel, which a *stopped* generation also looks like.
            starting: self.service.starting(),
            tun_ready: self.service.tun_ready(),
            running,
            generation: self.service.generation,
            start_error,
            last_packet_received: self.tunnel_manager.get_last_packet_received().await,
            silent_secs: self
                .tunnel_manager
                .silence()
                .await
                .map(|d| d.as_secs() as i64),
            tx_bytes: stats.as_ref().map(|s| s.tx_bytes),
            rx_bytes: stats.as_ref().map(|s| s.rx_bytes),
        }
    }

    async fn start_tunnel(
        self,
        _ctx: Context,
        generation: u64,
        config: crate::vpn::rpc::WireConfig,
        endpoint: String,
        params: TunnelParams,
    ) -> Result<(), String> {
        // A request for a generation we have moved past is not ours to obey.
        if generation != self.service.generation {
            return Err(format!(
                "stale request: generation {generation}, this service is {}",
                self.service.generation
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
        self.service.mark_stopped();

        // Stop the Android VPN service (foreground notification, TUN, stopSelf)
        #[cfg(target_os = "android")]
        super::jni_entry::stop_vpn_service();

        result
    }

    async fn ping(self, _ctx: Context) -> Result<(), String> {
        self.tunnel_manager.ping().await.map_err(|e| e.to_string())
    }

    async fn probe(self, _ctx: Context) -> Result<(), String> {
        self.tunnel_manager.probe().await.map_err(|e| e.to_string())
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
    super::rpc_listener::listen(std::path::Path::new(socket_path), move |stream, cancel| {
        debug!("UI process connected to RPC server");
        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport =
            tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());
        let channel = tarpc::server::BaseChannel::with_defaults(transport);
        let server = server.clone();
        tokio::spawn(async move {
            // The connection dies with its generation. Each of these tasks holds a clone of one
            // generation's state, and without the token they answered from it for as long as the
            // stream stayed open — so a client that had cached the connection went on talking to
            // an instance that had been torn down.
            tokio::select! {
                _ = channel.execute(server.serve()).for_each(|resp| async {
                    tokio::spawn(resp);
                }) => debug!("UI process disconnected from RPC server"),
                _ = cancel.cancelled() => {
                    debug!("this generation is gone; closing the connection it was serving");
                }
            }
        });
    })
    .map_err(|e| format!("Failed to bind Unix socket at {socket_path}: {e}"))
}
