//! UUID registry with database synchronization.
//!
//! Keeps the in-memory VLESS UUID registry in sync with PostgreSQL via:
//! - **LISTEN/NOTIFY**: Real-time updates when users or subscriptions change
//! - **Periodic full sync**: Safety net for missed notifications

use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::{MultiUserAuthenticator, RegistryUser};

/// Load all users with VLESS UUIDs and active subscriptions into the authenticator.
pub async fn full_sync(pool: &PgPool, auth: &Arc<MultiUserAuthenticator>) -> anyhow::Result<()> {
    let rows = sqlx::query!(
        r#"
        SELECT u.id as user_id, u.vless_uuid, cs.speed_limit_mbps
        FROM users u
        JOIN current_subscriptions cs ON cs.user_id = u.id AND cs.is_active
        WHERE u.vless_uuid IS NOT NULL
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

        let uuid_bytes = match Uuid::parse_str(uuid_str) {
            Ok(u) => u.into_bytes(),
            Err(e) => {
                error!(
                    user_id = row.user_id,
                    uuid = uuid_str,
                    error = %e,
                    "Invalid VLESS UUID format"
                );
                continue;
            }
        };

        users.push(RegistryUser {
            uuid: uuid_bytes,
            user_id: row.user_id,
            speed_limit_mbps: row.speed_limit_mbps,
        });
    }

    let count = users.len();
    let evicted = auth.sync_users(users);
    if !evicted.is_empty() {
        let summary = crate::stats::record_traffic(&evicted);
        info!(
            users = summary.users,
            bytes_read = summary.bytes_read,
            bytes_written = summary.bytes_written,
            "Recorded traffic of users removed from the registry"
        );
    }

    info!(count, "Registry synced from database");
    Ok(())
}

/// Notification channels the registry subscribes to.
const LISTEN_CHANNELS: [&str; 2] = ["vless_user_changed", "subscription_changed"];

/// Open a dedicated LISTEN connection subscribed to all registry channels.
///
/// Call this BEFORE the initial `full_sync`: anything committed between the sync
/// and the subscription would otherwise be missed until the periodic sync.
pub async fn connect_listener(pool: &PgPool) -> anyhow::Result<PgListener> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen_all(LISTEN_CHANNELS).await?;
    info!(channels = ?LISTEN_CHANNELS, "Listening for DB notifications");
    Ok(listener)
}

/// Background task: react to DB changes via LISTEN/NOTIFY.
///
/// `vless_user_changed` (a user's VLESS UUID was set/regenerated) and
/// `subscription_changed` (plan/speed limit changed, or the subscription expired)
/// both trigger a full registry re-sync.
///
/// Uses `try_recv` rather than `recv`: sqlx reconnects transparently on a dropped
/// connection but notifications sent meanwhile are gone, and `try_recv` reports
/// that as `Ok(None)` — our cue for a catch-up sync. On a hard error a new
/// listener is opened with backoff, and the catch-up sync runs only once LISTEN
/// is back in place so nothing slips between the two.
pub async fn listen_for_changes(
    mut listener: PgListener,
    pool: PgPool,
    auth: Arc<MultiUserAuthenticator>,
) {
    const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

    loop {
        match listener.try_recv().await {
            Ok(Some(notification)) => match notification.channel() {
                "vless_user_changed" | "subscription_changed" => {
                    if let Err(e) = full_sync(&pool, &auth).await {
                        error!("Sync after {} failed: {e:#}", notification.channel());
                    }
                }
                other => warn!("Unexpected notification channel: {other}"),
            },
            Ok(None) => {
                warn!("LISTEN connection was lost and re-established; resyncing");
                if let Err(e) = full_sync(&pool, &auth).await {
                    error!("Post-reconnect sync failed: {e:#}");
                }
            }
            Err(e) => {
                error!("LISTEN error: {e:#}, reconnecting...");
                let mut backoff = std::time::Duration::from_secs(1);
                loop {
                    tokio::time::sleep(backoff).await;
                    match connect_listener(&pool).await {
                        Ok(new_listener) => {
                            listener = new_listener;
                            info!("LISTEN reconnected");
                            if let Err(e) = full_sync(&pool, &auth).await {
                                error!("Post-reconnect sync failed: {e:#}");
                            }
                            break;
                        }
                        Err(e) => warn!(
                            "LISTEN reconnect failed: {e:#}, retrying in {}s",
                            backoff.as_secs()
                        ),
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
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
