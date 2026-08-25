//! The actor loop itself, driven end to end against a fake backend and a fake platform.
//!
//! The decision tables are covered row by row in `reconcile_tests.rs`; what those cannot see is
//! the routing *around* the tables — which report goes to which table while unwinding, when the
//! attempt handle is registered and forgotten, which epoch a waiter is released for, what happens
//! to a stale observation. Every test here runs the real loop with the clock paused, so a
//! deadline or a backoff elapses the instant nothing else can run.

use super::handle::{AttemptReport, Command, IntentRequest, TunnelHandle};
use super::types::*;
use super::{TunnelActor, observer};
use crate::vpn::backend::{BackendError, VpnBackend};
use crate::vpn::platform::{DnsSnapshot, Gateway, IpFamily, Platform, PlatformError, TunParams};
use crate::vpn::protocol::{InterfaceName, Protocol};
use crate::vpn::rollback::RollbackStack;
use crate::vpn::state::{ProtocolConfig, SavedVpnConfigs, WgConfig};
use crate::vpn::store::ConfigStore;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, watch};

/// Literal endpoint so the ladder's `lookup_host` never touches DNS.
const WG_CONFIG: &str = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = 127.0.0.1:51820
AllowedIPs = 0.0.0.0/0
";

// -------------------------------------------------------------------------------------- fakes

/// A backend whose tunnel is a flag: `start` raises it, `stop` lowers it, `observe` reports it.
/// Reachable by construction, like the in-process backend it stands in for.
#[derive(Default)]
struct FakeBackend {
    running: std::sync::Mutex<Option<RunningTunnel>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    /// Refuse every start, as a peer that was deleted server-side would look from here.
    refuse_starts: AtomicBool,
}

impl FakeBackend {
    fn running(&self) -> Option<RunningTunnel> {
        self.running.lock().unwrap().clone()
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VpnBackend for FakeBackend {
    async fn start(
        &self,
        config: &ProtocolConfig,
        _: &str,
        _: &TunParams,
        endpoint: SocketAddr,
    ) -> Result<(), BackendError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.refuse_starts.load(Ordering::SeqCst) {
            return Err(BackendError::Engine {
                detail: "start refused".into(),
            });
        }
        *self.running.lock().unwrap() = Some(RunningTunnel {
            protocol: config.protocol(),
            generation: None,
            endpoint: endpoint.to_string(),
            address: config.address(),
            connected_secs: Some(0),
            params: None,
            autonomous: false,
        });
        Ok(())
    }

    async fn stop(&self) -> Result<(), BackendError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        *self.running.lock().unwrap() = None;
        Ok(())
    }

    async fn observe(&self) -> Observation {
        Observation {
            observed_at: Instant::now(),
            view: WorldView::Reachable(TunnelObservation {
                generation: 0,
                running: self.running(),
                starting: false,
                tun_ready: true,
                start_error: None,
                raw_stats: Some(RawStats::default()),
                // A handshake straight away, so verification passes without waiting.
                last_packet_secs: Some(0),
            }),
        }
    }

    async fn ping(&self) -> Result<(), BackendError> {
        Ok(())
    }
}

/// A platform on which every step succeeds. While attempts are parked, `preflight` waits at the
/// ladder's very first step so a test can act while an attempt is provably in flight; each
/// `release` lets exactly one parked attempt continue.
struct FakePlatform {
    park_attempts: AtomicBool,
    released: Notify,
    /// Panic inside the ladder's link step, as a bug in a real platform would.
    crash_attempts: AtomicBool,
}

impl FakePlatform {
    fn new() -> Self {
        Self {
            park_attempts: AtomicBool::new(false),
            released: Notify::new(),
            crash_attempts: AtomicBool::new(false),
        }
    }

    fn crash_attempts(&self) {
        self.crash_attempts.store(true, Ordering::SeqCst);
    }

    fn park_attempts(&self) {
        self.park_attempts.store(true, Ordering::SeqCst);
    }

    /// Release one parked attempt. A permit is stored if none is parked yet.
    fn release(&self) {
        self.released.notify_one();
    }
}

#[async_trait]
impl Platform for FakePlatform {
    fn tun_params(&self) -> TunParams {
        TunParams::default()
    }
    async fn preflight(&self) -> Result<(), PlatformError> {
        if self.park_attempts.load(Ordering::SeqCst) {
            self.released.notified().await;
        }
        Ok(())
    }
    async fn prepare_link(&self, _: &InterfaceName) -> Result<(), PlatformError> {
        assert!(
            !self.crash_attempts.load(Ordering::SeqCst),
            "the fake platform was told to crash the attempt"
        );
        Ok(())
    }
    async fn release_link(&self, _: &InterfaceName) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn configure_address(
        &self,
        _: &InterfaceName,
        _: IpNetwork,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn deconfigure_address(
        &self,
        _: &InterfaceName,
        _: IpNetwork,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn default_gateway(&self, _: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        Ok(None)
    }
    async fn interface_index(&self, _: &InterfaceName) -> Option<u32> {
        None
    }
    async fn add_endpoint_route(
        &self,
        _: IpAddr,
        _: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn remove_endpoint_route(
        &self,
        _: IpAddr,
        _: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn add_routes(
        &self,
        _: &InterfaceName,
        _: &[IpNetwork],
        _: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn remove_routes(
        &self,
        _: &InterfaceName,
        _: &[IpNetwork],
        _: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn capture_dns(
        &self,
        _: &InterfaceName,
        _: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        Ok(DnsSnapshot::Resolvectl)
    }
    async fn configure_dns(
        &self,
        _: &InterfaceName,
        _: &[IpAddr],
        _: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn restore_dns(
        &self,
        _: &InterfaceName,
        _: &DnsSnapshot,
        _: Option<u32>,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    async fn ipv6_enabled(&self) -> bool {
        false
    }
}

// ------------------------------------------------------------------------------------ harness

fn policy() -> Policy {
    Policy {
        // Generous, so no test trips over the attempt budget by accident; the budget itself is
        // exercised with a short one on purpose.
        attempt_budget: Duration::from_secs(3600),
        ..Policy::default()
    }
}

fn configs() -> SavedVpnConfigs {
    SavedVpnConfigs {
        wireguard: Some(WgConfig::from_config_str(WG_CONFIG).expect("fixture must parse")),
        ..Default::default()
    }
}

fn up() -> IntentRequest {
    IntentRequest::Up {
        order: vec![Protocol::WireGuard],
        params: TunnelParams::default(),
    }
}

struct Harness {
    handle: TunnelHandle,
    cmd_tx: mpsc::Sender<Command>,
    states: watch::Receiver<TunnelState>,
    backend: Arc<FakeBackend>,
    platform: Arc<FakePlatform>,
}

impl Harness {
    fn spawn(policy: Policy) -> Self {
        Self::spawn_with(policy, ConfigStore::in_memory(configs()), None)
    }

    fn spawn_with_store(policy: Policy, store: ConfigStore) -> Self {
        Self::spawn_with(policy, store, None)
    }

    /// A world where a tunnel is already running before the actor exists — Android after the UI
    /// process was swiped away, or an always-on start. The only way to reach adoption through the
    /// real loop.
    fn spawn_adopting(policy: Policy, running: RunningTunnel) -> Self {
        Self::spawn_with(policy, ConfigStore::in_memory(configs()), Some(running))
    }

    fn spawn_with(policy: Policy, store: ConfigStore, running: Option<RunningTunnel>) -> Self {
        let backend = Arc::new(FakeBackend::default());
        *backend.running.lock().unwrap() = running;
        let platform = Arc::new(FakePlatform::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(super::CHANNEL_DEPTH);
        let (state_tx, state_rx) = watch::channel(TunnelState::initial());
        let actor = TunnelActor::new(
            backend.clone(),
            platform.clone(),
            None,
            policy.clone(),
            store,
            cmd_tx.clone(),
            state_tx,
        );
        tokio::spawn(observer(backend.clone(), cmd_tx.clone(), policy));
        tokio::spawn(actor.run(cmd_rx));
        Self {
            handle: TunnelHandle::new(cmd_tx.clone(), state_rx.clone()),
            cmd_tx,
            states: state_rx,
            backend,
            platform,
        }
    }

    /// Wait until the published state satisfies `pred`, or fail loudly.
    async fn wait_for(&mut self, what: &str, pred: impl Fn(&TunnelState) -> bool) -> TunnelState {
        let waited = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                if pred(&self.states.borrow()) {
                    return self.states.borrow().clone();
                }
                self.states.changed().await.expect("actor gone");
            }
        })
        .await;
        match waited {
            Ok(state) => state,
            Err(_) => panic!(
                "timed out waiting for {what}; last state: {:?}",
                self.states.borrow()
            ),
        }
    }

    async fn wait_for_phase(&mut self, phase: Phase) -> TunnelState {
        self.wait_for(&format!("phase {phase:?}"), |s| s.phase == phase)
            .await
    }

    async fn set(&self, intent: IntentRequest) -> IntentEpoch {
        self.handle
            .set_intent(intent)
            .await
            .expect("intent accepted")
            .epoch
    }

    async fn outcome(&self, epoch: IntentEpoch) -> CycleOutcome {
        tokio::time::timeout(Duration::from_secs(120), self.handle.await_cycle(epoch))
            .await
            .unwrap_or_else(|_| panic!("epoch {epoch} never resolved"))
            .expect("actor gone")
    }

    /// Bring a tunnel up through the real ladder.
    async fn connect(&mut self) -> IntentEpoch {
        let epoch = self.set(up()).await;
        assert!(matches!(
            self.outcome(epoch).await,
            CycleOutcome::Connected { .. }
        ));
        self.wait_for_phase(Phase::Connected).await;
        epoch
    }

    /// A report as the attempt task would send it, for the paths a cooperative ladder cannot be
    /// made to take on demand.
    async fn report(&self, epoch: IntentEpoch, result: AttemptResult) {
        self.cmd_tx
            .send(Command::AttemptDone(Box::new(AttemptReport {
                epoch,
                index: 0,
                result,
            })))
            .await
            .expect("actor gone");
    }
}

fn established(epoch: IntentEpoch) -> AttemptResult {
    AttemptResult::Established {
        view: UpStatus {
            epoch,
            protocol: Protocol::WireGuard,
            params: Some(TunnelParams::default()),
            adopted: false,
            server_endpoint: "127.0.0.1:51820".into(),
            assigned_ip: "10.0.0.2/32".into(),
            connected_at: 0,
            dark_since: None,
            resolved: false,
        },
        stack: RollbackStack::default(),
    }
}

// -------------------------------------------------------------------------------------- tests

#[tokio::test(start_paused = true)]
async fn a_connect_goes_up_through_the_ladder_and_a_down_tears_it_down() {
    let mut h = Harness::spawn(policy());

    let up_epoch = h.connect().await;
    let state = h.states.borrow().clone();
    assert_eq!(state.protocol, Some(Protocol::WireGuard));
    assert_eq!(state.epoch, up_epoch);
    assert!(h.backend.running().is_some(), "the fake tunnel is running");

    let down_epoch = h.set(IntentRequest::Down).await;
    assert_eq!(h.outcome(down_epoch).await, CycleOutcome::Down);
    h.wait_for_phase(Phase::Disconnected).await;
    assert!(
        h.backend.running().is_none(),
        "the teardown stopped the tunnel"
    );
    assert_eq!(h.backend.stops(), 1, "one teardown, one stop");
}

#[tokio::test(start_paused = true)]
async fn a_down_while_already_idle_still_resolves_its_epoch() {
    // Caught by the disconnect button: with nothing to tear down the table had nothing to
    // resolve, so the caller waiting on the Down epoch waited forever and the UI stayed busy.
    let mut h = Harness::spawn(policy());
    h.wait_for_phase(Phase::Disconnected).await;

    let epoch = h.set(IntentRequest::Down).await;
    assert_eq!(h.outcome(epoch).await, CycleOutcome::Down);

    // And again: every Down gets its own answer, not just the first.
    let again = h.set(IntentRequest::Down).await;
    assert_ne!(again, epoch);
    assert_eq!(h.outcome(again).await, CycleOutcome::Down);
}

#[tokio::test(start_paused = true)]
async fn a_late_success_while_unwinding_is_torn_down_and_never_published_as_connected() {
    // The only path a late-succeeding connect can take: the intent went Down while the attempt
    // was in flight, and the attempt reports Established anyway. Its stack is taken and undone,
    // and the status never says Connected — it is written from the tables, never by an attempt.
    let mut h = Harness::spawn(policy());
    h.platform.park_attempts();

    let up_epoch = h.set(up()).await;
    h.wait_for_phase(Phase::Connecting).await;
    let down_epoch = h.set(IntentRequest::Down).await;
    h.wait_for_phase(Phase::Disconnecting).await;

    h.report(up_epoch, established(up_epoch)).await;

    assert_eq!(h.outcome(down_epoch).await, CycleOutcome::Down);
    let state = h.wait_for_phase(Phase::Disconnected).await;
    assert_ne!(
        state.last_outcome,
        Some(CycleOutcome::Connected {
            protocol: Protocol::WireGuard,
            adopted: false
        })
    );
    assert!(
        !h.states.borrow().busy,
        "nothing is in flight once the late report has been absorbed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_attempt_unwinds_itself_and_the_actor_settles_on_down() {
    let mut h = Harness::spawn(policy());
    h.platform.park_attempts();

    h.set(up()).await;
    h.wait_for_phase(Phase::Connecting).await;
    let down_epoch = h.set(IntentRequest::Down).await;
    h.wait_for_phase(Phase::Disconnecting).await;

    // The ladder sees its token at the next checkpoint and reports Cancelled.
    h.platform.release();
    assert_eq!(h.outcome(down_epoch).await, CycleOutcome::Down);
    h.wait_for_phase(Phase::Disconnected).await;
    assert!(h.backend.running().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_stale_epochs_outcome_is_not_shown_as_the_current_one() {
    let mut h = Harness::spawn(policy());
    h.platform.park_attempts();

    // Attempt 1 is parked; a newer Up supersedes it.
    let first = h.set(up()).await;
    h.wait_for_phase(Phase::Connecting).await;
    let second = h.set(up()).await;
    h.wait_for_phase(Phase::Disconnecting).await;
    h.platform.release();

    // The superseded epoch is released as Cancelled...
    assert_eq!(h.outcome(first).await, CycleOutcome::Cancelled);
    // ...but the sticky outcome is about the intent the UI is looking at, which has none yet:
    // its attempt is the one now parked.
    let state = h
        .wait_for("the second attempt", |s| {
            s.phase == Phase::Connecting && s.epoch == second
        })
        .await;
    assert_eq!(
        state.last_outcome, None,
        "a superseded cycle's Cancelled must not stick"
    );

    h.platform.release();
    assert!(matches!(
        h.outcome(second).await,
        CycleOutcome::Connected { .. }
    ));
    let state = h.wait_for_phase(Phase::Connected).await;
    assert!(matches!(
        state.last_outcome,
        Some(CycleOutcome::Connected { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn an_attempt_task_that_panics_still_reports_and_the_cycle_ends() {
    // The attempt's report is the only thing that leaves Connecting on the success path, and
    // Unwinding has no deadline by design. A task that dies without reporting used to leave the
    // status waiting forever; its join error is now the report it failed to send.
    let mut h = Harness::spawn(policy());
    h.platform.crash_attempts();

    let epoch = h.set(up()).await;
    match h.outcome(epoch).await {
        CycleOutcome::Exhausted { failures } => {
            assert_eq!(failures.len(), 1);
            assert!(matches!(failures[0].error, AttemptError::Crashed { .. }));
        }
        other => panic!("expected the crash on record, got {other:?}"),
    }
    let state = h.wait_for_phase(Phase::Disconnected).await;
    assert_eq!(state.intent, IntentView::Down, "the intent is demoted");
    assert!(!state.busy);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_attempts_self_unwind_is_judged_by_a_fresh_look_not_the_stale_one() {
    // The attempt is parked; meanwhile the world reports a tunnel (one that appeared underneath
    // us). Cancelling the attempt makes it unwind its own — empty — ladder and report. That report
    // used to be judged against the observation taken *before* the cancel, which said Running, so
    // the actor re-ran a teardown with a backend stop it had no reason for, and burnt a retry.
    let mut h = Harness::spawn(policy());
    h.platform.park_attempts();

    h.set(up()).await;
    h.wait_for_phase(Phase::Connecting).await;
    *h.backend.running.lock().unwrap() = Some(RunningTunnel {
        protocol: Protocol::WireGuard,
        generation: None,
        endpoint: "127.0.0.1:51820".into(),
        address: "10.0.0.2/32".into(),
        connected_secs: Some(1),
        params: None,
        autonomous: false,
    });
    // Let the observer see it.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        h.states.borrow().phase == Phase::Connecting,
        "no observation interrupts an attempt"
    );

    let down_epoch = h.set(IntentRequest::Down).await;
    h.wait_for_phase(Phase::Disconnecting).await;
    h.platform.release();

    assert_eq!(h.outcome(down_epoch).await, CycleOutcome::Down);
    assert_eq!(
        h.backend.stops(),
        0,
        "the stale Running look must not drive a re-unwind; a fresh look decides what happens next"
    );
}

#[tokio::test(start_paused = true)]
async fn a_tunnel_that_dies_with_no_reconnect_budget_reports_lost_gave_up_on_the_same_epoch() {
    // The outcome the UI dedups on: Connected and LostGaveUp arrive on the *same* epoch, because
    // the intent never changed. Anything keyed on the epoch alone swallows the second one.
    let mut h = Harness::spawn(Policy {
        reconnect_passes: 1,
        ..policy()
    });
    let epoch = h.connect().await;

    // The tunnel disappears underneath us, and nothing can bring it back.
    h.backend.refuse_starts.store(true, Ordering::SeqCst);
    *h.backend.running.lock().unwrap() = None;

    let state = h
        .wait_for("lost_gave_up", |s| {
            matches!(s.last_outcome, Some(CycleOutcome::LostGaveUp { .. }))
        })
        .await;
    assert_eq!(state.epoch, epoch, "same epoch as the Connected before it");
    assert_eq!(state.phase, Phase::Disconnected);
    assert_eq!(state.intent, IntentView::Down, "the intent is demoted");
}

#[tokio::test(start_paused = true)]
async fn every_caller_asking_for_a_wipe_is_answered_once_the_tunnel_is_down() {
    // A second ClearConfigs while the first was still waiting for quiescence used to replace it,
    // dropping the first caller's reply channel: that caller got "actor gone" for a wipe that
    // then happened anyway.
    let mut h = Harness::spawn(policy());
    h.connect().await;

    let (first, second) = tokio::join!(h.handle.clear_configs(), h.handle.clear_configs());
    assert_eq!(first, Ok(()));
    assert_eq!(second, Ok(()));

    let state = h.wait_for_phase(Phase::Disconnected).await;
    assert!(state.configs.available.is_empty(), "the wipe happened");
    assert!(
        h.backend.running().is_none(),
        "after the tunnel was torn down"
    );
}

/// Real clock, not paused: the persister blocks a blocking-pool thread at the gate, and the
/// paused clock does not auto-advance while a blocking task is in flight — so the "still not
/// answered" timeout below would never elapse.
#[tokio::test]
async fn a_wipe_is_acknowledged_only_once_the_store_has_actually_been_wiped() {
    // The persister runs on a blocking thread; "forgotten" used to be answered as soon as the
    // in-memory copy was empty, while the delete was still queued behind it.
    use crate::vpn::store::testing::{Gate, gated_persister};

    let gate = Gate::closed();
    let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store =
        ConfigStore::with_persister(configs(), gated_persister(gate.clone(), writes.clone()));
    let mut h = Harness::spawn_with_store(policy(), store);
    h.connect().await;

    let handle = h.handle.clone();
    let mut cleared = std::pin::pin!(handle.clear_configs());
    // Polling is what sends the command; a second of real time is plenty for the teardown.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut cleared)
            .await
            .is_err(),
        "not answered while the delete has not run"
    );
    let state = h.wait_for_phase(Phase::Disconnected).await;
    assert!(state.configs.available.is_empty(), "gone from memory");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut cleared)
            .await
            .is_err(),
        "still not answered once down: the delete is what is being waited for"
    );

    gate.open();
    let answer = tokio::time::timeout(Duration::from_secs(120), cleared)
        .await
        .expect("answered once the delete ran");
    assert_eq!(answer, Ok(()));
    assert_eq!(writes.lock().unwrap().last(), Some(&"delete"));
}

#[tokio::test(start_paused = true)]
async fn looks_that_went_stale_while_the_actor_was_stalled_do_not_tear_down_a_healthy_tunnel() {
    // The failure this guards against: the actor task was held for a few seconds (a keyring
    // unlock dialog, at the time), the observer kept looking once a second, and when the actor
    // resumed it replayed the backlog. Every look but the last was older than the staleness
    // window, so each read as dark — and on desktop, with no darkness grace, the second dark
    // look declared the peer lost and tore down a tunnel that had been fine the whole time.
    let mut h = Harness::spawn(policy());
    h.connect().await;
    let running = h.backend.observe().await.view;

    // Ten looks queue up while the actor cannot run: none of these sends yields to it.
    let resumed = Instant::now();
    for age in (1..=10).rev() {
        h.cmd_tx
            .try_send(Command::Observed(Box::new(Observation {
                observed_at: resumed - Duration::from_secs(age),
                view: running.clone(),
            })))
            .expect("queue has room");
    }

    // Let the actor work through the backlog.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let state = h.states.borrow().clone();
    assert_eq!(state.phase, Phase::Connected, "the tunnel was never lost");
    assert!(
        state.backend_reachable,
        "and the last, fresh look is what it kept"
    );
    assert_eq!(h.backend.stops(), 0);
}

#[tokio::test(start_paused = true)]
async fn disconnecting_an_adopted_tunnel_stops_it_before_down_is_resolved() {
    // Adoption takes no rollback stack — there is nothing of ours to undo — so the teardown of an
    // adopted tunnel used to be an unwind of an empty stack: it stopped nothing, resolved Down
    // and published Disconnected while the tunnel was still up, and left row 2 to notice the
    // tunnel a second later and kill it then. On a logout that second was enough for the configs
    // and the autostart bundle to be wiped under a live tunnel.
    let mut h = Harness::spawn_adopting(
        policy(),
        RunningTunnel {
            protocol: Protocol::WireGuard,
            generation: None,
            endpoint: "127.0.0.1:51820".into(),
            address: "10.0.0.2/32".into(),
            connected_secs: Some(42),
            params: None,
            autonomous: false,
        },
    );

    let state = h
        .wait_for("the surviving tunnel to be adopted", |s| {
            s.phase == Phase::Connected
        })
        .await;
    assert!(state.adopted, "we did not start it");
    assert_eq!(h.backend.starts.load(Ordering::SeqCst), 0, "nor rebuild it");

    let down = h.set(IntentRequest::Down).await;
    assert_eq!(h.outcome(down).await, CycleOutcome::Down);
    // Read with nothing awaited in between, so this is the state at the instant Down resolved.
    assert_eq!(
        h.backend.stops(),
        1,
        "the tunnel is stopped by the teardown"
    );
    assert!(h.backend.running().is_none());
}
