//! How things end: the failure of one attempt, the outcome of a whole cycle, and what a
//! command call can be refused with.
//!
//! [`AttemptError`] and [`CycleOutcome`] cross into TypeScript; [`AttemptResult`] is the attempt
//! task's report to the actor and never leaves the process.

use super::intent::IntentEpoch;
use super::status::UpStatus;
use crate::vpn::backend::BackendError;
use crate::vpn::protocol::Protocol;
use crate::vpn::rollback::StepKind;
use serde::{Deserialize, Serialize};
use specta::Type;

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
    /// The consent dialog was asked for and never answered: Android refuses to start an activity
    /// for a process that is in the background, and the activity can also be recreated while the
    /// dialog is up, which loses the reply. Neither is a refusal, and neither is fixed by trying
    /// the next protocol — so, like a refusal, it ends the cycle.
    #[error("the VPN consent dialog did not answer")]
    ConsentUnavailable,
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
    #[error("tunnel backend failed: {error}")]
    Backend { error: BackendError },
    #[error("no handshake / no connectivity through the tunnel")]
    VerifyFailed,
    #[error("attempt exceeded its budget")]
    TimedOut,
    #[error("the VPN service failed to start the tunnel: {detail}")]
    PeerStartFailed { detail: String },
    #[error("cancelled")]
    Cancelled,
    /// The attempt task panicked or was aborted. Synthesised by the actor from the task's join
    /// error, so a crash can never leave the status waiting for a report that will not come. The
    /// ladder did *not* unwind itself: whatever it applied is recovered from the journal.
    #[error("the attempt task crashed: {detail}")]
    Crashed { detail: String },
}

impl AttemptError {
    /// Should this abort the whole cycle rather than move on to the next protocol?
    ///
    /// Without this, probing three protocols after the user denied VPN consent means three consent
    /// dialogs — and with a reconnect budget, up to nine.
    pub const fn is_fatal_for_cycle(&self) -> bool {
        matches!(
            self,
            Self::PermissionDenied
                | Self::ConsentUnavailable
                | Self::PlatformUnavailable { .. }
                | Self::Cancelled
                | Self::Crashed { .. }
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
        /// What failed on the way here, if the ladder had to step over anything.
        ///
        /// A cycle that ends connected is not a cycle in which nothing went wrong: the ladder
        /// tries protocols in order, so AmneziaWG can fail to verify — the signal that its peer
        /// was deleted server-side — and WireGuard carry the connection a second later. Reporting
        /// only the winner left that dead peer in place until something else happened to notice
        /// it, which on device meant the next app start.
        failures: Vec<AttemptFailure>,
    },
    /// Every protocol in the order failed, for every allowed pass — or one failed fatally.
    ///
    /// `failures` carries one entry per probe, so the caller can find exactly which protocol
    /// reported `verify_failed` and re-provision *that* peer, instead of assuming it was whichever
    /// protocol happened to be tried last.
    Exhausted { failures: Vec<AttemptFailure> },
    /// Was connected, the tunnel died, the reconnect budget ran out.
    LostGaveUp { protocol: Protocol, passes: u32 },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_and_helper_failures_stop_the_cycle_but_a_bad_peer_does_not() {
        assert!(AttemptError::PermissionDenied.is_fatal_for_cycle());
        assert!(AttemptError::ConsentUnavailable.is_fatal_for_cycle());
        assert!(
            AttemptError::PlatformUnavailable {
                detail: String::new()
            }
            .is_fatal_for_cycle()
        );
        assert!(AttemptError::Cancelled.is_fatal_for_cycle());
        assert!(
            AttemptError::Crashed {
                detail: String::new()
            }
            .is_fatal_for_cycle()
        );
        assert!(!AttemptError::VerifyFailed.is_fatal_for_cycle());
        assert!(!AttemptError::TimedOut.is_fatal_for_cycle());
    }
}
