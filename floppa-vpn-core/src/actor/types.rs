//! The actor's vocabulary.
//!
//! Three axes, deliberately separate:
//!
//! - [`Intent`] — what the user wants. Only commands change it. ([`intent`](super::intent))
//! - [`Status`] — what we are doing about that. Only [`reconcile`](super::reconcile) changes it,
//!   and it lives as a local variable of the actor task, so nothing else *can*.
//!   ([`status`](super::status))
//! - [`World`] — what is actually true, as last observed. ([`world`](super::world))
//!
//! [`Link`] sits beside them rather than among them: it is not an axis of the table but a gate on
//! two of its cells, and it says something about the network under the tunnel rather than about
//! the tunnel.
//!
//! Beside them: how things end ([`outcome`](super::outcome)), what the UI is shown
//! ([`snapshot`](super::snapshot)) and the knobs ([`policy`](super::policy)). Each lives in its
//! own module; this one re-exports them all, so a caller names one path for the vocabulary and
//! the split by axis is invisible to it.
//!
//! Auto-reconnect and auto protocol selection are not features in this design: they are what
//! happens when [`Intent::Up`] outlives a failure. That is why `userIntent`, `abortGen`,
//! `reconnectAttempts`, `reconnectTimeoutId` and `runAutoCycle` have no counterpart here.

pub use super::intent::{Intent, IntentEpoch, SplitMode, TunnelParams, UpIntent};
pub use super::outcome::{
    AttemptError, AttemptFailure, AttemptResult, CycleOutcome, IntentAccepted, IntentError,
};
pub use super::policy::{DnsFailurePolicy, Policy};
pub use super::snapshot::{
    AttemptProgress, ConfigSummary, ConfigsView, IntentView, Phase, RetryProgress, Traffic,
    TrafficStats, TunnelState,
};
pub use super::status::{AttemptPhase, Cycle, Status, UnwindReason, UpStatus};
pub use super::world::{
    Link, Observation, RawStats, RunningTunnel, TunnelObservation, UnreachableCause, World,
    WorldView,
};
