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
use crate::vpn::rpc_server::bring_up;
use crate::vpn::service_state::{ServiceRegistry, Started};
use crate::vpn::state::ProtocolConfig;
use crate::vpn::tunnel::TunnelManager;
use async_trait::async_trait;
use std::sync::Arc;

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
