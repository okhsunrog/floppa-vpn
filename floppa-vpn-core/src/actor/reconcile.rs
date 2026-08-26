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
    IntentEpoch, Link, Policy, RunningTunnel, Status, TunnelParams, UnwindReason, UpIntent,
    UpStatus, World,
};
use crate::protocol::Protocol;
use crate::rollback::{ExtraUndo, RollbackStack, UnwindReport};
use std::time::Instant;

/// What the actor must *do* as a result of a transition. The actor executes these; the table only
/// names them, which is what keeps the table pure.
#[derive(Debug)]
pub enum Effect {
    /// Spawn an attempt task for this protocol, building exactly `params`. The params travel
    /// with the effect rather than being read off the intent: by the time a retry begins, the
    /// intent may already be a different one.
    Begin {
        protocol: Protocol,
        epoch: IntentEpoch,
        index: usize,
        params: TunnelParams,
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
    /// Take over a tunnel the `:vpn` service started by itself — always-on, boot, a lockdown
    /// restore — by promoting the intent to Up for exactly that tunnel.
    ///
    /// Whether the service restarts is the system toggle's decision, not ours (see
    /// `vpn/autostart.rs`), so killing it as a foreign tunnel put the app in a fight with the OS:
    /// the system brought the tunnel back, the UI stopped it, and around again — with a
    /// notification on every turn and the UI showing Disconnected the whole time. Adopting it
    /// instead means the UI shows what is true, and a Disconnect is once more an explicit act of
    /// the user's. A wipe still stops it: see [`Intent::is_forget`].
    ///
    /// The actor mints the epoch; the table never invents one.
    AdoptAutonomous {
        protocol: Protocol,
        params: Option<TunnelParams>,
    },
    /// Demote the intent to Down at the same epoch, once a caller-issued Up has been given up
    /// on: an Up intent with params means the actor is working toward Up. The startup intent is
    /// the deliberate exception — it carries no params, so it rests in Idle without being
    /// demoted, and adopts a tunnel that appears later (row 5a).
    DemoteIntent,
    /// Ask the far side to prove it is there — a forced rehandshake, or a ping for VLESS.
    ///
    /// Fire-and-forget: the probe's own return value is not the verdict, the next observation is.
    /// A probe that fails outright and one that succeeds against a peer that no longer exists must
    /// end the same way, and only the tunnel's silence says which happened.
    Probe,
    /// Resolve waiters for an epoch. Always explicit: the actor executes effects after it has
    /// already written the next status, so "the current epoch" would be read off the wrong state.
    Resolve {
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

/// Begin an attempt — or park the cycle, if it is known there is no network to make one over.
///
/// Every path that would start an attempt comes through here, which is why the gate is one `if`
/// rather than a condition repeated across the table. Parking costs the cycle **nothing**: the
/// pass is not burnt, the index does not advance, no effect fires. It waits in `Retrying` with a
/// `resume_at` already in the past, so the moment the link is reported back rows 28/29 walk
/// straight into a real attempt — and while it is still gone they walk back into this parking
/// spot. No timer has to be cancelled and no far-future deadline has to be guessed at.
///
/// What this is for: a phone that loses signal for five minutes used to spend its entire reconnect
/// budget on attempts that could not have worked, give up, demote the intent, and leave the user
/// disconnected with the network back and nothing to notice it. A pass is only worth spending on a
/// network that exists.
///
/// A cold connect is parked on the same terms as a reconnect, deliberately. It is tempting to let
/// a user who presses Connect in airplane mode fail fast, but the same code path serves the system
/// start on boot — where the service routinely runs before Wi-Fi has associated, and failing fast
/// demotes the intent under an always-on lockdown. "Waiting for a network" is both the honest
/// thing to show a person and the correct thing to do for the system.
fn connecting(cycle: Cycle, now: Instant, policy: &Policy, link: Link) -> Decision {
    if link.is_offline() {
        return Decision::to(Status::Retrying {
            cycle,
            resume_at: now,
        });
    }
    let effect = Effect::Begin {
        protocol: cycle.protocol(),
        epoch: cycle.epoch,
        index: cycle.index,
        params: cycle.params.clone(),
    };
    Decision::to(Status::Connecting {
        cycle,
        phase: AttemptPhase::Preparing,
        deadline: now + policy.attempt_budget,
    })
    .with(effect)
}

/// The cycle has nothing left to spend — unless there was nothing to spend it on.
///
/// The three budget-exhaustion paths (a failed probe, an expired deadline, a teardown that ends
/// with no budget left) were three copies of the same four lines; they are this instead, so the
/// rule below is stated once.
///
/// **A cycle does not run out of budget on a device with no network.** The choke point in
/// [`connecting`] stops passes being spent while the link is known gone, but it cannot help a
/// cycle whose budget ran out *across* an outage — a phone on a train, in and out of coverage,
/// gets an online window just long enough to fail an attempt in, and enough of those drain the
/// budget without a single one of them having had a fair chance. Giving up then demotes the intent
/// and leaves the user disconnected once the signal is properly back, which is the whole defect
/// this file's link gate exists to remove.
///
/// Parking instead is not free budget: the cycle resumes with none, so the first attempt after the
/// network returns is also its last. What it buys is that the attempt happens at all.
fn give_up_or_park(cycle: Cycle, now: Instant, link: Link) -> Decision {
    if link.is_offline() {
        return Decision::to(Status::Retrying {
            cycle,
            resume_at: now,
        });
    }
    let epoch = cycle.epoch;
    Decision::to(Status::Idle)
        .with(Effect::DemoteIntent)
        .with(Effect::Resolve {
            epoch,
            outcome: cycle.gave_up(),
        })
}

fn unwinding(cycle: Option<Cycle>, reason: UnwindReason) -> Status {
    Status::Unwinding {
        cycle,
        reason,
        tries: 0,
    }
}

/// What tearing down this tunnel has to undo beyond the stack.
///
/// A tunnel we started is entirely described by the stack we hold for it, so unwinding that stack
/// stops it. An **adopted** tunnel is not: adoption takes no stack, because we did not apply
/// anything — so its unwind is an empty one that completes in microseconds and stops nothing.
/// Every row that left `Up{adopted}` with `extra: None` therefore resolved its cycle against a
/// world it had not touched, published Disconnected while the tunnel was still carrying traffic,
/// and left the tunnel to be noticed and killed by row 2 a second later. On a logout that second
/// was enough for the configs and the autostart bundle to be wiped under a live tunnel.
/// Does a running tunnel route what this intent asked for?
///
/// An intent with no params is asking for "whatever is there" and is satisfied by anything; one
/// that named its split rules is satisfied only by a tunnel whose owner reports exactly those.
/// A tunnel whose rules are unknown can never be proven to match, and silently keeping one with
/// the wrong rules is a data-leak-shaped bug.
fn same_rules(want: &Option<TunnelParams>, have: &Option<TunnelParams>) -> bool {
    match (want, have) {
        (None, _) => true,
        (Some(want), Some(have)) => want == have,
        (Some(_), None) => false,
    }
}

fn undo_for(u: &UpStatus) -> Option<ExtraUndo> {
    u.adopted.then_some(ExtraUndo::StopBackend)
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
fn start_or_idle(up: &UpIntent, now: Instant, policy: &Policy, link: Link) -> Decision {
    match Cycle::start(up, policy) {
        Some(cycle) => connecting(cycle, now, policy, link),
        None => Decision::to(Status::Idle),
    }
}

/// Adopt a running tunnel. `params` is what it was built with when that is known — reported by
/// the process that owns it, or our own from an attempt of the same epoch that we had already
/// given up on — and `None` for a tunnel whose split rules cannot be known.
fn adopt(
    up: &UpIntent,
    rt: &RunningTunnel,
    params: Option<TunnelParams>,
    now_unix: i64,
) -> Decision {
    let connected_at = rt
        .connected_secs
        .map_or(now_unix, |secs| now_unix - secs as i64);
    Decision::to(Status::Up(UpStatus {
        epoch: up.epoch,
        protocol: rt.protocol,
        params,
        adopted: true,
        server_endpoint: rt.endpoint.clone(),
        assigned_ip: rt.address.clone(),
        connected_at,
        dark_since: None,
        probing_since: None,
        resolved: true,
    }))
    .with(Effect::ResetSpeed)
    .with(Effect::RememberWinner(rt.protocol))
    .with(Effect::Resolve {
        epoch: up.epoch,
        outcome: CycleOutcome::Connected {
            protocol: rt.protocol,
            adopted: true,
            // Adoption ran no ladder, so nothing failed on the way.
            failures: Vec::new(),
        },
    })
}

/// Rows 17a–17c: what a running tunnel's silence means.
///
/// The caller applies the verdict on top of the ordinary alive path, which is why this reports
/// what it found rather than rewriting the status itself.
///
/// The order is deliberate and is the whole reason this is not a single threshold:
///
/// - **17a** silence past the bound, nothing asked yet: ask. A probe is one round trip and it is
///   the only thing that distinguishes a dead peer from a link that has simply had no reason to
///   rekey — a phone that spent the last hour asleep looks identical to one whose peer was
///   deleted, and tearing the first one down on wake is a bug, not a recovery.
/// - **17b** asked, still within the grace: wait. The answer arrives as a *fresh observation*,
///   never as the probe's return value: a probe can fail on a wedged socket while the tunnel is
///   fine, and can succeed against a peer that has been deleted.
/// - **17c** asked, grace spent, still silent: the peer is gone. This is a reconnect, not a stop —
///   the same cycle a tunnel that died outright gets, so the ladder can fall to another protocol
///   and the UI is told which one carried it.
///
/// Silence that ends at any point clears the clock, and the caller's alive path takes over.
fn judge_silence(
    u: &UpStatus,
    up: &UpIntent,
    rt: &RunningTunnel,
    now: Instant,
    policy: &Policy,
) -> Silence {
    // An owner that cannot say is not evidence of silence, and neither is silence inside the
    // bound. Both mean the peer is answering as far as anyone here can tell.
    let Some(secs) = rt.silent_secs else {
        return Silence::Answering;
    };
    if std::time::Duration::from_secs(secs.max(0) as u64) <= policy.silent_after {
        return Silence::Answering;
    }
    match u.probing_since {
        // 17a
        None => Silence::Decided(
            Decision::to(Status::Up(UpStatus {
                probing_since: Some(now),
                dark_since: None,
                ..u.clone()
            }))
            .with(Effect::Probe),
        ),
        // 17b
        Some(since) if now.saturating_duration_since(since) <= policy.probe_grace => {
            Silence::Awaiting
        }
        // 17c
        Some(_) => Silence::Decided(
            Decision::to(unwinding(
                Cycle::reconnect(up, policy),
                UnwindReason::PeerSilent,
            ))
            .with(Effect::Unwind { extra: undo_for(u) }),
        ),
    }
}

/// What [`judge_silence`] found.
enum Silence {
    /// The far side is answering, or has not been quiet long enough to doubt. Any probe clock is
    /// stale and gets cleared.
    Answering,
    /// A probe is out and its grace has not run out. The clock stands.
    Awaiting,
    /// Silence has changed the status.
    Decided(Decision),
}

/// The main table.
///
/// `now_unix` is the wall-clock timestamp used for `connected_at` only; every deadline comparison
/// uses the monotonic `now`.
pub fn reconcile(
    status: &Status,
    intent: &Intent,
    world: &World,
    link: Link,
    now: Instant,
    now_unix: i64,
    policy: &Policy,
) -> Decision {
    match status {
        // ---------------------------------------------------------------------------- Idle
        Status::Idle => match (relate(status, intent), world) {
            // 1, 3: nothing of ours, nothing wanted.
            (Rel::Down, World::Clear | World::Dark) => Decision::stay(status),
            // 2a: the system brought a tunnel back by itself (always-on, boot, a lockdown
            // restore). Nobody here asked for it, and that is precisely why it is not ours to
            // kill — so it is adopted, and stopping it stays a decision the user makes.
            (Rel::Down, World::Running(rt)) if rt.autonomous && !intent.is_forget() => {
                Decision::stay(status).with(Effect::AdoptAutonomous {
                    protocol: rt.protocol,
                    params: rt.params.clone(),
                })
            }
            // 2b: a tunnel exists but nobody wants one — including a system-started one when the
            // Down is a wipe, which has to leave nothing running whoever started it.
            (Rel::Down, World::Running(_)) => {
                Decision::to(unwinding(None, UnwindReason::ForeignTunnel)).with(Effect::Unwind {
                    extra: Some(ExtraUndo::StopBackend),
                })
            }
            // 4: the normal start — but only for an intent that knows what to build.
            (Rel::Same(up) | Rel::Newer(up), World::Clear) => start_or_idle(up, now, policy, link),
            (Rel::Same(up) | Rel::Newer(up), World::Running(rt)) => {
                // 5a: the bootstrap intent adopts. It is the one that carries no params, because
                // it is not asking for any particular tunnel — just for whatever is there. When
                // the owner reports the rules the tunnel was built with — the Android service
                // does, for a tunnel it started over the RPC or by itself from the autostart
                // bundle — they are taken along, so a later Connect with the same rules is a
                // hand-over rather than a rebuild.
                //
                // 5c: a caller-issued intent adopts too, but only a tunnel whose owner reports
                // exactly the rules it asked for. An intent that wants the tunnel that is
                // already there has nothing to rebuild.
                if up.accepts(rt.protocol) && same_rules(&up.params, &rt.params) {
                    adopt(up, rt, rt.params.clone(), now_unix)
                } else {
                    // 5b, 6: a tunnel whose split rules are unknown, or known to differ, cannot
                    // satisfy a caller who asked for specific ones: it gets a rebuild. Silently
                    // keeping a tunnel with the wrong split rules is a data-leak-shaped bug. An
                    // intent without params has no cycle: the obstruction is removed and it
                    // goes back to Idle rather than building something it cannot specify.
                    Decision::to(unwinding(
                        Cycle::start(up, policy),
                        UnwindReason::WrongProtocol,
                    ))
                    .with(Effect::Unwind {
                        extra: Some(ExtraUndo::StopBackend),
                    })
                }
            }
            // 7: from Idle there is nothing to protect, so a non-authoritative observation must
            // not block a connect.
            (Rel::Same(up) | Rel::Newer(up), World::Dark) => start_or_idle(up, now, policy, link),
        },

        // ---------------------------------------------------------------------- Connecting
        Status::Connecting {
            cycle, deadline, ..
        } => match relate(status, intent) {
            // 8, 9: an intent change cancels the attempt. The token is fired; the task is never
            // dropped, so it still unwinds its own ladder and reports.
            Rel::Down => Decision::to(unwinding(Some(cycle.clone()), UnwindReason::IntentDown))
                .with(Effect::CancelAttempt),
            Rel::Newer(_) => {
                Decision::to(unwinding(Some(cycle.clone()), UnwindReason::IntentChanged))
                    .with(Effect::CancelAttempt)
            }
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
                    Decision::to(unwinding(Some(cycle), UnwindReason::AttemptTimedOut))
                        .with(Effect::CancelAttempt)
                }
            }
        },

        // ------------------------------------------------------------------------------ Up
        Status::Up(u) => match (relate(status, intent), world) {
            // 12
            (Rel::Down, _) => Decision::to(unwinding(None, UnwindReason::IntentDown))
                .with(Effect::Unwind { extra: undo_for(u) }),
            // 13, 14, 15: a newer intent that the running tunnel already satisfies is a hand-over,
            // not a reconnect. Pressing Connect while connected does nothing.
            (Rel::Newer(up), world) if up.satisfied_by(u) => match world {
                World::Running(_) => Decision::to(Status::Up(UpStatus {
                    epoch: up.epoch,
                    resolved: true,
                    ..u.clone()
                }))
                .with(Effect::Resolve {
                    epoch: up.epoch,
                    outcome: CycleOutcome::Connected {
                        protocol: u.protocol,
                        adopted: u.adopted,
                        // A hand-over to a tunnel that is already up: this epoch tried nothing.
                        failures: Vec::new(),
                    },
                }),
                // Handed over on a dark observation: the epoch takes the tunnel but its waiter
                // stays unresolved until something authoritative confirms it.
                World::Dark => Decision::to(Status::Up(UpStatus {
                    epoch: up.epoch,
                    resolved: false,
                    ..u.clone()
                })),
                World::Clear => Decision::to(unwinding(
                    Cycle::reconnect(up, policy),
                    UnwindReason::TunnelDied,
                ))
                .with(Effect::Unwind { extra: undo_for(u) }),
            },
            // 16
            (Rel::Newer(up), _) => Decision::to(unwinding(
                Cycle::start(up, policy),
                UnwindReason::IntentChanged,
            ))
            .with(Effect::Unwind { extra: undo_for(u) }),
            // 17: the owner answered about our own tunnel. It is running — but "running" is a
            // statement about the device, not about the peer at the other end, and those come
            // apart exactly when a peer is deleted on the server: the device keeps running, this
            // row keeps saying Up, and no traffic passes. So the peer's silence is judged here
            // too, in the order silence → probe → verdict (rows 17a–17c below).
            (Rel::Same(up), World::Running(rt)) if rt.protocol == u.protocol => {
                // With no network at all, silence says nothing about the peer — of course it is
                // quiet, nothing can reach it. Judging it anyway is what tore the tunnel down
                // every time the phone spent three minutes in a lift: PeerSilent, unwind, and a
                // ladder that came back up on whichever protocol was first in the order rather
                // than the one that had been working.
                //
                // Treated as `Answering`, not merely skipped, and that distinction is the bug it
                // would otherwise reintroduce: `Answering` clears `probing_since`. A link that
                // dies midway through the probe grace would otherwise leave the clock running
                // while nothing could possibly answer it, and 17c would fire the instant the
                // network came back — tearing down the tunnel one second before the rebind reflex
                // fixed it.
                let silence = if link.is_offline() {
                    Silence::Answering
                } else {
                    judge_silence(u, up, rt, now, policy)
                };
                if let Silence::Decided(decision) = silence {
                    return decision;
                }
                let mut next = u.clone();
                next.dark_since = None;
                if matches!(silence, Silence::Answering) {
                    next.probing_since = None;
                }
                // Only for a tunnel somebody else described to us. For one we built, the ladder
                // already recorded what it asked for, and overwriting it here replaced the
                // configured `host:port` with the address it resolved to about a second after
                // Connected — so the server line in the UI changed under the user.
                if u.adopted {
                    next.server_endpoint = rt.endpoint.clone();
                    next.assigned_ip = rt.address.clone();
                }
                let resolve = !next.resolved;
                next.resolved = true;
                let decision = Decision::to(Status::Up(next));
                if resolve {
                    decision.with(Effect::Resolve {
                        epoch: u.epoch,
                        outcome: CycleOutcome::Connected {
                            protocol: u.protocol,
                            adopted: u.adopted,
                            failures: Vec::new(),
                        },
                    })
                } else {
                    decision
                }
            }
            // 18: something else is running a different protocol than the one we started.
            (Rel::Same(up), World::Running(_)) => Decision::to(unwinding(
                Cycle::reconnect(up, policy),
                UnwindReason::Usurped,
            ))
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            }),
            // 19: a live answer saying "not running" is a confirmed stop, on every platform. An
            // adopted tunnel that dies leaves the startup intent with nothing to rebuild from,
            // so its cycle is `None` and the teardown ends in Idle.
            (Rel::Same(up), World::Clear) => Decision::to(unwinding(
                Cycle::reconnect(up, policy),
                UnwindReason::TunnelDied,
            ))
            .with(Effect::Unwind { extra: undo_for(u) }),
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
                    Cycle::reconnect(up, policy),
                    UnwindReason::PeerLost,
                ))
                .with(Effect::Unwind { extra: undo_for(u) }),
            },
        },

        // ----------------------------------------------------------------------- Unwinding
        // 23: absorbing. All nine cells. The intent is still recorded and is acted upon the moment
        // the unwind reports done — but while a teardown is in flight, nothing starts. This single
        // block is what makes "a late disconnect tears down a newer connection" unwritable.
        Status::Unwinding { .. } => Decision::stay(status),

        // ------------------------------------------------------------------------ Retrying
        Status::Retrying { cycle, resume_at } => match (relate(status, intent), world) {
            // 24: the waiting cycle is cancelled; the Down itself is resolved by the actor the
            // moment it sees Idle with a Down intent.
            (Rel::Down, _) => Decision::to(Status::Idle).with(Effect::Resolve {
                epoch: cycle.epoch,
                outcome: CycleOutcome::Cancelled,
            }),
            // 25: a newer intent while waiting to retry. If a tunnel is up in the meantime it
            // is in the way — the same reasoning as rows 5b/6 and 16, which is what this row
            // used to contradict by starting an attempt on top of it.
            (Rel::Newer(up), World::Running(_)) => Decision::to(unwinding(
                Cycle::start(up, policy),
                UnwindReason::IntentChanged,
            ))
            .with(Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend),
            })
            .with(Effect::Resolve {
                epoch: cycle.epoch,
                outcome: CycleOutcome::Cancelled,
            }),
            (Rel::Newer(up), World::Clear | World::Dark) => start_or_idle(up, now, policy, link)
                .with(Effect::Resolve {
                    epoch: cycle.epoch,
                    outcome: CycleOutcome::Cancelled,
                }),
            // 26: a tunnel appeared while we were waiting to retry. Its params are what its
            // owner reports, and nothing else: a service generation is not an intent epoch, so
            // there is no way to recognise a late tunnel of our own by its identity alone.
            //
            // And it is adopted on the same terms as row 5c — the protocol must be wanted *and*
            // the rules must be the ones asked for. This row used to check only the protocol,
            // which is the asymmetry row 5c calls a data-leak-shaped bug: a tunnel routing
            // something else would have been kept, and row 17 would then have held it.
            (Rel::Same(up), World::Running(rt))
                if cycle.order.contains(&rt.protocol) && same_rules(&up.params, &rt.params) =>
            {
                adopt(up, rt, rt.params.clone(), now_unix)
            }
            // 27
            (Rel::Same(_), World::Running(_)) => {
                Decision::to(unwinding(Some(cycle.clone()), UnwindReason::WrongProtocol)).with(
                    Effect::Unwind {
                        extra: Some(ExtraUndo::StopBackend),
                    },
                )
            }
            // 28, 29
            (Rel::Same(_), World::Clear | World::Dark) => {
                if now < *resume_at {
                    Decision::stay(status)
                } else {
                    connecting(cycle.clone(), now, policy, link)
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
    link: Link,
    now: Instant,
    policy: &Policy,
) -> Decision {
    let Status::Connecting { cycle, .. } = status else {
        debug_assert!(
            false,
            "an attempt result outside Connecting must be routed to on_unwind_done"
        );
        return Decision::stay(status);
    };

    // The intent is always the one this cycle is working on. An intent change ticks the table
    // synchronously, before any further command is read, and rows 8, 9 and 11 leave Connecting
    // for Unwinding on the spot — so a report that finds the intent changed is routed to
    // `on_unwind_done` by the actor and never reaches this function. The `Down`/`Newer` arms
    // below are therefore unreachable; they keep the match total and fail safe.
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
                .with(Effect::Resolve {
                    epoch: cycle.epoch,
                    outcome: CycleOutcome::Connected {
                        protocol,
                        adopted: false,
                        // Everything the ladder stepped over to get here. A protocol that failed
                        // to verify is a peer worth repairing even though another one carried the
                        // connection.
                        failures: cycle.failures.clone(),
                    },
                })
            }
            Rel::Down | Rel::Newer(_) => {
                debug_assert!(
                    false,
                    "an intent change leaves Connecting before any report"
                );
                Decision::to(unwinding(None, UnwindReason::IntentChanged))
                    .with(Effect::TakeStack(stack))
                    .with(Effect::Resolve {
                        epoch: cycle.epoch,
                        outcome: CycleOutcome::Cancelled,
                    })
                    .with(Effect::Unwind { extra: None })
            }
        },

        AttemptResult::Failed(error) => {
            let mut cycle = cycle.clone();
            cycle.failures.push(AttemptFailure {
                protocol: cycle.protocol(),
                error: error.clone(),
                pass: cycle.pass,
            });

            // A0: a crashed task never ran its own unwind. Undo what the journal recorded and
            // stop the backend it may have started; the cycle ends once that is confirmed.
            if matches!(error, AttemptError::Crashed { .. }) {
                return Decision::to(unwinding(Some(cycle), UnwindReason::AttemptCrashed)).with(
                    Effect::Unwind {
                        extra: Some(ExtraUndo::StopBackend),
                    },
                );
            }

            // A4: a denied consent dialog or a missing helper will not be fixed by trying the next
            // protocol — it will just ask again. Three protocols with a reconnect budget would be
            // up to nine dialogs.
            if error.is_fatal_for_cycle() && !matches!(intent, Intent::Down { .. }) {
                return Decision::to(Status::Idle).with(Effect::DemoteIntent).with(
                    Effect::Resolve {
                        epoch: cycle.epoch,
                        outcome: CycleOutcome::Exhausted {
                            failures: cycle.failures,
                        },
                    },
                );
            }

            match relate(status, intent) {
                Rel::Down | Rel::Newer(_) => {
                    debug_assert!(
                        false,
                        "an intent change leaves Connecting before any report"
                    );
                    Decision::to(Status::Idle).with(Effect::Resolve {
                        epoch: cycle.epoch,
                        outcome: CycleOutcome::Cancelled,
                    })
                }
                Rel::Same(_) => {
                    if !cycle.is_last_probe() {
                        // A7: next protocol in the order.
                        cycle.advance();
                        connecting(cycle, now, policy, link)
                    } else if cycle.has_budget() {
                        // A8: another pass, after a backoff.
                        let backoff = policy.backoff(cycle.pass);
                        cycle.advance();
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        // A9: out of budget. How that reads depends on where the cycle came
                        // from — a cold connect that never worked is exhausted, a tunnel that
                        // was up and could not be brought back gave up.
                        give_up_or_park(cycle, now, link)
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
    link: Link,
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
                .with(Effect::Resolve {
                    epoch: intent.epoch(),
                    outcome: CycleOutcome::UnwindFailed,
                })
        };
    }
    // Deliberately not triggered by Dark: darkness is never authoritative, and re-unwinding
    // forever against an unreachable peer is the livelock this whole design avoids. It falls
    // through, lands in Retrying or Idle, and re-observes.

    match relate(status, intent) {
        // U1: the Down reached Down, whatever the teardown had originally been for. It used to
        // report `Cancelled` unless the unwind's own reason was IntentDown, so a Disconnect that
        // arrived during a reconnect teardown told its caller its request had been superseded —
        // by itself. `Cancelled` belongs to the cycle this Down displaced, and that is now
        // resolved separately, the way U2 and rows 24 and 25 already did.
        Rel::Down => {
            let decision = Decision::to(Status::Idle).with(Effect::Resolve {
                epoch: intent.epoch(),
                outcome: CycleOutcome::Down,
            });
            match cycle {
                Some(c) if c.epoch != intent.epoch() => decision.with(Effect::Resolve {
                    epoch: c.epoch,
                    outcome: CycleOutcome::Cancelled,
                }),
                _ => decision,
            }
        }
        // U2
        Rel::Newer(up) => {
            let decision = start_or_idle(up, now, policy, link);
            match cycle {
                Some(c) => decision.with(Effect::Resolve {
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
                    connecting(cycle, now, policy, link)
                }
                // U4, U5, U6
                UnwindReason::AttemptTimedOut => {
                    let mut cycle = cycle;
                    if !cycle.is_last_probe() {
                        cycle.advance();
                        connecting(cycle, now, policy, link)
                    } else if cycle.has_budget() {
                        let backoff = policy.backoff(cycle.pass);
                        cycle.advance();
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        give_up_or_park(cycle, now, link)
                    }
                }
                // U6a: the machine is clean again after a crash; the cycle is over, and the
                // failure it recorded says why.
                UnwindReason::AttemptCrashed => Decision::to(Status::Idle)
                    .with(Effect::DemoteIntent)
                    .with(Effect::Resolve {
                        epoch: cycle.epoch,
                        outcome: CycleOutcome::Exhausted {
                            failures: cycle.failures,
                        },
                    }),
                // U7, U8: a tunnel that was up and died gets the reconnect budget, not the cold
                // one — which is what keeps a user-initiated connect failing fast while a dropped
                // tunnel keeps trying.
                UnwindReason::TunnelDied
                | UnwindReason::PeerLost
                | UnwindReason::PeerSilent
                | UnwindReason::Usurped => {
                    let mut cycle = cycle;
                    if cycle.has_budget() {
                        // The pass is *not* burnt here: this cycle has not tried anything yet,
                        // and counting the tunnel's death as a pass spent one of the three the
                        // policy promises, so `reconnect_passes: 3` only ever ran two.
                        let backoff = policy.backoff(cycle.pass);
                        cycle.index = 0;
                        Decision::to(Status::Retrying {
                            cycle,
                            resume_at: now + backoff,
                        })
                    } else {
                        give_up_or_park(cycle, now, link)
                    }
                }
                // U9: the obstruction is gone, so proceed with what we wanted all along.
                UnwindReason::WrongProtocol => connecting(cycle, now, policy, link),
                // Both are only ever entered with no cycle (rows 2 and bootstrap), and a status
                // without a cycle has no epoch for an intent to be `Same` as.
                UnwindReason::ForeignTunnel | UnwindReason::CrashRecovery => {
                    debug_assert!(false, "{reason:?} never carries a cycle");
                    connecting(cycle, now, policy, link)
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
