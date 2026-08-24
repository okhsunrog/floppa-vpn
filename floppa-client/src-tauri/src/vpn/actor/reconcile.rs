//! The decision table: the single place tunnel status is allowed to change.
//!
//! Pure. No I/O, no locks, no clock access — `now` is an argument. That is what makes the whole
//! table unit-testable with plain structs, and it is why this module carries the bulk of the
//! actor's tests.
//!
//! The `match` on `(status, intent)` is total and has **no `_` arm**, so adding a [`Status`]
//! variant is a compile error rather than a silently ignored case. The previous design's
//! equivalent — a `match` with `_ => {}` — is exactly how a tunnel could get stuck in `Connecting`
//! forever: an unhandled combination just did nothing.
//!
//! Three things that used to be features are not implemented anywhere here, because they are
//! consequences of the table rather than code:
//!
//! - **auto-reconnect** is what happens when a tunnel dies and the Up intent did not change;
//! - **auto protocol selection** is what happens when an attempt fails and the cycle advances;
//! - **cancellation** is what happens when the intent changes while an attempt is in flight.

use super::types::{
    AttemptError, AttemptFailure, AttemptPhase, AttemptResult, Cycle, CycleOutcome, Intent,
    IntentEpoch, Policy, Status, UnwindOwner, UnwindReason, UpIntent, UpStatus, World,
};
use crate::vpn::protocol::Protocol;
use crate::vpn::rollback::{ExtraUndo, RollbackStack, UnwindReport};
use std::time::Instant;

/// What the actor must *do* as a result of a transition. The actor executes these; the table only
/// names them, which is what keeps the table pure.
#[derive(Debug)]
pub enum Effect {
    /// Spawn an attempt task for this protocol.
    Begin {
        protocol: Protocol,
        epoch: IntentEpoch,
        index: usize,
        total: usize,
    },
    /// Fire the in-flight attempt's cancellation token. Never drops the task: it must be given the
    /// chance to unwind its own partial ladder and report back.
    CancelAttempt,
    /// Unwind the stack the actor holds.
    Unwind {
        extra: Option<ExtraUndo>,
    },
    /// Take ownership of a stack handed over by a successful attempt.
    TakeStack(RollbackStack),
    ResetSpeed,
    /// Persist "this protocol actually worked". Emitted only on success.
    RememberWinner(Protocol),
    /// Demote the intent to Down at the same epoch, preserving the invariant that an Up intent
    /// means the actor is actively working toward Up.
    DemoteIntent,
    /// Resolve waiters for the current epoch.
    Resolve(CycleOutcome),
    /// Resolve waiters for a specific, usually superseded, epoch.
    ResolveFor {
        epoch: IntentEpoch,
        outcome: CycleOutcome,
    },
}

#[derive(Debug)]
pub struct Decision {
    pub next: Status,
    pub effects: Vec<Effect>,
}

impl Decision {
    fn stay(status: &Status) -> Self {
        Self {
            next: status.clone(),
            effects: Vec::new(),
        }
    }

    fn to(next: Status) -> Self {
        Self {
            next,
            effects: Vec::new(),
        }
    }

    fn with(mut self, effect: Effect) -> Self {
        self.effects.push(effect);
        self
    }
}

/// The intent, normalised against the epoch the current status belongs to.
///
/// `Same` means "this status is already working on this intent"; `Newer` means a fresher intent has
/// arrived and whatever is in flight is now stale.
enum Rel<'a> {
    Down,
    Same(&'a UpIntent),
    Newer(&'a UpIntent),
}

fn relate<'a>(status: &Status, intent: &'a Intent) -> Rel<'a> {
    match intent {
        Intent::Down { .. } => Rel::Down,
        Intent::Up(up) => {
            if status.epoch() == Some(up.epoch) {
                Rel::Same(up)
            } else {
                Rel::Newer(up)
            }
        }
    }
}

fn connecting(cycle: Cycle, now: Instant, policy: &Policy) -> Decision {
    let effect = Effect::Begin {
        protocol: cycle.protocol(),
        epoch: cycle.epoch,
        index: cycle.index,
        total: cycle.order.len(),
    };
    Decision::to(Status::Connecting {
        cycle,
        phase: AttemptPhase::Preparing,
        deadline: now + policy.attempt_budget,
    })
    .with(effect)
}

fn unwinding(owner: UnwindOwner, cycle: Option<Cycle>, reason: UnwindReason) -> Status {
    Status::Unwinding {
        owner,
        cycle,
        reason,
        tries: 0,
    }
}

/// Begin an attempt, unless the intent has no parameters to build a tunnel from.
///
/// An intent without [`TunnelParams`](super::types::TunnelParams) is not a weaker request for a
/// tunnel — it is a request to take over one that already exists. It cannot start anything,
/// because a tunnel cannot be built without knowing its split rules; adoption is the only thing it
/// can do. So when there is nothing to adopt, the honest answer is to do nothing.
///
/// This is what the startup intent uses. Treating it as an ordinary Up made the app connect by
/// itself on every launch.
fn start_or_idle(up: &UpIntent, now: Instant, policy: &Policy) -> Decision {
    if up.params.is_none() {
        return Decision::to(Status::Idle);
    }
    connecting(Cycle::start(up, policy), now, policy)
}

/// Adopt a tunnel someone else is running.
fn adopt(up: &UpIntent, rt: &super::types::RunningTunnel, now_unix: i64) -> Decision {
    let connected_at = rt
        .connected_secs
        .map_or(now_unix, |secs| now_unix - secs as i64);
    Decision::to(Status::Up(UpStatus {
        epoch: up.epoch,
        protocol: rt.protocol,
        params: None,
        adopted: true,
        server_endpoint: rt.endpoint.clone(),
        assigned_ip: rt.address.clone(),
        connected_at,
        dark_since: None,
        resolved: true,
    }))
    .with(Effect::ResetSpeed)
    .with(Effect::RememberWinner(rt.protocol))
    .with(Effect::Resolve(CycleOutcome::Connected {
        protocol: rt.protocol,
        adopted: true,
    }))
}

/// The main table.
///
/// `now_unix` is the wall-clock timestamp used for `connected_at` only; every deadline comparison
/// uses the monotonic `now`.
pub fn reconcile(
    status: &Status,
    intent: &Intent,
    world: &World,
    now: Instant,
    now_unix: i64,
    policy: &Policy,
) -> Decision {
    match status {
        // ---------------------------------------------------------------------------- Idle
        Status::Idle => match (relate(status, intent), world) {
            // 1, 3: nothing of ours, nothing wanted.
            (Rel::Down, World::Clear | World::Dark) => Decision::stay(status),
            // 2: a tunnel exists but nobody wants one.
            (Rel::Down, World::Running(_)) => Decision::to(unwinding(
                UnwindOwner::Actor,
                None,
                UnwindReason::ForeignTunnel,
            ))
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            }),
            // 4: the normal start — but only for an intent that knows what to build.
            (Rel::Same(up) | Rel::Newer(up), World::Clear) => start_or_idle(up, now, policy),
            (Rel::Same(up) | Rel::Newer(up), World::Running(rt)) => {
                // 5a: only the bootstrap intent adopts. It is the one that carries no params,
                // because it is not asking for any particular tunnel — just for whatever is there.
                if up.accepts(rt.protocol) && up.params.is_none() {
                    adopt(up, rt, now_unix)
                } else {
                    // 5b, 6: we cannot know what split rules a tunnel we did not start was built
                    // with, so a caller who asked for specific rules gets a rebuild. Silently
                    // keeping a tunnel with the wrong split rules is a data-leak-shaped bug.
                    Decision::to(unwinding(
                        UnwindOwner::Actor,
                        Some(Cycle::start(up, policy)),
                        UnwindReason::WrongProtocol,
                    ))
                    .with(Effect::Unwind {
                        extra: Some(ExtraUndo::StopBackend),
                    })
                }
            }
            // 7: from Idle there is nothing to protect, so a non-authoritative observation must
            // not block a connect.
            (Rel::Same(up) | Rel::Newer(up), World::Dark) => start_or_idle(up, now, policy),
        },

        // ---------------------------------------------------------------------- Connecting
        Status::Connecting {
            cycle, deadline, ..
        } => match relate(status, intent) {
            // 8, 9: an intent change cancels the attempt. The token is fired; the task is never
            // dropped, so it still unwinds its own ladder and reports.
            Rel::Down => Decision::to(unwinding(
                UnwindOwner::Attempt,
                Some(cycle.clone()),
                UnwindReason::IntentDown,
            ))
            .with(Effect::CancelAttempt),
            Rel::Newer(_) => Decision::to(unwinding(
                UnwindOwner::Attempt,
                Some(cycle.clone()),
                UnwindReason::IntentChanged,
            ))
            .with(Effect::CancelAttempt),
            Rel::Same(_) => {
                if now < *deadline {
                    // 10, and it is load-bearing: NO observation may tear down an in-flight
                    // attempt. Connecting is left only by the attempt's own report, an intent
                    // change, or this deadline. Reading a transient unreachable poll as "the
                    // tunnel is gone" is what used to kill a connect that was about to succeed.
                    Decision::stay(status)
                } else {
                    // 11
                    let mut cycle = cycle.clone();
                    cycle.failures.push(AttemptFailure {
                        protocol: cycle.protocol(),
                        error: AttemptError::TimedOut,
                        pass: cycle.pass,
                    });
                    Decision::to(unwinding(
                        UnwindOwner::Attempt,
                        Some(cycle),
                        UnwindReason::AttemptTimedOut,
                    ))
                    .with(Effect::CancelAttempt)
                }
            }
        },

        // ------------------------------------------------------------------------------ Up
        Status::Up(u) => match (relate(status, intent), world) {
            // 12
            (Rel::Down, _) => Decision::to(unwinding(
                UnwindOwner::Actor,
                None,
                UnwindReason::IntentDown,
            ))
            .with(Effect::Unwind { extra: None }),
            // 13, 14, 15: a newer intent that the running tunnel already satisfies is a hand-over,
            // not a reconnect. Pressing Connect while connected does nothing.
            (Rel::Newer(up), world) if up.satisfied_by(u) => match world {
                World::Running(_) => Decision::to(Status::Up(UpStatus {
                    epoch: up.epoch,
                    resolved: true,
                    ..u.clone()
                }))
                .with(Effect::Resolve(CycleOutcome::Connected {
                    protocol: u.protocol,
                    adopted: u.adopted,
                })),
                // Handed over on a dark observation: the epoch takes the tunnel but its waiter
                // stays unresolved until something authoritative confirms it.
                World::Dark => Decision::to(Status::Up(UpStatus {
                    epoch: up.epoch,
                    resolved: false,
                    ..u.clone()
                })),
                World::Clear => Decision::to(unwinding(
                    UnwindOwner::Actor,
                    Some(Cycle::reconnect(up, policy)),
                    UnwindReason::TunnelDied,
                ))
                .with(Effect::Unwind { extra: None }),
            },
            // 16
            (Rel::Newer(up), _) => Decision::to(unwinding(
                UnwindOwner::Actor,
                Some(Cycle::start(up, policy)),
                UnwindReason::IntentChanged,
            ))
            .with(Effect::Unwind { extra: None }),
            // 17: confirmed alive. This is also what resets the darkness clock.
            (Rel::Same(_), World::Running(rt)) if rt.protocol == u.protocol => {
                let mut next = u.clone();
                next.dark_since = None;
                next.server_endpoint = rt.endpoint.clone();
                next.assigned_ip = rt.address.clone();
                let resolve = !next.resolved;
                next.resolved = true;
                let decision = Decision::to(Status::Up(next));
                if resolve {
                    decision.with(Effect::Resolve(CycleOutcome::Connected {
                        protocol: u.protocol,
                        adopted: u.adopted,
                    }))
                } else {
                    decision
                }
            }
            // 18: something else is running a different protocol than the one we started.
            (Rel::Same(up), World::Running(_)) => Decision::to(unwinding(
                UnwindOwner::Actor,
                Some(Cycle::reconnect(up, policy)),
                UnwindReason::Usurped,
            ))
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            }),
            // 19: a live answer saying "not running" is a confirmed stop, on every platform.
            (Rel::Same(up), World::Clear) => Decision::to(unwinding(
                UnwindOwner::Actor,
                Some(Cycle::reconnect(up, policy)),
                UnwindReason::TunnelDied,
            ))
            .with(Effect::Unwind { extra: None }),
            // 20, 21, 22: darkness is never authoritative, so it only starts a clock. The grace is
            // wall-clock and armed on entry to Up, so it cannot be exhausted by how often anyone
            // polls — and on desktop it is zero, because an in-process backend always answers.
            (Rel::Same(up), World::Dark) => match u.dark_since {
                None => Decision::to(Status::Up(UpStatus {
                    dark_since: Some(now),
                    ..u.clone()
                })),
                Some(since) if now.saturating_duration_since(since) <= policy.dark_grace => {
                    Decision::stay(status)
                }
                Some(_) => Decision::to(unwinding(
                    UnwindOwner::Actor,
                    Some(Cycle::reconnect(up, policy)),
                    UnwindReason::PeerLost,
                ))
                .with(Effect::Unwind { extra: None }),
            },
        },

        // ----------------------------------------------------------------------- Unwinding
        // 23: absorbing. All nine cells. The intent is still recorded and is acted upon the moment
        // the unwind reports done — but while a teardown is in flight, nothing starts. This single
        // block is what makes "a late disconnect tears down a newer connection" unwritable.
        Status::Unwinding { .. } => Decision::stay(status),

        // ------------------------------------------------------------------------ Retrying
        Status::Retrying { cycle, resume_at } => match (relate(status, intent), world) {
            // 24
            (Rel::Down, _) => {
                Decision::to(Status::Idle).with(Effect::Resolve(CycleOutcome::Cancelled))
            }
            // 25
            (Rel::Newer(up), _) => {
                let stale = cycle.epoch;
                connecting(Cycle::start(up, policy), now, policy).with(Effect::ResolveFor {
                    epoch: stale,
                    outcome: CycleOutcome::Cancelled,
                })
            }
            // 26: the tunnel came back on its own while we were waiting to retry.
            (Rel::Same(up), World::Running(rt)) if cycle.order.contains(&rt.protocol) => {
                adopt(up, rt, now_unix)
            }
            // 27
            (Rel::Same(_), World::Running(_)) => Decision::to(unwinding(
                UnwindOwner::Actor,
                Some(cycle.clone()),
                UnwindReason::WrongProtocol,
            ))
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            }),
            // 28, 29
            (Rel::Same(_), World::Clear | World::Dark) => {
                if now < *resume_at {
                    Decision::stay(status)
                } else {
                    connecting(cycle.clone(), now, policy)
                }
            }
        },
    }
}

/// Table 2: an attempt reported its terminal result.
///
/// Note what is absent: any cleanup. A failing attempt has already unwound its own partial ladder
/// before reporting, so there is no per-error-path teardown to write — and therefore none to get
/// wrong or forget.
pub fn on_attempt_done(
    status: &Status,
    intent: &Intent,
    result: AttemptResult,
    now: Instant,
    now_unix: i64,
    policy: &Policy,
) -> Decision {
    let Status::Connecting { cycle, .. } = status else {
        debug_assert!(
            false,
            "an attempt result outside Connecting must be routed to on_unwind_done"
        );
        return Decision::stay(status);
    };

    match result {
        AttemptResult::Established { view, stack } => match relate(status, intent) {
            // A1
            Rel::Same(_) => {
                let protocol = view.protocol;
                Decision::to(Status::Up(UpStatus {
                    resolved: true,
                    ..view
                }))
                .with(Effect::TakeStack(stack))
                .with(Effect::ResetSpeed)
                .with(Effect::RememberWinner(protocol))
                .with(Effect::Resolve(CycleOutcome::Connected {
                    protocol,
                    adopted: false,
                }))
            }
            // A2: it succeeded, but nobody wants it any more. Take the stack so the teardown can
            // undo exactly what the attempt applied.
            Rel::Down => Decision::to(unwinding(
                UnwindOwner::Actor,
                None,
                UnwindReason::IntentDown,
            ))
            .with(Effect::TakeStack(stack))
            .with(Effect::Unwind { extra: None }),
            // A3
            Rel::Newer(up) => Decision::to(unwinding(
                UnwindOwner::Actor,
                Some(Cycle::start(up, policy)),
                UnwindReason::IntentChanged,
            ))
            .with(Effect::TakeStack(stack))
            .with(Effect::ResolveFor {
                epoch: cycle.epoch,
                outcome: CycleOutcome::Cancelled,
            })
            .with(Effect::Unwind { extra: None }),
        },

        AttemptResult::Failed(error) => {
            let mut cycle = cycle.clone();
            cycle.failures.push(AttemptFailure {
                protocol: cycle.protocol(),
                error: error.clone(),
                pass: cycle.pass,
            });

            // A4: a denied consent dialog or a missing helper will not be fixed by trying the next
            // protocol — it will just ask again. Three protocols with a reconnect budget would be
            // up to nine dialogs.
            if error.is_fatal_for_cycle() && !matches!(intent, Intent::Down { .. }) {
                return Decision::to(Status::Idle).with(Effect::DemoteIntent).with(
                    Effect::ResolveFor {
                        epoch: cycle.epoch,
                        outcome: CycleOutcome::Exhausted {
                            failures: cycle.failures,
                        },
                    },
                );
            }

            match relate(status, intent) {
                // A5
                Rel::Down => {
                    Decision::to(Status::Idle).with(Effect::Resolve(CycleOutcome::Cancelled))
                }
                // A6
                Rel::Newer(up) => {
                    let stale = cycle.epoch;
                    connecting(Cycle::start(up, policy), now, policy).with(Effect::ResolveFor {
                        epoch: stale,
                        outcome: CycleOutcome::Cancelled,
                    })
                }
                Rel::Same(_) => {
                    if !cycle.is_last_probe() {
                        // A7: next protocol in the order.
                        cycle.advance();
                        connecting(cycle, now, policy)
                    } else if cycle.has_budget() {
                        // A8: another pass, after a backoff.
                        let backoff = policy.backoff(cycle.pass);
                        cycle.advance();
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        // A9
                        Decision::to(Status::Idle).with(Effect::DemoteIntent).with(
                            Effect::ResolveFor {
                                epoch: cycle.epoch,
                                outcome: CycleOutcome::Exhausted {
                                    failures: cycle.failures,
                                },
                            },
                        )
                    }
                }
            }
        }

        // A10: a cancel is always issued *from* Unwinding, so its report is routed to Table 3.
        AttemptResult::Cancelled => {
            debug_assert!(
                false,
                "a cancelled attempt reports into on_unwind_done, not here"
            );
            let _ = now_unix;
            Decision::stay(status)
        }
    }
}

/// Table 3: an unwind finished.
pub fn on_unwind_done(
    status: &Status,
    intent: &Intent,
    report: &UnwindReport,
    world: &World,
    now: Instant,
    policy: &Policy,
) -> Decision {
    let Status::Unwinding {
        cycle,
        reason,
        tries,
        ..
    } = status
    else {
        debug_assert!(false, "unwind result outside Unwinding");
        return Decision::stay(status);
    };

    // Step 0: believe the world, not the report. An unwind that returned Ok while a tunnel is
    // still running has not actually finished, and starting a fresh attempt on top of it would
    // stack one tunnel on another.
    if matches!(world, World::Running(_)) {
        return if tries + 1 < policy.unwind_tries {
            // U0a
            Decision::to(Status::Unwinding {
                owner: UnwindOwner::Actor,
                cycle: cycle.clone(),
                reason: *reason,
                tries: tries + 1,
            })
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            })
        } else {
            // U0b: give up loudly rather than livelock.
            tracing::error!(
                residual = ?report.residual,
                "teardown could not be confirmed; the machine may still be configured"
            );
            Decision::to(Status::Idle)
                .with(Effect::DemoteIntent)
                .with(Effect::Resolve(CycleOutcome::UnwindFailed))
        };
    }
    // Deliberately not triggered by Dark: darkness is never authoritative, and re-unwinding
    // forever against an unreachable peer is the livelock this whole design avoids. It falls
    // through, lands in Retrying or Idle, and re-observes.

    match relate(status, intent) {
        // U1
        Rel::Down => {
            let outcome = if matches!(reason, UnwindReason::IntentDown) {
                CycleOutcome::Down
            } else {
                CycleOutcome::Cancelled
            };
            Decision::to(Status::Idle).with(Effect::Resolve(outcome))
        }
        // U2
        Rel::Newer(up) => {
            let decision = connecting(Cycle::start(up, policy), now, policy);
            match cycle {
                Some(c) => decision.with(Effect::ResolveFor {
                    epoch: c.epoch,
                    outcome: CycleOutcome::Cancelled,
                }),
                None => decision,
            }
        }
        Rel::Same(_) => {
            let Some(cycle) = cycle.clone() else {
                debug_assert!(false, "Up= unwind without a cycle");
                return Decision::to(Status::Idle);
            };
            match reason {
                // U3: the teardown was for an intent change, and the surviving intent still wants
                // a tunnel — start its order from the top.
                UnwindReason::IntentDown | UnwindReason::IntentChanged => {
                    let mut cycle = cycle;
                    cycle.index = 0;
                    connecting(cycle, now, policy)
                }
                // U4, U5, U6
                UnwindReason::AttemptFailed | UnwindReason::AttemptTimedOut => {
                    let mut cycle = cycle;
                    if !cycle.is_last_probe() {
                        cycle.advance();
                        connecting(cycle, now, policy)
                    } else if cycle.has_budget() {
                        let backoff = policy.backoff(cycle.pass);
                        cycle.advance();
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        Decision::to(Status::Idle).with(Effect::DemoteIntent).with(
                            Effect::ResolveFor {
                                epoch: cycle.epoch,
                                outcome: CycleOutcome::Exhausted {
                                    failures: cycle.failures,
                                },
                            },
                        )
                    }
                }
                // U7, U8: a tunnel that was up and died gets the reconnect budget, not the cold
                // one — which is what keeps a user-initiated connect failing fast while a dropped
                // tunnel keeps trying.
                UnwindReason::TunnelDied | UnwindReason::PeerLost | UnwindReason::Usurped => {
                    let mut cycle = cycle;
                    if cycle.has_budget() {
                        let backoff = policy.backoff(cycle.pass);
                        let protocol = cycle.protocol();
                        cycle.index = 0;
                        cycle.pass += 1;
                        let _ = protocol;
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        let protocol = cycle.protocol();
                        let passes = cycle.pass + 1;
                        Decision::to(Status::Idle).with(Effect::DemoteIntent).with(
                            Effect::ResolveFor {
                                epoch: cycle.epoch,
                                outcome: CycleOutcome::LostGaveUp { protocol, passes },
                            },
                        )
                    }
                }
                // U9: the obstruction is gone, so proceed with what we wanted all along.
                UnwindReason::WrongProtocol
                | UnwindReason::ForeignTunnel
                | UnwindReason::CrashRecovery => connecting(cycle, now, policy),
            }
        }
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
