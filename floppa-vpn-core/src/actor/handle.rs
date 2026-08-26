//! The actor boundary.
//!
//! Transport-agnostic by design, and now by fact: three operations — send a command, await a
//! reply, observe a state stream — behind [`TunnelControl`], with one implementation over channels
//! for an actor in this process and another over a socket for an actor in a different one. Which
//! is in use is a property of the platform, not of any caller: a Tauri command, the exit path and
//! the wipe path all call the same methods either way.
//!
//! What is *not* abstracted away is the difference between the two, because it is real. A local
//! actor can only be gone; a remote one can also be unreachable, restarted underneath us, or
//! answering about a world we cannot see. The actor's own vocabulary already draws that line —
//! [`World::Dark`](super::types::World) is never evidence — and pretending a socket is a channel
//! would erase it in exactly the place it matters.
//!
//! Everything that reaches the actor arrives as a [`Command`] on one channel, including the signals
//! that spawned tasks send back. That is what makes `biased` ordering in the loop the *only* place
//! priority is expressed: there is no second channel that could quietly overtake the first.

use super::types::{
    AttemptPhase, AttemptResult, CycleOutcome, IntentAccepted, IntentEpoch, IntentError, Link,
    Observation, SystemVpnMode, TunnelParams, TunnelState,
};
use crate::protocol::Protocol;
use crate::rollback::UnwindReport;
use crate::store::ConfigError;
use tokio::sync::{mpsc, oneshot, watch};

/// Externally tagged, like everything else that crosses to another process: it is the one enum
/// shape no codec has to be self-describing to read back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The platform noticed the device gain or lose its network.
    ///
    /// It arrives on the same channel as everything else, and that is the whole of "wake up when
    /// the network comes back": a parked cycle is waiting in `Retrying` for a table pass, and
    /// delivering this command *is* a table pass. There is no timer to cancel and no separate
    /// notification path that could quietly overtake the queue.
    LinkChanged(Link),
    /// The platform reported how the system is running this VPN.
    ///
    /// Both facts arrive together and are normalised into one value before they get here, so the
    /// pair cannot be seen half-updated — always-on and lockdown are one nested answer, and two
    /// commands could tear it.
    VpnModeChanged(SystemVpnMode),
}

/// The actor's boundary, as everything that is not the actor sees it.
///
/// Two implementations: [`LocalActor`] reaches an actor in this process over channels, and the
/// remote one reaches an actor in another process over a socket. Nothing above this line knows or
/// cares which — a Tauri command, the exit path and the wipe path all call the same methods.
///
/// Every operation is fallible already, which is what makes the second implementation possible
/// without redesigning anything: a local actor can be gone, and a remote one can additionally be
/// unreachable, but both of those are `ActorGone` to a caller who can only give up either way.
#[async_trait::async_trait]
pub trait TunnelControl: Send + Sync {
    /// The current snapshot. Always a local read — a mirror kept up to date, never a call — which
    /// is why the UI may poll it as freely as it likes.
    fn snapshot(&self) -> TunnelState;

    /// The stream of published states, for whoever is forwarding them onwards.
    fn states(&self) -> watch::Receiver<TunnelState>;

    async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError>;

    /// Wait for an epoch's cycle to finish. Safe to drop: dropping the receiver only discards the
    /// answer, it never cancels anything the actor is doing.
    async fn await_cycle(&self, epoch: IntentEpoch) -> Result<CycleOutcome, IntentError>;

    async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError>;

    async fn clear_configs(&self) -> Result<(), IntentError>;

    /// Forget which protocol last worked. Infallible only in the sense that there is nothing to
    /// refuse — a dead actor is still a failure, and swallowing it meant the settings modal said
    /// "reset" over an actor that had stopped answering.
    async fn forget_preferred(&self) -> Result<(), IntentError>;

    /// Resolves once the actor has nothing in flight. Used on exit.
    async fn await_quiescent(&self);

    /// Resolves once every config write queued so far has landed. Used on exit, after
    /// [`await_quiescent`](Self::await_quiescent).
    async fn flush_configs(&self);

    /// Tell the actor the device gained or lost its network.
    ///
    /// Reported by whoever holds the platform's watcher, which is always the process the actor
    /// lives in — the `:vpn` service on Android, nobody at all on desktop. It is a sibling of the
    /// rebind reflex rather than of the commands above: a statement of fact about the machine, not
    /// a request, and so it has no reply and nothing to refuse.
    async fn report_link(&self, link: Link);

    /// Tell the actor how the system is running this VPN. Same shape and same reasoning as
    /// [`report_link`](Self::report_link): a statement of fact from the process that can see it.
    async fn report_vpn_mode(&self, mode: SystemVpnMode);
}

/// A cloneable handle to the actor, held in Tauri state.
///
/// A newtype over the boundary rather than the boundary itself, so callers keep plain inherent
/// methods and nothing above has to name a trait object.
#[derive(Clone)]
pub struct TunnelHandle(std::sync::Arc<dyn TunnelControl>);

impl TunnelHandle {
    pub fn new(tx: mpsc::Sender<Command>, state: watch::Receiver<TunnelState>) -> Self {
        Self(std::sync::Arc::new(LocalActor { tx, state }))
    }

    /// A handle to an actor that is somewhere else. What "somewhere else" means is that
    /// implementation's business.
    pub fn remote(control: std::sync::Arc<dyn TunnelControl>) -> Self {
        Self(control)
    }

    pub fn snapshot(&self) -> TunnelState {
        self.0.snapshot()
    }

    pub fn states(&self) -> watch::Receiver<TunnelState> {
        self.0.states()
    }

    pub async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError> {
        self.0.set_intent(intent).await
    }

    pub async fn await_cycle(&self, epoch: IntentEpoch) -> Result<CycleOutcome, IntentError> {
        self.0.await_cycle(epoch).await
    }

    pub async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError> {
        self.0.import_config(raw).await
    }

    pub async fn clear_configs(&self) -> Result<(), IntentError> {
        self.0.clear_configs().await
    }

    pub async fn forget_preferred(&self) -> Result<(), IntentError> {
        self.0.forget_preferred().await
    }

    pub async fn await_quiescent(&self) {
        self.0.await_quiescent().await
    }

    pub async fn flush_configs(&self) {
        self.0.flush_configs().await
    }

    pub async fn report_link(&self, link: Link) {
        self.0.report_link(link).await
    }

    pub async fn report_vpn_mode(&self, mode: SystemVpnMode) {
        self.0.report_vpn_mode(mode).await
    }
}

/// The actor is in this process: a command channel and a state mirror it writes directly.
struct LocalActor {
    tx: mpsc::Sender<Command>,
    /// Read-only. The actor is the sole writer.
    state: watch::Receiver<TunnelState>,
}

#[async_trait::async_trait]
impl TunnelControl for LocalActor {
    fn snapshot(&self) -> TunnelState {
        self.state.borrow().clone()
    }

    fn states(&self) -> watch::Receiver<TunnelState> {
        self.state.clone()
    }

    async fn set_intent(&self, intent: IntentRequest) -> Result<IntentAccepted, IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::SetIntent { intent, reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)?
    }

    async fn await_cycle(&self, epoch: IntentEpoch) -> Result<CycleOutcome, IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::AwaitCycle { epoch, reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)
    }

    async fn import_config(&self, raw: String) -> Result<Protocol, ConfigError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ImportConfig { raw, reply })
            .await
            .map_err(|_| ConfigError::ActorGone)?;
        rx.await.map_err(|_| ConfigError::ActorGone)?
    }

    async fn clear_configs(&self) -> Result<(), IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ClearConfigs { reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)?
    }

    async fn forget_preferred(&self) -> Result<(), IntentError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ForgetPreferred { reply })
            .await
            .map_err(|_| IntentError::ActorGone)?;
        rx.await.map_err(|_| IntentError::ActorGone)
    }

    async fn await_quiescent(&self) {
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

    async fn flush_configs(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::FlushConfigs { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Sent, never tried: a dropped report is not a dropped notification but a cycle left parked
    /// on a network that has come back. The queue is the actor's own and drains at its loop's
    /// speed, so waiting on it costs the caller a task, not a thread.
    async fn report_link(&self, link: Link) {
        let _ = self.tx.send(Command::LinkChanged(link)).await;
    }

    async fn report_vpn_mode(&self, mode: SystemVpnMode) {
        let _ = self.tx.send(Command::VpnModeChanged(mode)).await;
    }
}
