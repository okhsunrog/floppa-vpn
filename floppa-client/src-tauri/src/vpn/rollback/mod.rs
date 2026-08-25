//! A rollback stack: every mutation of the machine is recorded before it is applied, and undone
//! in reverse.
//!
//! This replaces the previous symmetric-teardown design, where `Platform::cleanup()` unconditionally
//! ran "undo everything that might have been done" against hidden state held inside the platform
//! object (`original_resolv_conf`, `saved_gateway`, `saved_endpoint_ip`, `saved_routes`). That
//! design had three structural problems, all of which are unrepresentable here:
//!
//! 1. **The undo state was a second, competing stack.** It could be captured twice without an
//!    intervening restore — a second `configure_dns` overwrote the saved `/etc/resolv.conf` with
//!    the file floppa itself had just written, so the "restore" made the damage permanent. A
//!    [`Step`] owns its snapshot, and a `debug_assert` rejects two DNS steps on one stack.
//! 2. **Undo consumed its own evidence before trying.** `remove_routes` did `saved_routes.drain(..)`
//!    and `restore_dns` did `original_resolv_conf.take()` *before* running the command, and then
//!    discarded the result — so a failed undo was both unrecoverable and unretryable. Here a step
//!    is popped only after its undo returns `Ok`, and is retried first.
//! 3. **Applying and undoing could interleave.** Exactly one owner holds a stack at any instant.
//!
//! # The rule
//!
//! Push, then apply. Never the other order. The privileged helper can leave an address applied
//! while exiting non-zero, `add-routes` aborts mid-loop with earlier routes already installed, and
//! Windows `netsh` reports success in a language we do not parse — so "the apply returned an
//! error" never means "nothing happened". [`Evidence::Attempted`] records that ambiguity, and its
//! undo runs anyway.

use crate::vpn::backend::VpnBackend;
use crate::vpn::platform::{DnsSnapshot, Gateway, Platform};
use crate::vpn::protocol::InterfaceName;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use tracing::{info, warn};

pub mod journal;
pub use journal::{Journal, StackId};

/// One reversible mutation of the machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Step {
    /// Linux: the persistent TUN created by the helper. Windows: reaping a stale Wintun adapter.
    PrepareLink { iface: InterfaceName },
    /// The tunnel itself. Undo is `backend.stop()`, which the caller supplies as an
    /// [`ExtraUndo::StopBackend`] because the stack has no handle on the backend.
    StartBackend { iface: InterfaceName },
    Address {
        iface: InterfaceName,
        addr: IpNetwork,
    },
    /// The gateway is resolved *before* the push so the undo can match on destination AND gateway.
    /// Previously the gateway was saved and then thrown away, so the undo deleted any route to
    /// that destination — after a roaming event, which is exactly when a reconnect happens, that
    /// is the wrong route or none at all.
    EndpointRoute {
        endpoint: IpAddr,
        gateway: Option<Gateway>,
    },
    /// `routes` is the already-split list (a /0 becomes two /1s), so the undo removes exactly what
    /// was added rather than a hardcoded set.
    Routes {
        iface: InterfaceName,
        routes: Vec<IpNetwork>,
        if_index: Option<u32>,
    },
    Dns {
        iface: InterfaceName,
        snapshot: DnsSnapshot,
        if_index: Option<u32>,
    },
    /// Android's single platform step: `VpnService.Builder.establish()` applies the address,
    /// routes and DNS as one unit, so there is nothing finer to record.
    ///
    /// Its undo is cross-process, and the OS can also perform it unilaterally — revoked consent, a
    /// low-memory kill — so "undo returned Ok" is not the same as "it is gone". The caller
    /// re-observes rather than trusting the return value.
    AndroidService { epoch: u64 },
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    PrepareLink,
    StartBackend,
    Address,
    EndpointRoute,
    Routes,
    Dns,
    AndroidService,
}

impl Step {
    pub const fn kind(&self) -> StepKind {
        match self {
            Self::PrepareLink { .. } => StepKind::PrepareLink,
            Self::StartBackend { .. } => StepKind::StartBackend,
            Self::Address { .. } => StepKind::Address,
            Self::EndpointRoute { .. } => StepKind::EndpointRoute,
            Self::Routes { .. } => StepKind::Routes,
            Self::Dns { .. } => StepKind::Dns,
            Self::AndroidService { .. } => StepKind::AndroidService,
        }
    }

    /// Does this step's effect outlive the process that applied it?
    ///
    /// Only these are journalled. The rest are either self-healing (the TUN is persistent and
    /// deconfiguring it is unconditionally safe) or meaningless once the owning process is gone.
    ///
    /// The endpoint host route is durable too: it lives in the kernel's (or Windows') routing
    /// table independently of the tunnel interface, so it survives a crash just as the /1 routes
    /// do. Leaving it out meant the next connect on the same network hit `ip route add` with
    /// "File exists" for every protocol in the ladder.
    pub const fn durable(&self) -> bool {
        matches!(
            self,
            Self::EndpointRoute { .. } | Self::Routes { .. } | Self::Dns { .. }
        )
    }
}

/// What the apply actually did.
///
/// Undo takes a *reference*: a failed undo is retried with the same evidence, so it must not be
/// destroyed by the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Evidence {
    /// Pushed before `apply` ran, and `apply` has not reported back. The undo must still be
    /// attempted — a non-zero exit does not mean nothing was applied.
    Attempted,
    /// Apply returned `Ok`, and the step's payload has been upgraded with what it learned.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Applied {
    pub step: Step,
    pub evidence: Evidence,
}

/// Something to undo that no [`Step`] describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraUndo {
    /// Stop a tunnel this stack did not start (an adopted one), or the backend behind
    /// [`Step::StartBackend`].
    StopBackend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnwindReport {
    pub stack_empty: bool,
    /// Steps whose undo never succeeded, with the last error. Non-empty means the machine is not
    /// fully restored and the user should be told.
    pub residual: Vec<(StepKind, String)>,
}

impl UnwindReport {
    pub fn is_clean(&self) -> bool {
        self.stack_empty && self.residual.is_empty()
    }
}

#[derive(Debug)]
pub struct RollbackStack {
    /// Names this stack's records in the journal, so its writes replace only those. Several
    /// stacks share one journal over time — every attempt has its own, and the actor holds one —
    /// and what one of them gave up on must not be erased by the next one's bookkeeping.
    id: StackId,
    steps: Vec<Applied>,
    journal: Option<Journal>,
}

impl Default for RollbackStack {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RollbackStack {
    pub fn new(journal: Option<Journal>) -> Self {
        Self {
            id: StackId::next(),
            steps: Vec::new(),
            journal,
        }
    }

    /// Rebuild a stack from steps a previous process — or a crashed attempt — left in the journal.
    ///
    /// The steps are exactly as they were persisted, so unwinding this is indistinguishable from
    /// unwinding the stack that wrote it — which is the point: crash recovery is not a separate
    /// code path. The journal is rewritten as this stack's alone: `steps` is everything it held,
    /// and the records left under their old stacks would otherwise be recovered a second time.
    pub fn from_orphaned(steps: Vec<Applied>, journal: Option<Journal>) -> Self {
        let stack = Self {
            id: StackId::next(),
            steps,
            journal,
        };
        if let Some(journal) = &stack.journal {
            journal.claim(stack.id, stack.steps.iter().filter(|a| a.step.durable()));
        }
        stack
    }

    /// Record a step, persist it if durable, and only then let the caller apply it.
    pub fn push(&mut self, step: Step) {
        debug_assert!(
            !matches!(step, Step::Dns { .. })
                || !self
                    .steps
                    .iter()
                    .any(|a| matches!(a.step, Step::Dns { .. })),
            "two DNS steps on one stack: the second snapshot would capture our own resolv.conf"
        );
        self.steps.push(Applied {
            step,
            evidence: Evidence::Attempted,
        });
        self.persist();
    }

    /// Replace the top step's payload with what the apply actually learned (the real gateway,
    /// interface index or DNS snapshot) and mark it done.
    pub fn confirm_top(&mut self, step: Step) {
        if let Some(top) = self.steps.last_mut() {
            debug_assert_eq!(
                top.step.kind(),
                step.kind(),
                "confirm_top must confirm the step that was pushed"
            );
            top.step = step;
            top.evidence = Evidence::Done;
        }
        self.persist();
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn top(&self) -> Option<&Applied> {
        self.steps.last()
    }

    pub fn kinds(&self) -> Vec<StepKind> {
        self.steps.iter().map(|a| a.step.kind()).collect()
    }

    fn persist(&self) {
        self.persist_with(&[]);
    }

    /// Persist the durable steps on the stack plus `extra` — steps that have been popped because
    /// their undo gave up, but whose effect is still on the machine. Only this stack's records
    /// are replaced; what another stack left in the journal stays.
    fn persist_with(&self, extra: &[Applied]) {
        if let Some(journal) = &self.journal {
            journal.write(
                self.id,
                self.steps
                    .iter()
                    .chain(extra.iter())
                    .filter(|a| a.step.durable()),
            );
        }
    }
}

/// A /0 route is split into two /1s so it beats the system default without replacing it.
///
/// Pure, and shared with the CLI through `floppa-tunnel-config`, so the [`Step::Routes`] payload
/// records exactly the list that was handed to the OS.
pub use floppa_tunnel_config::route::split_default;

/// Undo everything on the stack, in reverse.
///
/// A step is popped only once its undo reports success; until then it is retried up to
/// `undo_retries` times. A step whose undo never succeeds is popped anyway — an unrecoverable undo
/// must not wedge the caller forever — but it is reported in [`UnwindReport::residual`] so the
/// failure is visible rather than silent, and if it was durable it stays in the journal so the
/// next start tries again. Previously the journal was cleared regardless, which turned "the undo
/// failed" into "nothing is left to undo" the moment the process restarted — and later, when it
/// was kept, the next attempt's stack overwrote the whole file with its own steps on its first
/// push, which lost it just the same. Records are per stack now (see [`journal`]).
pub async fn unwind(
    stack: &mut RollbackStack,
    extra: Option<ExtraUndo>,
    platform: &dyn Platform,
    backend: &dyn VpnBackend,
    undo_retries: u32,
) -> UnwindReport {
    let mut residual = Vec::new();
    // Steps popped without a successful undo. Their durable effects are still on the machine, so
    // they are carried in the journal alongside whatever is still on the stack.
    let mut failed: Vec<Applied> = Vec::new();

    while let Some(applied) = stack.steps.last().cloned() {
        let kind = applied.step.kind();
        let mut last_err = None;

        for attempt in 0..=undo_retries {
            match undo_step(&applied.step, platform, backend).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    warn!(?kind, attempt, error = %e, "undo failed, retrying");
                    last_err = Some(e);
                }
            }
        }

        if let Some(e) = last_err {
            warn!(?kind, error = %e, "undo did not succeed; machine may be partially configured");
            residual.push((kind, e));
            failed.push(applied);
        }
        stack.steps.pop();
        stack.persist_with(&failed);
    }

    if let Some(ExtraUndo::StopBackend) = extra
        && let Err(e) = backend.stop().await.map_err(|e| e.to_string())
    {
        warn!(error = %e, "stopping a foreign tunnel failed");
        residual.push((StepKind::StartBackend, e));
    }

    // Writing an empty set clears the journal; a non-empty one leaves the failed durable steps
    // for the next start to retry.
    stack.persist_with(&failed);
    if failed.iter().any(|a| a.step.durable()) {
        warn!("durable steps whose undo failed are kept in the rollback journal");
    }
    info!(residual = residual.len(), "unwind complete");
    UnwindReport {
        stack_empty: stack.steps.is_empty(),
        residual,
    }
}

async fn undo_step(
    step: &Step,
    platform: &dyn Platform,
    backend: &dyn VpnBackend,
) -> Result<(), String> {
    match step {
        Step::PrepareLink { iface } => platform
            .release_link(iface)
            .await
            .map_err(|e| e.to_string()),
        Step::StartBackend { .. } => backend.stop().await.map_err(|e| e.to_string()),
        Step::Address { iface, addr } => platform
            .deconfigure_address(iface, *addr)
            .await
            .map_err(|e| e.to_string()),
        Step::EndpointRoute { endpoint, gateway } => platform
            .remove_endpoint_route(*endpoint, gateway.as_ref())
            .await
            .map_err(|e| e.to_string()),
        Step::Routes {
            iface,
            routes,
            if_index,
        } => platform
            .remove_routes(iface, routes, *if_index)
            .await
            .map_err(|e| e.to_string()),
        Step::Dns {
            iface,
            snapshot,
            if_index,
        } => platform
            .restore_dns(iface, snapshot, *if_index)
            .await
            .map_err(|e| e.to_string()),
        // Stopping the service tears down the link and everything the builder applied with it.
        Step::AndroidService { .. } => backend.stop().await.map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface() -> InterfaceName {
        InterfaceName::default()
    }

    fn net(s: &str) -> IpNetwork {
        s.parse().unwrap()
    }

    #[test]
    fn push_records_the_step_as_attempted_before_it_is_applied() {
        let mut stack = RollbackStack::default();
        stack.push(Step::PrepareLink { iface: iface() });
        assert_eq!(stack.top().unwrap().evidence, Evidence::Attempted);
    }

    #[test]
    fn confirm_top_upgrades_the_payload_in_place() {
        let mut stack = RollbackStack::default();
        let endpoint: IpAddr = "1.2.3.4".parse().unwrap();
        stack.push(Step::EndpointRoute {
            endpoint,
            gateway: None,
        });
        let gw: Gateway = "192.168.1.1".parse().unwrap();
        stack.confirm_top(Step::EndpointRoute {
            endpoint,
            gateway: Some(gw),
        });

        let top = stack.top().unwrap();
        assert_eq!(top.evidence, Evidence::Done);
        match &top.step {
            Step::EndpointRoute { gateway, .. } => {
                assert_eq!(*gateway, Some(gw));
            }
            other => panic!("wrong step: {other:?}"),
        }
        assert_eq!(stack.len(), 1, "confirm must not push a second entry");
    }

    #[test]
    fn only_routes_dns_and_the_endpoint_route_survive_process_death() {
        assert!(
            Step::EndpointRoute {
                endpoint: "1.2.3.4".parse().unwrap(),
                gateway: Some("192.168.1.1".parse().unwrap()),
            }
            .durable()
        );
        assert!(
            Step::Routes {
                iface: iface(),
                routes: vec![],
                if_index: None
            }
            .durable()
        );
        assert!(
            Step::Dns {
                iface: iface(),
                snapshot: DnsSnapshot::Resolvectl,
                if_index: None
            }
            .durable()
        );
        assert!(!Step::PrepareLink { iface: iface() }.durable());
        assert!(!Step::StartBackend { iface: iface() }.durable());
        assert!(
            !Step::Address {
                iface: iface(),
                addr: net("10.0.0.2/32")
            }
            .durable()
        );
    }

    /// A platform whose route removal fails until released, and a backend that never runs.
    struct StuckRoutes {
        stuck: std::sync::atomic::AtomicBool,
    }

    impl StuckRoutes {
        fn stuck() -> Self {
            Self {
                stuck: std::sync::atomic::AtomicBool::new(true),
            }
        }

        /// From now on route removal succeeds — the helper came back, say.
        fn release(&self) {
            self.stuck.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl Platform for StuckRoutes {
        fn tun_params(&self) -> crate::vpn::platform::TunParams {
            Default::default()
        }
        async fn preflight(&self) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn prepare_link(
            &self,
            _: &InterfaceName,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn release_link(
            &self,
            _: &InterfaceName,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn configure_address(
            &self,
            _: &InterfaceName,
            _: IpNetwork,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn deconfigure_address(
            &self,
            _: &InterfaceName,
            _: IpNetwork,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn default_gateway(
            &self,
            _: crate::vpn::platform::IpFamily,
        ) -> Result<Option<Gateway>, crate::vpn::platform::PlatformError> {
            Ok(None)
        }
        async fn interface_index(&self, _: &InterfaceName) -> Option<u32> {
            None
        }
        async fn add_endpoint_route(
            &self,
            _: IpAddr,
            _: Option<&Gateway>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn remove_endpoint_route(
            &self,
            _: IpAddr,
            _: Option<&Gateway>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn add_routes(
            &self,
            _: &InterfaceName,
            _: &[IpNetwork],
            _: Option<u32>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn remove_routes(
            &self,
            _: &InterfaceName,
            _: &[IpNetwork],
            _: Option<u32>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            if self.stuck.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::vpn::platform::PlatformError::Failed(
                    "helper refused".into(),
                ));
            }
            Ok(())
        }
        async fn capture_dns(
            &self,
            _: &InterfaceName,
            _: Option<u32>,
        ) -> Result<DnsSnapshot, crate::vpn::platform::PlatformError> {
            Ok(DnsSnapshot::Resolvectl)
        }
        async fn configure_dns(
            &self,
            _: &InterfaceName,
            _: &[IpAddr],
            _: Option<u32>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn restore_dns(
            &self,
            _: &InterfaceName,
            _: &DnsSnapshot,
            _: Option<u32>,
        ) -> Result<(), crate::vpn::platform::PlatformError> {
            Ok(())
        }
        async fn ipv6_enabled(&self) -> bool {
            false
        }
    }

    struct NoBackend;

    #[async_trait::async_trait]
    impl VpnBackend for NoBackend {
        async fn start(
            &self,
            _: &crate::vpn::state::ProtocolConfig,
            _: &str,
            _: &crate::vpn::platform::TunParams,
            _: std::net::SocketAddr,
        ) -> Result<(), crate::vpn::backend::BackendError> {
            unreachable!()
        }
        async fn stop(&self) -> Result<(), crate::vpn::backend::BackendError> {
            Ok(())
        }
        async fn observe(&self) -> crate::vpn::actor::types::Observation {
            unreachable!()
        }
        async fn ping(&self) -> Result<(), crate::vpn::backend::BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_failed_durable_undo_stays_in_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        let mut stack = RollbackStack::new(Some(journal.clone()));

        let endpoint = Step::EndpointRoute {
            endpoint: "1.2.3.4".parse().unwrap(),
            gateway: None,
        };
        let routes = Step::Routes {
            iface: iface(),
            routes: vec![net("0.0.0.0/1")],
            if_index: None,
        };
        stack.push(endpoint.clone());
        stack.confirm_top(endpoint.clone());
        stack.push(routes.clone());
        stack.confirm_top(routes.clone());

        let report = unwind(&mut stack, None, &StuckRoutes::stuck(), &NoBackend, 0).await;

        assert!(
            stack.is_empty(),
            "an unrecoverable undo must not wedge the stack"
        );
        assert_eq!(report.residual.len(), 1);
        assert_eq!(report.residual[0].0, StepKind::Routes);

        // The endpoint route was removed and is gone from the journal; the routes were not, and
        // the next start must find them.
        let left = journal.read_orphaned();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].step, routes);
    }

    #[tokio::test]
    async fn a_clean_unwind_clears_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        let mut stack = RollbackStack::new(Some(journal.clone()));
        stack.push(Step::Dns {
            iface: iface(),
            snapshot: DnsSnapshot::Resolvectl,
            if_index: None,
        });
        assert!(Journal::default_path(dir.path()).exists());

        let report = unwind(&mut stack, None, &StuckRoutes::stuck(), &NoBackend, 0).await;
        assert!(report.is_clean());
        assert!(!Journal::default_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn a_failed_durable_undo_survives_the_next_stacks_journal_writes() {
        // The residue of one attempt's unwind used to live in the journal only until the next
        // attempt — a new stack on the same journal — pushed its first step and overwrote the
        // file with its own (empty) set of durable steps.
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        let platform = StuckRoutes::stuck();

        let routes = Step::Routes {
            iface: iface(),
            routes: vec![net("0.0.0.0/1")],
            if_index: None,
        };
        let mut first = RollbackStack::new(Some(journal.clone()));
        first.push(routes.clone());
        first.confirm_top(routes.clone());
        let report = unwind(&mut first, None, &platform, &NoBackend, 0).await;
        assert_eq!(report.residual.len(), 1);

        // The next attempt: a non-durable push, then a durable one, then a clean unwind.
        let mut second = RollbackStack::new(Some(journal.clone()));
        second.push(Step::PrepareLink { iface: iface() });
        assert_eq!(
            journal.read_orphaned().len(),
            1,
            "the first stack's residue survives the second stack's first write"
        );
        let dns = Step::Dns {
            iface: iface(),
            snapshot: DnsSnapshot::Resolvectl,
            if_index: None,
        };
        second.push(dns.clone());
        second.confirm_top(dns);
        assert_eq!(
            journal.read_orphaned().len(),
            2,
            "both stacks are in the file"
        );
        let report = unwind(&mut second, None, &platform, &NoBackend, 0).await;
        assert!(report.is_clean(), "the second stack had nothing stuck");
        let left = journal.read_orphaned();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].step, routes, "only the residue is left");

        // The next start: recovery picks it up and, with the helper back, finishes the job.
        let mut recovered = RollbackStack::from_orphaned(left, Some(journal.clone()));
        assert_eq!(recovered.len(), 1);
        platform.release();
        let report = unwind(&mut recovered, None, &platform, &NoBackend, 0).await;
        assert!(report.is_clean());
        assert!(!Journal::default_path(dir.path()).exists());
    }
}
