//! The actor's boundary, served over a socket.
//!
//! This runs in the process that owns the actor — on Android that is `:vpn` — and answers whoever
//! holds the other end of [`TunnelControl`](crate::actor::handle::TunnelControl). Every method
//! is a straight delegation to the local handle: the decisions are the actor's, and this file only
//! carries them across a process boundary.
//!
//! It is `#[cfg(unix)]` rather than Android-only for two reasons. Its tests then run on the host,
//! against a real socket, which is where a change to this is actually watched. And splitting the
//! desktop app into a privileged helper and a UI later needs this exact machinery rather than a
//! second copy of it.

use super::rpc::{Published, STATE_HOLD, VpnRpc};
pub use super::rpc_listener::RpcServerHandle;
use crate::actor::handle::{IntentRequest, TunnelHandle};
use crate::actor::types::{CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelState};
use crate::protocol::Protocol;
use crate::store::ConfigError;
use futures::StreamExt;
use tarpc::context::Context;
use tarpc::server::Channel;
use tokio_util::codec::length_delimited::LengthDelimitedCodec;
use tracing::{debug, warn};

#[derive(Clone)]
struct ActorServer {
    handle: TunnelHandle,
    /// Identity of this run of the actor. Random per process: it is only ever compared for
    /// equality, and what it has to be is *different from the last run's*.
    boot: u64,
}

impl ActorServer {
    fn published(&self, state: TunnelState) -> Published {
        Published {
            boot: self.boot,
            state,
        }
    }
}

impl VpnRpc for ActorServer {
    async fn state_since(self, _ctx: Context, boot: u64, seq: u64) -> Published {
        let mut states = self.handle.states();
        // A caller from a previous run knows nothing about this one, whatever its seq says.
        if boot != self.boot {
            return self.published(states.borrow().clone());
        }
        // Anything already newer is answered on the spot: the client is behind, not waiting.
        if states.borrow().seq > seq {
            return self.published(states.borrow().clone());
        }
        // Otherwise hold the call open. The timeout is not a failure — it returns whatever is
        // current, the client asks again with the same `seq`, and an idle connection proves itself
        // alive every hold instead of sitting silent forever.
        let held = tokio::time::timeout(STATE_HOLD, async {
            while states.changed().await.is_ok() {
                if states.borrow().seq > seq {
                    return;
                }
            }
        })
        .await;
        if held.is_err() {
            debug!(
                seq,
                "nothing new within the hold; answering with the current state"
            );
        }
        let state = states.borrow().clone();
        self.published(state)
    }

    async fn set_intent(
        self,
        _ctx: Context,
        intent: IntentRequest,
    ) -> Result<IntentAccepted, IntentError> {
        self.handle.set_intent(intent).await
    }

    async fn await_cycle(
        self,
        _ctx: Context,
        epoch: IntentEpoch,
    ) -> Result<CycleOutcome, IntentError> {
        self.handle.await_cycle(epoch).await
    }

    async fn import_config(self, _ctx: Context, raw: String) -> Result<Protocol, ConfigError> {
        self.handle.import_config(raw).await
    }

    async fn clear_configs(self, _ctx: Context) -> Result<(), IntentError> {
        self.handle.clear_configs().await
    }

    async fn forget_preferred(self, _ctx: Context) -> Result<(), IntentError> {
        self.handle.forget_preferred().await
    }

    async fn await_quiescent(self, _ctx: Context) {
        self.handle.await_quiescent().await
    }

    async fn flush_configs(self, _ctx: Context) {
        self.handle.flush_configs().await
    }

    async fn set_log_config(self, _ctx: Context, config: crate::logging::LogConfig) {
        crate::logging::apply_log_config(&config);
    }

    async fn start_log_capture(self, _ctx: Context, capture_id: String) {
        let Some(log_dir) = crate::logging::get_log_dir() else {
            warn!("cannot start a log capture here: the log directory is not initialised");
            return;
        };
        if let Err(e) = crate::logging::start_file_capture(log_dir, "vpn", &capture_id) {
            warn!("failed to start the log capture: {e}");
        }
    }

    async fn stop_log_capture(self, _ctx: Context) {
        let _ = crate::logging::stop_file_capture();
    }
}

/// Serve the actor on a Unix socket until the returned handle is dropped or shut down.
///
/// One server per process, not one per tunnel: what is being served is the actor, which outlives
/// every individual tunnel and every client that connects to ask about one.
pub fn serve(socket_path: &str, handle: TunnelHandle) -> Result<RpcServerHandle, String> {
    // `RandomState` is seeded per process by the standard library: entropy with no dependency and
    // no syscall, and equality is all this value is ever used for.
    let boot = {
        use std::hash::{BuildHasher, Hasher};
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    let server = ActorServer { handle, boot };
    super::rpc_listener::listen(std::path::Path::new(socket_path), move |stream, cancel| {
        debug!("a client connected to the actor");
        let framed = LengthDelimitedCodec::builder().new_framed(stream);
        let transport = tarpc::serde_transport::new(framed, tokio_serde::formats::Json::default());
        let channel = tarpc::server::BaseChannel::with_defaults(transport);
        let server = server.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = channel.execute(server.serve()).for_each(|resp| async {
                    tokio::spawn(resp);
                }) => debug!("a client disconnected from the actor"),
                _ = cancel.cancelled() => debug!("the server is shutting down; closing a connection"),
            }
        });
    })
    .map_err(|e| format!("failed to bind the actor socket at {socket_path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::handle::TunnelControl;
    use crate::actor::types::{IntentView, Phase, TunnelParams};
    use crate::remote::{RemoteActor, TunnelProcess};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::watch;

    /// An actor that only records. What is under test is the boundary, not the decisions.
    struct FakeActor {
        state: watch::Receiver<TunnelState>,
        intents: std::sync::Mutex<Vec<IntentRequest>>,
    }

    #[async_trait::async_trait]
    impl crate::actor::handle::TunnelControl for FakeActor {
        fn snapshot(&self) -> TunnelState {
            self.state.borrow().clone()
        }
        fn states(&self) -> watch::Receiver<TunnelState> {
            self.state.clone()
        }
        async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError> {
            self.intents.lock().unwrap().push(intent);
            Ok(IntentAccepted {
                epoch: IntentEpoch(1),
            })
        }
        async fn await_cycle(&self, _: IntentEpoch) -> Result<CycleOutcome, IntentError> {
            Ok(CycleOutcome::Down)
        }
        async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError> {
            if raw.is_empty() {
                Err(ConfigError::Empty)
            } else {
                Ok(Protocol::AmneziaWg)
            }
        }
        async fn clear_configs(&self) -> Result<(), IntentError> {
            Ok(())
        }
        async fn forget_preferred(&self) -> Result<(), IntentError> {
            Err(IntentError::ActorGone)
        }
        async fn await_quiescent(&self) {}
        async fn flush_configs(&self) {}
        async fn report_link(&self, _: crate::actor::types::Link) {}
        async fn report_vpn_mode(&self, _: crate::actor::types::SystemVpnMode) {}
    }

    /// A process that is always already there. Starting one is a platform's business, not this
    /// boundary's, and every platform that has one tests it on the platform.
    struct AlwaysRunning(AtomicUsize);

    #[async_trait::async_trait]
    impl TunnelProcess for AlwaysRunning {
        async fn ensure_running(&self) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn spawner() -> crate::actor::Spawn {
        Arc::new(|task| {
            tokio::spawn(task);
        })
    }

    fn connected(seq: u64) -> TunnelState {
        let mut state = TunnelState::initial();
        state.seq = seq;
        state.phase = Phase::Connected;
        state.intent = IntentView::Up;
        state.protocol = Some(Protocol::AmneziaWg);
        state
    }

    /// Wait for the mirror to satisfy a predicate, or say what it was stuck at.
    async fn wait_for(
        remote: &RemoteActor,
        what: &str,
        pred: impl Fn(&TunnelState) -> bool,
    ) -> TunnelState {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = remote.snapshot();
            if pred(&state) {
                return state;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}; mirror holds {:?}",
                remote.snapshot()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn the_mirror_follows_the_actor_across_a_restart_of_the_process_holding_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::rpc::SOCKET_NAME);
        let socket = path.to_string_lossy().to_string();

        let (state_tx, state_rx) = watch::channel(TunnelState::initial());
        let actor = Arc::new(FakeActor {
            state: state_rx,
            intents: std::sync::Mutex::new(Vec::new()),
        });
        let server = serve(&socket, TunnelHandle::remote(actor.clone())).expect("bind");

        let process = Arc::new(AlwaysRunning(AtomicUsize::new(0)));
        let remote = RemoteActor::new(dir.path(), process.clone(), &spawner());

        // Nothing has been heard yet, and the mirror says exactly that rather than claiming there
        // is no tunnel.
        assert_eq!(remote.snapshot().phase, Phase::Unknown);

        state_tx.send(connected(1)).expect("publish");
        let seen = wait_for(&remote, "the first state", |s| s.seq == 1).await;
        assert_eq!(seen.phase, Phase::Connected);
        assert_eq!(seen.protocol, Some(Protocol::AmneziaWg));

        // A second publish arrives on the same connection.
        state_tx.send(connected(2)).expect("publish");
        wait_for(&remote, "the next state", |s| s.seq == 2).await;

        // The process holding the actor goes away. The mirror must *keep what it last knew*: a
        // socket that is not there is not evidence that a tunnel stopped.
        server.shutdown_and_unlink();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            remote.snapshot().phase,
            Phase::Connected,
            "an unreachable process must never read as a tunnel that went down"
        );

        // It comes back as a *restarted process*: a new actor, whose sequence starts again from
        // the beginning. This is the case sequence numbers alone cannot survive — the mirror holds
        // seq 2 and everything the new run publishes is seq 1 — and it is the ordinary path after
        // Android restarts the service, not an exotic one. What makes it work is that the run has
        // an identity of its own, so a mismatch is adopted instead of compared.
        let (fresh_tx, fresh_rx) = watch::channel(TunnelState::initial());
        let mut restarted = TunnelState::initial();
        restarted.seq = 1;
        restarted.phase = Phase::Disconnected;
        fresh_tx.send(restarted).expect("publish");
        let fresh_actor = Arc::new(FakeActor {
            state: fresh_rx,
            intents: std::sync::Mutex::new(Vec::new()),
        });
        let _server = serve(&socket, TunnelHandle::remote(fresh_actor)).expect("rebind");

        wait_for(&remote, "the state of the restarted process", |s| {
            s.phase == Phase::Disconnected
        })
        .await;
        assert_eq!(
            remote.snapshot().seq,
            1,
            "the new run's numbering is adopted, not filtered against the old one's"
        );

        // And it keeps following that run from there.
        let mut later = connected(2);
        later.assigned_ip = Some("10.0.0.9/32".into());
        fresh_tx.send(later).expect("publish");
        wait_for(&remote, "the next state of the new run", |s| {
            s.assigned_ip.as_deref() == Some("10.0.0.9/32")
        })
        .await;
    }

    #[tokio::test]
    async fn every_operation_crosses_and_comes_back_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::rpc::SOCKET_NAME);
        let (_state_tx, state_rx) = watch::channel(TunnelState::initial());
        let actor = Arc::new(FakeActor {
            state: state_rx,
            intents: std::sync::Mutex::new(Vec::new()),
        });
        let _server =
            serve(&path.to_string_lossy(), TunnelHandle::remote(actor.clone())).expect("bind");

        let process = Arc::new(AlwaysRunning(AtomicUsize::new(0)));
        let remote = RemoteActor::new(dir.path(), process.clone(), &spawner());
        use crate::actor::handle::TunnelControl as _;

        let up = IntentRequest::Up {
            order: vec![Protocol::AmneziaWg, Protocol::WireGuard],
            params: TunnelParams::default(),
        };
        assert_eq!(
            remote.set_intent(up.clone()).await,
            Ok(IntentAccepted {
                epoch: IntentEpoch(1)
            })
        );
        assert_eq!(
            actor.intents.lock().unwrap().as_slice(),
            &[up],
            "the intent arrived as it was sent"
        );
        assert_eq!(
            process.0.load(Ordering::SeqCst),
            1,
            "an Up makes sure the process exists first"
        );

        // A Down does not: there is nothing to start a service for.
        assert!(remote.set_intent(IntentRequest::Down).await.is_ok());
        assert_eq!(process.0.load(Ordering::SeqCst), 1);

        assert_eq!(
            remote.await_cycle(IntentEpoch(1)).await,
            Ok(CycleOutcome::Down)
        );
        assert_eq!(
            remote.import_config("[Interface]".into()).await,
            Ok(Protocol::AmneziaWg)
        );
        // Errors cross as errors, not as "the call failed".
        assert_eq!(
            remote.import_config(String::new()).await,
            Err(ConfigError::Empty)
        );
        assert_eq!(remote.clear_configs().await, Ok(()));
        assert_eq!(
            remote.forget_preferred().await,
            Err(IntentError::ActorGone),
            "a refusal from the actor is not the same as being unable to ask"
        );
        remote.await_quiescent().await;
        remote.flush_configs().await;
    }

    #[tokio::test]
    async fn a_process_that_is_not_there_refuses_rather_than_inventing_an_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let process = Arc::new(AlwaysRunning(AtomicUsize::new(0)));
        // Nothing is serving: no socket file at all.
        let remote = RemoteActor::new(dir.path(), process, &spawner());
        use crate::actor::handle::TunnelControl as _;

        assert_eq!(
            remote.set_intent(IntentRequest::Down).await,
            Err(IntentError::ActorGone)
        );
        assert_eq!(
            remote.import_config("x".into()).await,
            Err(ConfigError::ActorGone)
        );
        assert_eq!(
            remote.snapshot().phase,
            Phase::Unknown,
            "and it still does not claim to know that there is no tunnel"
        );
    }
}
