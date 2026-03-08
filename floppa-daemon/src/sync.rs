use anyhow::Result;
use chrono::Utc;
use floppa_core::{Config, DbPool};
use sqlx::postgres::PgListener;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Main synchronization loop using PostgreSQL LISTEN/NOTIFY
/// - Listens for 'peer_changed' notifications for immediate sync
/// - Periodic sync for traffic stats and expired subscriptions
pub async fn run_sync_loop(pool: &DbPool, config: &Config) -> Result<()> {
    // Initialize traffic control if enabled
    if let Some(ref rate_limit) = config.wireguard.rate_limit
        && rate_limit.enabled
    {
        info!("Initializing traffic control");
        crate::tc::setup_tc(&config.wireguard.interface, rate_limit.total_bandwidth_mbps)?;
    }

    // Initial sync on startup
    info!("Running initial sync");
    sync_peers(pool, config).await?;

    // Reapply rate limits for active peers (tc rules are ephemeral)
    reapply_rate_limits(pool, config).await?;

    // Spawn listener task
    let pool_clone = pool.clone();
    let config_clone = config.clone();
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_changes(&pool_clone, &config_clone).await {
            error!(error = %e, "Listener task failed");
        }
    });

    // Periodic tasks (traffic stats, subscription checks)
    let periodic_handle = tokio::spawn({
        let pool = pool.clone();
        let config = config.clone();
        async move {
            let interval = Duration::from_secs(15);
            // In-memory cache of last-seen WireGuard counters per public_key.
            // Used to compute deltas so that DB counters survive WireGuard restarts.
            // Seed with current WG values so the first cycle computes a zero delta
            // instead of treating all accumulated counters as new traffic.
            let mut prev_wg_counters: HashMap<String, (u64, u64)> = HashMap::new();
            if let Ok(stats) = crate::wg::get_peer_stats(&config.wireguard.interface) {
                for (public_key, tx, rx, _) in stats {
                    prev_wg_counters.insert(public_key, (tx, rx));
                }
            }
            // Map public_key → (user_id, peer_id) for metrics labels
            let mut peer_user_map = load_peer_user_map(&pool).await.unwrap_or_default();
            loop {
                tokio::time::sleep(interval).await;
                if let Err(e) =
                    periodic_sync(&pool, &config, &mut prev_wg_counters, &peer_user_map).await
                {
                    error!(error = %e, "Periodic sync failed");
                }
                // Refresh the map periodically (cheap query)
                if let Ok(map) = load_peer_user_map(&pool).await {
                    peer_user_map = map;
                }
            }
        }
    });

    // Wait for either task to complete (they shouldn't under normal operation)
    tokio::select! {
        r = listener_handle => {
            error!("Listener task exited unexpectedly: {:?}", r);
        }
        r = periodic_handle => {
            error!("Periodic task exited unexpectedly: {:?}", r);
        }
    }

    Ok(())
}

/// Listen for PostgreSQL notifications and sync immediately
async fn listen_for_changes(pool: &DbPool, config: &Config) -> Result<()> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("peer_changed").await?;
    listener.listen("subscription_changed").await?;
    info!("Listening for peer_changed and subscription_changed notifications");

    loop {
        match listener.recv().await {
            Ok(notification) => {
                debug!(
                    channel = notification.channel(),
                    payload = ?notification.payload(),
                    "Received notification"
                );

                match notification.channel() {
                    "peer_changed" => {
                        if let Err(e) = sync_peers(pool, config).await {
                            error!(error = %e, "Failed to sync peers");
                        }
                    }
                    "subscription_changed" => {
                        // Payload is user_id
                        if let Ok(user_id) = notification.payload().parse::<i64>()
                            && let Err(e) = update_user_rate_limit(pool, config, user_id).await
                        {
                            error!(error = %e, user_id, "Failed to update rate limit");
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => {
                error!(error = %e, "Listener error, reconnecting...");
                let mut backoff = Duration::from_secs(1);
                loop {
                    tokio::time::sleep(backoff).await;
                    match PgListener::connect_with(pool).await {
                        Ok(mut new_listener) => {
                            if new_listener.listen("peer_changed").await.is_ok()
                                && new_listener.listen("subscription_changed").await.is_ok()
                            {
                                listener = new_listener;
                                info!("PgListener reconnected successfully");
                                // Catch up on any notifications missed during disconnection
                                if let Err(e) = sync_peers(pool, config).await {
                                    error!(error = %e, "Failed to sync peers after reconnect");
                                }
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                backoff_secs = backoff.as_secs(),
                                "PgListener reconnect failed, retrying..."
                            );
                        }
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}

/// Re-apply tc rate limits for all active peers.
/// Called on startup after tc infrastructure is (re)created, since tc rules
/// are ephemeral and don't survive daemon restarts.
async fn reapply_rate_limits(pool: &DbPool, config: &Config) -> Result<()> {
    let rate_limit_enabled = config
        .wireguard
        .rate_limit
        .as_ref()
        .map(|r| r.enabled)
        .unwrap_or(false);

    if !rate_limit_enabled {
        return Ok(());
    }

    let peers = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip AS "assigned_ip!",
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.sync_status = 'active'
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut applied = 0u32;
    for peer in &peers {
        if let Some(speed_limit) = peer.speed_limit_mbps {
            // Try update first (class may already exist from sync_peers),
            // fall back to add if the class doesn't exist yet
            let result = crate::tc::update_peer_limit(
                &config.wireguard.interface,
                &peer.assigned_ip,
                speed_limit as u32,
            )
            .or_else(|_| {
                crate::tc::add_peer_limit(
                    &config.wireguard.interface,
                    &peer.assigned_ip,
                    speed_limit as u32,
                )
            });
            if let Err(e) = result {
                error!(peer_id = peer.id, error = %e, "Failed to reapply rate limit");
            } else {
                applied += 1;
            }
        }
    }

    info!(
        total_active = peers.len(),
        rate_limited = applied,
        "Reapplied rate limits for active peers"
    );

    Ok(())
}

/// Sync pending peer additions/removals with WireGuard
async fn sync_peers(pool: &DbPool, config: &Config) -> Result<()> {
    let rate_limit_enabled = config
        .wireguard
        .rate_limit
        .as_ref()
        .map(|r| r.enabled)
        .unwrap_or(false);

    // Process pending additions
    let pending_add = sqlx::query!(
        r#"
        SELECT p.id, p.public_key AS "public_key!", p.assigned_ip AS "assigned_ip!", p.user_id,
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.sync_status = 'pending_add'
        "#,
    )
    .fetch_all(pool)
    .await?;

    for peer in pending_add {
        info!(peer_id = peer.id, ip = %peer.assigned_ip, "Adding peer to WireGuard");

        match crate::wg::add_peer(
            &config.wireguard.interface,
            &peer.public_key,
            &peer.assigned_ip,
        ) {
            Ok(()) => {
                // Apply rate limit if configured
                if rate_limit_enabled && let Some(speed_limit) = peer.speed_limit_mbps {
                    if let Err(e) = crate::tc::add_peer_limit(
                        &config.wireguard.interface,
                        &peer.assigned_ip,
                        speed_limit as u32,
                    ) {
                        error!(peer_id = peer.id, error = %e, "Failed to apply rate limit");
                    } else {
                        info!(peer_id = peer.id, speed_limit, "Rate limit applied");
                    }
                }

                sqlx::query!(
                    "UPDATE peers SET sync_status = 'active' WHERE id = $1",
                    peer.id
                )
                .execute(pool)
                .await?;
                info!(peer_id = peer.id, "Peer added successfully");
            }
            Err(e) => {
                error!(peer_id = peer.id, error = %e, "Failed to add peer");
            }
        }
    }

    // Process pending removals
    let pending_remove = sqlx::query!(
        r#"SELECT id, public_key AS "public_key!", assigned_ip AS "assigned_ip!" FROM peers WHERE sync_status = 'pending_remove'"#,
    )
    .fetch_all(pool)
    .await?;

    for peer in pending_remove {
        info!(peer_id = peer.id, "Removing peer from WireGuard");

        // Remove rate limit first (ignore errors - might not have one)
        if rate_limit_enabled {
            let _ = crate::tc::remove_peer_limit(&config.wireguard.interface, &peer.assigned_ip);
        }

        match crate::wg::remove_peer(&config.wireguard.interface, &peer.public_key) {
            Ok(()) => {
                sqlx::query!(
                    "UPDATE peers SET sync_status = 'removed' WHERE id = $1",
                    peer.id
                )
                .execute(pool)
                .await?;
                info!(peer_id = peer.id, "Peer removed successfully");
            }
            Err(e) => {
                error!(peer_id = peer.id, error = %e, "Failed to remove peer");
            }
        }
    }

    Ok(())
}

/// Periodic tasks: update traffic stats, check expired subscriptions
async fn periodic_sync(
    pool: &DbPool,
    config: &Config,
    prev_wg_counters: &mut HashMap<String, (u64, u64)>,
    peer_user_map: &HashMap<String, (i64, i64)>,
) -> Result<()> {
    update_traffic_stats(pool, config, prev_wg_counters, peer_user_map).await?;
    check_expired_subscriptions(pool).await?;
    Ok(())
}

/// Update rate limit for a user when their subscription changes
async fn update_user_rate_limit(pool: &DbPool, config: &Config, user_id: i64) -> Result<()> {
    let rate_limit_enabled = config
        .wireguard
        .rate_limit
        .as_ref()
        .map(|r| r.enabled)
        .unwrap_or(false);

    if !rate_limit_enabled {
        return Ok(());
    }

    // Get all active WireGuard peers and current speed limit from plan
    let peers = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip AS "assigned_ip!",
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.user_id = $1 AND p.sync_status = 'active'
        "#,
        user_id,
    )
    .fetch_all(pool)
    .await?;

    if peers.is_empty() {
        debug!(
            user_id,
            "No active WireGuard peers for user, skipping rate limit update"
        );
        return Ok(());
    }

    for peer in peers {
        match peer.speed_limit_mbps {
            Some(speed_limit) => {
                // Update or add rate limit
                // Try update first, if it fails (class doesn't exist), add new
                if crate::tc::update_peer_limit(
                    &config.wireguard.interface,
                    &peer.assigned_ip,
                    speed_limit as u32,
                )
                .is_err()
                {
                    // Class might not exist, try adding
                    crate::tc::add_peer_limit(
                        &config.wireguard.interface,
                        &peer.assigned_ip,
                        speed_limit as u32,
                    )?;
                }
                info!(
                    user_id,
                    peer_id = peer.id,
                    speed_limit,
                    "Updated rate limit"
                );
            }
            None => {
                // No speed limit (plan is unlimited or no active subscription)
                let _ =
                    crate::tc::remove_peer_limit(&config.wireguard.interface, &peer.assigned_ip);
                info!(
                    user_id,
                    peer_id = peer.id,
                    "Removed rate limit (unlimited plan)"
                );
            }
        }
    }

    Ok(())
}

/// Update traffic counters using delta-based accumulation.
///
/// WireGuard counters (`wg show dump`) reset to 0 on interface restart.
/// To keep DB counters as reliable lifetime totals, we track previous
/// WireGuard values in memory and add only the delta each cycle.
/// If new < old (counter reset), we treat the new value as the full delta.
///
async fn update_traffic_stats(
    pool: &DbPool,
    config: &Config,
    prev_wg_counters: &mut HashMap<String, (u64, u64)>,
    peer_user_map: &HashMap<String, (i64, i64)>,
) -> Result<()> {
    let stats = crate::wg::get_peer_stats(&config.wireguard.interface)?;

    for (public_key, wg_tx, wg_rx, last_handshake) in &stats {
        let (prev_tx, prev_rx) = prev_wg_counters.get(public_key).copied().unwrap_or((0, 0));

        // If wg counter < previous, the interface was restarted — treat current value as the delta
        let delta_tx = if *wg_tx >= prev_tx {
            *wg_tx - prev_tx
        } else {
            *wg_tx
        };
        let delta_rx = if *wg_rx >= prev_rx {
            *wg_rx - prev_rx
        } else {
            *wg_rx
        };

        prev_wg_counters.insert(public_key.clone(), (*wg_tx, *wg_rx));

        if delta_tx == 0 && delta_rx == 0 {
            // Still update last_handshake even if no traffic
            if last_handshake.is_some() {
                sqlx::query!(
                    "UPDATE peers SET last_handshake = $1 WHERE public_key = $2 AND sync_status = 'active'",
                    *last_handshake,
                    public_key,
                )
                .execute(pool)
                .await?;
            }
            continue;
        }

        // Record traffic in Prometheus counters (keyed by user_id + peer_id)
        if let Some(&(user_id, peer_id)) = peer_user_map.get(public_key) {
            let uid = user_id.to_string();
            let pid = peer_id.to_string();
            metrics::counter!("wg_tx_bytes_total", "user_id" => uid.clone(), "peer_id" => pid.clone())
                .increment(delta_tx);
            metrics::counter!("wg_rx_bytes_total", "user_id" => uid, "peer_id" => pid)
                .increment(delta_rx);
        }

        // Update last_handshake only (traffic tracking moved to VictoriaMetrics)
        if let Some(handshake) = last_handshake {
            sqlx::query!(
                "UPDATE peers SET last_handshake = $1 WHERE public_key = $2 AND sync_status = 'active'",
                *handshake,
                public_key,
            )
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// Load a mapping of public_key → (user_id, peer_id) for active peers.
async fn load_peer_user_map(pool: &DbPool) -> Result<HashMap<String, (i64, i64)>> {
    let rows = sqlx::query!(
        r#"SELECT public_key AS "public_key!", user_id, id AS peer_id FROM peers WHERE sync_status = 'active'"#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.public_key, (r.user_id, r.peer_id)))
        .collect())
}

async fn check_expired_subscriptions(pool: &DbPool) -> Result<()> {
    let now = Utc::now();

    // Find users with expired subscriptions and active peers
    let expired = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT p.id
        FROM peers p
        JOIN users u ON p.user_id = u.id
        WHERE p.sync_status = 'active'
        AND NOT EXISTS (
            SELECT 1 FROM subscriptions s
            WHERE s.user_id = u.id
            AND (s.expires_at IS NULL OR s.expires_at > $1)
        )
        "#,
        now,
    )
    .fetch_all(pool)
    .await?;

    for peer_id in expired {
        info!(
            peer_id = peer_id,
            "Marking peer for removal (subscription expired)"
        );
        sqlx::query!(
            "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1",
            peer_id
        )
        .execute(pool)
        .await?;
        // This will trigger notification via the DB trigger
    }

    Ok(())
}
