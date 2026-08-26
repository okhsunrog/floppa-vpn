//! Projection of the actor's internal state onto the one value the UI consumes.
//!
//! Pure. Keeping this in one function is what guarantees the UI can never see a torn combination
//! — the phase, the probe progress and the retry countdown are computed together from the same
//! status, so a spinner cannot disagree with its own label.

use super::types::{
    AttemptProgress, ConfigsView, CycleOutcome, Intent, IntentView, Link, Phase, RetryProgress,
    Status, Traffic, TrafficStats, TunnelState, World,
};
use std::time::Instant;

/// Build the published snapshot.
///
/// `seq` is supplied by the caller and only ever increases, so a consumer can discard anything that
/// is not strictly newer than what it already holds — which closes the race between seeding from a
/// direct read and receiving the first pushed update.
#[allow(clippy::too_many_arguments)]
pub fn render(
    seq: u64,
    status: &Status,
    intent: &Intent,
    world: &World,
    link: Link,
    traffic: Traffic,
    configs: &ConfigsView,
    last_outcome: Option<CycleOutcome>,
    outcome_serial: u64,
    now: Instant,
    observed_once: bool,
) -> TunnelState {
    let phase = phase_of(status, observed_once);

    let (protocol, params, adopted, server_endpoint, assigned_ip, connected_at) = match status {
        Status::Up(u) => (
            Some(u.protocol),
            u.params.clone(),
            u.adopted,
            Some(u.server_endpoint.clone()),
            Some(u.assigned_ip.clone()),
            Some(u.connected_at),
        ),
        _ => (None, None, false, None, None, None),
    };

    let attempt = match status {
        Status::Connecting { cycle, .. } => Some(AttemptProgress {
            protocol: cycle.protocol(),
            index: cycle.index as u32 + 1,
            total: cycle.order.len() as u32,
        }),
        _ => None,
    };

    let retry = match status {
        Status::Retrying { cycle, resume_at } => Some(RetryProgress {
            // The pass about to run, not the ones already burnt: `cycle.pass` counts backwards
            // from the user's point of view, and "0/3" is not a thing to show anyone.
            pass: cycle.pass + 1,
            max: cycle.passes_allowed,
            resume_in_ms: resume_at
                .saturating_duration_since(now)
                .as_millis()
                .min(u32::MAX as u128) as u32,
        }),
        _ => None,
    };

    // Stats are only meaningful while a tunnel is actually up.
    let (stats, last_packet_received) = match status {
        Status::Up(_) => (traffic.stats, traffic.last_packet_secs),
        _ => (TrafficStats::default(), None),
    };

    TunnelState {
        seq,
        phase,
        // Derived here, once, from the phase they belong to — so they cannot describe a different
        // phase than the one in the same snapshot.
        busy: phase.is_busy(),
        cancellable: phase.is_cancellable(),
        intent: if intent.is_up() {
            IntentView::Up
        } else {
            IntentView::Down
        },
        epoch: intent.epoch(),
        intent_order: match intent {
            Intent::Up(up) => up.order.clone(),
            Intent::Down { .. } => Vec::new(),
        },
        protocol,
        params,
        adopted,
        attempt,
        retry,
        server_endpoint,
        assigned_ip,
        connected_at,
        last_packet_received,
        stats,
        last_outcome,
        outcome_serial,
        configs: configs.clone(),
        backend_reachable: !world.is_dark(),
        link,
    }
}

/// The single mapping from internal status to the phase the UI renders.
///
/// `Unwinding` reports as `Disconnecting` regardless of *why* we are unwinding, because from the
/// user's point of view a teardown is a teardown; the reason only affects what happens next.
fn phase_of(status: &Status, observed_once: bool) -> Phase {
    match status {
        // Idle means "we are doing nothing", which is true from the moment the actor starts. But
        // "there is no tunnel" is a claim about the world, and until something authoritative has
        // been observed we are not entitled to make it — a tunnel may well be running.
        Status::Idle if !observed_once => Phase::Unknown,
        Status::Idle => Phase::Disconnected,
        Status::Connecting { phase, .. } => match phase {
            super::types::AttemptPhase::Verifying => Phase::VerifyingConnection,
            _ => Phase::Connecting,
        },
        Status::Up(_) => Phase::Connected,
        Status::Unwinding { .. } => Phase::Disconnecting,
        Status::Retrying { .. } => Phase::Retrying,
    }
}

/// Whether the actor has nothing in flight and nothing applied.
///
/// Only `Idle` qualifies: `Up` means a tunnel exists, and the three transient states mean work is
/// still running. This is what "clear the configs" and "exit cleanly" wait for, rather than
/// branching on whatever status a caller last happened to observe.
pub fn is_quiescent(status: &Status) -> bool {
    matches!(status, Status::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::types::*;
    use crate::protocol::Protocol;
    use std::time::Duration;

    fn render_with(status: &Status, intent: &Intent, now: Instant) -> TunnelState {
        render_observed(status, intent, now, true)
    }

    fn render_observed(
        status: &Status,
        intent: &Intent,
        now: Instant,
        observed_once: bool,
    ) -> TunnelState {
        render(
            1,
            status,
            intent,
            &World::Clear,
            Link::Unknown,
            Traffic::default(),
            &ConfigsView::default(),
            None,
            0,
            now,
            observed_once,
        )
    }

    fn cycle(order: &[Protocol]) -> Cycle {
        Cycle {
            epoch: IntentEpoch(1),
            order: order.to_vec(),
            params: TunnelParams::default(),
            index: 0,
            pass: 0,
            passes_allowed: 3,
            born_from_loss: false,
            failures: Vec::new(),
        }
    }

    #[test]
    fn a_busy_phase_and_its_progress_always_arrive_together() {
        // The historical failure: the spinner came from one source and the label from another, so
        // the button could show a spinner while saying "Connect". Here both are read off one value.
        let now = Instant::now();
        let status = Status::Connecting {
            cycle: cycle(&[Protocol::AmneziaWg, Protocol::WireGuard]),
            phase: AttemptPhase::Preparing,
            deadline: now + Duration::from_secs(25),
        };
        let state = render_with(&status, &Intent::default(), now);

        assert!(state.busy);
        assert!(state.cancellable);
        let progress = state.attempt.expect("a busy connect must report progress");
        assert_eq!(progress.protocol, Protocol::AmneziaWg);
        assert_eq!(progress.index, 1);
        assert_eq!(progress.total, 2);
    }

    #[test]
    fn verifying_is_distinguishable_from_connecting() {
        let now = Instant::now();
        let status = Status::Connecting {
            cycle: cycle(&[Protocol::AmneziaWg]),
            phase: AttemptPhase::Verifying,
            deadline: now + Duration::from_secs(25),
        };
        assert_eq!(
            render_with(&status, &Intent::default(), now).phase,
            Phase::VerifyingConnection
        );
    }

    #[test]
    fn before_the_world_has_answered_we_do_not_claim_there_is_no_tunnel() {
        // Reported from a device: opening the app while a tunnel was already running flashed
        // "disconnected" for an instant. Idle is true from the moment the actor starts, but
        // "there is no tunnel" is a claim about the world, and we had not looked yet.
        let now = Instant::now();
        let state = render_observed(&Status::Idle, &Intent::default(), now, false);

        assert_eq!(state.phase, Phase::Unknown);
        assert!(state.busy, "pending, not actionable");
        assert!(!state.cancellable, "there is nothing to cancel");
    }

    #[test]
    fn once_the_world_has_answered_idle_means_disconnected() {
        let now = Instant::now();
        let state = render_observed(&Status::Idle, &Intent::default(), now, true);
        assert_eq!(state.phase, Phase::Disconnected);
        assert!(!state.busy);
    }

    #[test]
    fn an_unreachable_look_still_counts_as_having_looked() {
        // The first attempt at this required a *reachable* answer before leaving Unknown. On
        // Android the peer only exists while a tunnel does, so with no tunnel there is nothing to
        // reach — and the UI sat at "checking" forever. Unknown has to be a state we can leave.
        let now = Instant::now();
        let state = render(
            1,
            &Status::Idle,
            &Intent::default(),
            &World::Dark,
            Link::Unknown,
            Traffic::default(),
            &ConfigsView::default(),
            None,
            0,
            now,
            true,
        );
        assert_eq!(state.phase, Phase::Disconnected);
        assert!(!state.busy, "must not spin forever");
    }

    #[test]
    fn an_idle_actor_is_not_busy_and_reports_no_progress() {
        let now = Instant::now();
        let state = render_with(&Status::Idle, &Intent::default(), now);
        assert_eq!(state.phase, Phase::Disconnected);
        assert!(!state.busy);
        assert!(state.attempt.is_none());
        assert!(state.retry.is_none());
        assert!(state.protocol.is_none());
    }

    /// The snapshot answers "is the button busy?" itself, so no consumer needs its own copy of
    /// which phases count as work in progress. Two lists that happen to agree are not one source
    /// of truth; they are a bug waiting for the next phase to be added.
    #[test]
    fn every_status_publishes_booleans_that_match_its_own_phase() {
        let now = Instant::now();
        let statuses = [
            Status::Idle,
            Status::Connecting {
                cycle: cycle(&[Protocol::AmneziaWg]),
                phase: AttemptPhase::Preparing,
                deadline: now + Duration::from_secs(25),
            },
            Status::Connecting {
                cycle: cycle(&[Protocol::AmneziaWg]),
                phase: AttemptPhase::Verifying,
                deadline: now + Duration::from_secs(25),
            },
            Status::Unwinding {
                cycle: None,
                reason: UnwindReason::IntentDown,
                tries: 0,
            },
            Status::Retrying {
                cycle: cycle(&[Protocol::AmneziaWg]),
                resume_at: now + Duration::from_secs(1),
            },
        ];

        for observed_once in [false, true] {
            for status in &statuses {
                let state = render_observed(status, &Intent::default(), now, observed_once);
                assert_eq!(
                    state.busy,
                    state.phase.is_busy(),
                    "{:?} published busy={} for phase {:?}",
                    status,
                    state.busy,
                    state.phase
                );
                assert_eq!(state.cancellable, state.phase.is_cancellable());
            }
        }
    }

    #[test]
    fn a_retry_reports_how_long_is_left() {
        let now = Instant::now();
        let status = Status::Retrying {
            cycle: cycle(&[Protocol::AmneziaWg]),
            resume_at: now + Duration::from_secs(4),
        };
        let state = render_with(&status, &Intent::default(), now);
        assert_eq!(state.phase, Phase::Retrying);
        assert!(state.busy, "a retry is still work in progress");
        let retry = state.retry.expect("a retry must report its countdown");
        assert!((3_900..=4_000).contains(&retry.resume_in_ms));
    }

    #[test]
    fn darkness_is_reported_without_claiming_the_tunnel_is_down() {
        let now = Instant::now();
        let state = render(
            1,
            &Status::Up(UpStatus {
                epoch: IntentEpoch(1),
                protocol: Protocol::AmneziaWg,
                params: None,
                adopted: false,
                server_endpoint: "e".into(),
                assigned_ip: "10.0.0.2/32".into(),
                connected_at: 0,
                dark_since: Some(now),
                probing_since: None,
                resolved: true,
            }),
            &Intent::default(),
            &World::Dark,
            Link::Unknown,
            Traffic::default(),
            &ConfigsView::default(),
            None,
            0,
            now,
            true,
        );
        assert!(!state.backend_reachable);
        assert_eq!(
            state.phase,
            Phase::Connected,
            "an unreachable backend is not a disconnected tunnel"
        );
    }
}
