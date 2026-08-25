//! The Android backend, seen from inside the service that owns the tunnel.
//!
//! This is what the actor talks to now that it lives in `:vpn`. Every question it used to ask over
//! the socket — is a tunnel running, what is it, how quiet is the far side — is a local read here,
//! and the answers are authoritative by construction: there is no such thing as an unreachable
//! observation when the thing being observed is in this process.
//!
//! What remains cross-process is the *descriptor*. Only `VpnService.Builder.establish()` can make
//! one, only Kotlin can call it, and it arrives asynchronously — so the ladder still asks the
//! [`ServiceHost`](crate::vpn::host::ServiceHost) to start a generation and then polls until this
//! backend reports that generation holding a descriptor. That handshake is unchanged; only its two
//! ends have moved into one process.

use super::{BackendError, VpnBackend};
use crate::vpn::actor::types::{
    Observation, RawStats, RunningTunnel, TunnelObservation, TunnelParams, WorldView,
};
use crate::vpn::platform::TunParams;
use crate::vpn::service_state::{GenerationPhase, ServiceRegistry, ServiceState, Started};
use crate::vpn::state::ProtocolConfig;
use crate::vpn::tunnel::TunnelManager;
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};

/// Bring a tunnel up on the descriptor the service is holding.
///
/// The one start path, whoever asked. What is recorded about the running tunnel — its rules,
/// whether anyone asked for it — and what the notification says come out of this function alone.
///
/// The protocol arrives with the config rather than being recovered by inspecting it, and the
/// endpoint arrives already resolved: by the time a tunnel is being started, this device's DNS
/// points into a tunnel that does not exist yet, so resolving here would fail.
async fn bring_up(
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
            crate::vpn::jni_entry::set_service_connected(true);
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

pub struct AndroidServiceBackend {
    tunnel_manager: Arc<TunnelManager>,
    services: Arc<ServiceRegistry>,
}

impl AndroidServiceBackend {
    pub fn new(tunnel_manager: Arc<TunnelManager>, services: Arc<ServiceRegistry>) -> Self {
        Self {
            tunnel_manager,
            services,
        }
    }
}

#[async_trait]
impl VpnBackend for AndroidServiceBackend {
    /// Not this backend's job: on Android a tunnel is built on a descriptor the service hands
    /// over, which is what [`Self::start_tunnel`] does.
    async fn start(
        &self,
        _config: &ProtocolConfig,
        _interface_name: &str,
        _tun_params: &TunParams,
        _endpoint: std::net::SocketAddr,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn start_tunnel(
        &self,
        generation: u64,
        config: &ProtocolConfig,
        endpoint: std::net::SocketAddr,
        params: &TunnelParams,
    ) -> Result<(), BackendError> {
        // A request naming a generation this process has moved past is not ours to obey — the
        // descriptor it means belongs to an instance that is already gone.
        let service = self.services.serving(generation).ok_or_else(|| {
            BackendError::ServiceRefused {
                detail: format!("generation {generation} is no longer being served"),
            }
        })?;
        bring_up(
            &service,
            &self.tunnel_manager,
            config.clone(),
            endpoint,
            Started {
                params: params.clone(),
                autonomous: false,
            },
        )
        .await
        .map_err(|detail| BackendError::ServiceRefused { detail })
    }

    async fn stop(&self) -> Result<(), BackendError> {
        let result = self.tunnel_manager.stop().await;
        if let Some(service) = self.services.current() {
            self.services.end(service.generation);
        }
        // The Android side goes too: the notification, the descriptor and the started lifecycle
        // are all one unit, and leaving them standing over a stopped tunnel is a route to nowhere
        // with a notification on top.
        crate::vpn::jni_entry::stop_vpn_service();
        result
    }

    async fn observe(&self) -> Observation {
        let service = self.services.current();
        let stats = self.tunnel_manager.get_stats().await;
        let connected_secs = self
            .tunnel_manager
            .get_connection_duration()
            .await
            .map(|d| d.as_secs());
        let silent_secs = self
            .tunnel_manager
            .silence()
            .await
            .map(|d| d.as_secs() as i64);

        // Running-ness and identity come out of the same read, so they cannot disagree. `started`
        // is written before a tunnel is observable, so a running tunnel without it cannot happen;
        // the fallback is there to keep this total.
        let running = self.tunnel_manager.meta().await.map(|m| {
            let started = service.as_ref().and_then(|s| s.started());
            RunningTunnel {
                protocol: m.protocol,
                generation: service.as_ref().map(|s| s.generation),
                endpoint: m.endpoint,
                address: m.address,
                connected_secs,
                params: started.as_ref().map(|s| s.params.clone()),
                autonomous: started.as_ref().is_some_and(|s| s.autonomous),
                silent_secs,
            }
        });

        Observation {
            observed_at: std::time::Instant::now(),
            view: WorldView::Reachable(TunnelObservation {
                generation: service.as_ref().map(|s| s.generation).unwrap_or_default(),
                running,
                starting: service.as_ref().is_some_and(|s| s.starting()),
                tun_ready: service.as_ref().is_some_and(|s| s.tun_ready()),
                start_error: service.as_ref().and_then(|s| s.error()),
                raw_stats: stats.map(|s| RawStats {
                    tx_bytes: s.tx_bytes,
                    rx_bytes: s.rx_bytes,
                }),
                last_packet_secs: self.tunnel_manager.get_last_packet_received().await,
            }),
        }
    }

    async fn ping(&self) -> Result<(), BackendError> {
        self.tunnel_manager.ping().await
    }

    async fn probe(&self) -> Result<(), BackendError> {
        self.tunnel_manager.probe().await
    }
}
