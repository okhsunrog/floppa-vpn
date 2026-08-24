//! The actor's vocabulary.
//!
//! Three axes, deliberately separate:
//!
//! - [`Intent`] — what the user wants. Only commands change it.
//! - [`Status`] — what we are doing about that. Only [`reconcile`](super::reconcile) changes it,
//!   and it lives as a local variable of the actor task, so nothing else *can*.
//! - [`World`] — what is actually true, as last observed.
//!
//! Auto-reconnect and auto protocol selection are not features in this design: they are what
//! happens when [`Intent::Up`] outlives a failure. That is why `userIntent`, `abortGen`,
//! `reconnectAttempts`, `reconnectTimeoutId` and `runAutoCycle` have no counterpart here.

use crate::vpn::protocol::Protocol;
use crate::vpn::rollback::StepKind;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------- intent

/// Monotonic, bumped on every accepted intent change — including Down.
///
/// Carried into the Android service and echoed back by it, so an observation from a previous
/// service instance, or a stop for a superseded generation, is rejectable by value rather than by
/// guesswork.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize, Type,
)]
pub struct IntentEpoch(pub u64);

impl std::fmt::Display for IntentEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SplitMode {
    #[default]
    All,
    Include,
    Exclude,
}

/// Everything a *self-initiated* reconnect needs, because at reconnect time there is no caller to
/// supply it. `apps` is sorted and deduped on construction, so `PartialEq` means "the same tunnel"
/// rather than "the same list written the same way".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct TunnelParams {
    pub split_mode: SplitMode,
    pub apps: Vec<String>,
}

impl TunnelParams {
    pub fn new(split_mode: SplitMode, mut apps: Vec<String>) -> Self {
        apps.sort_unstable();
        apps.dedup();
        Self { split_mode, apps }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpIntent {
    pub epoch: IntentEpoch,
    /// Probe order, most preferred first. Non-empty by construction.
    pub order: Vec<Protocol>,
    /// `None` means "any tunnel of a protocol in `order` satisfies me" — used only by the
    /// bootstrap adoption intent. Every caller-issued Up carries `Some`.
    pub params: Option<TunnelParams>,
}

impl UpIntent {
    pub fn accepts(&self, p: Protocol) -> bool {
        self.order.contains(&p)
    }

    /// Is an already-established tunnel good enough for this intent?
    ///
    /// This is what makes "press Connect while connected" a no-op and "change the split rules,
    /// then reconnect" a real teardown — with no branch in the frontend.
    pub fn satisfied_by(&self, up: &UpStatus) -> bool {
        self.accepts(up.protocol)
            && match (&self.params, &up.params) {
                (None, _) => true,
                (Some(want), Some(have)) => want == have,
                // An adopted tunnel's split rules are unknown, so it cannot be proven to match.
                (Some(_), None) => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Down { epoch: IntentEpoch },
    Up(UpIntent),
}

impl Intent {
    pub fn epoch(&self) -> IntentEpoch {
        match self {
            Self::Down { epoch } => *epoch,
            Self::Up(u) => u.epoch,
        }
    }

    pub fn params(&self) -> Option<&TunnelParams> {
        match self {
            Self::Up(u) => u.params.as_ref(),
            Self::Down { .. } => None,
        }
    }

    pub fn is_up(&self) -> bool {
        matches!(self, Self::Up(_))
    }
}

impl Default for Intent {
    fn default() -> Self {
        Self::Down {
            epoch: IntentEpoch(0),
        }
    }
}

// ---------------------------------------------------------------------------------------- status

/// The in-progress auto-select walk.
///
/// Carried through Connecting → Unwinding → Retrying, so what to do after a rollback is computable
/// from the status alone with no side state. This one struct replaces `runAutoCycle`, `abortGen`,
/// `reconnectAttempts` and `reconnectTimeoutId`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cycle {
    pub epoch: IntentEpoch,
    pub order: Vec<Protocol>,
    pub params: Option<TunnelParams>,
    /// Index into `order` of the protocol currently being probed.
    pub index: usize,
    /// How many complete passes over `order` have already been burnt.
    pub pass: u32,
    /// How many passes this cycle may use. A cold connect gets one — walk the order once, then
    /// fail fast. A cycle born from a tunnel that *died* gets the full reconnect budget.
    pub passes_allowed: u32,
    /// One entry per failed probe, across passes. This is what lets the caller find which
    /// protocol reported `verify_failed` — rather than assuming it was the last one tried.
    pub failures: Vec<AttemptFailure>,
}

impl Cycle {
    pub fn start(up: &UpIntent, policy: &Policy) -> Self {
        Self {
            epoch: up.epoch,
            order: up.order.clone(),
            params: up.params.clone(),
            index: 0,
            pass: 0,
            passes_allowed: policy.cold_passes,
            failures: Vec::new(),
        }
    }

    /// Born from a lost tunnel, so it gets the reconnect budget rather than the cold one.
    pub fn reconnect(up: &UpIntent, policy: &Policy) -> Self {
        Self {
            passes_allowed: policy.reconnect_passes,
            ..Self::start(up, policy)
        }
    }

    pub fn protocol(&self) -> Protocol {
        self.order[self.index]
    }

    pub fn is_last_probe(&self) -> bool {
        self.index + 1 >= self.order.len()
    }

    pub fn has_budget(&self) -> bool {
        self.pass + 1 < self.passes_allowed
    }

    /// Advance to the next protocol, wrapping to the next pass at the end of the order.
    pub fn advance(&mut self) {
        if self.is_last_probe() {
            self.index = 0;
            self.pass += 1;
        } else {
            self.index += 1;
        }
    }
}

/// Cosmetic sub-phase of an in-flight attempt, reported by the attempt task.
///
/// Never a reconcile input: it describes how far along the ladder we are, not what is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Preparing,
    Starting,
    Configuring,
    Verifying,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpStatus {
    pub epoch: IntentEpoch,
    pub protocol: Protocol,
    /// `None` for an adopted tunnel: we did not start it, so we do not know its split rules.
    pub params: Option<TunnelParams>,
    pub adopted: bool,
    pub server_endpoint: String,
    pub assigned_ip: String,
    pub connected_at: i64,
    /// When the peer first stopped answering, `None` while it answers.
    ///
    /// This replaces the `unreachable_polls` counter. It is a clock, not a tally, so it cannot be
    /// distorted by how many pollers happen to be running — and there are now none.
    pub dark_since: Option<Instant>,
    /// Whether this epoch's waiter has already been resolved `Connected`. An epoch can enter Up on
    /// a `Dark` observation during adoption hand-over, which is never authoritative enough to
    /// announce success from.
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindOwner {
    /// The attempt task is unwinding its own partial ladder; its terminal report ends the unwind.
    Attempt,
    /// The actor spawned an unwind over the stack it holds.
    Actor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindReason {
    IntentDown,
    IntentChanged,
    AttemptFailed,
    AttemptTimedOut,
    /// Confirmed not running while we believed we were up.
    TunnelDied,
    /// Dark for longer than the grace period.
    PeerLost,
    /// Something is running a different protocol than the one we started.
    Usurped,
    /// A tunnel exists but no Up intent does.
    ForeignTunnel,
    /// Adoption refused: the running protocol is not in the order, or the params differ.
    WrongProtocol,
    /// Steps found in the on-disk journal at startup.
    CrashRecovery,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Nothing of ours is running and no stack is held.
    Idle,
    Connecting {
        cycle: Cycle,
        phase: AttemptPhase,
        deadline: Instant,
    },
    Up(UpStatus),
    /// An unwind is in flight.
    ///
    /// This variant **absorbs every input**. It is the reason a late teardown can no longer race a
    /// newer connect: while unwinding there is no transition that starts anything.
    Unwinding {
        owner: UnwindOwner,
        cycle: Option<Cycle>,
        reason: UnwindReason,
        /// How many times this unwind has been re-run after the world still reported Running.
        tries: u32,
    },
    Retrying {
        cycle: Cycle,
        resume_at: Instant,
    },
}

impl Status {
    pub fn cycle(&self) -> Option<&Cycle> {
        match self {
            Self::Connecting { cycle, .. } | Self::Retrying { cycle, .. } => Some(cycle),
            Self::Unwinding { cycle, .. } => cycle.as_ref(),
            Self::Idle | Self::Up(_) => None,
        }
    }

    /// Which epoch this status belongs to. `None` for Idle, which is what makes Idle unable to
    /// resolve anyone's waiter.
    pub fn epoch(&self) -> Option<IntentEpoch> {
        match self {
            Self::Idle => None,
            Self::Up(u) => Some(u.epoch),
            other => other.cycle().map(|c| c.epoch),
        }
    }

    pub fn is_unwinding(&self) -> bool {
        matches!(self, Self::Unwinding { .. })
    }
}

// ----------------------------------------------------------------------------------- observation

/// One look at the world.
///
/// Replaces the old `Option`-returning info call, whose `None` conflated "no tunnel", "peer not
/// started", "peer refused", "transport died" and "call timed out" into a single value that the
/// caller then had to guess about.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub observed_at: Instant,
    pub view: WorldView,
}

impl Observation {
    /// Boot value: we have never looked.
    pub fn unknown(now: Instant) -> Self {
        Self {
            observed_at: now,
            view: WorldView::Unreachable(UnreachableCause::NotStarted),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorldView {
    Reachable(TunnelObservation),
    Unreachable(UnreachableCause),
}

impl WorldView {
    /// The protocol of a tunnel the peer confirmed is running.
    ///
    /// `None` covers both "no tunnel" and "we could not ask" — which is fine for the one caller
    /// that waits for a tunnel to appear, and is exactly why every *other* caller goes through
    /// [`World::classify`] instead, where those two are kept apart.
    pub fn running_protocol(&self) -> Option<Protocol> {
        match self {
            Self::Reachable(t) => t.running.as_ref().map(|r| r.protocol),
            Self::Unreachable(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreachableCause {
    /// Nothing is listening: the peer never bound its socket, or it is an older build.
    NotStarted,
    /// Connection refused: a stale socket file, or the peer was killed. Strong evidence of death.
    ConnectRefused,
    /// The transport died mid-call.
    TransportBroken,
    /// Our per-call deadline expired: the peer is alive but wedged.
    Timeout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TunnelObservation {
    /// Which generation of the service answered.
    ///
    /// Starting a tunnel tears down the previous service first, and that teardown is asynchronous
    /// — so "something answered" is not the same as "the service we just asked for answered". The
    /// dying previous instance replies quite happily right up until it does not.
    pub epoch: u64,
    pub running: Option<RunningTunnel>,
    /// True between "the peer bound its socket" and "the tunnel start returned". Requires the RPC
    /// server to bind ahead of the tunnel start, which is what turns a failed Android start into
    /// typed state instead of a blind timeout.
    pub starting: bool,
    pub start_error: Option<String>,
    pub raw_stats: Option<RawStats>,
    /// Seconds since the last inbound packet.
    pub last_packet_secs: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

/// The identity of a tunnel that is actually running, reported by the process that owns it.
///
/// Never inferred from the stored config: that is what made an adopted Android tunnel claim
/// whichever protocol the settings happened to name.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningTunnel {
    pub protocol: Protocol,
    /// Which intent started it. `None` when adopting after a restart.
    pub epoch: Option<IntentEpoch>,
    pub endpoint: String,
    pub address: String,
    pub connected_secs: Option<u64>,
}

/// The third axis of the decision table, derived purely from an [`Observation`], the clock and the
/// [`Policy`].
#[derive(Debug, Clone, PartialEq)]
pub enum World {
    /// The peer answered: there is no tunnel. Authoritative on every platform.
    Clear,
    /// The peer answered: this tunnel is running.
    Running(RunningTunnel),
    /// The peer did not answer, is mid-start, or its last answer is too old to trust.
    /// **Never authoritative** — this is the whole point of the type.
    Dark,
}

impl World {
    pub fn classify(obs: &Observation, now: Instant, policy: &Policy) -> World {
        if now.saturating_duration_since(obs.observed_at) > policy.obs_stale_after {
            return World::Dark;
        }
        match &obs.view {
            WorldView::Unreachable(_) => World::Dark,
            // A peer that is mid-start is not yet evidence of anything.
            WorldView::Reachable(t) if t.starting => World::Dark,
            WorldView::Reachable(t) => match &t.running {
                Some(rt) => World::Running(rt.clone()),
                None => World::Clear,
            },
        }
    }

    pub fn is_dark(&self) -> bool {
        matches!(self, Self::Dark)
    }
}

// -------------------------------------------------------------------------- errors and outcomes

/// Why a single attempt failed.
///
/// Note what is deliberately absent: a blanket `From<String>`. The old `ConnectError` had one, and
/// it stamped every `?`-propagated error as a generic tunnel error — which is how a DNS-resolve
/// failure and an address-parse failure ended up indistinguishable from a dead peer. Every variant
/// here is constructed at exactly one call site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptError {
    #[error("VPN permission denied by the user")]
    PermissionDenied,
    #[error("no stored config for {protocol}")]
    NoConfig { protocol: Protocol },
    #[error("network helper unavailable: {detail}")]
    PlatformUnavailable { detail: String },
    #[error("could not resolve `{host}`: {detail}")]
    ResolveFailed { host: String, detail: String },
    #[error("config is not usable: {detail}")]
    InvalidConfig { detail: String },
    #[error("{step:?} failed: {detail}")]
    Platform { step: StepKind, detail: String },
    #[error("tunnel backend failed: {detail}")]
    Backend { detail: String },
    #[error("no handshake / no connectivity through the tunnel")]
    VerifyFailed,
    #[error("attempt exceeded its budget")]
    TimedOut,
    #[error("the VPN service failed to start the tunnel: {detail}")]
    PeerStartFailed { detail: String },
    #[error("cancelled")]
    Cancelled,
}

impl AttemptError {
    /// Should this abort the whole cycle rather than move on to the next protocol?
    ///
    /// Without this, probing three protocols after the user denied VPN consent means three consent
    /// dialogs — and with a reconnect budget, up to nine.
    pub const fn is_fatal_for_cycle(&self) -> bool {
        matches!(
            self,
            Self::PermissionDenied | Self::PlatformUnavailable { .. } | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AttemptFailure {
    pub protocol: Protocol,
    pub error: AttemptError,
    pub pass: u32,
}

/// How a cycle ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CycleOutcome {
    Connected {
        protocol: Protocol,
        adopted: bool,
    },
    /// Every protocol in the order failed, for every allowed pass — or one failed fatally.
    ///
    /// `failures` carries one entry per probe, so the caller can find exactly which protocol
    /// reported `verify_failed` and re-provision *that* peer, instead of assuming it was whichever
    /// protocol happened to be tried last.
    Exhausted {
        failures: Vec<AttemptFailure>,
    },
    /// Was connected, the tunnel died, the reconnect budget ran out.
    LostGaveUp {
        protocol: Protocol,
        passes: u32,
    },
    /// A teardown could not be confirmed: after the allowed re-runs the world still reported a
    /// running tunnel. The intent is demoted and the machine may be dirty.
    UnwindFailed,
    /// Superseded by a newer intent, or torn down by an explicit Down.
    Cancelled,
    /// An explicit Down reached terminal Down.
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntentError {
    #[error("probe order is empty")]
    EmptyOrder,
    #[error("no stored config for any of the requested protocols")]
    NoUsableConfig,
    #[error("the tunnel actor is not running")]
    ActorGone,
    #[error("timed out waiting for the tunnel to settle")]
    SettleTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IntentAccepted {
    pub epoch: IntentEpoch,
}

/// The terminal report from an attempt task.
#[derive(Debug)]
pub enum AttemptResult {
    /// The ladder completed and verification passed. The stack is handed over so the actor can
    /// undo exactly what was applied, whenever that becomes necessary.
    Established {
        view: UpStatus,
        stack: crate::vpn::rollback::RollbackStack,
    },
    /// The attempt failed and has ALREADY unwound its own partial ladder. There is nothing left
    /// for the actor to clean up — which is why no failure path in the actor performs teardown.
    Failed(AttemptError),
    /// The attempt observed its cancellation token, unwound, and stopped.
    Cancelled,
}

// ----------------------------------------------------------------------------- published snapshot

/// The five original status literals are preserved verbatim so the existing indicator component
/// and its translation keys keep working. `Retrying` and `Unknown` are the additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// We have not yet had an authoritative look at the world.
    ///
    /// Distinct from [`Self::Disconnected`], which is a claim: there is no tunnel. Collapsing the
    /// two is why opening the app with a tunnel already running flashed "disconnected" before the
    /// first observation landed — the UI reported an answer it did not have yet.
    Unknown,
    Disconnected,
    Connecting,
    VerifyingConnection,
    Connected,
    Disconnecting,
    Retrying,
}

impl Phase {
    /// The single boolean the button needs.
    ///
    /// Spinner, label, icon, colour and disabled state all derive from this, which is what makes
    /// "spinner showing while the label says Connect" unrepresentable: there is no second source.
    /// `Unknown` counts as busy: the honest thing to show while we do not know is a pending
    /// indicator, not an actionable button offering to do something we cannot yet judge.
    ///
    /// Published as [`TunnelState::busy`] rather than left for the consumer to re-derive. It was
    /// re-derived, in TypeScript, from a second copy of this list — so the claim above was true of
    /// this function and false of the app: adding a phase here would not have reached the button.
    pub const fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Unknown
                | Self::Connecting
                | Self::VerifyingConnection
                | Self::Disconnecting
                | Self::Retrying
        )
    }

    /// Whether the primary button should offer to cancel rather than to connect.
    pub const fn is_cancellable(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::VerifyingConnection | Self::Retrying
        )
    }
}

/// Which protocol is being probed, and how far through the order we are.
///
/// Part of the *same* snapshot as [`Phase`], so the cancel/connect swap and the label can no
/// longer disagree with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AttemptProgress {
    pub protocol: Protocol,
    pub index: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RetryProgress {
    pub pass: u32,
    pub max: u32,
    pub resume_in_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IntentView {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct TrafficStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_bytes_per_sec: f64,
    pub rx_bytes_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigSummary {
    pub protocol: Protocol,
    pub address: String,
    pub server_endpoint: String,
    pub dns: Option<String>,
    pub allowed_ips: String,
    pub mtu: u16,
}

/// `available` is a set; the order lives only in an [`UpIntent`]. `preferred` is "the protocol that
/// last actually worked", written only after a successful attempt — never before a probe.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct ConfigsView {
    pub available: Vec<Protocol>,
    pub preferred: Option<Protocol>,
    pub summaries: Vec<ConfigSummary>,
}

/// Everything the UI can know about the tunnel, in one value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TunnelState {
    /// Bumped only when a state is actually published. Consumers drop any snapshot whose `seq` is
    /// not newer than the one they hold, which closes the seed-versus-first-event race at startup.
    pub seq: u64,
    pub phase: Phase,
    /// [`Phase::is_busy`] for [`Self::phase`], carried so the consumer never restates which phases
    /// count as work in progress.
    pub busy: bool,
    /// [`Phase::is_cancellable`] for [`Self::phase`].
    pub cancellable: bool,
    pub intent: IntentView,
    pub epoch: IntentEpoch,
    pub intent_order: Vec<Protocol>,
    /// The protocol actually running — distinct from the preferred one.
    pub protocol: Option<Protocol>,
    pub adopted: bool,
    pub attempt: Option<AttemptProgress>,
    pub retry: Option<RetryProgress>,
    pub server_endpoint: Option<String>,
    pub assigned_ip: Option<String>,
    pub connected_at: Option<i64>,
    pub last_packet_received: Option<i64>,
    pub stats: TrafficStats,
    /// Sticky until the next accepted intent.
    pub last_outcome: Option<CycleOutcome>,
    pub configs: ConfigsView,
    /// False while the world is dark. This never by itself means the tunnel is down.
    pub backend_reachable: bool,
}

impl TunnelState {
    pub fn initial() -> Self {
        Self {
            seq: 0,
            // Not Disconnected: at seq 0 nothing has been observed, and claiming there is no
            // tunnel before looking is what made an already-running tunnel flash as down.
            phase: Phase::Unknown,
            busy: Phase::Unknown.is_busy(),
            cancellable: Phase::Unknown.is_cancellable(),
            intent: IntentView::Down,
            epoch: IntentEpoch(0),
            intent_order: Vec::new(),
            protocol: None,
            adopted: false,
            attempt: None,
            retry: None,
            server_endpoint: None,
            assigned_ip: None,
            connected_at: None,
            last_packet_received: None,
            stats: TrafficStats::default(),
            last_outcome: None,
            configs: ConfigsView::default(),
            backend_reachable: false,
        }
    }
}

// ---------------------------------------------------------------------------------------- policy

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsFailurePolicy {
    Fatal,
    Tolerate,
}

/// Plain data, so [`reconcile`](super::reconcile) stays a pure function of its arguments.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Observation cadence while idle.
    pub poll_idle: Duration,
    /// Observation cadence while something is happening or the tunnel is up.
    pub poll_active: Duration,
    /// Hard per-call deadline. Replaces the transport's own 10s default, which is what made any
    /// debounce measured in poll *counts* meaningless.
    pub rpc_deadline: Duration,
    /// An observation older than this is dark regardless of what it said.
    pub obs_stale_after: Duration,
    /// How long darkness is tolerated before an up tunnel is declared lost. Zero on desktop, where
    /// the backend is in-process and always answers.
    pub dark_grace: Duration,
    /// Wall-clock budget for one attempt, ladder and verification included.
    pub attempt_budget: Duration,
    pub verify_wg: Duration,
    pub verify_vless: Duration,
    /// Budget for the Android observe-then-stop undo.
    pub android_stop_budget: Duration,
    /// Passes over the order for a cold, user-initiated connect. One means fail fast.
    pub cold_passes: u32,
    /// Passes over the order after a tunnel that was up died.
    pub reconnect_passes: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// How many times an unwind is re-run while the world still reports a running tunnel.
    pub unwind_tries: u32,
    /// How many times one step's undo is retried before it is logged and popped.
    pub undo_retries: u32,
    pub dns_failure: DnsFailurePolicy,
    /// Bound on waiting for a terminal Down when clearing configs.
    pub settle_timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            poll_idle: Duration::from_secs(5),
            poll_active: Duration::from_secs(1),
            rpc_deadline: Duration::from_secs(2),
            obs_stale_after: Duration::from_secs(3),
            dark_grace: Duration::ZERO,
            attempt_budget: Duration::from_secs(25),
            verify_wg: Duration::from_secs(5),
            verify_vless: Duration::from_secs(10),
            android_stop_budget: Duration::from_secs(8),
            cold_passes: 1,
            reconnect_passes: 3,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            unwind_tries: 3,
            undo_retries: 2,
            dns_failure: DnsFailurePolicy::Tolerate,
            settle_timeout: Duration::from_secs(15),
        }
    }
}

impl Policy {
    /// Adapt to the backend: only a cross-process backend can go dark, and only it needs the
    /// extra budget for a consent dialog.
    pub fn for_backend(grace: Duration) -> Self {
        let android = !grace.is_zero();
        Self {
            dark_grace: grace,
            attempt_budget: if android {
                Duration::from_secs(40)
            } else {
                Duration::from_secs(25)
            },
            ..Self::default()
        }
    }

    pub fn backoff(&self, pass: u32) -> Duration {
        self.backoff_base
            .saturating_mul(1u32 << pass.min(5))
            .min(self.backoff_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(order: &[Protocol], params: Option<TunnelParams>) -> UpIntent {
        UpIntent {
            epoch: IntentEpoch(1),
            order: order.to_vec(),
            params,
        }
    }

    fn up_status(protocol: Protocol, params: Option<TunnelParams>) -> UpStatus {
        UpStatus {
            epoch: IntentEpoch(1),
            protocol,
            params,
            adopted: false,
            server_endpoint: "example:51820".into(),
            assigned_ip: "10.0.0.2/32".into(),
            connected_at: 0,
            dark_since: None,
            resolved: true,
        }
    }

    #[test]
    fn tunnel_params_compare_by_content_not_by_spelling() {
        let a = TunnelParams::new(SplitMode::Include, vec!["b".into(), "a".into(), "b".into()]);
        let b = TunnelParams::new(SplitMode::Include, vec!["a".into(), "b".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_running_tunnel_satisfies_an_intent_that_wants_the_same_thing() {
        let params = TunnelParams::new(SplitMode::All, vec![]);
        let intent = up(&[Protocol::AmneziaWg], Some(params.clone()));
        assert!(intent.satisfied_by(&up_status(Protocol::AmneziaWg, Some(params))));
    }

    #[test]
    fn different_split_rules_are_not_satisfied_by_the_running_tunnel() {
        let intent = up(
            &[Protocol::AmneziaWg],
            Some(TunnelParams::new(SplitMode::Include, vec!["x".into()])),
        );
        let running = up_status(
            Protocol::AmneziaWg,
            Some(TunnelParams::new(SplitMode::All, vec![])),
        );
        assert!(!intent.satisfied_by(&running));
    }

    #[test]
    fn an_adopted_tunnel_cannot_satisfy_an_intent_that_specifies_params() {
        let intent = up(
            &[Protocol::AmneziaWg],
            Some(TunnelParams::new(SplitMode::All, vec![])),
        );
        assert!(!intent.satisfied_by(&up_status(Protocol::AmneziaWg, None)));
    }

    #[test]
    fn the_bootstrap_adoption_intent_accepts_any_params() {
        let intent = up(&[Protocol::AmneziaWg], None);
        assert!(intent.satisfied_by(&up_status(Protocol::AmneziaWg, None)));
        assert!(intent.satisfied_by(&up_status(
            Protocol::AmneziaWg,
            Some(TunnelParams::default())
        )));
    }

    #[test]
    fn a_protocol_outside_the_order_never_satisfies_the_intent() {
        let intent = up(&[Protocol::AmneziaWg], None);
        assert!(!intent.satisfied_by(&up_status(Protocol::Vless, None)));
    }

    #[test]
    fn cycle_walks_the_order_then_starts_another_pass() {
        let policy = Policy::default();
        let mut cycle = Cycle::start(
            &up(&[Protocol::AmneziaWg, Protocol::WireGuard], None),
            &policy,
        );
        assert_eq!(cycle.protocol(), Protocol::AmneziaWg);
        assert!(!cycle.is_last_probe());

        cycle.advance();
        assert_eq!(cycle.protocol(), Protocol::WireGuard);
        assert!(cycle.is_last_probe());
        assert_eq!(cycle.pass, 0);

        cycle.advance();
        assert_eq!(cycle.protocol(), Protocol::AmneziaWg);
        assert_eq!(cycle.pass, 1, "wrapping the order starts the next pass");
    }

    #[test]
    fn a_cold_connect_gets_one_pass_and_a_reconnect_gets_the_full_budget() {
        let policy = Policy::default();
        let intent = up(&[Protocol::AmneziaWg], None);
        assert_eq!(Cycle::start(&intent, &policy).passes_allowed, 1);
        assert_eq!(
            Cycle::reconnect(&intent, &policy).passes_allowed,
            policy.reconnect_passes
        );
        assert!(!Cycle::start(&intent, &policy).has_budget());
        assert!(Cycle::reconnect(&intent, &policy).has_budget());
    }

    #[test]
    fn a_stale_observation_is_dark_even_when_it_reported_a_tunnel() {
        let policy = Policy::default();
        let now = Instant::now();
        let obs = Observation {
            observed_at: now,
            view: WorldView::Reachable(TunnelObservation {
                epoch: 0,
                running: None,
                starting: false,
                start_error: None,
                raw_stats: None,
                last_packet_secs: None,
            }),
        };
        assert_eq!(World::classify(&obs, now, &policy), World::Clear);

        let later = now + policy.obs_stale_after + Duration::from_millis(1);
        assert_eq!(World::classify(&obs, later, &policy), World::Dark);
    }

    #[test]
    fn a_peer_that_is_mid_start_is_not_evidence_of_anything() {
        let policy = Policy::default();
        let now = Instant::now();
        let obs = Observation {
            observed_at: now,
            view: WorldView::Reachable(TunnelObservation {
                epoch: 0,
                running: None,
                starting: true,
                start_error: None,
                raw_stats: None,
                last_packet_secs: None,
            }),
        };
        assert_eq!(World::classify(&obs, now, &policy), World::Dark);
    }

    #[test]
    fn every_unreachable_cause_classifies_as_dark_never_as_clear() {
        let policy = Policy::default();
        let now = Instant::now();
        for cause in [
            UnreachableCause::NotStarted,
            UnreachableCause::ConnectRefused,
            UnreachableCause::TransportBroken,
            UnreachableCause::Timeout,
        ] {
            let obs = Observation {
                observed_at: now,
                view: WorldView::Unreachable(cause),
            };
            assert_eq!(
                World::classify(&obs, now, &policy),
                World::Dark,
                "{cause:?} must never be read as 'no tunnel'"
            );
        }
    }

    #[test]
    fn consent_and_helper_failures_stop_the_cycle_but_a_bad_peer_does_not() {
        assert!(AttemptError::PermissionDenied.is_fatal_for_cycle());
        assert!(
            AttemptError::PlatformUnavailable {
                detail: String::new()
            }
            .is_fatal_for_cycle()
        );
        assert!(AttemptError::Cancelled.is_fatal_for_cycle());
        assert!(!AttemptError::VerifyFailed.is_fatal_for_cycle());
        assert!(!AttemptError::TimedOut.is_fatal_for_cycle());
    }

    #[test]
    fn busy_phases_are_exactly_the_ones_that_should_spin() {
        assert!(Phase::Connecting.is_busy());
        assert!(Phase::VerifyingConnection.is_busy());
        assert!(Phase::Disconnecting.is_busy());
        assert!(Phase::Retrying.is_busy());
        assert!(!Phase::Connected.is_busy());
        assert!(!Phase::Disconnected.is_busy());
    }

    #[test]
    fn backoff_grows_then_saturates() {
        let policy = Policy::default();
        assert_eq!(policy.backoff(0), Duration::from_secs(1));
        assert_eq!(policy.backoff(1), Duration::from_secs(2));
        assert_eq!(policy.backoff(2), Duration::from_secs(4));
        assert_eq!(policy.backoff(20), policy.backoff_max);
    }
}
