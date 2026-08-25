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

use super::{BackendError, VpnBackend};
use crate::vpn::actor::types::{
    Observation, RawStats, RunningTunnel, TunnelObservation, TunnelParams, UnreachableCause,
    WorldView,
};
use crate::vpn::rpc::VpnRpcClient;
use crate::vpn::state::ProtocolConfig;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri_plugin_vpn::VpnExt;
use tokio::sync::Mutex;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, warn};

pub struct AndroidIpcBackend {
    socket_path: String,
    /// For the out-of-band stop: the intent path through the plugin is the only way to reach a
    /// service instance whose RPC socket is not answering.
    app: tauri::AppHandle,
    client: Mutex<Option<VpnRpcClient>>,
    /// Tracks whether the last connection attempt failed, to suppress repeated log messages.
    /// Only the first failure and recovery are logged.
    last_connect_failed: AtomicBool,
}

impl AndroidIpcBackend {
    pub fn new(socket_path: String, app: tauri::AppHandle) -> Self {
        Self {
            socket_path,
            app,
            client: Mutex::new(None),
            last_connect_failed: AtomicBool::new(false),
        }
    }

    /// Stop the service through the plugin's intent path rather than the RPC.
    async fn stop_by_intent(&self) -> Result<(), BackendError> {
        self.app
            .vpn()
            .stop()
            .await
            .map_err(|e| BackendError::ServiceUnreachable {
                detail: format!("failed to stop the VPN service by intent: {e}"),
            })
    }

    /// Get or create a tarpc client connection.
    /// Lazily connects on first use, reconnects on error.
    async fn get_client(&self) -> Result<VpnRpcClient, String> {
        self.get_client_typed().await.map_err(|(_, msg)| msg)
    }

    /// As [`Self::get_client`], but keeping *why* the connection failed.
    ///
    /// The distinction is the whole point of the observation type: "nothing is listening" and
    /// "something refused us" are different facts, and neither of them means "there is no tunnel".
    async fn get_client_typed(&self) -> Result<VpnRpcClient, (UnreachableCause, String)> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| {
                let cause = match e.kind() {
                    std::io::ErrorKind::NotFound => UnreachableCause::NotStarted,
                    std::io::ErrorKind::ConnectionRefused => UnreachableCause::ConnectRefused,
                    _ => UnreachableCause::TransportBroken,
                };
                (cause, format!("Failed to connect to VPN socket: {e}"))
            })?;

        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport =
            tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());

        let client = VpnRpcClient::new(tarpc::client::Config::default(), transport).spawn();
        debug!("opened a new connection to the VPN service");

        *guard = Some(client.clone());
        Ok(client)
    }

    /// A per-call deadline of our own.
    ///
    /// tarpc's default is 10 seconds, which is longer than any sensible poll interval — so a wedged
    /// peer used to stall every observation behind it, and any debounce counted in polls became
    /// meaningless because the polls stopped arriving.
    fn deadline(timeout: std::time::Duration) -> tarpc::context::Context {
        let mut ctx = tarpc::context::current();
        ctx.deadline = std::time::Instant::now() + timeout;
        ctx
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
        _tun_params: &crate::vpn::platform::TunParams,
        _endpoint: std::net::SocketAddr,
    ) -> Result<(), BackendError> {
        // On Android the tunnel is started by the `:vpn` service, asked through `start_tunnel`.
        Err(BackendError::Unsupported)
    }

    async fn start_tunnel(
        &self,
        epoch: u64,
        config: &ProtocolConfig,
        endpoint: std::net::SocketAddr,
        params: &TunnelParams,
    ) -> Result<(), BackendError> {
        let client = self
            .get_client_typed()
            .await
            .map_err(|(_, detail)| BackendError::ServiceUnreachable { detail })?;

        // A generous deadline: this is the call that actually brings the tunnel up, unlike the
        // polls that only ask about it.
        debug!(%epoch, "sending start_tunnel");
        let ctx = Self::deadline(std::time::Duration::from_secs(15));
        let wire = crate::vpn::rpc::WireConfig::from(config);
        match client
            .start_tunnel(ctx, epoch, wire, endpoint.to_string(), params.clone())
            .await
        {
            Ok(result) => result.map_err(|detail| BackendError::ServiceRefused { detail }),
            Err(e) => {
                self.invalidate_client().await;
                Err(BackendError::ServiceUnreachable {
                    detail: format!("RPC error: {e}"),
                })
            }
        }
    }

    async fn stop(&self) -> Result<(), BackendError> {
        let client = match self.get_client_typed().await {
            Ok(client) => client,
            // Nothing is listening — which is not the same as nothing running. A service whose
            // RPC bind failed, or whose socket file was lost, is still foreground and holding an
            // established TUN with a default route into it, and the RPC is the one thing that
            // cannot reach it. This used to return Ok("nothing to stop"), leaving that instance
            // with no in-band way to be stopped at all. The intent path is cheap when the service
            // really is gone: it starts and immediately stops an empty instance.
            Err((UnreachableCause::NotStarted | UnreachableCause::ConnectRefused, reason)) => {
                debug!("RPC unreachable ({reason}); stopping the service by intent");
                return self.stop_by_intent().await;
            }
            Err((_, detail)) => return Err(BackendError::ServiceUnreachable { detail }),
        };

        match client.stop(tarpc::context::current()).await {
            Ok(result) => {
                self.invalidate_client().await;
                result.map_err(|detail| BackendError::ServiceRefused { detail })
            }
            Err(e) => {
                self.invalidate_client().await;
                Err(BackendError::ServiceUnreachable {
                    detail: format!("RPC error: {e}"),
                })
            }
        }
    }

    async fn ping(&self) -> Result<(), BackendError> {
        let client = self
            .get_client_typed()
            .await
            .map_err(|(_, detail)| BackendError::ServiceUnreachable { detail })?;
        match client.ping(tarpc::context::current()).await {
            Ok(result) => result.map_err(|detail| BackendError::ServiceRefused { detail }),
            Err(e) => {
                self.invalidate_client().await;
                Err(BackendError::ServiceUnreachable {
                    detail: format!("RPC error: {e}"),
                })
            }
        }
    }

    async fn set_log_config(&self, config: &crate::logging::LogConfig) {
        if let Ok(client) = self.get_client().await
            && let Err(e) = client
                .set_log_config(tarpc::context::current(), config.clone())
                .await
        {
            warn!("Failed to set log config on VPN process: {e}");
            self.invalidate_client().await;
        }
    }

    async fn start_log_capture(&self, capture_id: &str) {
        if let Ok(client) = self.get_client().await
            && let Err(e) = client
                .start_log_capture(tarpc::context::current(), capture_id.to_string())
                .await
        {
            warn!("Failed to start log capture on VPN process: {e}");
            self.invalidate_client().await;
        }
    }

    async fn stop_log_capture(&self) {
        if let Ok(client) = self.get_client().await
            && let Err(e) = client.stop_log_capture(tarpc::context::current()).await
        {
            warn!("Failed to stop log capture on VPN process: {e}");
            self.invalidate_client().await;
        }
    }

    async fn observe(&self) -> Observation {
        let observed_at = std::time::Instant::now();
        let unreachable = |cause| Observation {
            observed_at,
            view: WorldView::Unreachable(cause),
        };

        let client = match self.get_client_typed().await {
            Ok(c) => {
                if self.last_connect_failed.swap(false, Ordering::Relaxed) {
                    debug!("Reconnected to :vpn process");
                }
                c
            }
            Err((cause, _)) => {
                if !self.last_connect_failed.swap(true, Ordering::Relaxed) {
                    debug!("VPN service not reachable: {cause:?}");
                }
                return unreachable(cause);
            }
        };

        let ctx = Self::deadline(RPC_DEADLINE);
        match client.get_full_info(ctx).await {
            Ok(info) => Observation {
                observed_at,
                view: WorldView::Reachable(TunnelObservation {
                    epoch: info.epoch,
                    running: info.running.map(|r| RunningTunnel {
                        protocol: r.protocol,
                        epoch: Some(crate::vpn::actor::types::IntentEpoch(info.epoch)),
                        endpoint: r.endpoint,
                        address: r.address,
                        connected_secs: r.connected_secs,
                        params: Some(r.params),
                        autonomous: r.autonomous,
                    }),
                    starting: info.starting,
                    start_error: info.start_error,
                    raw_stats: match (info.tx_bytes, info.rx_bytes) {
                        (Some(tx_bytes), Some(rx_bytes)) => Some(RawStats { tx_bytes, rx_bytes }),
                        _ => None,
                    },
                    last_packet_secs: info.last_packet_received,
                }),
            },
            Err(e) => {
                warn!("RPC get_full_info failed: {e}");
                self.invalidate_client().await;
                let cause = if matches!(e, tarpc::client::RpcError::DeadlineExceeded) {
                    UnreachableCause::Timeout
                } else {
                    UnreachableCause::TransportBroken
                };
                unreachable(cause)
            }
        }
    }

    /// The tunnel lives in another process that Android may restart underneath us, so a gap in
    /// answers must be tolerated for a while before the tunnel is presumed gone.
    fn liveness_grace(&self) -> std::time::Duration {
        LIVENESS_GRACE
    }
}

/// Bound on a single RPC. Shorter than any poll interval, so a wedged peer delays one observation
/// rather than starving all of them.
const RPC_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the `:vpn` process may stay silent before its tunnel is presumed lost.
const LIVENESS_GRACE: std::time::Duration = std::time::Duration::from_secs(6);
