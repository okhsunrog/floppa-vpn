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
//!
//! Observations share that channel on purpose: a look taken before an attempt started its tunnel
//! must be handled before that attempt's report, or it reads as "no tunnel" against a fresh Up.
//! The one thing the queue must not do is replay looks that went stale while this task could not
//! run — each of those classifies as dark, and on desktop, with no darkness grace, the second one
//! declared a healthy tunnel lost. A look that is already stale when it is handled is dropped.

pub mod attempt;
pub mod handle;
pub mod intent;
pub mod outcome;
pub mod policy;
pub mod reconcile;
pub mod snapshot;
pub mod status;
pub mod types;
pub mod view;
pub mod world;

#[cfg(all(test, not(target_os = "android")))]
#[path = "actor_tests.rs"]
mod tests;

use self::handle::{AttemptReport, Command, IntentRequest, TunnelHandle};
use self::reconcile::{Decision, Effect};
use self::types::{
    AttemptError, AttemptPhase, AttemptResult, ConfigsView, CycleOutcome, Intent, IntentAccepted,
    IntentEpoch, IntentError, Observation, Policy, Status, Traffic, TrafficStats, TunnelState,
    UpIntent, World, WorldView,
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
    /// The store's projection, rebuilt only when the store changes — every publish reads it.
    configs_view: ConfigsView,
    speed: SpeedTracker,
    /// Traffic as of the last observation. Computed there, once, so the speed tracker sees each
    /// sample exactly once and rendering stays pure.
    traffic: Traffic,
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

    // ---- bookkeeping ----
    next_epoch: u64,
    /// Identities for the `:vpn` service instances this process starts. Separate from
    /// [`IntentEpoch`] on purpose — see [`ServiceGenerations`](crate::vpn::autostart::ServiceGenerations).
    #[cfg(target_os = "android")]
    generations: crate::vpn::autostart::ServiceGenerations,
    seq: u64,
    last_outcome: Option<CycleOutcome>,
    recent_outcomes: VecDeque<(IntentEpoch, CycleOutcome)>,
    cycle_waiters: Vec<(IntentEpoch, oneshot::Sender<CycleOutcome>)>,
    quiescent_waiters: Vec<oneshot::Sender<()>>,
    /// Everyone waiting for a wipe, each with their own deadline. A second request while one is
    /// pending joins the wait rather than replacing — and silently dropping — the first.
    pending_clear: Vec<(oneshot::Sender<Result<(), IntentError>>, Instant)>,

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
            configs_view: configs.view(),
            configs,
            speed: SpeedTracker::new(),
            traffic: Traffic::default(),
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
            next_epoch: 1,
            #[cfg(target_os = "android")]
            generations: crate::vpn::autostart::ServiceGenerations::new(),
            seq: 0,
            last_outcome: None,
            recent_outcomes: VecDeque::new(),
            cycle_waiters: Vec::new(),
            quiescent_waiters: Vec::new(),
            pending_clear: Vec::new(),
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
                        // Every handle is gone. Unreachable in practice — Tauri holds a handle
                        // for the life of the process, and exit goes through a Down intent and
                        // `await_quiescent` — so there is no teardown here to keep honest.
                        info!("every tunnel handle was dropped; the actor stops");
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
        self.recover_journal();
        if !self.held_stack.is_empty() {
            self.status = Status::Unwinding {
                cycle: None,
                reason: types::UnwindReason::CrashRecovery,
                tries: 0,
            };
            self.exec(Effect::Unwind { extra: None });
            return;
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
                } else if epoch < self.intent.epoch() {
                    // Superseded before anyone asked about it. `accept_intent` releases the
                    // waiters that exist at the time, and a caller sends its intent and its wait
                    // as two calls with a state read in between — so an intent accepted in that
                    // gap left this one waiting for a cycle that had already been abandoned, and
                    // the button spun until the next intent.
                    debug!(%epoch, "answering a wait for an epoch that was already superseded");
                    let _ = reply.send(CycleOutcome::Cancelled);
                } else {
                    self.cycle_waiters.push((epoch, reply));
                }
                Reconcile::No
            }

            Command::ImportConfig { raw, reply } => {
                let result = self.edit_configs(|c| c.import(&raw));
                let _ = reply.send(result);
                Reconcile::No
            }

            Command::ClearConfigs { reply } => {
                // Go down and wait for genuine quiescence before wiping, rather than deciding from
                // whatever status the caller last observed — which is how a live adopted tunnel
                // could survive being forgotten. The wipe itself happens in `settle_pending`,
                // once the tick issued below has confirmed Idle.
                let _ = self.accept_intent(IntentRequest::Forget);
                self.pending_clear
                    .push((reply, Instant::now() + self.policy.settle_timeout));
                Reconcile::Yes
            }

            Command::ForgetPreferred { reply } => {
                self.edit_configs(|c| c.set_preferred(None));
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

            Command::FlushConfigs { reply } => {
                // Answered from a task of its own: the store writes on a blocking thread, and this
                // loop never waits on one.
                let flushed = self.configs.flush();
                tokio::spawn(async move {
                    flushed.wait().await;
                    let _ = reply.send(());
                });
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

                // A look that went stale in the queue says nothing about now, and the world would
                // read as dark from it regardless of what it saw. It was superseded, not missed:
                // the observer keeps looking once a second, so a fresh one is behind it.
                let age = Instant::now().saturating_duration_since(obs.observed_at);
                if age > self.policy.obs_stale_after {
                    debug!(?age, "dropping a look that went stale in the queue");
                    return Reconcile::No;
                }
                self.traffic = self.traffic_of(&obs);
                self.last_obs = *obs;
                Reconcile::Yes
            }
        }
    }

    /// Feed one sample to the speed tracker. Every observation is a sample, whatever the status:
    /// entering Up resets the tracker, so the first sample after that only sets the baseline.
    fn traffic_of(&mut self, obs: &Observation) -> Traffic {
        match &obs.view {
            WorldView::Reachable(t) => {
                let raw = t.raw_stats.unwrap_or_default();
                let (tx_bytes_per_sec, rx_bytes_per_sec) =
                    self.speed.update(raw.tx_bytes, raw.rx_bytes);
                Traffic {
                    stats: TrafficStats {
                        tx_bytes: raw.tx_bytes,
                        rx_bytes: raw.rx_bytes,
                        tx_bytes_per_sec,
                        rx_bytes_per_sec,
                    },
                    last_packet_secs: t.last_packet_secs,
                }
            }
            WorldView::Unreachable(_) => Traffic::default(),
        }
    }

    /// The one way to change the store, so its cached projection can never go stale.
    fn edit_configs<T>(&mut self, edit: impl FnOnce(&mut ConfigStore) -> T) -> T {
        let out = edit(&mut self.configs);
        self.configs_view = self.configs.view();
        out
    }

    /// Accept an intent change and mint a new epoch for it.
    ///
    /// There is no "busy" rejection here, and that is deliberate: with a single owner and a
    /// write-only intent queue, a caller cannot arrive at a bad moment. Whether anything actually
    /// starts is decided by the table, inside the loop.
    fn accept_intent(&mut self, request: IntentRequest) -> Result<IntentAccepted, IntentError> {
        let epoch = self.mint_epoch();

        let intent = match request {
            IntentRequest::Down => Intent::Down {
                epoch,
                forget: false,
            },
            IntentRequest::Forget => Intent::Down {
                epoch,
                forget: true,
            },
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

    /// A sub-phase update. Never a reconcile input: it moves the label, not the state.
    ///
    /// With one exception, and it is not cosmetic: leaving `Preparing` restarts the attempt's
    /// budget, once. Preparing is where the desktop ladder installs its privileged helper, and
    /// that spawns `pkexec` and waits for a human — so on a first run the budget was spent
    /// waiting for a password prompt, row 11 recorded the protocol as `TimedOut`, and the
    /// password the user then typed arrived to find the attempt cancelled. The budget is meant to
    /// bound *our* work, and our work starts here.
    fn apply_progress(&mut self, epoch: IntentEpoch, index: usize, phase: AttemptPhase) {
        let budget = self.policy.attempt_budget;
        if let Status::Connecting {
            cycle,
            phase: current,
            deadline,
        } = &mut self.status
            && cycle.epoch == epoch
            && cycle.index == index
        {
            if *current == AttemptPhase::Preparing && phase != AttemptPhase::Preparing {
                *deadline = Instant::now() + budget;
            }
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

        let now = Instant::now();

        // A crashed task took its stack down with it. The durable steps are in the journal, so
        // they are picked up from there — the same recovery as after a crashed process.
        if let AttemptResult::Failed(AttemptError::Crashed { .. }) = &report.result {
            self.recover_journal();
        }

        // While unwinding, the attempt was cancelled *by* that unwind, so its report completes the
        // teardown rather than being a connect outcome. This is the only path a late-succeeding
        // connect can take, and it is why one can never publish "connected" after a teardown: the
        // status is written from the tables, never by an attempt.
        if matches!(self.status, Status::Unwinding { .. }) {
            match report.result {
                AttemptResult::Established { stack, .. } => {
                    // It won the race and applied things. Take the stack so the teardown undoes
                    // exactly what it did, and let that unwind's completion drive the next step.
                    self.held_stack = stack;
                    self.exec(Effect::Unwind { extra: None });
                }
                AttemptResult::Failed(AttemptError::Crashed { .. }) => {
                    // It never unwound anything. Undo what the journal recovered and stop
                    // whatever it may have started.
                    self.exec(Effect::Unwind {
                        extra: Some(crate::vpn::rollback::ExtraUndo::StopBackend),
                    });
                }
                AttemptResult::Failed(_) | AttemptResult::Cancelled => {
                    // It has already unwound its own ladder — so the teardown it belonged to is
                    // complete, and is judged exactly like one the actor ran itself.
                    let finished = UnwindReport {
                        finished_at: now,
                        stack_empty: true,
                        residual: Vec::new(),
                    };
                    let world = self.judge_unwind(&finished, now);
                    let decision = reconcile::on_unwind_done(
                        &self.status,
                        &self.intent,
                        &finished,
                        &world,
                        now,
                        &self.policy,
                    );
                    self.apply(decision, now);
                }
            }
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
        let world = self.judge_unwind(&report, now);
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

    /// The world a finished teardown is judged against: only a look taken *after* it finished.
    ///
    /// An observation from before the unwind says nothing about whether it worked, and every
    /// retry here happens in microseconds — far faster than the poll interval — so re-checking
    /// the same pre-teardown observation would fail the same way every time and burn the whole
    /// retry budget in under a millisecond. Treating it as dark falls through to the ordinary
    /// rows, which re-observe. One judge for both the actor's own unwinds and an attempt's
    /// self-unwind: the latter used to be judged against the stale look, and so paid for an extra
    /// stop and a burnt retry every time.
    ///
    /// The cutoff is the unwind's *end*, not its start. A desktop unwind of a real stack is four
    /// privileged calls before the backend is stopped, so a look that began in the middle of it
    /// saw a tunnel that the same unwind went on to stop — and step 0 of the table read that as
    /// "the teardown did not work", ran another one, and spent a retry on an artefact of timing.
    fn judge_unwind(&self, report: &UnwindReport, now: Instant) -> World {
        if self.last_obs.observed_at < report.finished_at {
            return World::Dark;
        }
        World::classify(&self.last_obs, now, &self.policy)
    }

    /// Pick up whatever a crashed attempt or a previous process left recorded in the journal.
    fn recover_journal(&mut self) {
        let Some(journal) = &self.journal else {
            return;
        };
        let orphaned = journal.read_orphaned();
        if orphaned.is_empty() {
            return;
        }
        warn!(
            count = orphaned.len(),
            "recovering network changes from the journal"
        );
        self.held_stack = RollbackStack::from_orphaned(orphaned, Some(journal.clone()));
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

        self.resolve_idle_down();
        self.settle_pending(now);
        self.publish_if_changed();
    }

    /// A Down intent that finds nothing to tear down has still reached Down.
    ///
    /// The table resolves a Down epoch only at the end of a teardown, so a Down issued while Idle
    /// resolved nobody — and the caller waiting on it (the disconnect button) waited forever. Epoch
    /// zero is the boot intent nobody asked for, so nobody is waiting on it.
    fn resolve_idle_down(&mut self) {
        let Intent::Down { epoch, .. } = self.intent else {
            return;
        };
        if epoch == IntentEpoch::default() || !view::is_quiescent(&self.status) {
            return;
        }
        if self.recent_outcomes.iter().any(|(e, _)| *e == epoch) {
            return;
        }
        self.publish_outcome(epoch, CycleOutcome::Down);
    }

    fn exec(&mut self, effect: Effect) {
        match effect {
            Effect::Begin {
                protocol,
                epoch,
                index,
                params,
            } => {
                debug_assert!(self.attempt.is_none(), "beginning while an attempt is live");

                let Some(config) = self.configs.get(protocol) else {
                    // A missing config is an attempt failure, not a panic and not a silent stall.
                    // It still goes through the report channel, because the handle is registered
                    // first — a result that skipped the channel would be discarded as stale and
                    // the status would wait forever.
                    let cancel = CancellationToken::new();
                    let tx = self.cmd_tx.clone();
                    self.spawn_attempt(
                        epoch,
                        index,
                        attempt::run_immediate_failure(
                            tx,
                            epoch,
                            index,
                            types::AttemptError::NoConfig { protocol },
                        ),
                    );
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
                    // One per service start, never reused: the ladder hands it to the service and
                    // every later "is this ours?" check compares against it.
                    #[cfg(target_os = "android")]
                    generation: self.generations.mint(),
                    index,
                    protocol,
                    config,
                    iface: self.iface.clone(),
                    params,
                    backend: self.backend.clone(),
                    platform: self.platform.clone(),
                    journal: self.journal.clone(),
                    policy: self.policy.clone(),
                    cancel: cancel.clone(),
                    tx: self.cmd_tx.clone(),
                    #[cfg(target_os = "android")]
                    app: self.app.clone(),
                };
                self.spawn_attempt(epoch, index, attempt::run(ctx));
                self.attempt = Some(AttemptHandle {
                    epoch,
                    index,
                    cancel,
                });
            }

            // Issued at most once per attempt: it is the effect of leaving Connecting, and
            // Unwinding absorbs everything until the attempt has reported.
            Effect::CancelAttempt => match &self.attempt {
                Some(handle) => {
                    // Signal only. The task is never dropped: it unwinds its own stack to
                    // completion and reports, which is the whole reason no failure path in this
                    // file performs teardown. Its report is judged like any other unwind's, from
                    // the moment it lands.
                    handle.cancel.cancel();
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
                self.unwind = Some(self.spawn_unwind(async move {
                    unwind(
                        &mut stack,
                        extra,
                        platform.as_ref(),
                        backend.as_ref(),
                        retries,
                    )
                    .await
                }));
            }

            Effect::TakeStack(stack) => self.held_stack = stack,
            Effect::ResetSpeed => {
                self.speed.reset();
                self.traffic = Traffic::default();
            }

            // Only ever emitted on success, which is why a failed cycle can no longer leave the
            // last *failed* protocol recorded as the preferred one.
            Effect::RememberWinner(protocol) => {
                self.edit_configs(|c| c.set_preferred(Some(protocol)));
            }

            Effect::DemoteIntent => {
                let epoch = self.intent.epoch();
                self.intent = Intent::Down {
                    epoch,
                    forget: false,
                };
            }

            // The one effect that promotes an intent, and the reason it exists at all: a tunnel
            // the system started on its own is not a foreign tunnel to be killed. The epoch is
            // minted here because the table never invents one.
            Effect::AdoptAutonomous { protocol, params } => {
                let epoch = self.mint_epoch();
                info!(%protocol, %epoch, "adopting the tunnel the system started on its own");
                self.intent = Intent::Up(UpIntent {
                    epoch,
                    order: vec![protocol],
                    params,
                });
                self.last_outcome = None;
            }

            Effect::Resolve { epoch, outcome } => self.publish_outcome(epoch, outcome),
        }
    }

    /// Run an attempt task and make sure it reports even if it does not return.
    ///
    /// A task that panics, or is aborted with the runtime, sends nothing — and `Unwinding` has
    /// no deadline, because a teardown holding the only record of what was applied must not be
    /// abandoned by a timer. Its join error is turned into the report it failed to send, so the
    /// status can never wait forever on a task that no longer exists.
    fn spawn_attempt(
        &self,
        epoch: IntentEpoch,
        index: usize,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let join = tokio::spawn(task);
        let tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = join.await {
                let detail = if e.is_panic() {
                    "panicked".to_string()
                } else {
                    e.to_string()
                };
                tracing::error!(%epoch, index, %detail, "attempt task crashed");
                let _ = tx
                    .send(Command::AttemptDone(Box::new(AttemptReport {
                        epoch,
                        index,
                        result: AttemptResult::Failed(AttemptError::Crashed { detail }),
                    })))
                    .await;
            }
        });
    }

    /// Run an unwind task and make sure it reports even if it does not return.
    ///
    /// The same guarantee `spawn_attempt` gives, and for a stronger reason: `Unwinding` has no
    /// deadline and absorbs every intent, so an unwind task that panicked left the actor with
    /// `self.unwind` set forever — every later `Effect::Unwind` returned silently, Connect and
    /// Disconnect did nothing, and a wipe could only time out. Whatever the task was holding is
    /// gone with it, so the synthesised report says the stack is not empty: the durable steps are
    /// in the journal, and the next start recovers them from there.
    fn spawn_unwind(
        &self,
        task: impl std::future::Future<Output = UnwindReport> + Send + 'static,
    ) -> JoinHandle<()> {
        let tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let join = tokio::spawn(task);
            let report = match join.await {
                Ok(report) => report,
                Err(e) => {
                    let detail = if e.is_panic() {
                        "panicked".to_string()
                    } else {
                        e.to_string()
                    };
                    tracing::error!(%detail, "the unwind task crashed");
                    UnwindReport {
                        finished_at: Instant::now(),
                        stack_empty: false,
                        residual: vec![(
                            crate::vpn::rollback::StepKind::StartBackend,
                            format!("the unwind task did not finish: {detail}"),
                        )],
                    }
                }
            };
            let _ = tx.send(Command::UnwindDone(Box::new(report))).await;
        })
    }

    fn publish_outcome(&mut self, epoch: IntentEpoch, outcome: CycleOutcome) {
        info!(%epoch, ?outcome, "cycle finished");
        // Sticky for the UI only when it is about the intent the UI is looking at. A superseded
        // cycle's `Cancelled`, released after a newer intent was accepted, must not overwrite what
        // that newer intent is about to report.
        if epoch == self.intent.epoch() {
            self.last_outcome = Some(outcome.clone());
        }

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
        if !self.pending_clear.is_empty() {
            let persisted = self.edit_configs(|c| c.clear());
            info!("configs cleared once the tunnel was down; answering once the store is wiped");
            // The last-good bundle holds the same keys, and an always-on start that found it
            // after a Forget would bring back a tunnel the user just asked to be rid of.
            #[cfg(target_os = "android")]
            if let Ok(dir) = crate::vpn::config::config_dir() {
                tokio::task::spawn_blocking(move || crate::vpn::autostart::remove(&dir));
            }
            // Answered only once the delete has actually run, from a task of its own: "forgotten"
            // used to be said as soon as the in-memory copy was empty, while the keys were still
            // in the keyring — and an app quit right then kept them there.
            let waiters = std::mem::take(&mut self.pending_clear);
            tokio::spawn(async move {
                persisted.wait().await;
                for (reply, _) in waiters {
                    let _ = reply.send(Ok(()));
                }
            });
        }
    }

    fn expire_pending_clear(&mut self, now: Instant) {
        let (expired, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending_clear)
            .into_iter()
            .partition(|(_, deadline)| now >= *deadline);
        self.pending_clear = waiting;
        for (reply, _) in expired {
            warn!("timed out waiting for the tunnel to settle before clearing configs");
            let _ = reply.send(Err(IntentError::SettleTimeout));
        }
    }

    // ------------------------------------------------------------------------------ output

    fn render(&self, seq: u64) -> TunnelState {
        let now = Instant::now();
        let world = World::classify(&self.last_obs, now, &self.policy);
        view::render(
            seq,
            &self.status,
            &self.intent,
            &world,
            self.traffic,
            &self.configs_view,
            self.last_outcome.clone(),
            now,
            self.observed_once,
        )
    }

    fn publish(&mut self) {
        self.seq += 1;
        let state = self.render(self.seq);
        let _ = self.state_tx.send(state);
    }

    fn publish_if_changed(&mut self) {
        let candidate = self.render(self.seq + 1);
        let differs = !self.state_tx.borrow().eq_ignoring_seq(&candidate);
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
        let clear_deadline = self.pending_clear.iter().map(|(_, at)| *at).min();
        match (status_deadline, clear_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
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
