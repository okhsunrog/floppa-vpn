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

use crate::backend::VpnBackend;
use crate::platform::{DnsSnapshot, Gateway, Platform, PolicyRoutingSnapshot};
use crate::protocol::InterfaceName;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use tracing::{info, warn};

pub mod journal;
pub use journal::{Journal, StackId};

/// One reversible mutation of the machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Step {
    /// Linux: the persistent TUN created by the helper. Windows: reaping a stale Wintun adapter.
    PrepareLink {
        iface: InterfaceName,
    },
    /// The tunnel itself. Undo is `backend.stop()`, which the caller supplies as an
    /// [`ExtraUndo::StopBackend`] because the stack has no handle on the backend.
    StartBackend {
        iface: InterfaceName,
    },
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
    PolicyRouting {
        snapshot: PolicyRoutingSnapshot,
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
    AndroidService {
        generation: u64,
    },
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
    PolicyRouting,
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
            Self::PolicyRouting { .. } => StepKind::PolicyRouting,
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
            Self::EndpointRoute { .. }
                | Self::Routes { .. }
                | Self::Dns { .. }
                | Self::PolicyRouting { .. }
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
    /// When the last undo returned. The world is judged against a look taken *after* this: one
    /// started while the unwind was still running can have seen a tunnel this unwind then
    /// stopped, and reading it as "still running" cost an extra teardown and a burnt retry.
    pub finished_at: std::time::Instant,
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
    pub fn from_orphaned(steps: Vec<Applied>, journal: Option<Journal>) -> std::io::Result<Self> {
        let stack = Self {
            id: StackId::next(),
            steps,
            journal,
        };
        if let Some(journal) = &stack.journal {
            journal.claim(stack.id, stack.steps.iter().filter(|a| a.step.durable()))?;
        }
        Ok(stack)
    }

    /// Record a step, persist it if durable, and only then let the caller apply it.
    pub fn push(&mut self, step: Step) -> std::io::Result<()> {
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
        if let Err(error) = self.persist() {
            // The caller has not applied this step. Do not undo a mutation we never made.
            self.steps.pop();
            return Err(error);
        }
        Ok(())
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
        // The pre-apply record already contains the complete undo payload. Confirmation is
        // cosmetic; if it cannot be saved, recovery still treats the operation as attempted.
        if let Err(error) = self.persist() {
            warn!(%error, "could not confirm rollback record; attempted record remains");
        }
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

    fn persist(&self) -> std::io::Result<()> {
        self.persist_with(&[])
    }

    /// Persist the durable steps on the stack plus `extra` — steps that have been popped because
    /// their undo gave up, but whose effect is still on the machine. Only this stack's records
    /// are replaced; what another stack left in the journal stays.
    fn persist_with(&self, extra: &[Applied]) -> std::io::Result<()> {
        if let Some(journal) = &self.journal {
            journal.write(
                self.id,
                self.steps
                    .iter()
                    .chain(extra.iter())
                    .filter(|a| a.step.durable()),
            )?;
        }
        Ok(())
    }
}

/// A /0 route is split into two /1s so it beats the system default without replacing it.
///
/// Pure, and shared with the CLI through `floppa-tunnel-config`, so the [`Step::Routes`] payload
/// records exactly the list that was handed to the OS.
pub use floppa_tunnel_config::route::split_default;

/// Undo everything on the stack, in reverse.
///
/// Each undo has a bounded retry budget. Failed steps remain in memory and in the journal,
/// so another unwind of the same stack cannot erase evidence that only lived on disk. Other
/// steps are still undone, and the caller receives a residual report rather than waiting forever.
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
        if let Err(error) = stack.persist_with(&failed) {
            // Keep cleaning the live machine, but never report a clean result while stale
            // journal entries could be replayed on the next start.
            residual.push((kind, format!("could not update rollback journal: {error}")));
        }
    }

    if let Some(ExtraUndo::StopBackend) = extra
        && let Err(e) = backend.stop().await.map_err(|e| e.to_string())
    {
        warn!(error = %e, "stopping a foreign tunnel failed");
        residual.push((StepKind::StartBackend, e));
    }

    // Preserve original application order: the next unwind visits failures in reverse again.
    failed.reverse();
    stack.steps = failed;
    if let Err(error) = stack.persist() {
        residual.push((
            StepKind::PrepareLink,
            format!("could not finalize rollback journal: {error}"),
        ));
    }
    if stack.steps.iter().any(|a| a.step.durable()) {
        warn!("durable steps whose undo failed are kept in the rollback journal");
    }
    info!(residual = residual.len(), "unwind complete");
    UnwindReport {
        finished_at: std::time::Instant::now(),
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
        Step::PolicyRouting { snapshot } => platform
            .restore_policy_routing(snapshot)
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
        stack.push(Step::PrepareLink { iface: iface() }).unwrap();
        assert_eq!(stack.top().unwrap().evidence, Evidence::Attempted);
    }

    #[test]
    fn failed_write_does_not_enqueue_an_unapplied_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path().join("missing-parent/rollback.json"));
        let mut stack = RollbackStack::new(Some(journal));
        assert!(
            stack
                .push(Step::Dns {
                    iface: iface(),
                    snapshot: DnsSnapshot::Resolvectl,
                    if_index: None,
                })
                .is_err()
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn confirm_top_upgrades_the_payload_in_place() {
        let mut stack = RollbackStack::default();
        let endpoint: IpAddr = "1.2.3.4".parse().unwrap();
        stack
            .push(Step::EndpointRoute {
                endpoint,
                gateway: None,
            })
            .unwrap();
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
    fn routing_and_dns_survive_process_death() {
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
        async fn restore_policy_routing(
            &self,
            snapshot: &PolicyRoutingSnapshot,
        ) -> Result<(), crate::platform::PlatformError> {
            assert!(snapshot.ipv4 && snapshot.ipv6);
            assert_eq!(snapshot.src_valid_mark, Some(0));
            if self.stuck.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::platform::PlatformError::Failed(
                    "helper unavailable".into(),
                ));
            }
            Ok(())
        }
        fn tun_params(&self) -> crate::platform::TunParams {
            Default::default()
        }
        async fn preflight(&self) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn prepare_link(
            &self,
            _: &InterfaceName,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn release_link(
            &self,
            _: &InterfaceName,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn configure_address(
            &self,
            _: &InterfaceName,
            _: IpNetwork,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn deconfigure_address(
            &self,
            _: &InterfaceName,
            _: IpNetwork,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn default_gateway(
            &self,
            _: crate::platform::IpFamily,
        ) -> Result<Option<Gateway>, crate::platform::PlatformError> {
            Ok(None)
        }
        async fn interface_index(&self, _: &InterfaceName) -> Option<u32> {
            None
        }
        async fn add_endpoint_route(
            &self,
            _: IpAddr,
            _: Option<&Gateway>,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn remove_endpoint_route(
            &self,
            _: IpAddr,
            _: Option<&Gateway>,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn add_routes(
            &self,
            _: &InterfaceName,
            _: &[IpNetwork],
            _: Option<u32>,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn remove_routes(
            &self,
            _: &InterfaceName,
            _: &[IpNetwork],
            _: Option<u32>,
        ) -> Result<(), crate::platform::PlatformError> {
            if self.stuck.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::platform::PlatformError::Failed(
                    "helper refused".into(),
                ));
            }
            Ok(())
        }
        async fn capture_dns(
            &self,
            _: &InterfaceName,
            _: Option<u32>,
        ) -> Result<DnsSnapshot, crate::platform::PlatformError> {
            Ok(DnsSnapshot::Resolvectl)
        }
        async fn configure_dns(
            &self,
            _: &InterfaceName,
            _: &[IpAddr],
            _: Option<u32>,
        ) -> Result<(), crate::platform::PlatformError> {
            Ok(())
        }
        async fn restore_dns(
            &self,
            _: &InterfaceName,
            _: &DnsSnapshot,
            _: Option<u32>,
        ) -> Result<(), crate::platform::PlatformError> {
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
            _: &crate::state::ProtocolConfig,
            _: &str,
            _: &crate::platform::TunParams,
            _: std::net::SocketAddr,
        ) -> Result<(), crate::backend::BackendError> {
            unreachable!()
        }
        async fn stop(&self) -> Result<(), crate::backend::BackendError> {
            Ok(())
        }
        async fn observe(&self) -> crate::actor::types::Observation {
            unreachable!()
        }
        async fn probe(&self) -> Result<(), crate::backend::BackendError> {
            Ok(())
        }

        async fn ping(&self) -> Result<(), crate::backend::BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn partial_policy_apply_is_journalled_and_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        let mut stack = RollbackStack::new(Some(journal.clone()));
        let step = Step::PolicyRouting {
            snapshot: PolicyRoutingSnapshot {
                ipv4: true,
                ipv6: true,
                src_valid_mark: Some(0),
            },
        };
        stack.push(step.clone()).unwrap(); // crash before confirmation still needs recovery
        let orphaned = journal.read_orphaned().unwrap();
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].step, step);
        let mut recovered = RollbackStack::from_orphaned(orphaned, Some(journal.clone())).unwrap();
        let platform = StuckRoutes::stuck();
        let report = unwind(&mut recovered, None, &platform, &NoBackend, 0).await;
        assert_eq!(report.residual.len(), 1);
        assert_eq!(report.residual[0].0, StepKind::PolicyRouting);
        let mut recovered =
            RollbackStack::from_orphaned(journal.read_orphaned().unwrap(), Some(journal.clone()))
                .unwrap();
        platform.release();
        assert!(
            unwind(&mut recovered, None, &platform, &NoBackend, 0)
                .await
                .residual
                .is_empty()
        );
        assert!(journal.read_orphaned().unwrap().is_empty());
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
        stack.push(endpoint.clone()).unwrap();
        stack.confirm_top(endpoint.clone());
        stack.push(routes.clone()).unwrap();
        stack.confirm_top(routes.clone());

        let report = unwind(&mut stack, None, &StuckRoutes::stuck(), &NoBackend, 0).await;

        assert!(
            !stack.is_empty(),
            "failed undo must remain retryable without blocking the caller"
        );
        assert_eq!(report.residual.len(), 1);
        assert_eq!(report.residual[0].0, StepKind::Routes);

        // The endpoint route was removed and is gone from the journal; the routes were not, and
        // the next start must find them.
        let left = journal.read_orphaned().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].step, routes);
        let report = unwind(&mut stack, None, &StuckRoutes::stuck(), &NoBackend, 0).await;
        assert!(!report.is_clean());
        assert_eq!(
            journal.read_orphaned().unwrap(),
            left,
            "a second unwind preserves failures"
        );
        let platform = StuckRoutes::stuck();
        platform.release();
        let report = unwind(&mut stack, None, &platform, &NoBackend, 0).await;
        assert!(report.is_clean());
        assert!(journal.read_orphaned().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_clean_unwind_clears_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(Journal::default_path(dir.path()));
        let mut stack = RollbackStack::new(Some(journal.clone()));
        stack
            .push(Step::Dns {
                iface: iface(),
                snapshot: DnsSnapshot::Resolvectl,
                if_index: None,
            })
            .unwrap();
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
        first.push(routes.clone()).unwrap();
        first.confirm_top(routes.clone());
        let report = unwind(&mut first, None, &platform, &NoBackend, 0).await;
        assert_eq!(report.residual.len(), 1);

        // The next attempt: a non-durable push, then a durable one, then a clean unwind.
        let mut second = RollbackStack::new(Some(journal.clone()));
        second.push(Step::PrepareLink { iface: iface() }).unwrap();
        assert_eq!(
            journal.read_orphaned().unwrap().len(),
            1,
            "the first stack's residue survives the second stack's first write"
        );
        let dns = Step::Dns {
            iface: iface(),
            snapshot: DnsSnapshot::Resolvectl,
            if_index: None,
        };
        second.push(dns.clone()).unwrap();
        second.confirm_top(dns);
        assert_eq!(
            journal.read_orphaned().unwrap().len(),
            2,
            "both stacks are in the file"
        );
        let report = unwind(&mut second, None, &platform, &NoBackend, 0).await;
        assert!(report.is_clean(), "the second stack had nothing stuck");
        let left = journal.read_orphaned().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].step, routes, "only the residue is left");

        // The next start: recovery picks it up and, with the helper back, finishes the job.
        let mut recovered = RollbackStack::from_orphaned(left, Some(journal.clone())).unwrap();
        assert_eq!(recovered.len(), 1);
        platform.release();
        let report = unwind(&mut recovered, None, &platform, &NoBackend, 0).await;
        assert!(report.is_clean());
        assert!(!Journal::default_path(dir.path()).exists());
    }
}
