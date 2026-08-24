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
pub use journal::Journal;

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
        }
    }

    /// Does this step's effect outlive the process that applied it?
    ///
    /// Only these are journalled. The rest are either self-healing (the TUN is persistent and
    /// deconfiguring it is unconditionally safe) or meaningless once the owning process is gone.
    pub const fn durable(&self) -> bool {
        matches!(self, Self::Routes { .. } | Self::Dns { .. })
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
    /// Apply neither returned nor was observed — its budget elapsed while it was still blocking.
    /// Best-effort undo, which is legal because every undo primitive swallows its errors.
    Unknown,
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

#[derive(Debug, Default)]
pub struct RollbackStack {
    steps: Vec<Applied>,
    journal: Option<Journal>,
}

impl RollbackStack {
    pub fn new(journal: Option<Journal>) -> Self {
        Self {
            steps: Vec::new(),
            journal,
        }
    }

    /// Rebuild a stack from steps a previous process left in its journal.
    ///
    /// The steps are exactly as they were persisted, so unwinding this is indistinguishable from
    /// unwinding the stack that wrote it — which is the point: crash recovery is not a separate
    /// code path.
    pub fn from_orphaned(steps: Vec<Applied>, journal: Option<Journal>) -> Self {
        Self { steps, journal }
    }

    /// The synthetic single-step stack for an adopted tunnel: nothing local was applied, so the
    /// only thing to undo is the tunnel itself.
    pub fn adopted(iface: InterfaceName) -> Self {
        Self {
            steps: vec![Applied {
                step: Step::StartBackend { iface },
                evidence: Evidence::Done,
            }],
            journal: None,
        }
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

    pub fn mark_top_unknown(&mut self) {
        if let Some(top) = self.steps.last_mut() {
            top.evidence = Evidence::Unknown;
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
        if let Some(journal) = &self.journal {
            journal.write(self.steps.iter().filter(|a| a.step.durable()));
        }
    }

    fn clear_journal(&self) {
        if let Some(journal) = &self.journal {
            journal.clear();
        }
    }
}

/// A /0 route is split into two /1s so it beats the system default without replacing it.
///
/// Pure, and shared by every platform, so the [`Step::Routes`] payload records exactly the list
/// that was handed to the OS.
pub fn split_default(allowed_ips: &[IpNetwork], include_ipv6: bool) -> Vec<IpNetwork> {
    let mut routes = Vec::new();
    for network in allowed_ips {
        if network.is_ipv6() && !include_ipv6 {
            continue;
        }
        if network.prefix() == 0 {
            let halves: [&str; 2] = if network.is_ipv4() {
                ["0.0.0.0/1", "128.0.0.0/1"]
            } else {
                ["::/1", "8000::/1"]
            };
            for half in halves {
                if let Ok(net) = half.parse() {
                    routes.push(net);
                }
            }
        } else {
            routes.push(*network);
        }
    }
    routes
}

/// Undo everything on the stack, in reverse.
///
/// A step is popped only once its undo reports success; until then it is retried up to
/// `undo_retries` times. A step whose undo never succeeds is popped anyway — an unrecoverable undo
/// must not wedge the caller forever — but it is reported in [`UnwindReport::residual`] so the
/// failure is visible rather than silent.
pub async fn unwind(
    stack: &mut RollbackStack,
    extra: Option<ExtraUndo>,
    platform: &dyn Platform,
    backend: &dyn VpnBackend,
    undo_retries: u32,
) -> UnwindReport {
    let mut residual = Vec::new();

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
        }
        stack.steps.pop();
        stack.persist();
    }

    if let Some(ExtraUndo::StopBackend) = extra
        && let Err(e) = backend.stop().await
    {
        warn!(error = %e, "stopping a foreign tunnel failed");
        residual.push((StepKind::StartBackend, e));
    }

    stack.clear_journal();
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
        Step::StartBackend { .. } => backend.stop().await,
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
        stack.confirm_top(Step::EndpointRoute {
            endpoint,
            gateway: Some(Gateway("192.168.1.1".into())),
        });

        let top = stack.top().unwrap();
        assert_eq!(top.evidence, Evidence::Done);
        match &top.step {
            Step::EndpointRoute { gateway, .. } => {
                assert_eq!(gateway.as_ref().unwrap().0, "192.168.1.1");
            }
            other => panic!("wrong step: {other:?}"),
        }
        assert_eq!(stack.len(), 1, "confirm must not push a second entry");
    }

    #[test]
    fn only_routes_and_dns_survive_process_death() {
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

    #[test]
    fn adopted_stack_has_only_the_tunnel_to_undo() {
        let stack = RollbackStack::adopted(iface());
        assert_eq!(stack.kinds(), vec![StepKind::StartBackend]);
        assert_eq!(stack.top().unwrap().evidence, Evidence::Done);
    }

    #[test]
    fn default_route_is_split_into_two_halves() {
        assert_eq!(
            split_default(&[net("0.0.0.0/0")], false),
            vec![net("0.0.0.0/1"), net("128.0.0.0/1")]
        );
    }

    #[test]
    fn specific_routes_pass_through_untouched() {
        let given = vec![net("10.0.0.0/8"), net("192.168.1.0/24")];
        assert_eq!(split_default(&given, false), given);
    }

    #[test]
    fn ipv6_is_dropped_when_the_host_has_it_disabled() {
        let given = vec![net("0.0.0.0/0"), net("::/0")];
        assert_eq!(
            split_default(&given, false),
            vec![net("0.0.0.0/1"), net("128.0.0.0/1")]
        );
        assert_eq!(
            split_default(&given, true),
            vec![
                net("0.0.0.0/1"),
                net("128.0.0.0/1"),
                net("::/1"),
                net("8000::/1")
            ]
        );
    }
}
