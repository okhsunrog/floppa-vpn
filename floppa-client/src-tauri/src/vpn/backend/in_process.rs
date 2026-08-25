//! In-process VPN backend.
//!
//! The tunnel runs directly in the current process using gotatun.
//! Used on desktop platforms (Linux, Windows, macOS).

use super::{BackendError, VpnBackend};
use crate::vpn::actor::types::{Observation, RunningTunnel, TunnelObservation, WorldView};
use crate::vpn::platform::TunParams;
use crate::vpn::state::ProtocolConfig;
use crate::vpn::tunnel::TunnelManager;
use async_trait::async_trait;

pub struct InProcessBackend {
    tunnel_manager: TunnelManager,
}

impl InProcessBackend {
    pub fn new() -> Self {
        Self {
            tunnel_manager: TunnelManager::default(),
        }
    }
}

#[async_trait]
impl VpnBackend for InProcessBackend {
    async fn start(
        &self,
        config: &ProtocolConfig,
        interface_name: &str,
        tun_params: &TunParams,
        endpoint: std::net::SocketAddr,
    ) -> Result<(), BackendError> {
        match config {
            ProtocolConfig::WireGuard(wg) => {
                self.tunnel_manager
                    .start_wireguard(wg.tunnel(), interface_name, tun_params, endpoint)
                    .await
            }
            ProtocolConfig::AmneziaWg(awg) => {
                self.tunnel_manager
                    .start_wireguard(&awg.tunnel(), interface_name, tun_params, endpoint)
                    .await
            }
            ProtocolConfig::Vless(vless) => {
                self.tunnel_manager
                    .start_vless(&vless.to_shoes_config(), interface_name, tun_params)
                    .await
            }
        }
    }

    async fn stop(&self) -> Result<(), BackendError> {
        self.tunnel_manager.stop().await
    }

    async fn ping(&self) -> Result<(), BackendError> {
        self.tunnel_manager.ping().await
    }

    async fn probe(&self) -> Result<(), BackendError> {
        self.tunnel_manager.probe().await
    }

    /// Always reachable by construction: the tunnel is in this process, so an answer is always
    /// authoritative and there is no such thing as a dark observation here.
    async fn observe(&self) -> Observation {
        let stats = self.tunnel_manager.get_stats().await;
        let running = match self.tunnel_manager.meta().await {
            Some(meta) => Some(RunningTunnel {
                protocol: meta.protocol,
                generation: None,
                endpoint: meta.endpoint,
                address: meta.address,
                connected_secs: self
                    .tunnel_manager
                    .get_connection_duration()
                    .await
                    .map(|d| d.as_secs()),
                // In-process, the actor is the only thing that ever starts a tunnel, and it
                // keeps the params itself; a tunnel found here after a restart cannot exist.
                params: None,
                autonomous: false,
                silent_secs: self
                    .tunnel_manager
                    .silence()
                    .await
                    .map(|d| d.as_secs() as i64),
            }),
            None => None,
        };

        Observation {
            observed_at: std::time::Instant::now(),
            view: WorldView::Reachable(TunnelObservation {
                // In-process: there is no separate service to have generations of.
                generation: 0,
                running,
                starting: false,
                // In-process there is no descriptor hand-over to wait for.
                tun_ready: true,
                start_error: None,
                raw_stats: stats,
                last_packet_secs: self.tunnel_manager.get_last_packet_received().await,
            }),
        }
    }
}
