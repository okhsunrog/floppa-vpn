//! The actor as reached from another process.
//!
//! The other implementation of [`TunnelControl`]: same operations, a socket instead of a channel.
//! Two things make that substitution honest rather than a pretence.
//!
//! **The state is mirrored, not fetched.** `snapshot()` must stay a free local read — the UI calls
//! it on every render — so a background task keeps a `watch` filled by long-polling `state_since`,
//! and every caller reads the mirror. The seq the actor stamps on each publish is what makes that
//! exact: the poll asks for anything newer than what the mirror holds, so nothing is missed and
//! nothing is replayed, and a reconnect after the socket drops resumes from the same place.
//!
//! **Unreachable is not down.** Whatever this cannot ask, it does not answer for: while the socket
//! is not there the mirror keeps the last state it saw and reports `Phase::Unknown` only until the
//! first answer arrives. Turning a failed connection into "disconnected" is the one thing that
//! must never happen here — it is the same mistake the decision table spends `World::Dark`
//! avoiding, one layer up.

use super::rpc::{SOCKET_NAME, STATE_POLL_DEADLINE, VpnRpcClient};
use crate::vpn::actor::handle::{IntentRequest, TunnelControl};
use crate::vpn::actor::types::{
    CycleOutcome, IntentAccepted, IntentEpoch, IntentError, Phase, TunnelState,
};
use crate::vpn::protocol::Protocol;
use crate::vpn::store::ConfigError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, watch};
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, warn};

/// How long to wait before reconnecting to a socket that is not answering.
///
/// The process on the other end is a service the system may be restarting; retrying in a tight
/// loop would achieve nothing but a busy log.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Bound on an ordinary call. Long enough for the actor to do real work behind it, short enough
/// that a wedged peer does not hold a UI action forever.
const CALL_DEADLINE: Duration = Duration::from_secs(20);

/// Bound on `await_cycle`, which is held open for as long as a connect cycle takes. The actor's
/// own budgets end a cycle; this only has to outlast them.
const CYCLE_DEADLINE: Duration = Duration::from_secs(300);

/// Whatever is needed to make the process holding the actor exist, and to be allowed to talk to it.
///
/// On Android that is the VPN service: it has to be running for the socket to be there at all, and
/// the consent dialog can only be shown from an activity in this process. On a future desktop
/// split it would be starting a privileged helper. The remote handle knows it has a socket to
/// connect to; how that socket comes to exist is not its business.
#[async_trait::async_trait]
pub trait TunnelProcess: Send + Sync {
    /// Make sure the process holding the actor is running, before a call that needs it to be.
    ///
    /// Called before an intent that asks for a tunnel — never before a read. It is allowed to be
    /// slow and allowed to fail; a failure is reported as [`IntentError::ActorGone`], which is what
    /// "there is nobody to ask" means to every caller.
    async fn ensure_running(&self) -> Result<(), String>;
}

pub struct RemoteActor {
    client: Mutex<Option<VpnRpcClient>>,
    socket_path: PathBuf,
    state: watch::Receiver<TunnelState>,
    process: Arc<dyn TunnelProcess>,
}

impl RemoteActor {
    /// Connect to the actor in another process and start mirroring its state.
    ///
    /// Returns immediately: the mirror starts at [`TunnelState::initial`], whose phase is
    /// `Unknown` rather than `Disconnected` precisely because nothing has been observed yet.
    pub fn new(
        dir: &std::path::Path,
        process: Arc<dyn TunnelProcess>,
        spawn: &crate::vpn::actor::Spawn,
    ) -> Arc<Self> {
        let socket_path = dir.join(SOCKET_NAME);
        let (state_tx, state_rx) = watch::channel(TunnelState::initial());
        let remote = Arc::new(Self {
            client: Mutex::new(None),
            socket_path,
            state: state_rx,
            process,
        });
        spawn(Box::pin(mirror(remote.clone(), state_tx)));
        remote
    }

    /// The connection, opened if it is not already open.
    ///
    /// Cached because tarpc multiplexes: the mirror's long poll and a user's Connect share one
    /// connection quite happily, and opening a second would only double what has to be noticed
    /// when the peer goes away.
    async fn client(&self) -> Result<VpnRpcClient, String> {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }
        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| format!("cannot reach the tunnel process: {e}"))?;
        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport = tarpc::serde_transport::new(framed, tokio_serde::formats::Json::default());
        let client = VpnRpcClient::new(tarpc::client::Config::default(), transport).spawn();
        debug!("opened a connection to the tunnel process");
        *guard = Some(client.clone());
        Ok(client)
    }

    async fn drop_client(&self) {
        *self.client.lock().await = None;
    }

    fn deadline(timeout: Duration) -> tarpc::context::Context {
        let mut ctx = tarpc::context::current();
        ctx.deadline = std::time::Instant::now() + timeout;
        ctx
    }

    /// Run one call, dropping the connection if the transport failed so the next one reconnects.
    ///
    /// A transport failure says nothing about what the actor did or did not do — the call may well
    /// have been executed — which is why nothing here retries. The caller is told it could not be
    /// asked, and the mirror tells it what actually happened a moment later.
    async fn call<T, F, Fut>(&self, what: &str, run: F) -> Result<T, String>
    where
        F: FnOnce(VpnRpcClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, tarpc::client::RpcError>>,
    {
        let client = self.client().await?;
        match run(client).await {
            Ok(value) => Ok(value),
            Err(e) => {
                self.drop_client().await;
                warn!("{what} could not be delivered to the tunnel process: {e}");
                Err(format!("{what}: {e}"))
            }
        }
    }
}

/// Keep the mirror filled, forever.
///
/// Reconnects on its own: the process holding the actor is a service the system starts and stops,
/// so its socket coming and going is ordinary, not exceptional. What must never happen is this
/// task ending — the mirror would then quietly freeze at whatever it last saw.
async fn mirror(remote: Arc<RemoteActor>, state_tx: watch::Sender<TunnelState>) {
    loop {
        let seq = state_tx.borrow().seq;
        let asked = remote
            .call("state_since", |client| async move {
                client
                    .state_since(RemoteActor::deadline(STATE_POLL_DEADLINE), seq)
                    .await
            })
            .await;

        match asked {
            Ok(state) => {
                // Strictly newer only: the hold expiring answers with the state we already have,
                // and republishing it would wake every listener for nothing.
                if state.seq > seq || state_tx.borrow().phase == Phase::Unknown {
                    let _ = state_tx.send(state);
                }
            }
            Err(_) => {
                // The last state stands. It is what was true when we could still ask, and calling
                // it "disconnected" because a socket is missing is exactly the inference this
                // whole design refuses to make.
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

#[async_trait::async_trait]
impl TunnelControl for RemoteActor {
    fn snapshot(&self) -> TunnelState {
        self.state.borrow().clone()
    }

    fn states(&self) -> watch::Receiver<TunnelState> {
        self.state.clone()
    }

    async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError> {
        // Only an intent that wants a tunnel needs the process to exist; a Down is meaningless
        // when there is nothing running, and starting a service to hear that would be absurd.
        if matches!(intent, IntentRequest::Up { .. }) {
            self.process.ensure_running().await.map_err(|e| {
                warn!("the tunnel process could not be started: {e}");
                IntentError::ActorGone
            })?;
        }
        self.call("set_intent", |client| async move {
            client
                .set_intent(RemoteActor::deadline(CALL_DEADLINE), intent)
                .await
        })
        .await
        .map_err(|_| IntentError::ActorGone)?
    }

    async fn await_cycle(&self, epoch: IntentEpoch) -> Result<CycleOutcome, IntentError> {
        self.call("await_cycle", |client| async move {
            client
                .await_cycle(RemoteActor::deadline(CYCLE_DEADLINE), epoch)
                .await
        })
        .await
        .map_err(|_| IntentError::ActorGone)?
    }

    async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError> {
        self.call("import_config", |client| async move {
            client
                .import_config(RemoteActor::deadline(CALL_DEADLINE), raw)
                .await
        })
        .await
        .map_err(|_| ConfigError::ActorGone)?
    }

    async fn clear_configs(&self) -> Result<(), IntentError> {
        self.call("clear_configs", |client| async move {
            client
                .clear_configs(RemoteActor::deadline(CALL_DEADLINE))
                .await
        })
        .await
        .map_err(|_| IntentError::ActorGone)?
    }

    async fn forget_preferred(&self) -> Result<(), IntentError> {
        self.call("forget_preferred", |client| async move {
            client
                .forget_preferred(RemoteActor::deadline(CALL_DEADLINE))
                .await
        })
        .await
        .map_err(|_| IntentError::ActorGone)?
    }

    async fn await_quiescent(&self) {
        let _ = self
            .call("await_quiescent", |client| async move {
                client
                    .await_quiescent(RemoteActor::deadline(CYCLE_DEADLINE))
                    .await
            })
            .await;
    }

    async fn flush_configs(&self) {
        let _ = self
            .call("flush_configs", |client| async move {
                client
                    .flush_configs(RemoteActor::deadline(CALL_DEADLINE))
                    .await
            })
            .await;
    }
}

/// The three log calls, which are about the process rather than about the tunnel.
///
/// Separate from [`TunnelControl`] because they answer a different question — "what is that
/// process writing to its log" — and because the capture session that needs them exists on
/// platforms where there is no second process at all.
#[async_trait::async_trait]
impl crate::logging::capture::LogRelay for RemoteActor {
    async fn set_log_config(&self, config: &crate::logging::LogConfig) {
        let config = config.clone();
        let _ = self
            .call("set_log_config", |client| async move {
                client
                    .set_log_config(RemoteActor::deadline(CALL_DEADLINE), config)
                    .await
            })
            .await;
    }

    async fn start_log_capture(&self, capture_id: &str) {
        let capture_id = capture_id.to_string();
        let _ = self
            .call("start_log_capture", |client| async move {
                client
                    .start_log_capture(RemoteActor::deadline(CALL_DEADLINE), capture_id)
                    .await
            })
            .await;
    }

    async fn stop_log_capture(&self) {
        let _ = self
            .call("stop_log_capture", |client| async move {
                client
                    .stop_log_capture(RemoteActor::deadline(CALL_DEADLINE))
                    .await
            })
            .await;
    }
}
