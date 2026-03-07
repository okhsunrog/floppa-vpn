//! Traffic statistics flushing to PostgreSQL.
//!
//! Periodically reads per-user traffic counters from the authenticator's
//! limiters and writes accumulated deltas to the database.

use std::sync::Arc;

use sqlx::PgPool;
use tracing::{error, info};

use crate::auth::MultiUserAuthenticator;

/// Flush traffic counters from the authenticator to the database.
pub async fn flush_traffic(auth: &MultiUserAuthenticator, pool: &PgPool) -> anyhow::Result<()> {
    let deltas = auth.flush_traffic();
    if deltas.is_empty() {
        return Ok(());
    }

    for (user_id, rx_delta, tx_delta) in &deltas {
        sqlx::query!(
            r#"
            UPDATE users
            SET vless_tx_bytes = vless_tx_bytes + $1,
                vless_rx_bytes = vless_rx_bytes + $2
            WHERE id = $3
            "#,
            *tx_delta as i64,
            *rx_delta as i64,
            user_id,
        )
        .execute(pool)
        .await?;
    }

    info!(users = deltas.len(), "Flushed traffic stats");
    Ok(())
}

/// Background task: periodic traffic flush.
pub async fn flush_loop(pool: PgPool, auth: Arc<MultiUserAuthenticator>, interval_secs: u64) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;
        if let Err(e) = flush_traffic(&auth, &pool).await {
            error!("Traffic flush failed: {e:#}");
        }
    }
}
