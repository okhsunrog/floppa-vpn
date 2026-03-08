//! Traffic statistics recording via Prometheus counters.
//!
//! Periodically reads per-user traffic counters from the authenticator's
//! limiters and records them as Prometheus metrics for VictoriaMetrics.

use std::sync::Arc;

use crate::auth::MultiUserAuthenticator;

/// Flush traffic counters from the authenticator to Prometheus metrics.
pub fn flush_traffic(auth: &MultiUserAuthenticator) {
    let deltas = auth.flush_traffic();
    if deltas.is_empty() {
        return;
    }

    for (user_id, rx_delta, tx_delta) in &deltas {
        let uid = user_id.to_string();
        metrics::counter!("vless_tx_bytes_total", "user_id" => uid.clone()).increment(*tx_delta);
        metrics::counter!("vless_rx_bytes_total", "user_id" => uid).increment(*rx_delta);
    }
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
