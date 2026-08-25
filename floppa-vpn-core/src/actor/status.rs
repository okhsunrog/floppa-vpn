//! What we are doing about the intent. The second axis of the decision table.
//!
//! Only [`reconcile`](super::reconcile) changes a [`Status`], and it lives as a local variable of
//! the actor task, so nothing else *can*. Everything an in-flight attempt or a rollback needs to
//! know about itself is carried inside the status — the [`Cycle`] most of all — so what to do next
//! is computable from the status alone, with no side state.

use super::intent::{IntentEpoch, TunnelParams, UpIntent};
use super::outcome::AttemptFailure;
use super::policy::Policy;
use crate::protocol::Protocol;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::Instant;

/// The in-progress auto-select walk.
///
/// Carried through Connecting → Unwinding → Retrying, so what to do after a rollback is computable
/// from the status alone with no side state. This one struct replaces `runAutoCycle`, `abortGen`,
/// `reconnectAttempts` and `reconnectTimeoutId`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cycle {
    pub epoch: IntentEpoch,
    pub order: Vec<Protocol>,
    /// What every attempt of this cycle builds. Required: a cycle exists to start tunnels, and a
    /// tunnel cannot be started without knowing its split rules.
    pub params: TunnelParams,
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
    /// Whether this cycle exists because a tunnel that was up went away.
    ///
    /// It is the difference between the two ways a cycle can run out of budget: a cold connect
    /// that never worked is `Exhausted`, a tunnel that was working and could not be brought back
    /// is `LostGaveUp`. Without it, the only path that reported `LostGaveUp` was the one where the
    /// budget was already gone before the first retry — unreachable at the default
    /// `reconnect_passes`, so a dropped tunnel that gave up reported `Exhausted` for an epoch
    /// nobody was waiting on, and the user was left silently disconnected.
    pub born_from_loss: bool,
}

impl Cycle {
    /// `None` for an intent that carries no params: it cannot build a tunnel, only adopt one, so
    /// there is no cycle to run for it. Every caller-issued Up produces `Some`.
    pub fn start(up: &UpIntent, policy: &Policy) -> Option<Self> {
        let params = up.params.clone()?;
        Some(Self {
            epoch: up.epoch,
            order: up.order.clone(),
            params,
            index: 0,
            pass: 0,
            passes_allowed: policy.cold_passes,
            failures: Vec::new(),
            born_from_loss: false,
        })
    }

    /// Born from a lost tunnel, so it gets the reconnect budget rather than the cold one.
    pub fn reconnect(up: &UpIntent, policy: &Policy) -> Option<Self> {
        Self::start(up, policy).map(|cycle| Self {
            passes_allowed: policy.reconnect_passes,
            born_from_loss: true,
            ..cycle
        })
    }

    pub fn protocol(&self) -> Protocol {
        self.order[self.index]
    }

    pub fn is_last_probe(&self) -> bool {
        self.index + 1 >= self.order.len()
    }

    /// Is there a pass left after the one `pass` counts as burnt?
    pub fn has_budget(&self) -> bool {
        self.pass + 1 < self.passes_allowed
    }

    /// How this cycle ends when its budget is gone.
    ///
    /// One place, so every exhaustion path — a failed probe, an expired deadline, either table —
    /// says the same thing about the same cycle.
    pub fn gave_up(self) -> super::outcome::CycleOutcome {
        if self.born_from_loss {
            super::outcome::CycleOutcome::LostGaveUp {
                protocol: self.protocol(),
                passes: self.pass + 1,
            }
        } else {
            super::outcome::CycleOutcome::Exhausted {
                failures: self.failures,
            }
        }
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
    /// When the far side was last asked to prove it is there, after going silent for longer than
    /// the policy allows. `None` while it is answering.
    ///
    /// Separate from [`Self::dark_since`] because they are different failures: dark is "the
    /// process that owns the tunnel is not answering *us*", silent is "the tunnel is fine and the
    /// peer at the other end has stopped answering *it*". The second one is invisible from
    /// outside the tunnel — it is why a peer deleted on the server used to read as Connected
    /// forever.
    pub probing_since: Option<Instant>,
    /// Whether this epoch's waiter has already been resolved `Connected`. An epoch can enter Up on
    /// a `Dark` observation during adoption hand-over, which is never authoritative enough to
    /// announce success from.
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindReason {
    IntentDown,
    IntentChanged,
    AttemptTimedOut,
    /// The attempt task died without reporting, so it never unwound its own ladder. Whatever the
    /// journal recorded is undone here, and the cycle ends: a crash is a bug, not a bad peer.
    AttemptCrashed,
    /// Confirmed not running while we believed we were up.
    TunnelDied,
    /// Dark for longer than the grace period.
    PeerLost,
    /// The tunnel is up and the far side stopped answering it: silent past the policy's bound,
    /// and still silent after being asked to prove otherwise.
    PeerSilent,
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
    ///
    /// Who is doing the unwinding is deliberately not recorded. It was, and nothing read it: an
    /// attempt cancelled mid-ladder unwinds itself and reports, and that report is routed by the
    /// status already being `Unwinding` — not by a field saying so.
    Unwinding {
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

    #[test]
    fn cycle_walks_the_order_then_starts_another_pass() {
        let policy = Policy::default();
        let mut cycle = Cycle::start(
            &up(
                &[Protocol::AmneziaWg, Protocol::WireGuard],
                Some(TunnelParams::default()),
            ),
            &policy,
        )
        .expect("an intent with params starts a cycle");
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
        let intent = up(&[Protocol::AmneziaWg], Some(TunnelParams::default()));
        let cold = Cycle::start(&intent, &policy).unwrap();
        let reconnect = Cycle::reconnect(&intent, &policy).unwrap();
        assert_eq!(cold.passes_allowed, 1);
        assert_eq!(reconnect.passes_allowed, policy.reconnect_passes);
        assert!(!cold.has_budget());
        assert!(reconnect.has_budget());
        assert!(!cold.born_from_loss);
        assert!(reconnect.born_from_loss);
    }

    #[test]
    fn an_intent_without_params_has_no_cycle_to_run() {
        // It can adopt a tunnel; it cannot build one, because it does not know the split rules.
        let policy = Policy::default();
        let intent = up(&[Protocol::AmneziaWg], None);
        assert!(Cycle::start(&intent, &policy).is_none());
        assert!(Cycle::reconnect(&intent, &policy).is_none());
    }
}
