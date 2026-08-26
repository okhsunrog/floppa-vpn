//! The published snapshot: everything the UI can know about the tunnel, in one value.
//!
//! Rendered from the three axes by [`view`](super::view), a pure projection. Nothing here is a
//! reconcile input — the actor never reads its own snapshot back.

use super::intent::{IntentEpoch, TunnelParams};
// Doc-only: the link in `ConfigsView`'s comment. That comment is copied verbatim into
// `bindings.ts` by specta, so it must stay a bare link rather than name the module.
#[cfg(doc)]
use super::intent::UpIntent;
use super::outcome::CycleOutcome;
use super::world::Link;
use crate::protocol::Protocol;
use serde::{Deserialize, Serialize};
use specta::Type;

/// The five original status literals are preserved verbatim so the existing indicator component
/// and its translation keys keep working. `Retrying` and `Unknown` are the additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// We have not yet had an authoritative look at the world.
    ///
    /// Distinct from [`Self::Disconnected`], which is a claim: there is no tunnel. Collapsing the
    /// two is why opening the app with a tunnel already running flashed "disconnected" before the
    /// first observation landed — the UI reported an answer it did not have yet.
    Unknown,
    Disconnected,
    Connecting,
    VerifyingConnection,
    Connected,
    Disconnecting,
    Retrying,
}

impl Phase {
    /// The single boolean the button needs.
    ///
    /// Spinner, label, icon, colour and disabled state all derive from this, which is what makes
    /// "spinner showing while the label says Connect" unrepresentable: there is no second source.
    /// `Unknown` counts as busy: the honest thing to show while we do not know is a pending
    /// indicator, not an actionable button offering to do something we cannot yet judge.
    ///
    /// Published as [`TunnelState::busy`] rather than left for the consumer to re-derive, so
    /// this list is the only one: a phase added here reaches the button without a second copy
    /// having to be kept in step.
    pub const fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Unknown
                | Self::Connecting
                | Self::VerifyingConnection
                | Self::Disconnecting
                | Self::Retrying
        )
    }

    /// Whether the primary button should offer to cancel rather than to connect.
    pub const fn is_cancellable(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::VerifyingConnection | Self::Retrying
        )
    }
}

/// Which protocol is being probed, and how far through the order we are.
///
/// Part of the *same* snapshot as [`Phase`], so the cancel/connect swap and the label can no
/// longer disagree with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AttemptProgress {
    pub protocol: Protocol,
    pub index: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RetryProgress {
    pub pass: u32,
    pub max: u32,
    pub resume_in_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IntentView {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct TrafficStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_bytes_per_sec: f64,
    pub rx_bytes_per_sec: f64,
}

/// What the last observation said about traffic, computed once per observation by the actor so
/// the speed tracker sees every sample exactly once — and rendering stays a pure projection.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Traffic {
    pub stats: TrafficStats,
    /// Seconds since the last inbound packet.
    pub last_packet_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConfigSummary {
    pub protocol: Protocol,
    pub address: String,
    pub server_endpoint: String,
    pub dns: Option<String>,
    pub allowed_ips: String,
    pub mtu: u16,
}

/// `available` is a set; the order lives only in an [`UpIntent`]. `preferred` is "the protocol that
/// last actually worked", written only after a successful attempt — never before a probe.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct ConfigsView {
    pub available: Vec<Protocol>,
    pub preferred: Option<Protocol>,
    pub summaries: Vec<ConfigSummary>,
}

/// Everything the UI can know about the tunnel, in one value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TunnelState {
    /// Bumped only when a state is actually published. Consumers drop any snapshot whose `seq` is
    /// not newer than the one they hold, which closes the seed-versus-first-event race at startup.
    pub seq: u64,
    pub phase: Phase,
    /// [`Phase::is_busy`] for [`Self::phase`], carried so the consumer never restates which phases
    /// count as work in progress.
    pub busy: bool,
    /// [`Phase::is_cancellable`] for [`Self::phase`].
    pub cancellable: bool,
    pub intent: IntentView,
    pub epoch: IntentEpoch,
    pub intent_order: Vec<Protocol>,
    /// The protocol actually running — distinct from the preferred one.
    pub protocol: Option<Protocol>,
    /// The split rules the running tunnel was actually built with, when they are known.
    ///
    /// `None` when nothing is running, and for an adopted tunnel whose owner does not report them
    /// (only the in-process backend, which cannot find a tunnel it did not start). Published
    /// because it is the only way the UI can tell "the settings changed since this tunnel came
    /// up" from data: that used to be a component flag, which a moment of `retrying` cleared and
    /// leaving the page destroyed — while the tunnel carried on with the old rules.
    pub params: Option<TunnelParams>,
    pub adopted: bool,
    pub attempt: Option<AttemptProgress>,
    pub retry: Option<RetryProgress>,
    pub server_endpoint: Option<String>,
    pub assigned_ip: Option<String>,
    pub connected_at: Option<i64>,
    pub last_packet_received: Option<i64>,
    pub stats: TrafficStats,
    /// Sticky until the next accepted intent.
    pub last_outcome: Option<CycleOutcome>,
    /// Which cycle [`Self::last_outcome`] came from, counting up from zero.
    ///
    /// The only thing that identifies an outcome. Neither the intent's epoch nor the outcome's own
    /// tag can do it: a reconnect runs under the *same* intent, so a tunnel that dropped and came
    /// back reports `connected` twice under one epoch — and a consumer deduplicating by that pair
    /// swallowed the second, which is precisely the one that says a protocol was stepped over.
    pub outcome_serial: u64,
    pub configs: ConfigsView,
    /// False while the world is dark. This never by itself means the tunnel is down.
    pub backend_reachable: bool,
    /// Whether the device has a network under the tunnel, where the platform says so.
    ///
    /// A field rather than a [`Phase`], because it is orthogonal to every one of them: a tunnel
    /// can be Connected with the network gone (the phone is in a lift and the tunnel is intact),
    /// and a cycle can be Retrying with the network gone (it is parked, waiting, spending
    /// nothing). A phase cannot say either of those, and inventing one for each combination is how
    /// a state machine acquires a state per adjective.
    ///
    /// [`Link::Unknown`] on every platform without a watcher, which is every platform but Android
    /// — so a consumer must treat it as "do not mention the network", never as bad news.
    pub link: Link,
}

impl TunnelState {
    /// Equal apart from `seq`, which differs on every publish by construction.
    pub fn eq_ignoring_seq(&self, other: &Self) -> bool {
        let Self {
            seq: _,
            phase,
            busy,
            cancellable,
            intent,
            epoch,
            intent_order,
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
            configs,
            backend_reachable,
            link,
        } = self;
        *phase == other.phase
            && *busy == other.busy
            && *cancellable == other.cancellable
            && *intent == other.intent
            && *epoch == other.epoch
            && *intent_order == other.intent_order
            && *protocol == other.protocol
            && *params == other.params
            && *adopted == other.adopted
            && *attempt == other.attempt
            && *retry == other.retry
            && *server_endpoint == other.server_endpoint
            && *assigned_ip == other.assigned_ip
            && *connected_at == other.connected_at
            && *last_packet_received == other.last_packet_received
            && *stats == other.stats
            && *last_outcome == other.last_outcome
            && *outcome_serial == other.outcome_serial
            && *configs == other.configs
            && *backend_reachable == other.backend_reachable
            && *link == other.link
    }

    pub fn initial() -> Self {
        Self {
            seq: 0,
            // Not Disconnected: at seq 0 nothing has been observed, and claiming there is no
            // tunnel before looking is what made an already-running tunnel flash as down.
            phase: Phase::Unknown,
            busy: Phase::Unknown.is_busy(),
            cancellable: Phase::Unknown.is_cancellable(),
            intent: IntentView::Down,
            epoch: IntentEpoch(0),
            intent_order: Vec::new(),
            protocol: None,
            params: None,
            adopted: false,
            attempt: None,
            retry: None,
            server_endpoint: None,
            assigned_ip: None,
            connected_at: None,
            last_packet_received: None,
            stats: TrafficStats::default(),
            last_outcome: None,
            outcome_serial: 0,
            configs: ConfigsView::default(),
            backend_reachable: false,
            link: Link::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_phases_are_exactly_the_ones_that_should_spin() {
        assert!(Phase::Connecting.is_busy());
        assert!(Phase::VerifyingConnection.is_busy());
        assert!(Phase::Disconnecting.is_busy());
        assert!(Phase::Retrying.is_busy());
        assert!(!Phase::Connected.is_busy());
        assert!(!Phase::Disconnected.is_busy());
    }
}
