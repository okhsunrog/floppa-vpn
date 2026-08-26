//! What this client adds to the shared provisioning in `floppa-api-client`.
//!
//! The server talk, the peer logic and the API types are shared with `floppa-cli` and live there.
//! What is here is what only an app with a tunnel actor has: the session file both of this app's
//! processes read, and the reading of a finished connect cycle as "a peer may have been deleted".

pub mod server;
pub mod session;
pub mod watcher;

use floppa_api_client::{PeerProtocol, SyncError, SyncResult};
use serde::{Deserialize, Serialize};
use specta::Type;

/// How a sync ended, in the words the connection card needs.
///
/// A translation of [`SyncResult`] rather than the thing itself, and deliberately: what the card
/// wants is a tag it can look up in a locale file, in the user's language. `floppa-api-client`
/// describes the server, not this app's vocabulary for talking to a person — and it has no
/// `specta`, which is the practical half of the same point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SyncOutcome {
    /// Everything this device is entitled to is provisioned and stored.
    Ok,
    /// The server answered, and refused.
    Failed { error: SyncFailure },
    /// Nothing was learned and nothing was changed — no server, or nobody signed in.
    Offline,
}

/// Why a sync was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncFailure {
    NoSubscription,
    PeerLimitReached,
    /// Anything else. `detail` is the server's own words, for a card that has no better ones.
    CreateFailed {
        detail: String,
    },
}

impl From<SyncResult> for SyncOutcome {
    fn from(result: SyncResult) -> Self {
        match result {
            SyncResult::Ok => SyncOutcome::Ok,
            SyncResult::Offline => SyncOutcome::Offline,
            SyncResult::Failed(error) => SyncOutcome::Failed {
                error: match error {
                    SyncError::NoSubscription => SyncFailure::NoSubscription,
                    SyncError::PeerLimitReached => SyncFailure::PeerLimitReached,
                    SyncError::CreateFailed { detail } => SyncFailure::CreateFailed { detail },
                },
            },
        }
    }
}

use crate::vpn::actor::types::{AttemptError, AttemptFailure, CycleOutcome};
use crate::vpn::protocol::Protocol;

/// What a finished cycle asks of provisioning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomePlan {
    /// Nothing to do. The cycle either succeeded outright or failed for a reason no new peer
    /// would fix — whoever shows errors deals with it.
    Ignore,
    /// A protocol failed to verify on a cycle that *did* connect over another one. Its peer may
    /// have been deleted; check, and recreate it if so. Quiet: the tunnel is up.
    Repair { protocol: PeerProtocol },
    /// The same check on a cycle that connected over nothing. If the peer was indeed gone, a
    /// reconnect is owed once a new one exists.
    Reprovision { protocol: PeerProtocol },
}

/// Decide what a finished cycle means for the peers on the server.
///
/// The one thing this cannot decide is *why* an attempt failed — that arrives typed from the
/// actor. `VerifyFailed` for a protocol that has a peer is the signal that the peer may be gone,
/// and it is found by name: assuming it was "whichever protocol was tried last" is wrong the
/// moment the order has more than one entry.
pub fn plan_outcome(outcome: &CycleOutcome) -> OutcomePlan {
    match outcome {
        // Connected is not "nothing went wrong": the ladder tries protocols in order, so a peer
        // deleted under AmneziaWG shows up as a verification failure a second before WireGuard
        // carries the connection. That peer is worth repairing now rather than on some later
        // connect that has no fallback left.
        CycleOutcome::Connected { failures, .. } => match first_verify_failure(failures) {
            Some(protocol) => OutcomePlan::Repair { protocol },
            None => OutcomePlan::Ignore,
        },
        CycleOutcome::Exhausted { failures } => match first_verify_failure(failures) {
            Some(protocol) => OutcomePlan::Reprovision { protocol },
            None => OutcomePlan::Ignore,
        },
        // A tunnel that was up and then died: whatever was carrying it is the candidate. VLESS
        // has no per-device peer, so it never is one.
        CycleOutcome::LostGaveUp { protocol, .. } => match peer_protocol(*protocol) {
            Some(protocol) => OutcomePlan::Reprovision { protocol },
            None => OutcomePlan::Ignore,
        },
        CycleOutcome::UnwindFailed | CycleOutcome::Cancelled | CycleOutcome::Down => {
            OutcomePlan::Ignore
        }
    }
}

/// The first protocol in `failures` that failed *verification* and has a peer to lose.
fn first_verify_failure(failures: &[AttemptFailure]) -> Option<PeerProtocol> {
    failures
        .iter()
        .find(|f| matches!(f.error, AttemptError::VerifyFailed))
        .and_then(|f| peer_protocol(f.protocol))
}

/// This client's protocol as the server's, where the server has one for it.
///
/// `None` for VLESS: it is provisioned per user, not per device, so no lookup would find
/// anything and no failure of it is evidence about a peer.
pub fn peer_protocol(protocol: Protocol) -> Option<PeerProtocol> {
    match protocol {
        Protocol::WireGuard => Some(PeerProtocol::Wireguard),
        Protocol::AmneziaWg => Some(PeerProtocol::Amneziawg),
        Protocol::Vless => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(protocol: Protocol, error: AttemptError) -> AttemptFailure {
        AttemptFailure {
            protocol,
            error,
            pass: 0,
        }
    }

    #[test]
    fn a_cycle_that_connected_over_a_fallback_owes_the_one_it_stepped_over_a_peer() {
        let outcome = CycleOutcome::Connected {
            protocol: Protocol::WireGuard,
            adopted: false,
            failures: vec![failure(Protocol::AmneziaWg, AttemptError::VerifyFailed)],
        };
        assert_eq!(
            plan_outcome(&outcome),
            OutcomePlan::Repair {
                protocol: PeerProtocol::Amneziawg
            }
        );
    }

    #[test]
    fn a_cycle_that_connected_cleanly_owes_nothing() {
        let outcome = CycleOutcome::Connected {
            protocol: Protocol::AmneziaWg,
            adopted: false,
            failures: vec![],
        };
        assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
    }

    #[test]
    fn the_protocol_that_failed_verification_is_found_by_name_not_by_position() {
        // WireGuard was tried last and timed out; AmneziaWG is the one whose peer may be gone.
        let outcome = CycleOutcome::Exhausted {
            failures: vec![
                failure(Protocol::AmneziaWg, AttemptError::VerifyFailed),
                failure(Protocol::WireGuard, AttemptError::TimedOut),
            ],
        };
        assert_eq!(
            plan_outcome(&outcome),
            OutcomePlan::Reprovision {
                protocol: PeerProtocol::Amneziawg
            }
        );
    }

    #[test]
    fn a_failure_no_new_peer_would_fix_asks_for_no_new_peer() {
        let outcome = CycleOutcome::Exhausted {
            failures: vec![failure(
                Protocol::WireGuard,
                AttemptError::ResolveFailed {
                    host: "vpn.example".into(),
                    detail: "no DNS".into(),
                },
            )],
        };
        assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
    }

    #[test]
    fn vless_never_asks_for_a_peer_because_it_has_none() {
        let outcome = CycleOutcome::Exhausted {
            failures: vec![failure(Protocol::Vless, AttemptError::VerifyFailed)],
        };
        assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);

        let lost = CycleOutcome::LostGaveUp {
            protocol: Protocol::Vless,
            passes: 3,
        };
        assert_eq!(plan_outcome(&lost), OutcomePlan::Ignore);
    }

    #[test]
    fn the_endings_that_are_nobodys_fault_ask_for_nothing() {
        for outcome in [
            CycleOutcome::Cancelled,
            CycleOutcome::Down,
            CycleOutcome::UnwindFailed,
        ] {
            assert_eq!(plan_outcome(&outcome), OutcomePlan::Ignore);
        }
    }
}
