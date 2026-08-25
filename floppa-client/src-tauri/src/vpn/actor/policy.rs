//! The knobs. Plain data, so [`reconcile`](super::reconcile) stays a pure function of its
//! arguments and a test can turn any of them.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsFailurePolicy {
    Fatal,
    Tolerate,
}

/// Plain data, so [`reconcile`](super::reconcile) stays a pure function of its arguments.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Observation cadence. One rate, not one per situation: the actor is never so idle that it
    /// stops caring whether a tunnel appeared underneath it.
    pub poll_active: Duration,
    /// An observation older than this is dark regardless of what it said.
    pub obs_stale_after: Duration,
    /// How long darkness is tolerated before an up tunnel is declared lost. Zero on desktop, where
    /// the backend is in-process and always answers.
    pub dark_grace: Duration,
    /// Wall-clock budget for one attempt, ladder and verification included.
    pub attempt_budget: Duration,
    /// How long the Android consent dialog may go unanswered before the attempt gives up on it.
    ///
    /// It is not "how long a person may take to read it" — a dialog a person is looking at is
    /// answered, and the attempt budget is what bounds that. It bounds the case where no dialog
    /// was ever shown: Android refuses to start an activity from a background process, so the
    /// call simply never returns, and without this the attempt sat there until the actor
    /// cancelled it.
    pub consent_budget: Duration,
    pub verify_wg: Duration,
    pub verify_vless: Duration,
    /// Passes over the order for a cold, user-initiated connect. One means fail fast.
    pub cold_passes: u32,
    /// Passes over the order after a tunnel that was up died.
    pub reconnect_passes: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// How many times an unwind is re-run while the world still reports a running tunnel.
    pub unwind_tries: u32,
    /// How many times one step's undo is retried before it is logged and popped.
    pub undo_retries: u32,
    pub dns_failure: DnsFailurePolicy,
    /// Bound on waiting for a terminal Down when clearing configs.
    pub settle_timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            poll_active: Duration::from_secs(1),
            obs_stale_after: Duration::from_secs(3),
            dark_grace: Duration::ZERO,
            attempt_budget: Duration::from_secs(25),
            consent_budget: Duration::from_secs(20),
            verify_wg: Duration::from_secs(5),
            verify_vless: Duration::from_secs(10),
            cold_passes: 1,
            reconnect_passes: 3,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            unwind_tries: 3,
            undo_retries: 2,
            dns_failure: DnsFailurePolicy::Tolerate,
            settle_timeout: Duration::from_secs(15),
        }
    }
}

impl Policy {
    /// Adapt to the backend: only a cross-process backend can go dark, and only it needs the
    /// extra budget for a consent dialog.
    pub fn for_backend(grace: Duration) -> Self {
        let android = !grace.is_zero();
        Self {
            dark_grace: grace,
            attempt_budget: if android {
                Duration::from_secs(40)
            } else {
                Duration::from_secs(25)
            },
            ..Self::default()
        }
    }

    pub fn backoff(&self, pass: u32) -> Duration {
        self.backoff_base
            .saturating_mul(1u32 << pass.min(5))
            .min(self.backoff_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_saturates() {
        let policy = Policy::default();
        assert_eq!(policy.backoff(0), Duration::from_secs(1));
        assert_eq!(policy.backoff(1), Duration::from_secs(2));
        assert_eq!(policy.backoff(2), Duration::from_secs(4));
        assert_eq!(policy.backoff(20), policy.backoff_max);
    }
}
