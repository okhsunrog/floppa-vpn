//! Traffic statistics collection and DB flushing.
//!
//! Collects per-peer byte counts in memory and periodically
//! flushes deltas to PostgreSQL.

use std::sync::atomic::{AtomicI64, Ordering};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{error, info};

/// Collects traffic stats for all active peers.
#[derive(Debug)]
pub struct TrafficCollector {
    counters: DashMap<i64, (AtomicI64, AtomicI64)>, // peer_id -> (tx, rx)
}

impl TrafficCollector {
    pub fn new() -> Self {
        Self {
            counters: DashMap::new(),
        }
    }

    /// Record bytes transferred for a peer.
    pub fn record(&self, peer_id: i64, tx: i64, rx: i64) {
        self.counters
            .entry(peer_id)
            .and_modify(|(existing_tx, existing_rx)| {
                existing_tx.fetch_add(tx, Ordering::Relaxed);
                existing_rx.fetch_add(rx, Ordering::Relaxed);
            })
            .or_insert_with(|| (AtomicI64::new(tx), AtomicI64::new(rx)));
    }

    /// Drain all counters and return the deltas.
    fn drain(&self) -> Vec<(i64, i64, i64)> {
        let mut deltas = Vec::new();
        // Swap each counter to zero and collect non-zero deltas
        for entry in self.counters.iter() {
            let peer_id = *entry.key();
            let (tx, rx) = entry.value();
            let tx_delta = tx.swap(0, Ordering::Relaxed);
            let rx_delta = rx.swap(0, Ordering::Relaxed);
            if tx_delta > 0 || rx_delta > 0 {
                deltas.push((peer_id, tx_delta, rx_delta));
            }
        }
        deltas
    }

    /// Flush accumulated traffic stats to the database.
    pub async fn flush(&self, pool: &PgPool) -> anyhow::Result<()> {
        let deltas = self.drain();
        if deltas.is_empty() {
            return Ok(());
        }

        for (peer_id, tx_delta, rx_delta) in &deltas {
            let traffic_delta = tx_delta + rx_delta;
            sqlx::query!(
                r#"
                UPDATE peers
                SET tx_bytes = tx_bytes + $1,
                    rx_bytes = rx_bytes + $2,
                    traffic_used_bytes = traffic_used_bytes + $3,
                    last_handshake = NOW()
                WHERE id = $4
                "#,
                tx_delta,
                rx_delta,
                traffic_delta,
                peer_id,
            )
            .execute(pool)
            .await?;
        }

        info!(peers = deltas.len(), "Flushed traffic stats");
        Ok(())
    }
}

/// Background task: periodic traffic flush.
pub async fn flush_loop(
    pool: PgPool,
    collector: std::sync::Arc<TrafficCollector>,
    interval_secs: u64,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;
        if let Err(e) = collector.flush(&pool).await {
            error!("Traffic flush failed: {e:#}");
        }
    }
}
