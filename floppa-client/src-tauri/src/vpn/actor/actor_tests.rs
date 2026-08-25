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
use crate::vpn::backend::VpnBackend;
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
    ) -> Result<(), String> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        *self.running.lock().unwrap() = Some(RunningTunnel {
            protocol: config.protocol(),
            epoch: None,
            endpoint: endpoint.to_string(),
            address: config.address().to_string(),
            connected_secs: Some(0),
        });
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        *self.running.lock().unwrap() = None;
        Ok(())
    }

    async fn observe(&self) -> Observation {
        Observation {
            observed_at: Instant::now(),
            view: WorldView::Reachable(TunnelObservation {
                epoch: 0,
                running: self.running(),
                starting: false,
                start_error: None,
                raw_stats: Some(RawStats::default()),
                // A handshake straight away, so verification passes without waiting.
                last_packet_secs: Some(0),
            }),
        }
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A platform on which every step succeeds. While attempts are parked, `preflight` waits at the
/// ladder's very first step so a test can act while an attempt is provably in flight; each
/// `release` lets exactly one parked attempt continue.
struct FakePlatform {
    park_attempts: AtomicBool,
    released: Notify,
}

impl FakePlatform {
    fn new() -> Self {
        Self {
            park_attempts: AtomicBool::new(false),
            released: Notify::new(),
        }
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
        let backend = Arc::new(FakeBackend::default());
        let platform = Arc::new(FakePlatform::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(super::CHANNEL_DEPTH);
        let (state_tx, state_rx) = watch::channel(TunnelState::initial());
        let actor = TunnelActor::new(
            backend.clone(),
            platform.clone(),
            None,
            policy.clone(),
            ConfigStore::in_memory(configs()),
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
