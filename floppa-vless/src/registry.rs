//! UUID registry with database synchronization.
//!
//! Keeps the in-memory VLESS UUID registry in sync with PostgreSQL via:
//! - **LISTEN/NOTIFY**: Real-time updates when users or subscriptions change
//! - **Periodic full sync**: Safety net for missed notifications

use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tracing::{error, info, warn};

use crate::auth::MultiUserAuthenticator;

/// Load all users with VLESS UUIDs and active subscriptions into the authenticator.
pub async fn full_sync(pool: &PgPool, auth: &Arc<MultiUserAuthenticator>) -> anyhow::Result<()> {
    let rows = sqlx::query!(
        r#"
        SELECT u.id as user_id, u.vless_uuid,
               pl.default_speed_limit_mbps as speed_limit_mbps
        FROM users u
        JOIN subscriptions s ON s.user_id = u.id
        JOIN plans pl ON s.plan_id = pl.id
        WHERE u.vless_uuid IS NOT NULL
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        "#
    )
    .fetch_all(pool)
    .await?;

    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        let uuid_str = match &row.vless_uuid {
            Some(u) => u,
            None => continue,
        };

        let uuid_bytes = match parse_vless_uuid(uuid_str) {
            Some(b) => b,
            None => {
                error!(
                    user_id = row.user_id,
                    uuid = uuid_str,
                    "Invalid VLESS UUID format"
                );
                continue;
            }
        };

        users.push((uuid_bytes, row.user_id, row.speed_limit_mbps));
    }

    let count = users.len();
    auth.sync_users(users);

    info!(count, "Registry synced from database");
    Ok(())
}

/// Background task: listen for DB changes via LISTEN/NOTIFY.
///
/// Reacts to `peer_changed` and `subscription_changed` channels
/// by re-syncing the registry. Reconnects with exponential backoff
/// on connection failure.
pub async fn listen_for_changes(pool: PgPool, auth: Arc<MultiUserAuthenticator>) {
    let mut backoff_secs = 1u64;
    const MAX_BACKOFF_SECS: u64 = 30;

    loop {
        match run_listener(&pool, &auth).await {
            Ok(()) => {
                // Shouldn't return Ok normally
                warn!("LISTEN loop exited unexpectedly, reconnecting...");
                backoff_secs = 1;
            }
            Err(e) => {
                error!("LISTEN error: {e:#}, reconnecting in {backoff_secs}s...");
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            }
        }

        // After reconnection, do a full sync to catch anything missed
        if let Err(e) = full_sync(&pool, &auth).await {
            error!("Post-reconnect sync failed: {e:#}");
        }
    }
}

async fn run_listener(pool: &PgPool, auth: &Arc<MultiUserAuthenticator>) -> anyhow::Result<()> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("vless_user_changed").await?;
    listener.listen("subscription_changed").await?;
    info!("Listening for DB notifications (vless_user_changed, subscription_changed)");

    loop {
        let notification = listener.recv().await?;
        match notification.channel() {
            "vless_user_changed" | "subscription_changed" => {
                // Both channels trigger a full registry re-sync.
                // vless_user_changed: a user's VLESS UUID was set/regenerated.
                // subscription_changed: a user's plan (speed limit) may have changed,
                //   or subscription expired → user should be removed from registry.
                if let Err(e) = full_sync(pool, auth).await {
                    error!("Sync after {} failed: {e:#}", notification.channel());
                }
            }
            other => {
                warn!("Unexpected notification channel: {other}");
            }
        }
    }
}

/// Background task: periodic full sync as safety net.
pub async fn periodic_sync_loop(
    pool: PgPool,
    auth: Arc<MultiUserAuthenticator>,
    interval_secs: u64,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    // First tick fires immediately, skip it since we already did initial sync.
    interval.tick().await;

    loop {
        interval.tick().await;
        if let Err(e) = full_sync(&pool, &auth).await {
            error!("Periodic registry sync failed: {e:#}");
        }
    }
}

/// Parse a UUID string (with or without dashes) into 16 bytes.
fn parse_vless_uuid(s: &str) -> Option<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }

    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}
