//! The decision table, row by row.
//!
//! Every test names the property rather than the row number, so a failure says what broke rather
//! than which cell moved. Where a row exists to kill a specific historical defect, the test says
//! so — those are the ones that must never be "fixed" by relaxing them.

use super::*;
use crate::vpn::actor::types::*;
use crate::vpn::protocol::Protocol;
use std::time::Duration;

const AWG: Protocol = Protocol::AmneziaWg;
const WG: Protocol = Protocol::WireGuard;
const VLESS: Protocol = Protocol::Vless;

fn policy() -> Policy {
    Policy {
        dark_grace: Duration::from_secs(6),
        ..Policy::default()
    }
}

fn t0() -> Instant {
    Instant::now()
}

fn up_intent(epoch: u64, order: &[Protocol], params: Option<TunnelParams>) -> Intent {
    Intent::Up(UpIntent {
        epoch: IntentEpoch(epoch),
        order: order.to_vec(),
        params,
    })
}

fn down(epoch: u64) -> Intent {
    Intent::Down {
        epoch: IntentEpoch(epoch),
    }
}

fn params() -> Option<TunnelParams> {
    Some(TunnelParams::new(SplitMode::All, vec![]))
}

fn running(protocol: Protocol) -> World {
    World::Running(RunningTunnel {
        protocol,
        epoch: None,
        endpoint: "vpn.example:51820".into(),
        address: "10.0.0.2/32".into(),
        connected_secs: Some(30),
    })
}

fn up_status(epoch: u64, protocol: Protocol, params: Option<TunnelParams>) -> UpStatus {
    UpStatus {
        epoch: IntentEpoch(epoch),
        protocol,
        params,
        adopted: false,
        server_endpoint: "vpn.example:51820".into(),
        assigned_ip: "10.0.0.2/32".into(),
        connected_at: 1_000,
        dark_since: None,
        resolved: true,
    }
}

fn cycle(epoch: u64, order: &[Protocol], passes_allowed: u32) -> Cycle {
    Cycle {
        epoch: IntentEpoch(epoch),
        order: order.to_vec(),
        params: params(),
        index: 0,
        pass: 0,
        passes_allowed,
        failures: Vec::new(),
    }
}

fn go(status: &Status, intent: &Intent, world: &World, now: Instant) -> Decision {
    reconcile(status, intent, world, now, 1_000, &policy())
}

/// Effects are matched by shape, since some carry non-comparable payloads.
fn has_begin(d: &Decision, protocol: Protocol) -> bool {
    d.effects
        .iter()
        .any(|e| matches!(e, Effect::Begin { protocol: p, .. } if *p == protocol))
}

fn has_unwind(d: &Decision) -> bool {
    d.effects.iter().any(|e| matches!(e, Effect::Unwind { .. }))
}

fn has_stop_foreign(d: &Decision) -> bool {
    d.effects.iter().any(|e| {
        matches!(
            e,
            Effect::Unwind {
                extra: Some(ExtraUndo::StopBackend)
            }
        )
    })
}

fn has_cancel(d: &Decision) -> bool {
    d.effects.iter().any(|e| matches!(e, Effect::CancelAttempt))
}

fn has_remember(d: &Decision) -> bool {
    d.effects
        .iter()
        .any(|e| matches!(e, Effect::RememberWinner(_)))
}

fn has_demote(d: &Decision) -> bool {
    d.effects.iter().any(|e| matches!(e, Effect::DemoteIntent))
}

fn outcome(d: &Decision) -> Option<&CycleOutcome> {
    resolved(d).map(|(_, o)| o)
}

/// Every resolution carries the epoch it is for; the epoch is part of what the table promises.
fn resolved(d: &Decision) -> Option<(IntentEpoch, &CycleOutcome)> {
    d.effects.iter().find_map(|e| match e {
        Effect::Resolve { epoch, outcome } => Some((*epoch, outcome)),
        _ => None,
    })
}

// ------------------------------------------------------------------------------------------ Idle

#[test]
fn idle_and_down_stays_idle() {
    for world in [World::Clear, World::Dark] {
        let d = go(&Status::Idle, &down(0), &world, t0());
        assert!(matches!(d.next, Status::Idle));
        assert!(d.effects.is_empty());
    }
}

#[test]
fn a_tunnel_nobody_wants_is_torn_down() {
    let d = go(&Status::Idle, &down(0), &running(AWG), t0());
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::ForeignTunnel,
            ..
        }
    ));
    assert!(has_stop_foreign(&d));
}

#[test]
fn up_from_idle_starts_the_first_protocol_in_the_order() {
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG, WG], params()),
        &World::Clear,
        t0(),
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, AWG));
}

#[test]
fn darkness_does_not_block_a_connect_from_idle() {
    // There is nothing to protect when idle, so a non-authoritative observation must not wedge us.
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        t0(),
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, AWG));
}

#[test]
fn only_the_bootstrap_intent_adopts_a_running_tunnel() {
    // params: None is the bootstrap intent — it asks for "whatever is there".
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], None),
        &running(AWG),
        t0(),
    );
    match &d.next {
        Status::Up(u) => {
            assert!(u.adopted);
            assert_eq!(u.protocol, AWG);
            assert!(u.resolved);
        }
        other => panic!("expected adoption, got {other:?}"),
    }
    assert!(has_remember(&d));
    assert!(matches!(
        resolved(&d),
        Some((
            IntentEpoch(1),
            CycleOutcome::Connected { adopted: true, .. }
        ))
    ));
}

#[test]
fn a_caller_who_specified_split_rules_never_adopts_an_unknown_tunnel() {
    // Keeping a tunnel built with unknown split rules would be a data-leak-shaped bug.
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], params()),
        &running(AWG),
        t0(),
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::WrongProtocol,
            ..
        }
    ));
    assert!(has_stop_foreign(&d));
}

#[test]
fn the_startup_intent_never_starts_a_tunnel_by_itself() {
    // Caught on a device: startup used an ordinary Up intent, so the table did what it was asked
    // and connected — the app dialled out on every launch. An intent with no parameters cannot
    // build a tunnel, because it does not know its split rules; adoption is all it can do.
    for world in [World::Clear, World::Dark] {
        let d = go(&Status::Idle, &up_intent(1, &[AWG], None), &world, t0());
        assert!(
            matches!(d.next, Status::Idle),
            "startup must stay idle when there is nothing to adopt ({world:?})"
        );
        assert!(d.effects.is_empty());
    }
}

#[test]
fn a_request_that_carries_parameters_still_starts_normally() {
    for world in [World::Clear, World::Dark] {
        let d = go(&Status::Idle, &up_intent(1, &[AWG], params()), &world, t0());
        assert!(matches!(d.next, Status::Connecting { .. }), "{world:?}");
        assert!(has_begin(&d, AWG));
    }
}

#[test]
fn a_tunnel_of_an_unwanted_protocol_is_replaced_not_adopted() {
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], None),
        &running(VLESS),
        t0(),
    );
    assert!(matches!(d.next, Status::Unwinding { .. }));
}

// ------------------------------------------------------------------------------------ Connecting

fn connecting_status(now: Instant, order: &[Protocol]) -> Status {
    Status::Connecting {
        cycle: cycle(1, order, 1),
        phase: AttemptPhase::Preparing,
        deadline: now + Duration::from_secs(25),
    }
}

#[test]
fn no_observation_can_tear_down_an_in_flight_attempt() {
    // The historical failure: a saturated unreachable-poll counter made the first unreachable poll
    // during verification reset the connection state, under a connect that was about to succeed.
    let now = t0();
    let status = connecting_status(now, &[AWG]);
    for world in [World::Clear, World::Dark, running(VLESS)] {
        let d = go(&status, &up_intent(1, &[AWG], params()), &world, now);
        assert!(
            matches!(d.next, Status::Connecting { .. }),
            "{world:?} must not interrupt an attempt"
        );
        assert!(d.effects.is_empty());
    }
}

#[test]
fn an_attempt_is_cancelled_but_never_dropped_when_the_intent_goes_down() {
    let now = t0();
    let d = go(
        &connecting_status(now, &[AWG]),
        &down(2),
        &World::Clear,
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::IntentDown,
            ..
        }
    ));
    assert!(has_cancel(&d), "the token is fired");
    assert!(
        !has_unwind(&d),
        "the attempt unwinds its own ladder; the actor must not also unwind"
    );
}

#[test]
fn a_newer_intent_cancels_the_in_flight_attempt() {
    let now = t0();
    let d = go(
        &connecting_status(now, &[AWG]),
        &up_intent(2, &[WG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::IntentChanged,
            ..
        }
    ));
    assert!(has_cancel(&d));
}

#[test]
fn an_attempt_that_overruns_its_budget_is_cancelled_and_recorded() {
    let now = t0();
    let status = connecting_status(now, &[AWG]);
    let later = now + Duration::from_secs(26);
    let d = go(
        &status,
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        later,
    );

    match &d.next {
        Status::Unwinding {
            reason: UnwindReason::AttemptTimedOut,
            cycle: Some(c),
            ..
        } => {
            assert_eq!(c.failures.len(), 1);
            assert_eq!(c.failures[0].error, AttemptError::TimedOut);
        }
        other => panic!("expected a recorded timeout, got {other:?}"),
    }
    assert!(has_cancel(&d));
}

// -------------------------------------------------------------------------------------------- Up

#[test]
fn a_confirmed_running_tunnel_clears_the_darkness_clock() {
    let now = t0();
    let mut u = up_status(1, AWG, params());
    u.dark_since = Some(now);
    let d = go(
        &Status::Up(u),
        &up_intent(1, &[AWG], params()),
        &running(AWG),
        now,
    );
    match &d.next {
        Status::Up(next) => assert!(next.dark_since.is_none()),
        other => panic!("expected Up, got {other:?}"),
    }
}

#[test]
fn a_confirmed_stop_is_believed_immediately_on_every_platform() {
    // Clear means the peer answered "not running". That is authoritative, unlike darkness.
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::TunnelDied,
            ..
        }
    ));
}

#[test]
fn darkness_starts_a_clock_rather_than_declaring_the_tunnel_dead() {
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        now,
    );
    match &d.next {
        Status::Up(u) => assert_eq!(u.dark_since, Some(now)),
        other => panic!("expected the clock to arm, got {other:?}"),
    }
}

#[test]
fn darkness_within_the_grace_period_changes_nothing() {
    let now = t0();
    let mut u = up_status(1, AWG, params());
    u.dark_since = Some(now);
    let d = go(
        &Status::Up(u),
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        now + Duration::from_secs(5),
    );
    assert!(matches!(d.next, Status::Up(_)));
    assert!(d.effects.is_empty());
}

#[test]
fn darkness_past_the_grace_period_declares_the_peer_lost() {
    let now = t0();
    let mut u = up_status(1, AWG, params());
    u.dark_since = Some(now);
    let d = go(
        &Status::Up(u),
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        now + Duration::from_secs(7),
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::PeerLost,
            ..
        }
    ));
}

#[test]
fn the_darkness_grace_is_a_clock_not_a_poll_count() {
    // The historical failure: the debounce was counted in polls, so running two pollers made it
    // expire three times faster. Here the same elapsed time gives the same answer regardless of
    // how many times we ask.
    let now = t0();
    let mut u = up_status(1, AWG, params());
    u.dark_since = Some(now);
    let status = Status::Up(u);
    let intent = up_intent(1, &[AWG], params());

    for _ in 0..50 {
        let d = go(&status, &intent, &World::Dark, now + Duration::from_secs(1));
        assert!(
            matches!(d.next, Status::Up(_)),
            "asking more often must not shorten the grace"
        );
    }
}

#[test]
fn a_tunnel_running_the_wrong_protocol_is_reclaimed() {
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &running(VLESS),
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::Usurped,
            ..
        }
    ));
    assert!(has_stop_foreign(&d));
}

#[test]
fn pressing_connect_while_already_connected_is_a_hand_over_not_a_reconnect() {
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(2, &[AWG], params()),
        &running(AWG),
        now,
    );
    match &d.next {
        Status::Up(u) => {
            assert_eq!(u.epoch, IntentEpoch(2), "the new epoch takes the tunnel");
            assert!(u.resolved);
        }
        other => panic!("expected a hand-over, got {other:?}"),
    }
    assert!(!has_unwind(&d), "nothing is torn down");
    assert!(
        matches!(
            resolved(&d),
            Some((IntentEpoch(2), CycleOutcome::Connected { .. }))
        ),
        "the waiter released is the new epoch's, not the old one's"
    );
}

#[test]
fn a_hand_over_on_a_dark_observation_does_not_announce_success() {
    // Darkness is never authoritative enough to publish "connected" from.
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(2, &[AWG], params()),
        &World::Dark,
        now,
    );
    match &d.next {
        Status::Up(u) => {
            assert_eq!(u.epoch, IntentEpoch(2));
            assert!(!u.resolved, "the waiter holds until something confirms");
        }
        other => panic!("expected an unresolved hand-over, got {other:?}"),
    }
    assert!(outcome(&d).is_none());
}

#[test]
fn an_unresolved_hand_over_resolves_once_the_tunnel_is_confirmed() {
    let now = t0();
    let mut u = up_status(2, AWG, params());
    u.resolved = false;
    let d = go(
        &Status::Up(u),
        &up_intent(2, &[AWG], params()),
        &running(AWG),
        now,
    );
    assert!(matches!(
        outcome(&d),
        Some(CycleOutcome::Connected { adopted: false, .. })
    ));
}

#[test]
fn changing_the_split_rules_forces_a_real_teardown() {
    let now = t0();
    let want = Some(TunnelParams::new(SplitMode::Include, vec!["x".into()]));
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(2, &[AWG], want),
        &running(AWG),
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::IntentChanged,
            ..
        }
    ));
}

#[test]
fn a_dying_tunnel_gets_the_reconnect_budget_not_the_cold_one() {
    // This single distinction is what keeps a user-initiated connect failing fast while a tunnel
    // that dropped keeps trying.
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    match &d.next {
        Status::Unwinding { cycle: Some(c), .. } => {
            assert_eq!(c.passes_allowed, policy().reconnect_passes)
        }
        other => panic!("expected a reconnect cycle, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------- Unwinding

#[test]
fn unwinding_absorbs_every_input() {
    // This is what makes "a late teardown runs against a newer connection" unwritable.
    let now = t0();
    let status = Status::Unwinding {
        cycle: Some(cycle(1, &[AWG], 1)),
        reason: UnwindReason::IntentDown,
        tries: 0,
    };
    for intent in [
        down(5),
        up_intent(1, &[AWG], params()),
        up_intent(9, &[WG], params()),
    ] {
        for world in [World::Clear, World::Dark, running(AWG)] {
            let d = go(&status, &intent, &world, now);
            assert!(
                matches!(d.next, Status::Unwinding { .. }),
                "unwinding must absorb ({intent:?}, {world:?})"
            );
            assert!(d.effects.is_empty());
        }
    }
}

// -------------------------------------------------------------------------------------- Retrying

fn retrying(now: Instant, order: &[Protocol]) -> Status {
    Status::Retrying {
        cycle: cycle(1, order, 3),
        resume_at: now + Duration::from_secs(2),
    }
}

#[test]
fn a_retry_waits_until_its_backoff_elapses() {
    let now = t0();
    let d = go(
        &retrying(now, &[AWG]),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Retrying { .. }));

    let d = go(
        &retrying(now, &[AWG]),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now + Duration::from_secs(3),
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, AWG));
}

#[test]
fn a_tunnel_that_comes_back_on_its_own_ends_the_retry() {
    let now = t0();
    let d = go(
        &retrying(now, &[AWG]),
        &up_intent(1, &[AWG], params()),
        &running(AWG),
        now,
    );
    assert!(matches!(d.next, Status::Up(_)));
    assert!(has_remember(&d));
}

#[test]
fn going_down_during_a_retry_is_immediate() {
    let now = t0();
    let d = go(&retrying(now, &[AWG]), &down(2), &World::Clear, now);
    assert!(matches!(d.next, Status::Idle));
    assert!(
        matches!(
            resolved(&d),
            Some((IntentEpoch(1), CycleOutcome::Cancelled))
        ),
        "it is the waiting cycle that is cancelled; the Down is resolved by the actor once idle"
    );
}

// ------------------------------------------------------------------------------ attempt outcomes

fn attempt_done(status: &Status, intent: &Intent, result: AttemptResult, now: Instant) -> Decision {
    on_attempt_done(status, intent, result, now, &policy())
}

fn established(protocol: Protocol) -> AttemptResult {
    AttemptResult::Established {
        view: up_status(1, protocol, params()),
        stack: RollbackStack::default(),
    }
}

#[test]
fn a_successful_attempt_becomes_up_and_hands_over_its_stack() {
    let now = t0();
    let d = attempt_done(
        &connecting_status(now, &[AWG]),
        &up_intent(1, &[AWG], params()),
        established(AWG),
        now,
    );
    assert!(matches!(d.next, Status::Up(_)));
    assert!(
        d.effects.iter().any(|e| matches!(e, Effect::TakeStack(_))),
        "the actor must own what the attempt applied"
    );
    assert!(has_remember(&d));
    assert!(matches!(
        resolved(&d),
        Some((
            IntentEpoch(1),
            CycleOutcome::Connected { adopted: false, .. }
        ))
    ));
}

#[test]
fn an_attempt_that_succeeds_after_the_user_gave_up_is_torn_down_with_its_own_stack() {
    // A late-succeeding connect can never publish "connected" after a teardown, because status is
    // assigned only from the table and never by the attempt.
    let now = t0();
    let d = attempt_done(
        &connecting_status(now, &[AWG]),
        &down(2),
        established(AWG),
        now,
    );
    assert!(matches!(d.next, Status::Unwinding { .. }));
    assert!(d.effects.iter().any(|e| matches!(e, Effect::TakeStack(_))));
    assert!(has_unwind(&d));
}

#[test]
fn a_failure_moves_to_the_next_protocol_in_the_order() {
    let now = t0();
    let d = attempt_done(
        &connecting_status(now, &[AWG, WG, VLESS]),
        &up_intent(1, &[AWG, WG, VLESS], params()),
        AttemptResult::Failed(AttemptError::VerifyFailed),
        now,
    );
    assert!(has_begin(&d, WG));
    match &d.next {
        Status::Connecting { cycle, .. } => {
            assert_eq!(cycle.failures.len(), 1);
            assert_eq!(cycle.failures[0].protocol, AWG);
        }
        other => panic!("expected the next probe, got {other:?}"),
    }
}

#[test]
fn a_denied_consent_dialog_stops_the_whole_cycle() {
    // Otherwise three protocols means three dialogs, and a reconnect budget means up to nine.
    let now = t0();
    let d = attempt_done(
        &connecting_status(now, &[AWG, WG, VLESS]),
        &up_intent(1, &[AWG, WG, VLESS], params()),
        AttemptResult::Failed(AttemptError::PermissionDenied),
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(
        has_demote(&d),
        "an Up intent must mean we are still working"
    );
    assert!(matches!(outcome(&d), Some(CycleOutcome::Exhausted { .. })));
}

#[test]
fn exhausting_a_cold_cycle_reports_every_failure_not_just_the_last() {
    // This is what lets the caller re-provision the peer of the protocol that actually failed
    // verification, instead of assuming it was whichever one happened to be tried last.
    let now = t0();
    let mut c = cycle(1, &[AWG, WG], 1);
    c.index = 1;
    c.failures.push(AttemptFailure {
        protocol: AWG,
        error: AttemptError::VerifyFailed,
        pass: 0,
    });
    let status = Status::Connecting {
        cycle: c,
        phase: AttemptPhase::Verifying,
        deadline: now + Duration::from_secs(25),
    };

    let d = attempt_done(
        &status,
        &up_intent(1, &[AWG, WG], params()),
        AttemptResult::Failed(AttemptError::VerifyFailed),
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    match outcome(&d) {
        Some(CycleOutcome::Exhausted { failures }) => {
            assert_eq!(failures.len(), 2);
            assert_eq!(failures[0].protocol, AWG);
            assert_eq!(failures[1].protocol, WG);
        }
        other => panic!("expected every failure, got {other:?}"),
    }
}

#[test]
fn the_last_probe_of_a_budgeted_cycle_schedules_another_pass() {
    let now = t0();
    let status = Status::Connecting {
        cycle: cycle(1, &[AWG], 3),
        phase: AttemptPhase::Verifying,
        deadline: now + Duration::from_secs(25),
    };
    let d = attempt_done(
        &status,
        &up_intent(1, &[AWG], params()),
        AttemptResult::Failed(AttemptError::VerifyFailed),
        now,
    );
    match &d.next {
        Status::Retrying { cycle, .. } => assert_eq!(cycle.pass, 1),
        other => panic!("expected a retry, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------- unwind outcomes

fn clean() -> UnwindReport {
    UnwindReport {
        stack_empty: true,
        residual: Vec::new(),
    }
}

fn unwind_done(status: &Status, intent: &Intent, world: &World, now: Instant) -> Decision {
    on_unwind_done(status, intent, &clean(), world, now, &policy())
}

fn unwinding_status(reason: UnwindReason, cycle: Option<Cycle>, tries: u32) -> Status {
    Status::Unwinding {
        cycle,
        reason,
        tries,
    }
}

#[test]
fn an_unwind_that_did_not_actually_stop_the_tunnel_is_retried() {
    // The report said Ok, but the world says a tunnel is still running. Starting a fresh attempt
    // here would stack one tunnel on top of another.
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(cycle(1, &[AWG], 3)), 0),
        &up_intent(1, &[AWG], params()),
        &running(AWG),
        now,
    );
    match &d.next {
        Status::Unwinding { tries, .. } => assert_eq!(*tries, 1),
        other => panic!("expected a re-unwind, got {other:?}"),
    }
    assert!(has_stop_foreign(&d));
}

#[test]
fn an_unwind_that_never_succeeds_gives_up_loudly_rather_than_looping() {
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(cycle(1, &[AWG], 3)), 2),
        &up_intent(1, &[AWG], params()),
        &running(AWG),
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(has_demote(&d));
    assert!(matches!(outcome(&d), Some(CycleOutcome::UnwindFailed)));
}

#[test]
fn a_teardown_judged_against_a_pre_teardown_look_must_not_burn_its_retries() {
    // Caught on a device. The actor was checking the world using the observation it already had,
    // which had been taken *before* the unwind ran and still said Running. Retries happen in
    // microseconds and polling is once a second, so all three re-runs consulted the same stale
    // answer and the budget was spent in under a millisecond — every single time.
    //
    // The actor now passes Dark in that situation, and this is what Dark must do here: fall
    // through and let a fresh observation decide, rather than re-unwinding.
    let now = t0();
    for tries in 0..policy().unwind_tries {
        let d = unwind_done(
            &unwinding_status(UnwindReason::IntentDown, None, tries),
            &down(2),
            &World::Dark,
            now,
        );
        assert!(
            !matches!(d.next, Status::Unwinding { .. }),
            "a stale look must not drive a re-unwind (tries={tries})"
        );
        assert!(!matches!(outcome(&d), Some(CycleOutcome::UnwindFailed)));
    }
}

#[test]
fn darkness_never_triggers_a_re_unwind() {
    // Re-unwinding forever against an unreachable peer is exactly the livelock to avoid.
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(cycle(1, &[AWG], 3)), 0),
        &up_intent(1, &[AWG], params()),
        &World::Dark,
        now,
    );
    assert!(!matches!(d.next, Status::Unwinding { .. }));
}

#[test]
fn a_completed_teardown_for_an_explicit_down_reports_down() {
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::IntentDown, None, 0),
        &down(2),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(
        matches!(resolved(&d), Some((IntentEpoch(2), CycleOutcome::Down))),
        "the Down epoch is the one that reached Down"
    );
}

#[test]
fn a_lost_tunnel_with_budget_left_retries_rather_than_giving_up() {
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(cycle(1, &[AWG], 3)), 0),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Retrying { .. }));
}

#[test]
fn a_lost_tunnel_out_of_budget_reports_that_it_gave_up() {
    let now = t0();
    let mut c = cycle(1, &[AWG], 3);
    c.pass = 2;
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(c), 0),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(matches!(outcome(&d), Some(CycleOutcome::LostGaveUp { .. })));
}

#[test]
fn clearing_an_obstruction_proceeds_with_what_was_wanted_all_along() {
    let now = t0();
    for reason in [
        UnwindReason::WrongProtocol,
        UnwindReason::ForeignTunnel,
        UnwindReason::CrashRecovery,
    ] {
        let d = unwind_done(
            &unwinding_status(reason, Some(cycle(1, &[AWG], 1)), 0),
            &up_intent(1, &[AWG], params()),
            &World::Clear,
            now,
        );
        assert!(
            matches!(d.next, Status::Connecting { .. }),
            "{reason:?} should proceed to connect"
        );
        assert!(has_begin(&d, AWG));
    }
}

#[test]
fn a_newer_intent_arriving_during_a_teardown_is_honoured_the_moment_it_completes() {
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::IntentDown, Some(cycle(1, &[AWG], 1)), 0),
        &up_intent(7, &[WG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, WG));
    assert!(
        matches!(
            resolved(&d),
            Some((IntentEpoch(1), CycleOutcome::Cancelled))
        ),
        "the superseded epoch's waiter is released, not the new one's"
    );
}
