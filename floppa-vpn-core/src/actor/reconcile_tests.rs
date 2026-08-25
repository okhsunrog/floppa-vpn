//! The decision table, row by row.
//!
//! Every test names the property rather than the row number, so a failure says what broke rather
//! than which cell moved. Where a row exists to kill a specific historical defect, the test says
//! so — those are the ones that must never be "fixed" by relaxing them.

use super::*;
use crate::actor::types::*;
use crate::protocol::Protocol;
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
        forget: false,
    }
}

/// The Down a wipe issues: it must leave nothing running, whoever started it.
fn forget(epoch: u64) -> Intent {
    Intent::Down {
        epoch: IntentEpoch(epoch),
        forget: true,
    }
}

fn params() -> Option<TunnelParams> {
    Some(TunnelParams::new(SplitMode::All, vec![]))
}

fn running(protocol: Protocol) -> World {
    World::Running(RunningTunnel {
        protocol,
        generation: None,
        endpoint: "vpn.example:51820".into(),
        address: "10.0.0.2/32".into(),
        connected_secs: Some(30),
        params: None,
        autonomous: false,
        // Answering. Silence is a separate axis, exercised by its own tests.
        silent_secs: Some(0),
    })
}

/// A tunnel whose owner reports the rules it was built with, as the Android service does.
fn running_with(protocol: Protocol, params: TunnelParams) -> World {
    match running(protocol) {
        World::Running(rt) => World::Running(RunningTunnel {
            params: Some(params),
            ..rt
        }),
        other => other,
    }
}

/// A tunnel the Android service brought up by itself from the autostart bundle: it reports the
/// rules it was built with and a generation no UI process can have minted.
fn autonomous(protocol: Protocol, params: TunnelParams) -> World {
    World::Running(RunningTunnel {
        protocol,
        generation: Some(7),
        endpoint: "203.0.113.7:51820".into(),
        address: "10.0.0.2/32".into(),
        connected_secs: Some(30),
        params: Some(params),
        autonomous: true,
        silent_secs: Some(0),
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
        probing_since: None,
        resolved: true,
    }
}

/// A cycle born from a tunnel that died: the reconnect budget, and an ending that says so.
fn reconnect_cycle(epoch: u64, order: &[Protocol], policy: &Policy) -> Cycle {
    Cycle {
        born_from_loss: true,
        passes_allowed: policy.reconnect_passes,
        ..cycle(epoch, order, policy.reconnect_passes)
    }
}

fn cycle(epoch: u64, order: &[Protocol], passes_allowed: u32) -> Cycle {
    Cycle {
        epoch: IntentEpoch(epoch),
        order: order.to_vec(),
        params: TunnelParams::default(),
        index: 0,
        pass: 0,
        passes_allowed,
        born_from_loss: false,
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

fn begin_params(d: &Decision) -> Option<&TunnelParams> {
    d.effects.iter().find_map(|e| match e {
        Effect::Begin { params, .. } => Some(params),
        _ => None,
    })
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
fn a_tunnel_the_system_started_by_itself_is_adopted_rather_than_killed() {
    // Whether always-on restarts the service is the system toggle's decision, not ours. Killing
    // the tunnel it brought back put the app in a fight with the OS — restart, kill, restart —
    // with the UI claiming Disconnected throughout. Adopting it shows what is true, and leaves
    // stopping it as an explicit act of the user's.
    let rules = TunnelParams::new(SplitMode::Exclude, vec!["org.example".into()]);
    let d = go(
        &Status::Idle,
        &down(7),
        &autonomous(AWG, rules.clone()),
        t0(),
    );
    assert!(matches!(d.next, Status::Idle), "nothing is torn down");
    assert!(!has_stop_foreign(&d));
    match d.effects.as_slice() {
        [Effect::AdoptAutonomous { protocol, params }] => {
            assert_eq!(*protocol, AWG);
            assert_eq!(params.as_ref(), Some(&rules), "with the rules it reports");
        }
        other => panic!("expected the intent to be promoted, got {other:?}"),
    }
}

#[test]
fn a_wipe_stops_even_a_tunnel_the_system_started() {
    // Forgetting the account is the one Down that has to leave nothing running, whoever started
    // it: an always-on tunnel surviving a logout is the previous account's tunnel.
    let d = go(
        &Status::Idle,
        &forget(7),
        &autonomous(AWG, TunnelParams::default()),
        t0(),
    );
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

// ------------------------------------------------------------------- autonomous (always-on) tunnels

#[test]
fn a_tunnel_the_service_started_by_itself_is_adopted_at_startup_with_its_rules() {
    // The system brought the service up for always-on VPN and it rebuilt the tunnel from the
    // bundle the last connect wrote. The UI process that opens later must take it over as Up,
    // with the protocol and the split rules the service reports — not tear it down as foreign.
    let rules = TunnelParams::new(SplitMode::Exclude, vec!["org.example".into()]);
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG, WG], None),
        &autonomous(AWG, rules.clone()),
        t0(),
    );
    match &d.next {
        Status::Up(u) => {
            assert!(u.adopted);
            assert_eq!(u.protocol, AWG);
            assert_eq!(
                u.params.as_ref(),
                Some(&rules),
                "the owner's rules are taken along"
            );
            assert_eq!(u.server_endpoint, "203.0.113.7:51820");
            assert!(u.resolved);
        }
        other => panic!("expected adoption, got {other:?}"),
    }
    assert!(has_remember(&d));
    assert!(!has_unwind(&d));
    assert!(matches!(
        resolved(&d),
        Some((
            IntentEpoch(1),
            CycleOutcome::Connected {
                protocol: AWG,
                adopted: true,
                ..
            }
        ))
    ));
}

#[test]
fn an_adopted_autonomous_tunnel_is_torn_down_by_the_users_disconnect() {
    // Adopted first, then Disconnect. Nothing on the held stack started this tunnel, so the
    // first unwind undoes nothing; the world still reporting Running is what makes the table
    // re-run it with a backend stop. Only a confirmed Clear ends in Idle with Down resolved.
    let rules = TunnelParams::default();
    let now = t0();
    let adopted = go(
        &Status::Idle,
        &up_intent(1, &[AWG], None),
        &autonomous(AWG, rules.clone()),
        now,
    )
    .next;
    assert!(matches!(&adopted, Status::Up(u) if u.adopted));

    let d = go(&adopted, &down(2), &autonomous(AWG, rules.clone()), now);
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::IntentDown,
            cycle: None,
            ..
        }
    ));
    assert!(has_unwind(&d));

    // The teardown reports done while the service still answers Running: not done.
    let d = unwind_done(&d.next, &down(2), &autonomous(AWG, rules), now);
    assert!(matches!(d.next, Status::Unwinding { tries: 1, .. }));
    assert!(has_stop_foreign(&d));

    let d = unwind_done(&d.next, &down(2), &World::Clear, now);
    assert!(matches!(d.next, Status::Idle));
    assert!(matches!(
        resolved(&d),
        Some((IntentEpoch(2), CycleOutcome::Down))
    ));
}

#[test]
fn switching_protocol_away_from_an_adopted_autonomous_tunnel_rebuilds() {
    // The always-on tunnel is AmneziaWG; the user picks VLESS. Not satisfied by what is running,
    // so it is torn down and the new order is started once the teardown is confirmed.
    let now = t0();
    let adopted = go(
        &Status::Idle,
        &up_intent(1, &[AWG, VLESS], None),
        &autonomous(AWG, TunnelParams::default()),
        now,
    )
    .next;

    let d = go(
        &adopted,
        &up_intent(2, &[VLESS], params()),
        &autonomous(AWG, TunnelParams::default()),
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::IntentChanged,
            cycle: Some(_),
            ..
        }
    ));
    assert!(has_unwind(&d));

    let d = unwind_done(
        &d.next,
        &up_intent(2, &[VLESS], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, VLESS));
}

#[test]
fn a_connect_with_the_rules_the_running_tunnel_reports_is_a_hand_over() {
    // The service says what it built; a caller asking for exactly that has nothing to rebuild.
    // This is row 5c, and it is the only way a caller-issued intent ever adopts from Idle.
    let rules = TunnelParams::new(SplitMode::Include, vec!["org.example".into()]);
    let d = go(
        &Status::Idle,
        &up_intent(3, &[AWG], Some(rules.clone())),
        &autonomous(AWG, rules.clone()),
        t0(),
    );
    match &d.next {
        Status::Up(u) => {
            assert!(u.adopted);
            assert_eq!(u.params.as_ref(), Some(&rules));
        }
        other => panic!("expected a hand-over, got {other:?}"),
    }
    assert!(matches!(
        resolved(&d),
        Some((
            IntentEpoch(3),
            CycleOutcome::Connected { adopted: true, .. }
        ))
    ));
}

#[test]
fn a_connect_with_other_rules_than_the_running_tunnel_reports_rebuilds() {
    // Known to differ is as good a reason as unknown: keeping it would route the wrong apps.
    let d = go(
        &Status::Idle,
        &up_intent(
            3,
            &[AWG],
            Some(TunnelParams::new(SplitMode::Exclude, vec!["a".into()])),
        ),
        &autonomous(AWG, TunnelParams::default()),
        t0(),
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::WrongProtocol,
            cycle: Some(_),
            ..
        }
    ));
    assert!(has_stop_foreign(&d));
}

#[test]
fn an_autonomous_tunnel_of_a_protocol_with_no_config_is_replaced_not_adopted() {
    // The bootstrap order is built from the stored configs. A service tunnel of a protocol the
    // user has since removed the config for is not one this intent accepts.
    let d = go(
        &Status::Idle,
        &up_intent(1, &[WG], None),
        &autonomous(VLESS, TunnelParams::default()),
        t0(),
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            reason: UnwindReason::WrongProtocol,
            cycle: None,
            ..
        }
    ));
    assert!(has_stop_foreign(&d));
}

#[test]
fn with_no_bundle_the_service_starts_nothing_and_startup_rests_idle() {
    // The `:vpn` side of "bundle missing" is Kotlin stopping the service; from here it is the
    // same world as any other launch with nothing running, and the startup intent must not
    // dial out by itself.
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], None),
        &World::Clear,
        t0(),
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(d.effects.is_empty());
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

// ------------------------------------------------------------------------- a silent peer (17a-c)

/// A running tunnel whose far side has been quiet for `secs`.
fn quiet(protocol: Protocol, secs: i64) -> World {
    match running(protocol) {
        World::Running(rt) => World::Running(RunningTunnel {
            silent_secs: Some(secs),
            ..rt
        }),
        other => other,
    }
}

fn has_probe(d: &Decision) -> bool {
    d.effects.iter().any(|e| matches!(e, Effect::Probe))
}

#[test]
fn a_tunnel_whose_peer_stops_answering_is_probed_before_it_is_judged() {
    // The defect this exists for: a peer deleted on the server leaves the device running and the
    // observation saying Running, so the status stayed Up — the app said "connected" for as long
    // as anyone left it, with no traffic passing.
    //
    // But silence is not proof. A phone that spent an hour asleep looks exactly like this, and so
    // does a config with no keepalive that nobody has used. So the first thing silence buys is a
    // question, not a verdict.
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &quiet(AWG, policy().silent_after.as_secs() as i64 + 1),
        now,
    );
    match &d.next {
        Status::Up(u) => assert_eq!(u.probing_since, Some(now), "the clock starts here"),
        other => panic!("expected to stay Up while probing, got {other:?}"),
    }
    assert!(has_probe(&d));
    assert!(!has_unwind(&d), "nothing is torn down on suspicion alone");
}

#[test]
fn a_probe_that_is_still_out_changes_nothing() {
    let now = t0();
    let mut status = up_status(1, AWG, params());
    status.probing_since = Some(now);
    let d = go(
        &Status::Up(status),
        &up_intent(1, &[AWG], params()),
        &quiet(AWG, 600),
        now + policy().probe_grace,
    );
    match &d.next {
        Status::Up(u) => assert_eq!(u.probing_since, Some(now), "the clock is not restarted"),
        other => panic!("expected to stay Up, got {other:?}"),
    }
    // One question per silence: re-asking every second would be a rehandshake per second.
    assert!(!has_probe(&d));
}

#[test]
fn a_peer_that_stays_silent_after_being_asked_is_lost_and_reconnected() {
    let now = t0();
    let mut status = up_status(1, AWG, params());
    status.probing_since = Some(now);
    let d = go(
        &Status::Up(status),
        &up_intent(1, &[AWG], params()),
        &quiet(AWG, 600),
        now + policy().probe_grace + Duration::from_secs(1),
    );
    match &d.next {
        // A reconnect, not a stop: the ladder may still find another protocol that works, which
        // is the whole point of ending here rather than in Idle.
        Status::Unwinding {
            reason: UnwindReason::PeerSilent,
            cycle: Some(c),
            ..
        } => assert_eq!(c.passes_allowed, policy().reconnect_passes),
        other => panic!("expected a reconnect after the probe went unanswered, got {other:?}"),
    }
    assert!(has_unwind(&d));
}

#[test]
fn a_peer_that_answers_the_probe_clears_the_suspicion() {
    let now = t0();
    let mut status = up_status(1, AWG, params());
    status.probing_since = Some(now);
    let d = go(
        &Status::Up(status),
        &up_intent(1, &[AWG], params()),
        // The rehandshake landed: the owner now reports a fresh one.
        &quiet(AWG, 0),
        now + policy().probe_grace + Duration::from_secs(1),
    );
    match &d.next {
        Status::Up(u) => assert_eq!(
            u.probing_since, None,
            "the clock is cleared, not left armed"
        ),
        other => panic!("expected to stay Up, got {other:?}"),
    }
}

#[test]
fn an_owner_that_cannot_say_how_quiet_it_is_never_costs_the_tunnel() {
    // `None` is "I have no such signal", which every desktop tunnel of an older build and every
    // protocol without a handshake reports. Reading it as silence would tear down healthy tunnels
    // on a missing field.
    let now = t0();
    let world = match running(AWG) {
        World::Running(rt) => World::Running(RunningTunnel {
            silent_secs: None,
            ..rt
        }),
        other => other,
    };
    let d = go(
        &Status::Up(up_status(1, AWG, params())),
        &up_intent(1, &[AWG], params()),
        &world,
        now,
    );
    assert!(matches!(&d.next, Status::Up(u) if u.probing_since.is_none()));
    assert!(!has_probe(&d));
    assert!(!has_unwind(&d));
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
    // Adopted on the same terms as row 5c: the protocol must be one we want, and the rules must
    // be the ones we asked for.
    let now = t0();
    let d = go(
        &retrying(now, &[AWG]),
        &up_intent(1, &[AWG], params()),
        &running_with(AWG, TunnelParams::default()),
        now,
    );
    match &d.next {
        Status::Up(u) => assert_eq!(u.params.as_ref(), Some(&TunnelParams::default())),
        other => panic!("expected adoption, got {other:?}"),
    }
    assert!(has_remember(&d));
}

#[test]
fn a_tunnel_whose_rules_are_unknown_or_different_is_not_adopted_while_retrying() {
    // The asymmetry row 5c calls a data-leak-shaped bug: this row used to check only the
    // protocol, so a tunnel routing something else — or one whose owner says nothing about what
    // it routes — was kept, and row 17 then held it for as long as it lived. A service
    // generation is not an intent epoch, so there is no identity to recognise it by either.
    let now = t0();
    let intent = up_intent(1, &[AWG], params());
    let elsewhere = TunnelParams::new(SplitMode::Include, vec!["org.example".into()]);
    for world in [running(AWG), running_with(AWG, elsewhere)] {
        let d = go(&retrying(now, &[AWG]), &intent, &world, now);
        assert!(
            matches!(
                d.next,
                Status::Unwinding {
                    reason: UnwindReason::WrongProtocol,
                    ..
                }
            ),
            "expected a rebuild, got {:?}",
            d.next
        );
        assert!(has_stop_foreign(&d));
    }
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
fn a_cycle_that_connects_still_reports_what_it_stepped_over() {
    // The device case this comes from: AmneziaWG failed to verify because its peer had been
    // deleted on the server, WireGuard connected a second later, and the outcome said only
    // "connected" — so nothing ever repaired the dead AmneziaWG peer. It survived until the next
    // app start happened to re-provision it.
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
        established(WG),
        now,
    );
    match outcome(&d) {
        Some(CycleOutcome::Connected {
            protocol, failures, ..
        }) => {
            assert_eq!(*protocol, WG, "the one that carried it");
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].protocol, AWG, "the one that needs repairing");
        }
        other => panic!("expected a connected outcome, got {other:?}"),
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
fn a_crashed_attempt_is_cleaned_up_rather_than_trusted_to_have_unwound() {
    // Every other failure has already unwound its own ladder. A crash has not: the task died with
    // its stack, so what the journal recorded is undone and the backend stopped in case it got
    // that far.
    let now = t0();
    let d = attempt_done(
        &connecting_status(now, &[AWG, WG]),
        &up_intent(1, &[AWG, WG], params()),
        AttemptResult::Failed(AttemptError::Crashed {
            detail: "panicked".into(),
        }),
        now,
    );
    match &d.next {
        Status::Unwinding {
            reason: UnwindReason::AttemptCrashed,
            cycle: Some(c),
            ..
        } => assert!(matches!(c.failures[0].error, AttemptError::Crashed { .. })),
        other => panic!("expected a crash teardown, got {other:?}"),
    }
    assert!(has_stop_foreign(&d));
    assert!(
        !has_begin(&d, WG),
        "a crash is a bug, not a bad peer: the next protocol is not tried on a dirty machine"
    );
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
        finished_at: std::time::Instant::now(),
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
    let mut c = reconnect_cycle(1, &[AWG], &policy());
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
fn a_dropped_tunnel_that_never_comes_back_gives_up_at_the_default_policy() {
    // The whole reconnect life of a dropped tunnel at `reconnect_passes: 3`, driven through the
    // two tables. Before this, U7 burnt a pass for the death itself, so only two passes ever ran
    // — and every failure after the first went through A9, which said `Exhausted`. `LostGaveUp`
    // was unreachable at the shipped policy, and the frontend, which only reacts to
    // `LostGaveUp` for a cycle nobody awaited, left the user silently disconnected.
    let policy = policy();
    let mut now = t0();
    let intent = up_intent(1, &[AWG], params());

    // The tunnel is confirmed gone: the unwind that follows starts the reconnect cycle.
    let d = unwind_done(
        &unwinding_status(
            UnwindReason::TunnelDied,
            Some(reconnect_cycle(1, &[AWG], &policy)),
            0,
        ),
        &intent,
        &World::Clear,
        now,
    );
    let mut cycle = match d.next {
        Status::Retrying { cycle, resume_at } => {
            assert_eq!(cycle.pass, 0, "nothing has been tried yet");
            now = resume_at;
            cycle
        }
        other => panic!("expected a retry, got {other:?}"),
    };

    // Three passes actually run, and only then does it give up.
    for expected_pass in 0..policy.reconnect_passes {
        assert_eq!(cycle.pass, expected_pass);
        let status = Status::Connecting {
            cycle: cycle.clone(),
            phase: AttemptPhase::Verifying,
            deadline: now + Duration::from_secs(25),
        };
        let d = attempt_done(
            &status,
            &intent,
            AttemptResult::Failed(AttemptError::VerifyFailed),
            now,
        );
        match d.next {
            Status::Retrying {
                cycle: next,
                resume_at,
            } => {
                assert!(
                    expected_pass + 1 < policy.reconnect_passes,
                    "gave up a pass early"
                );
                now = resume_at;
                cycle = next;
            }
            Status::Idle => {
                assert_eq!(expected_pass + 1, policy.reconnect_passes);
                assert!(has_demote(&d));
                match outcome(&d) {
                    Some(CycleOutcome::LostGaveUp { protocol, passes }) => {
                        assert_eq!(*protocol, AWG);
                        assert_eq!(*passes, policy.reconnect_passes);
                    }
                    other => panic!("expected LostGaveUp, got {other:?}"),
                }
                return;
            }
            other => panic!("expected a retry or an ending, got {other:?}"),
        }
    }
    panic!("the cycle never ended");
}

#[test]
fn a_cold_connect_that_never_worked_is_exhausted_not_lost() {
    // The other half of the same rule: nothing was ever up, so there is nothing to have lost.
    let now = t0();
    let status = Status::Connecting {
        cycle: cycle(1, &[AWG], 1),
        phase: AttemptPhase::Verifying,
        deadline: now + Duration::from_secs(25),
    };
    let d = attempt_done(
        &status,
        &up_intent(1, &[AWG], params()),
        AttemptResult::Failed(AttemptError::VerifyFailed),
        now,
    );
    assert!(matches!(outcome(&d), Some(CycleOutcome::Exhausted { .. })));
}

#[test]
fn once_a_crash_is_cleaned_up_the_cycle_ends_with_the_crash_on_record() {
    let now = t0();
    let mut c = cycle(1, &[AWG], 1);
    c.failures.push(AttemptFailure {
        protocol: AWG,
        error: AttemptError::Crashed {
            detail: "panicked".into(),
        },
        pass: 0,
    });
    let d = unwind_done(
        &unwinding_status(UnwindReason::AttemptCrashed, Some(c), 0),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(has_demote(&d));
    match resolved(&d) {
        Some((IntentEpoch(1), CycleOutcome::Exhausted { failures })) => {
            assert!(matches!(failures[0].error, AttemptError::Crashed { .. }))
        }
        other => panic!("expected the crash reported, got {other:?}"),
    }
}

#[test]
fn clearing_an_obstruction_proceeds_with_what_was_wanted_all_along() {
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::WrongProtocol, Some(cycle(1, &[AWG], 1)), 0),
        &up_intent(1, &[AWG], params()),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Connecting { .. }));
    assert!(has_begin(&d, AWG));
}

#[test]
fn a_foreign_tunnel_torn_down_for_an_intent_that_cannot_build_one_ends_in_idle() {
    // The startup intent finds a tunnel of a protocol it does not want: the obstruction is
    // removed, and then there is nothing it can build — it knows no split rules — so it rests.
    // Previously this rebuilt the tunnel with default split rules, which is the app connecting
    // by itself.
    let now = t0();
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], None),
        &running(VLESS),
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            cycle: None,
            reason: UnwindReason::WrongProtocol,
            ..
        }
    ));
    let d = unwind_done(&d.next, &up_intent(1, &[AWG], None), &World::Clear, now);
    assert!(matches!(d.next, Status::Idle), "nothing to build from");
    assert!(d.effects.is_empty());
}

#[test]
fn an_adopted_tunnel_that_dies_is_not_rebuilt_by_the_startup_intent() {
    let now = t0();
    let d = go(
        &Status::Up(up_status(1, AWG, None)),
        &up_intent(1, &[AWG], None),
        &World::Clear,
        now,
    );
    assert!(matches!(
        d.next,
        Status::Unwinding {
            cycle: None,
            reason: UnwindReason::TunnelDied,
            ..
        }
    ));
    let d = unwind_done(&d.next, &up_intent(1, &[AWG], None), &World::Clear, now);
    assert!(matches!(d.next, Status::Idle));
}

#[test]
fn every_attempt_is_started_with_the_cycles_own_params() {
    // The params travel with the Begin effect: by the time a retry starts, the intent that
    // supplied them may already be a different one.
    let now = t0();
    let want = TunnelParams::new(SplitMode::Exclude, vec!["x".into()]);
    let d = go(
        &Status::Idle,
        &up_intent(1, &[AWG], Some(want.clone())),
        &World::Clear,
        now,
    );
    assert_eq!(begin_params(&d), Some(&want));
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

#[test]
fn an_expired_deadline_walks_the_order_then_the_passes_then_gives_up() {
    // U4, U5 and U6: the deadline's own ending, which the table tests never reached — every
    // AttemptTimedOut test stopped at the entry to Unwinding. A timeout advances the cycle
    // exactly as a reported failure does, and ends it the same way.
    let now = t0();
    let intent = up_intent(1, &[AWG, WG], params());

    // U4: not the last protocol in the order — try the next one.
    let mut first = cycle(1, &[AWG, WG], 2);
    let d = unwind_done(
        &unwinding_status(UnwindReason::AttemptTimedOut, Some(first.clone()), 0),
        &intent,
        &World::Clear,
        now,
    );
    assert!(has_begin(&d, WG), "expected the next protocol, got {d:?}");

    // U5: the last protocol, with a pass left — back off and start the order again.
    first.index = 1;
    let d = unwind_done(
        &unwinding_status(UnwindReason::AttemptTimedOut, Some(first.clone()), 0),
        &intent,
        &World::Clear,
        now,
    );
    match &d.next {
        Status::Retrying { cycle, .. } => assert_eq!((cycle.index, cycle.pass), (0, 1)),
        other => panic!("expected a retry, got {other:?}"),
    }

    // U6: the last protocol of the last pass — the cycle is over.
    first.pass = 1;
    let d = unwind_done(
        &unwinding_status(UnwindReason::AttemptTimedOut, Some(first), 0),
        &intent,
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    assert!(has_demote(&d));
    assert!(matches!(outcome(&d), Some(CycleOutcome::Exhausted { .. })));
}

#[test]
fn a_down_that_lands_during_someone_elses_teardown_still_reports_down() {
    // U1 used to report Cancelled unless the teardown's own reason was IntentDown, so a
    // Disconnect that arrived while a dead tunnel was being torn down was told its request had
    // been superseded — by itself. Cancelled belongs to the cycle the Down displaced, and it is
    // resolved separately.
    let now = t0();
    let d = unwind_done(
        &unwinding_status(UnwindReason::TunnelDied, Some(cycle(1, &[AWG], 3)), 0),
        &down(2),
        &World::Clear,
        now,
    );
    assert!(matches!(d.next, Status::Idle));
    let resolutions: Vec<_> = d
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Resolve { epoch, outcome } => Some((*epoch, outcome.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        resolutions,
        vec![
            (IntentEpoch(2), CycleOutcome::Down),
            (IntentEpoch(1), CycleOutcome::Cancelled),
        ]
    );
}

#[test]
fn a_newer_intent_the_running_tunnel_already_satisfies_is_a_hand_over_not_a_rebuild() {
    // Row 13, and rows 14/15 beside it: pressing Connect with the same rules while connected
    // hands the tunnel to the new epoch. Only a confirmed Clear — the tunnel is really gone —
    // turns it into a reconnect, and darkness resolves nobody.
    let now = t0();
    let status = Status::Up(up_status(1, AWG, params()));
    let intent = up_intent(2, &[AWG], params());

    let d = go(&status, &intent, &running(AWG), now);
    match &d.next {
        Status::Up(u) => assert_eq!(u.epoch, IntentEpoch(2)),
        other => panic!("expected a hand-over, got {other:?}"),
    }
    assert!(matches!(
        resolved(&d),
        Some((IntentEpoch(2), CycleOutcome::Connected { .. }))
    ));

    // 15: dark is not authoritative, so the epoch takes the tunnel but nobody is told it worked.
    let d = go(&status, &intent, &World::Dark, now);
    match &d.next {
        Status::Up(u) => {
            assert_eq!(u.epoch, IntentEpoch(2));
            assert!(!u.resolved, "nothing authoritative has confirmed it");
        }
        other => panic!("expected a hand-over, got {other:?}"),
    }
    assert!(resolved(&d).is_none());

    // 14: it is confirmed gone, so the new epoch gets the reconnect budget rather than a
    // hand-over of nothing.
    let d = go(&status, &intent, &World::Clear, now);
    match &d.next {
        Status::Unwinding {
            cycle: Some(c),
            reason: UnwindReason::TunnelDied,
            ..
        } => {
            assert_eq!(c.epoch, IntentEpoch(2));
            assert!(c.born_from_loss);
        }
        other => panic!("expected a reconnect teardown, got {other:?}"),
    }
}
