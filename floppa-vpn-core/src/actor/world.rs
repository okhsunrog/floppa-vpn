//! What is actually true, as last observed. The third axis of the decision table.
//!
//! An [`Observation`] is one look at the backend; [`World`] is what that look, the clock and the
//! [`Policy`] make of it. The distinction that matters is [`World::Dark`]: not knowing is never
//! evidence that there is no tunnel.

use super::intent::TunnelParams;
use super::policy::Policy;
use crate::protocol::Protocol;
use std::time::Instant;

/// One look at the world.
///
/// Replaces the old `Option`-returning info call, whose `None` conflated "no tunnel", "peer not
/// started", "peer refused", "transport died" and "call timed out" into a single value that the
/// caller then had to guess about.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub observed_at: Instant,
    pub view: WorldView,
}

impl Observation {
    /// Boot value: we have never looked.
    pub fn unknown(now: Instant) -> Self {
        Self {
            observed_at: now,
            view: WorldView::Unreachable(UnreachableCause::NotStarted),
        }
    }
}

/// A look either found the owner and learned everything, or did not reach it at all — which is why
/// one variant is a full description and the other a cause.
// The size difference is inherent: describing a tunnel takes strings and split rules, and saying
// "nobody answered" takes a discriminant. Boxing would buy an allocation on every look to shrink a
// value that is already only moved inside `Box<Observation>` on the actor's channel.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum WorldView {
    Reachable(TunnelObservation),
    Unreachable(UnreachableCause),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreachableCause {
    /// Nothing is listening: the peer never bound its socket, or it is an older build.
    NotStarted,
    /// Connection refused: a stale socket file, or the peer was killed. Strong evidence of death.
    ConnectRefused,
    /// The transport died mid-call.
    TransportBroken,
    /// Our per-call deadline expired: the peer is alive but wedged.
    Timeout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TunnelObservation {
    /// Which generation of the service answered.
    ///
    /// Starting a tunnel tears down the previous service first, and that teardown is asynchronous
    /// — so "something answered" is not the same as "the service we just asked for answered". The
    /// dying previous instance replies quite happily right up until it does not.
    ///
    /// Minted per service start by [`ServiceGenerations`](crate::autostart::ServiceGenerations),
    /// never by the cycle: an intent epoch is shared by every pass of a cycle and restarts at 1
    /// in each UI process, so comparing generations by it matched instances we had moved past.
    pub generation: u64,
    pub running: Option<RunningTunnel>,
    /// True between "the peer bound its socket" and "the tunnel start returned". Requires the RPC
    /// server to bind ahead of the tunnel start, which is what turns a failed Android start into
    /// typed state instead of a blind timeout.
    pub starting: bool,
    /// The peer holds an established TUN (Android: `establish()` succeeded). A reachable peer
    /// without one is still coming up, and a tunnel must not be requested from it yet.
    pub tun_ready: bool,
    pub start_error: Option<String>,
    pub raw_stats: Option<RawStats>,
    /// Seconds since the last inbound packet.
    pub last_packet_secs: Option<i64>,
}

/// What one observation says about a service the caller is waiting to hand a tunnel to.
///
/// Pure so it is testable on the host: the Android attempt polls the service and feeds each
/// observation through here, and the whole "did our generation come up, and can it take a
/// tunnel yet" judgement lives in one place instead of in the poll loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceReadiness {
    /// Our generation holds an established TUN: a tunnel can be requested now.
    Ready,
    /// Our generation is up but `establish()` has not handed it a descriptor yet.
    Establishing,
    /// Our generation reported why it could not start.
    Failed(String),
    /// Something answered, but not the generation we started (a dying predecessor, usually).
    OtherGeneration(u64),
}

impl TunnelObservation {
    pub fn readiness_for(&self, wanted: u64) -> ServiceReadiness {
        if self.generation != wanted {
            return ServiceReadiness::OtherGeneration(self.generation);
        }
        if let Some(detail) = &self.start_error {
            return ServiceReadiness::Failed(detail.clone());
        }
        if self.tun_ready {
            ServiceReadiness::Ready
        } else {
            ServiceReadiness::Establishing
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

/// The identity of a tunnel that is actually running, reported by the process that owns it.
///
/// Never inferred from the stored config: that is what made an adopted Android tunnel claim
/// whichever protocol the settings happened to name.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningTunnel {
    pub protocol: Protocol,
    /// Which service generation is carrying it. `None` for a backend that has no generations —
    /// the in-process one, where the tunnel and the observer are the same process.
    pub generation: Option<u64>,
    pub endpoint: String,
    pub address: String,
    pub connected_secs: Option<u64>,
    /// The split rules it was built with, when the owning process reports them. Known for every
    /// tunnel the Android service starts — over the RPC or from the autostart bundle — and
    /// unknown for one found by an in-process backend after a restart. Knowing them is what
    /// lets a tunnel be adopted *with* its rules rather than as a black box.
    pub params: Option<TunnelParams>,
    /// Started by the service on its own (always-on, boot, lockdown), from the bundle the last
    /// successful connect wrote — not by any intent of this or any other UI process.
    pub autonomous: bool,
    /// Seconds since the far side last gave any evidence of being there: a completed handshake for
    /// the WireGuard family, an inbound packet for VLESS.
    ///
    /// Reported by the process that owns the tunnel, because only it knows which of the two
    /// applies and holds the peer's timers. Never on its own a reason to tear anything down: a
    /// sleeping phone and a config without a keepalive are both silent with nothing wrong, so what
    /// silence buys is a probe, not a verdict.
    pub silent_secs: Option<i64>,
}

/// The third axis of the decision table, derived purely from an [`Observation`], the clock and the
/// [`Policy`].
#[derive(Debug, Clone, PartialEq)]
pub enum World {
    /// The peer answered: there is no tunnel. Authoritative on every platform.
    Clear,
    /// The peer answered: this tunnel is running.
    Running(RunningTunnel),
    /// The peer did not answer, is mid-start, or its last answer is too old to trust.
    /// **Never authoritative** — this is the whole point of the type.
    Dark,
}

impl World {
    pub fn classify(obs: &Observation, now: Instant, policy: &Policy) -> World {
        if now.saturating_duration_since(obs.observed_at) > policy.obs_stale_after {
            return World::Dark;
        }
        match &obs.view {
            WorldView::Unreachable(_) => World::Dark,
            // A peer that is mid-start is not yet evidence of anything.
            WorldView::Reachable(t) if t.starting => World::Dark,
            WorldView::Reachable(t) => match &t.running {
                Some(rt) => World::Running(rt.clone()),
                None => World::Clear,
            },
        }
    }

    pub fn is_dark(&self) -> bool {
        matches!(self, Self::Dark)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_stale_observation_is_dark_even_when_it_reported_a_tunnel() {
        let policy = Policy::default();
        let now = Instant::now();
        let obs = Observation {
            observed_at: now,
            view: WorldView::Reachable(TunnelObservation {
                generation: 0,
                running: None,
                starting: false,
                tun_ready: true,
                start_error: None,
                raw_stats: None,
                last_packet_secs: None,
            }),
        };
        assert_eq!(World::classify(&obs, now, &policy), World::Clear);

        let later = now + policy.obs_stale_after + Duration::from_millis(1);
        assert_eq!(World::classify(&obs, later, &policy), World::Dark);
    }

    #[test]
    fn a_peer_that_is_mid_start_is_not_evidence_of_anything() {
        let policy = Policy::default();
        let now = Instant::now();
        let obs = Observation {
            observed_at: now,
            view: WorldView::Reachable(TunnelObservation {
                generation: 0,
                running: None,
                starting: true,
                tun_ready: false,
                start_error: None,
                raw_stats: None,
                last_packet_secs: None,
            }),
        };
        assert_eq!(World::classify(&obs, now, &policy), World::Dark);
    }

    #[test]
    fn every_unreachable_cause_classifies_as_dark_never_as_clear() {
        let policy = Policy::default();
        let now = Instant::now();
        for cause in [
            UnreachableCause::NotStarted,
            UnreachableCause::ConnectRefused,
            UnreachableCause::TransportBroken,
            UnreachableCause::Timeout,
        ] {
            let obs = Observation {
                observed_at: now,
                view: WorldView::Unreachable(cause),
            };
            assert_eq!(
                World::classify(&obs, now, &policy),
                World::Dark,
                "{cause:?} must never be read as 'no tunnel'"
            );
        }
    }

    fn service(generation: u64, tun_ready: bool, start_error: Option<&str>) -> TunnelObservation {
        TunnelObservation {
            generation,
            running: None,
            starting: start_error.is_none(),
            tun_ready,
            start_error: start_error.map(str::to_owned),
            raw_stats: None,
            last_packet_secs: None,
        }
    }

    #[test]
    fn readiness_waits_for_the_descriptor_and_surfaces_a_failed_establish() {
        // Bound but not established yet: keep polling, do not request a tunnel.
        assert_eq!(
            service(7, false, None).readiness_for(7),
            ServiceReadiness::Establishing
        );
        // establish() done: go.
        assert_eq!(
            service(7, true, None).readiness_for(7),
            ServiceReadiness::Ready
        );
        // establish() failed: the reason, immediately — not a timeout.
        assert_eq!(
            service(
                7,
                false,
                Some("VpnService.Builder.establish() returned null")
            )
            .readiness_for(7),
            ServiceReadiness::Failed("VpnService.Builder.establish() returned null".into())
        );
        // A failure beats a descriptor that somehow also arrived.
        assert_eq!(
            service(7, true, Some("late")).readiness_for(7),
            ServiceReadiness::Failed("late".into())
        );
        // The predecessor still answering is not ours, whatever it says.
        assert_eq!(
            service(6, true, None).readiness_for(7),
            ServiceReadiness::OtherGeneration(6)
        );
    }
}
