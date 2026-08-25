//! The actor boundary.
//!
//! Deliberately transport-agnostic: three operations — send a command, await a reply, observe a
//! state stream — which today are an mpsc channel and a `watch`, and which map without redesign
//! onto a socket if the actor ever moves into a privileged daemon process.
//!
//! Everything that reaches the actor arrives as a [`Command`] on one channel, including the signals
//! that spawned tasks send back. That is what makes `biased` ordering in the loop the *only* place
//! priority is expressed: there is no second channel that could quietly overtake the first.

use super::types::{
    AttemptPhase, AttemptResult, CycleOutcome, IntentAccepted, IntentEpoch, IntentError,
    Observation, TunnelParams, TunnelState,
};
use crate::vpn::protocol::Protocol;
use crate::vpn::rollback::UnwindReport;
use crate::vpn::store::ConfigError;
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentRequest {
    Down,
    /// A Down that must also leave nothing running, whoever started it. Only a wipe asks for it:
    /// an always-on tunnel the system brought back is adopted after an ordinary Disconnect, and
    /// forgetting the account is exactly the case where that must not happen.
    Forget,
    Up {
        order: Vec<Protocol>,
        params: TunnelParams,
    },
}

#[derive(Debug)]
pub struct AttemptReport {
    pub epoch: IntentEpoch,
    pub index: usize,
    pub result: AttemptResult,
}

#[derive(Debug)]
pub enum Command {
    // ------------------------------------------------------------------ from Tauri commands
    SetIntent {
        intent: IntentRequest,
        reply: oneshot::Sender<Result<IntentAccepted, IntentError>>,
    },
    /// Resolves when this epoch's cycle reaches a terminal state. Late callers still get an answer,
    /// because recent outcomes are retained.
    AwaitCycle {
        epoch: IntentEpoch,
        reply: oneshot::Sender<CycleOutcome>,
    },
    ImportConfig {
        raw: String,
        reply: oneshot::Sender<Result<Protocol, ConfigError>>,
    },
    /// Goes Down, waits for genuine quiescence, *then* wipes — rather than branching on whatever
    /// status the caller last saw, which is how a live adopted tunnel could survive "forget me".
    /// Every caller gets an answer: a second request while one is pending joins the wait.
    ClearConfigs {
        reply: oneshot::Sender<Result<(), IntentError>>,
    },
    ForgetPreferred {
        reply: oneshot::Sender<()>,
    },
    /// Used on app exit: resolves once nothing is in flight.
    AwaitQuiescent {
        reply: oneshot::Sender<()>,
    },
    /// Used on app exit, after quiescence: resolves once every queued config write has landed.
    FlushConfigs {
        reply: oneshot::Sender<()>,
    },

    // --------------------------------------------------------- internal, from spawned tasks
    /// Cosmetic sub-phase of the in-flight attempt. Never a reconcile input.
    AttemptProgress {
        epoch: IntentEpoch,
        index: usize,
        phase: AttemptPhase,
    },
    AttemptDone(Box<AttemptReport>),
    UnwindDone(Box<UnwindReport>),
    Observed(Box<Observation>),
}

/// A cloneable handle to the actor, held in Tauri state.
///
/// Callable from a Tauri command *and* from plain Rust, which matters because teardown-on-exit and
/// clear-configs both need it outside a command context.
#[derive(Clone)]
pub struct TunnelHandle {
    tx: mpsc::Sender<Command>,
    /// Read-only. The actor is the sole writer.
    state: watch::Receiver<TunnelState>,
}

impl TunnelHandle {
    pub fn new(tx: mpsc::Sender<Command>, state: watch::Receiver<TunnelState>) -> Self {
        Self { tx, state }
    }

    /// The current snapshot. A local read — never IPC, which is why the UI can poll it freely.
    pub fn snapshot(&self) -> TunnelState {
        self.state.borrow().clone()
    }

    pub async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::SetIntent { intent, reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)?
    }

    /// Wait for an epoch's cycle to finish. Safe to drop: dropping the receiver only discards the
    /// answer, it never cancels anything the actor is doing.
    pub async fn await_cycle(&self, epoch: IntentEpoch) -> Result<CycleOutcome, IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::AwaitCycle { epoch, reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)
    }

    pub async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ImportConfig { raw, reply })
            .await
            .map_err(|_| ConfigError::ActorGone)?;
        rx.await.map_err(|_| ConfigError::ActorGone)?
    }

    pub async fn clear_configs(&self) -> Result<(), IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ClearConfigs { reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)?
    }

    pub async fn forget_preferred(&self) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::ForgetPreferred { reply })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    /// Resolves once the actor has nothing in flight. Used on exit.
    pub async fn await_quiescent(&self) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::AwaitQuiescent { reply })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    /// Resolves once every config write queued so far has landed. Used on exit, after
    /// [`await_quiescent`](Self::await_quiescent).
    pub async fn flush_configs(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::FlushConfigs { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }
}
