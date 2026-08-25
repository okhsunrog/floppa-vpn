//! Traffic statistics recording via Prometheus counters.
//!
//! Periodically reads per-user traffic counters from the authenticator's
//! limiters and records them as Prometheus metrics for VictoriaMetrics.

use std::sync::Arc;

use crate::auth::{MultiUserAuthenticator, UserTraffic};

/// Totals of one flush, for logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushSummary {
    pub users: usize,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

/// Record per-user traffic deltas into the Prometheus counters.
pub fn record_traffic(deltas: &[UserTraffic]) -> FlushSummary {
    let mut summary = FlushSummary::default();
    for delta in deltas {
        let uid = delta.user_id.to_string();
        metrics::counter!("vless_tx_bytes_total", "user_id" => uid.clone())
            .increment(delta.bytes_written);
        metrics::counter!("vless_rx_bytes_total", "user_id" => uid).increment(delta.bytes_read);
        summary.users += 1;
        summary.bytes_read += delta.bytes_read;
        summary.bytes_written += delta.bytes_written;
    }
    summary
}

/// Flush traffic counters from the authenticator to Prometheus metrics.
pub fn flush_traffic(auth: &MultiUserAuthenticator) -> FlushSummary {
    record_traffic(&auth.flush_traffic())
}

/// Background task: periodic traffic flush.
pub async fn flush_loop(auth: Arc<MultiUserAuthenticator>, interval_secs: u64) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;
        flush_traffic(&auth);
    }
}
