//! The tunnel actor: one task owns the tunnel, and nothing else may touch it.
//!
//! `status`, `intent`, the held rollback stack, the config store, the speed tracker and the last
//! observation are **local variables of one task**. There is no lock over them and no second
//! writer, which is a stronger guarantee than any amount of careful locking: the races the previous
//! design had are not fixed here, they are unwritable.
//!
//! Everything arrives on one channel — external commands and the signals spawned tasks send back —
//! so `biased` ordering in the loop is the only place priority is expressed. A user pressing Cancel
//! in the same wakeup as an expiring deadline is handled first, every time.

pub mod attempt;
pub mod handle;
pub mod reconcile;
pub mod types;
pub mod view;

#[cfg(all(test, not(target_os = "android")))]
#[path = "actor_tests.rs"]
mod tests;

use self::handle::{AttemptReport, Command, IntentRequest, TunnelHandle};
use self::reconcile::{Decision, Effect};
use self::types::{
    AttemptPhase, AttemptResult, CycleOutcome, Intent, IntentAccepted, IntentEpoch, IntentError,
    Observation, Policy, Status, TunnelState, UpIntent, World,
};
use crate::vpn::backend::VpnBackend;
use crate::vpn::platform::{Platform, PlatformImpl};
use crate::vpn::protocol::{InterfaceName, Protocol};
use crate::vpn::rollback::{Journal, RollbackStack, UnwindReport, unwind};
use crate::vpn::state::SpeedTracker;
use crate::vpn::store::ConfigStore;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// How many finished cycles are remembered, so a caller that asks late still gets its answer
/// instead of hanging on a waiter for something that already happened.
const RECENT_OUTCOMES: usize = 8;

/// Channel depth. Generous: the actor never blocks on I/O while draining, so this only absorbs
/// bursts of observations and progress reports.
const CHANNEL_DEPTH: usize = 64;

struct AttemptHandle {
    epoch: IntentEpoch,
    index: usize,
    cancel: CancellationToken,
}

/// Whether a command changed anything the decision table cares about.
enum Reconcile {
    Yes,
    No,
}

pub struct TunnelActor {
    // ---- the state, owned outright ----
    status: Status,
    intent: Intent,
    held_stack: RollbackStack,
    configs: ConfigStore,
    speed: SpeedTracker,
    last_obs: Observation,
    /// Whether the world has ever answered us. Until it has, "there is no tunnel" is a claim we
    /// are not entitled to make, and the published phase says so.
    observed_once: bool,

    // ---- collaborators ----
    backend: Arc<dyn VpnBackend>,
    platform: Arc<dyn Platform>,
    policy: Policy,
    iface: InterfaceName,
    journal: Option<Journal>,
    /// Only the Android ladder needs it; the publisher takes its own clone when it is spawned.
    #[cfg(target_os = "android")]
    app: tauri::AppHandle,

    // ---- in-flight work ----
    attempt: Option<AttemptHandle>,
    unwind: Option<JoinHandle<()>>,
    /// When the in-flight unwind started, so its result is judged against a later look at the
    /// world rather than one taken before it ran.
    unwind_started: Option<Instant>,
    cancel_issued: bool,

    // ---- bookkeeping ----
    next_epoch: u64,
    seq: u64,
    last_outcome: Option<CycleOutcome>,
    recent_outcomes: VecDeque<(IntentEpoch, CycleOutcome)>,
    cycle_waiters: Vec<(IntentEpoch, oneshot::Sender<CycleOutcome>)>,
    quiescent_waiters: Vec<oneshot::Sender<()>>,
    pending_clear: Option<(oneshot::Sender<Result<(), IntentError>>, Instant)>,

    cmd_tx: mpsc::Sender<Command>,
    state_tx: watch::Sender<TunnelState>,
}

impl TunnelActor {
    /// Spawn the actor and return a handle to it.
    pub fn spawn(
        backend: Arc<dyn VpnBackend>,
        platform: Arc<PlatformImpl>,
        journal: Option<Journal>,
        app: tauri::AppHandle,
    ) -> TunnelHandle {
        let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_DEPTH);
        let (state_tx, state_rx) = watch::channel(TunnelState::initial());
        let policy = Policy::for_backend(backend.liveness_grace());

        // Tauri's `setup` runs outside a Tokio runtime context, so these must go through Tauri's
        // runtime handle. Everything the actor spawns afterwards runs inside its own task and can
        // use `tokio::spawn` directly.
        tauri::async_runtime::spawn(observer(backend.clone(), cmd_tx.clone(), policy.clone()));
        tauri::async_runtime::spawn(publisher(state_rx.clone(), app.clone()));

        let actor_tx = cmd_tx.clone();
        tauri::async_runtime::spawn(async move {
            // Reading the OS keyring can block for as long as an unlock dialog stays open. It
            // runs on a blocking thread, and the loop starts only once it is done — so nothing
            // queues behind it, and the actor task itself never blocks.
            let configs = ConfigStore::load().await;
            let actor = Self::new(
                backend,
                platform,
                journal,
                policy,
                configs,
                actor_tx,
                state_tx,
                #[cfg(target_os = "android")]
                app,
            );
            actor.run(cmd_rx).await;
        });

        TunnelHandle::new(cmd_tx, state_rx)
    }

    /// Assemble the actor. Everything that touches the outside world — the backend, the platform,
    /// the journal and the config store — is handed in, which is what lets the tests drive the
    /// real loop against fakes.
    #[allow(clippy::too_many_arguments)]
    fn new(
        backend: Arc<dyn VpnBackend>,
        platform: Arc<dyn Platform>,
        journal: Option<Journal>,
        policy: Policy,
        configs: ConfigStore,
        cmd_tx: mpsc::Sender<Command>,
        state_tx: watch::Sender<TunnelState>,
        #[cfg(target_os = "android")] app: tauri::AppHandle,
    ) -> Self {
        Self {
            status: Status::Idle,
            intent: Intent::default(),
            held_stack: RollbackStack::default(),
            configs,
            speed: SpeedTracker::new(),
            last_obs: Observation::unknown(Instant::now()),
            observed_once: false,
            backend,
            platform,
            policy,
            iface: InterfaceName::default(),
            journal,
            #[cfg(target_os = "android")]
            app,
            attempt: None,
            unwind: None,
            unwind_started: None,
            cancel_issued: false,
            next_epoch: 1,
            seq: 0,
            last_outcome: None,
            recent_outcomes: VecDeque::new(),
            cycle_waiters: Vec::new(),
            quiescent_waiters: Vec::new(),
            pending_clear: None,
            cmd_tx,
            state_tx,
        }
    }

    async fn run(mut self, mut cmds: mpsc::Receiver<Command>) {
        self.bootstrap();
        self.publish();

        loop {
            // Recomputed from the status every iteration, so there is no timer that can drift out
            // of sync with what the status actually is.
            let deadline = self.deadline();

            tokio::select! {
                biased;

                // Commands and signals first, and nothing in this arm awaits I/O. That is the
                // structural reason a disconnect can no longer queue behind a slow round trip to
                // the tunnel process.
                cmd = cmds.recv() => {
                    let Some(cmd) = cmd else {
                        self.shutdown().await;
                        return;
                    };
                    match self.handle(cmd).await {
                        Reconcile::Yes => self.tick(Instant::now()),
                        Reconcile::No => self.publish_if_changed(),
                    }
                }

                _ = sleep_until(deadline), if deadline.is_some() => {
                    let now = Instant::now();
                    self.expire_pending_clear(now);
                    self.tick(now);
                }
            }
        }
    }

    /// Runs once, before the loop. Recovers anything a previous process left applied, and adopts a
    /// tunnel that outlived us.
    fn bootstrap(&mut self) {
        if let Some(journal) = &self.journal {
            let orphaned = journal.read_orphaned();
            if !orphaned.is_empty() {
                warn!(
                    count = orphaned.len(),
                    "a previous run left network changes applied"
                );
                self.held_stack = RollbackStack::from_orphaned(orphaned, Some(journal.clone()));
                self.status = Status::Unwinding {
                    cycle: None,
                    reason: types::UnwindReason::CrashRecovery,
                    tries: 0,
                };
                self.exec(Effect::Unwind { extra: None });
                return;
            }
        }

        // An intent with no params: it asks for "whatever is already there", which is the only
        // intent allowed to adopt a tunnel whose split rules we cannot know.
        let order = self.configs.resolve_order(&Protocol::ALL);
        if !order.is_empty() {
            self.intent = Intent::Up(UpIntent {
                epoch: self.mint_epoch(),
                order,
                params: None,
            });
            debug!("bootstrap intent will adopt a surviving tunnel if there is one");
        }
    }

    // ------------------------------------------------------------------------------ commands

    async fn handle(&mut self, cmd: Command) -> Reconcile {
        match cmd {
            Command::SetIntent { intent, reply } => {
                let result = self.accept_intent(intent);
                let changed = result.is_ok();
                let _ = reply.send(result);
                if changed {
                    Reconcile::Yes
                } else {
                    Reconcile::No
                }
            }

            Command::AwaitCycle { epoch, reply } => {
                // Answer immediately if that cycle already finished, so a caller that asks late
                // does not wait for an event that has been and gone.
                if let Some((_, outcome)) = self.recent_outcomes.iter().find(|(e, _)| *e == epoch) {
                    let _ = reply.send(outcome.clone());
                } else {
                    self.cycle_waiters.push((epoch, reply));
                }
                Reconcile::No
            }

            Command::ImportConfig { raw, reply } => {
                let result = self.configs.import(&raw);
                let _ = reply.send(result);
                Reconcile::No
            }

            Command::ListConfigs { reply } => {
                let _ = reply.send(self.configs.view());
                Reconcile::No
            }

            Command::ClearConfigs { reply } => {
                // Go down and wait for genuine quiescence before wiping, rather than deciding from
                // whatever status the caller last observed — which is how a live adopted tunnel
                // could survive being forgotten.
                let _ = self.accept_intent(IntentRequest::Down);
                if view::is_quiescent(&self.status) {
                    self.configs.clear();
                    let _ = reply.send(Ok(()));
                } else {
                    self.pending_clear = Some((reply, Instant::now() + self.policy.settle_timeout));
                }
                Reconcile::Yes
            }

            Command::ForgetPreferred { reply } => {
                self.configs.set_preferred(None);
                let _ = reply.send(());
                Reconcile::No
            }

            Command::AwaitQuiescent { reply } => {
                if view::is_quiescent(&self.status) {
                    let _ = reply.send(());
                } else {
                    self.quiescent_waiters.push(reply);
                }
                Reconcile::No
            }

            Command::AttemptProgress {
                epoch,
                index,
                phase,
            } => {
                self.apply_progress(epoch, index, phase);
                Reconcile::No
            }

            Command::AttemptDone(report) => {
                self.on_attempt_done(*report);
                Reconcile::No
            }

            Command::UnwindDone(report) => {
                self.on_unwind_done(*report);
                Reconcile::No
            }

            Command::Observed(obs) => {
                // Any delivered observation counts, including an unreachable one.
                //
                // Requiring a *reachable* answer looks more rigorous and is wrong: on Android the
                // peer only exists while a tunnel does, so with no tunnel there is nothing to
                // reach, and the UI would sit at "checking" forever. What matters is that a look
                // completed — and the boot placeholder never arrives through this channel, so it
                // cannot be mistaken for one.
                self.observed_once = true;
                self.last_obs = *obs;
                Reconcile::Yes
            }
        }
    }

    /// Accept an intent change and mint a new epoch for it.
    ///
    /// There is no "busy" rejection here, and that is deliberate: with a single owner and a
    /// write-only intent queue, a caller cannot arrive at a bad moment. Whether anything actually
    /// starts is decided by the table, inside the loop.
    fn accept_intent(&mut self, request: IntentRequest) -> Result<IntentAccepted, IntentError> {
        let epoch = self.mint_epoch();

        let intent = match request {
            IntentRequest::Down => Intent::Down { epoch },
            IntentRequest::Up { order, params } => {
                if order.is_empty() {
                    return Err(IntentError::EmptyOrder);
                }
                let order = self.configs.resolve_order(&order);
                if order.is_empty() {
                    return Err(IntentError::NoUsableConfig);
                }
                Intent::Up(UpIntent {
                    epoch,
                    order,
                    params: Some(params),
                })
            }
        };

        // Anyone still waiting on a superseded epoch is released now, rather than being left to
        // time out on something that will never complete.
        let superseded: Vec<IntentEpoch> = self
            .cycle_waiters
            .iter()
            .map(|(e, _)| *e)
            .filter(|e| *e < epoch)
            .collect();
        for e in superseded {
            self.publish_outcome(e, CycleOutcome::Cancelled);
        }

        info!(?intent, "intent accepted");
        self.intent = intent;
        self.last_outcome = None;
        Ok(IntentAccepted { epoch })
    }

    fn mint_epoch(&mut self) -> IntentEpoch {
        let epoch = IntentEpoch(self.next_epoch);
        self.next_epoch += 1;
        epoch
    }

    /// A cosmetic sub-phase update. Never a reconcile input: it moves the label, not the state.
    fn apply_progress(&mut self, epoch: IntentEpoch, index: usize, phase: AttemptPhase) {
        if let Status::Connecting {
            cycle,
            phase: current,
            ..
        } = &mut self.status
            && cycle.epoch == epoch
            && cycle.index == index
        {
            *current = phase;
        }
    }

    fn on_attempt_done(&mut self, report: AttemptReport) {
        // Discard a report from an attempt we have already moved past.
        let stale = self
            .attempt
            .as_ref()
            .is_none_or(|a| a.epoch != report.epoch || a.index != report.index);
        if stale {
            debug!(epoch = %report.epoch, "discarding a stale attempt report");
            return;
        }
        self.attempt = None;
        self.cancel_issued = false;

        let now = Instant::now();

        // While unwinding, the attempt was cancelled *by* that unwind, so its report completes the
        // teardown rather than being a connect outcome. This is the only path a late-succeeding
        // connect can take, and it is why one can never publish "connected" after a teardown: the
        // status is written from the tables, never by an attempt.
        if matches!(self.status, Status::Unwinding { .. }) {
            if let AttemptResult::Established { stack, .. } = report.result {
                // It won the race and applied things. Take the stack so the teardown undoes
                // exactly what it did, and let that unwind's completion drive the next step.
                self.held_stack = stack;
                self.exec(Effect::Unwind { extra: None });
                return;
            }
            // It failed or was cancelled, and has already unwound its own ladder — so the teardown
            // it belonged to is complete.
            let finished = UnwindReport {
                stack_empty: true,
                residual: Vec::new(),
            };
            let world = World::classify(&self.last_obs, now, &self.policy);
            let decision = reconcile::on_unwind_done(
                &self.status,
                &self.intent,
                &finished,
                &world,
                now,
                &self.policy,
            );
            self.apply(decision, now);
            return;
        }

        let decision = reconcile::on_attempt_done(
            &self.status,
            &self.intent,
            report.result,
            now,
            &self.policy,
        );
        self.apply(decision, now);
    }

    fn on_unwind_done(&mut self, report: UnwindReport) {
        self.unwind = None;
        let now = Instant::now();

        // Judge the teardown only against a look taken *after* it ran.
        //
        // An observation from before the unwind says nothing about whether it worked, and every
        // retry here happens in microseconds — far faster than the poll interval — so re-checking
        // the same pre-teardown observation would fail the same way every time and burn the whole
        // retry budget in under a millisecond. Treating it as dark falls through to the ordinary
        // rows, which re-observe.
        let world = match self.unwind_started {
            Some(started) if self.last_obs.observed_at < started => World::Dark,
            _ => World::classify(&self.last_obs, now, &self.policy),
        };
        self.unwind_started = None;
        let decision = reconcile::on_unwind_done(
            &self.status,
            &self.intent,
            &report,
            &world,
            now,
            &self.policy,
        );
        self.apply(decision, now);
    }

    // ---------------------------------------------------------------------- the only writer

    fn tick(&mut self, now: Instant) {
        let world = World::classify(&self.last_obs, now, &self.policy);
        let now_unix = chrono::Utc::now().timestamp();
        let decision = reconcile::reconcile(
            &self.status,
            &self.intent,
            &world,
            now,
            now_unix,
            &self.policy,
        );
        self.apply(decision, now);
    }

    /// The single place `status` is ever assigned.
    ///
    /// Because it is assigned unconditionally from the table on every pass, "forgot to restore the
    /// status on this error path" — the bug that used to strand a connect in `Connecting` until the
    /// app was restarted — is not expressible: no code outside this function writes it.
    fn apply(&mut self, decision: Decision, now: Instant) {
        let Decision { next, effects } = decision;

        let changed = self.status != next;
        if changed {
            info!(from = ?self.status, to = ?next, "tunnel status");
            self.status = next;
        }

        for effect in effects {
            self.exec(effect);
        }

        self.settle_pending(now);
        self.publish_if_changed();
    }

    fn exec(&mut self, effect: Effect) {
        match effect {
            Effect::Begin {
                protocol,
                epoch,
                index,
            } => {
                debug_assert!(self.attempt.is_none(), "beginning while an attempt is live");

                let Some(config) = self.configs.get(protocol) else {
                    // A missing config is an attempt failure, not a panic and not a silent stall.
                    // It still goes through the report channel, because the handle is registered
                    // first — a result that skipped the channel would be discarded as stale and
                    // the status would wait forever.
                    let cancel = CancellationToken::new();
                    let tx = self.cmd_tx.clone();
                    tokio::spawn(attempt::run_immediate_failure(
                        tx,
                        epoch,
                        index,
                        types::AttemptError::NoConfig { protocol },
                    ));
                    self.attempt = Some(AttemptHandle {
                        epoch,
                        index,
                        cancel,
                    });
                    return;
                };

                let cancel = CancellationToken::new();
                let ctx = attempt::AttemptCtx {
                    epoch,
                    index,
                    protocol,
                    config,
                    iface: self.iface.clone(),
                    params: self.intent.params().cloned().unwrap_or_default(),
                    backend: self.backend.clone(),
                    platform: self.platform.clone(),
                    journal: self.journal.clone(),
                    policy: self.policy.clone(),
                    cancel: cancel.clone(),
                    tx: self.cmd_tx.clone(),
                    #[cfg(target_os = "android")]
                    app: self.app.clone(),
                };
                tokio::spawn(attempt::run(ctx));
                self.attempt = Some(AttemptHandle {
                    epoch,
                    index,
                    cancel,
                });
                self.cancel_issued = false;
            }

            Effect::CancelAttempt => match &self.attempt {
                Some(handle) => {
                    if !self.cancel_issued {
                        self.cancel_issued = true;
                        // Signal only. The task is never dropped: it unwinds its own stack to
                        // completion and reports, which is the whole reason no failure path in
                        // this file performs teardown.
                        handle.cancel.cancel();
                    }
                }
                // The attempt already reported. Fall through to an unwind of an empty stack, which
                // is a no-op that still drives the state machine forward.
                None => self.exec(Effect::Unwind { extra: None }),
            },

            Effect::Unwind { extra } => {
                if self.unwind.is_some() {
                    return;
                }
                debug_assert!(self.attempt.is_none(), "unwinding while an attempt is live");
                let mut stack = std::mem::take(&mut self.held_stack);
                let platform = self.platform.clone();
                let backend = self.backend.clone();
                let retries = self.policy.undo_retries;
                let tx = self.cmd_tx.clone();
                self.unwind_started = Some(Instant::now());
                self.unwind = Some(tokio::spawn(async move {
                    let report = unwind(
                        &mut stack,
                        extra,
                        platform.as_ref(),
                        backend.as_ref(),
                        retries,
                    )
                    .await;
                    let _ = tx.send(Command::UnwindDone(Box::new(report))).await;
                }));
            }

            Effect::TakeStack(stack) => self.held_stack = stack,
            Effect::ResetSpeed => self.speed.reset(),

            // Only ever emitted on success, which is why a failed cycle can no longer leave the
            // last *failed* protocol recorded as the preferred one.
            Effect::RememberWinner(protocol) => self.configs.set_preferred(Some(protocol)),

            Effect::DemoteIntent => {
                let epoch = self.intent.epoch();
                self.intent = Intent::Down { epoch };
            }

            Effect::Resolve(outcome) => {
                let epoch = self.status.epoch().unwrap_or_else(|| self.intent.epoch());
                self.publish_outcome(epoch, outcome);
            }
            Effect::ResolveFor { epoch, outcome } => self.publish_outcome(epoch, outcome),
        }
    }

    fn publish_outcome(&mut self, epoch: IntentEpoch, outcome: CycleOutcome) {
        info!(%epoch, ?outcome, "cycle finished");
        self.last_outcome = Some(outcome.clone());

        self.recent_outcomes.push_back((epoch, outcome.clone()));
        while self.recent_outcomes.len() > RECENT_OUTCOMES {
            self.recent_outcomes.pop_front();
        }

        let mut remaining = Vec::new();
        for (waiter_epoch, reply) in std::mem::take(&mut self.cycle_waiters) {
            if waiter_epoch == epoch {
                let _ = reply.send(outcome.clone());
            } else {
                remaining.push((waiter_epoch, reply));
            }
        }
        self.cycle_waiters = remaining;
    }

    /// Release anyone waiting for the actor to go quiet, and complete a deferred wipe.
    fn settle_pending(&mut self, now: Instant) {
        if !view::is_quiescent(&self.status) {
            self.expire_pending_clear(now);
            return;
        }

        for reply in std::mem::take(&mut self.quiescent_waiters) {
            let _ = reply.send(());
        }
        if let Some((reply, _)) = self.pending_clear.take() {
            self.configs.clear();
            info!("configs cleared once the tunnel was down");
            let _ = reply.send(Ok(()));
        }
    }

    fn expire_pending_clear(&mut self, now: Instant) {
        if let Some((_, deadline)) = &self.pending_clear
            && now >= *deadline
        {
            let (reply, _) = self.pending_clear.take().expect("checked above");
            warn!("timed out waiting for the tunnel to settle before clearing configs");
            let _ = reply.send(Err(IntentError::SettleTimeout));
        }
    }

    // ------------------------------------------------------------------------------ output

    fn publish(&mut self) {
        self.seq += 1;
        let world = World::classify(&self.last_obs, Instant::now(), &self.policy);
        let state = view::render(
            self.seq,
            &self.status,
            &self.intent,
            &self.last_obs,
            &world,
            self.configs.view(),
            self.last_outcome.clone(),
            &mut self.speed,
            Instant::now(),
            self.observed_once,
        );
        let _ = self.state_tx.send(state);
    }

    fn publish_if_changed(&mut self) {
        // `seq` differs on every render, so compare everything else.
        let world = World::classify(&self.last_obs, Instant::now(), &self.policy);
        let candidate = view::render(
            self.seq + 1,
            &self.status,
            &self.intent,
            &self.last_obs,
            &world,
            self.configs.view(),
            self.last_outcome.clone(),
            &mut self.speed,
            Instant::now(),
            self.observed_once,
        );
        let differs = {
            let current = self.state_tx.borrow();
            let mut same_seq = candidate.clone();
            same_seq.seq = current.seq;
            *current != same_seq
        };
        if differs {
            self.seq += 1;
            let _ = self.state_tx.send(candidate);
        }
    }

    /// The next moment the status itself wants attention: an attempt budget, a retry backoff, a
    /// darkness grace period, or a pending wipe giving up.
    fn deadline(&self) -> Option<Instant> {
        let status_deadline = match &self.status {
            Status::Connecting { deadline, .. } => Some(*deadline),
            Status::Retrying { resume_at, .. } => Some(*resume_at),
            Status::Up(u) => u.dark_since.map(|since| since + self.policy.dark_grace),
            Status::Idle | Status::Unwinding { .. } => None,
        };
        let clear_deadline = self.pending_clear.as_ref().map(|(_, at)| *at);
        match (status_deadline, clear_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    async fn shutdown(&mut self) {
        info!("tunnel actor shutting down");
        if let Some(handle) = &self.attempt {
            handle.cancel.cancel();
        }
        if !self.held_stack.is_empty() {
            let mut stack = std::mem::take(&mut self.held_stack);
            let report = unwind(
                &mut stack,
                None,
                self.platform.as_ref(),
                self.backend.as_ref(),
                1,
            )
            .await;
            if !report.is_clean() {
                warn!(residual = ?report.residual, "shutdown rollback left residue");
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        // Never fires; the `if` guard on the select arm keeps this unreachable.
        None => std::future::pending().await,
    }
}

/// Forwards every published state to the UI as an event.
///
/// Reading from a `watch` means a listener that falls behind is given the newest value rather than
/// a backlog: the UI wants the current state, never a replay of states that are no longer true.
async fn publisher(mut states: watch::Receiver<TunnelState>, app: tauri::AppHandle) {
    use tauri_specta::Event as _;

    while states.changed().await.is_ok() {
        let state = states.borrow_and_update().clone();
        if let Err(e) = crate::vpn::events::TunnelStateChanged(state).emit(&app) {
            warn!(error = %e, "failed to emit the tunnel state");
        }
    }
}

/// Owns the observation clock.
///
/// This lives in Rust rather than in the UI precisely so its cadence cannot be distorted by how
/// many timers a frontend happens to be running — and, on mobile, so it does not stop when the
/// webview is backgrounded and its timers are throttled.
async fn observer(backend: Arc<dyn VpnBackend>, tx: mpsc::Sender<Command>, policy: Policy) {
    loop {
        let obs = backend.observe().await;
        // One poll in flight at a time: the next only starts after this one is delivered.
        if tx.send(Command::Observed(Box::new(obs))).await.is_err() {
            return;
        }
        tokio::time::sleep(policy.poll_active).await;
    }
}
