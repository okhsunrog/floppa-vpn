use crate::wg::{PeerStat, WgTool};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use floppa_core::{Config, DbPool, Protocol};
use sqlx::postgres::PgListener;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Runtime parameters for one protocol's interface. WireGuard is always present;
/// AmneziaWG is added when `[amneziawg]` is configured. Each peer is routed to its
/// target by its `protocol` column.
struct ProtoTarget {
    protocol: Protocol,
    tool: WgTool,
    interface: String,
    rate_limit_enabled: bool,
    total_bandwidth_mbps: u32,
}

/// Everything the sync functions need: the DB pool and the per-protocol interface
/// targets, built once from config.
struct SyncContext {
    pool: DbPool,
    targets: Vec<ProtoTarget>,
    /// `pending_add` peers already reported as having no configured interface, so the
    /// periodic sync does not repeat the warning every 15 s for as long as they sit there.
    unroutable_reported: Mutex<HashSet<i64>>,
}

impl SyncContext {
    fn new(pool: DbPool, config: &Config) -> Self {
        let mut targets = vec![ProtoTarget {
            protocol: Protocol::WireGuard,
            tool: WgTool::Wg,
            interface: config.wireguard.interface.clone(),
            rate_limit_enabled: config
                .wireguard
                .rate_limit
                .as_ref()
                .map(|r| r.enabled)
                .unwrap_or(false),
            total_bandwidth_mbps: config
                .wireguard
                .rate_limit
                .as_ref()
                .map(|r| r.total_bandwidth_mbps)
                .unwrap_or(1000),
        }];

        if let Some(awg) = &config.amneziawg {
            targets.push(ProtoTarget {
                protocol: Protocol::AmneziaWg,
                tool: WgTool::Awg,
                interface: awg.interface.clone(),
                rate_limit_enabled: awg.rate_limit.as_ref().map(|r| r.enabled).unwrap_or(false),
                total_bandwidth_mbps: awg
                    .rate_limit
                    .as_ref()
                    .map(|r| r.total_bandwidth_mbps)
                    .unwrap_or(1000),
            });
        }

        Self {
            pool,
            targets,
            unroutable_reported: Mutex::new(HashSet::new()),
        }
    }

    /// The interface target for a peer's protocol, if that protocol is configured.
    fn target(&self, protocol: Protocol) -> Option<&ProtoTarget> {
        self.targets.iter().find(|t| t.protocol == protocol)
    }

    /// Warn that a `pending_add` peer's protocol has no interface on this host — once per
    /// peer, since it stays pending (and gets re-read by every sync) until the protocol is
    /// configured or the peer is removed.
    fn warn_unroutable_once(&self, peer_id: i64, protocol: Protocol) {
        let first_time = self
            .unroutable_reported
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(peer_id);
        if first_time {
            warn!(
                peer_id,
                protocol = protocol.as_db_str(),
                "No configured interface for peer protocol — peer stays pending_add"
            );
        }
    }

    /// Whether any interface has tc rate limiting enabled.
    fn any_rate_limited(&self) -> bool {
        self.targets.iter().any(|t| t.rate_limit_enabled)
    }
}

/// Last-seen `wg show dump` byte counters for one peer.
#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    rx_bytes: u64,
    tx_bytes: u64,
}

impl From<&PeerStat> for Counters {
    fn from(stat: &PeerStat) -> Self {
        Self {
            rx_bytes: stat.rx_bytes,
            tx_bytes: stat.tx_bytes,
        }
    }
}

/// Notification channels the daemon subscribes to.
const LISTEN_CHANNELS: [&str; 2] = ["peer_changed", "subscription_changed"];

/// Main synchronization loop using PostgreSQL LISTEN/NOTIFY
/// - Listens for 'peer_changed' notifications for immediate sync
/// - Periodic sync for traffic stats and expired subscriptions
pub async fn run_sync_loop(pool: &DbPool, config: &Config) -> Result<()> {
    let ctx = Arc::new(SyncContext::new(pool.clone(), config));

    // Initialize traffic control if enabled (per protocol interface)
    for target in &ctx.targets {
        if target.rate_limit_enabled {
            info!(interface = %target.interface, "Initializing traffic control");
            crate::tc::setup_tc(&target.interface, target.total_bandwidth_mbps)?;
        }
    }

    // Subscribe BEFORE the initial sync: a peer created while we reconcile would
    // otherwise fire a NOTIFY nobody is listening to, and stay pending until the
    // next periodic sync.
    let listener = connect_listener(&ctx.pool).await?;

    // Initial sync on startup: first put every `active` peer back onto its
    // interface (WireGuard/AmneziaWG interfaces don't survive a reboot and may
    // be recreated empty; `sync_peers` only looks at pending rows), then apply
    // pending changes, then re-create the ephemeral tc limits.
    info!("Running initial sync");
    reconcile_active_peers(&ctx).await?;
    sync_peers(&ctx).await?;
    reapply_rate_limits(&ctx).await?;

    // Spawn listener task
    let listener_handle = tokio::spawn({
        let ctx = ctx.clone();
        async move {
            if let Err(e) = listen_for_changes(listener, &ctx).await {
                error!(error = %e, "Listener task failed");
            }
        }
    });

    // Periodic tasks (pending peers safety net, traffic stats, subscription checks)
    let periodic_handle = tokio::spawn({
        let ctx = ctx.clone();
        async move {
            let interval = Duration::from_secs(15);
            // In-memory cache of last-seen WireGuard counters per public_key.
            // Used to compute deltas so that DB counters survive WireGuard restarts.
            // Seed with current WG values so the first cycle computes a zero delta
            // instead of treating all accumulated counters as new traffic.
            let mut prev_wg_counters: HashMap<String, Counters> = HashMap::new();
            for target in &ctx.targets {
                if let Ok(stats) = crate::wg::get_peer_stats(target.tool, &target.interface) {
                    for stat in &stats {
                        prev_wg_counters.insert(stat.public_key.clone(), Counters::from(stat));
                    }
                }
            }
            // Map public_key → (user_id, peer_id) for metrics labels
            let mut peer_user_map = load_peer_user_map(&ctx.pool).await.unwrap_or_default();
            loop {
                tokio::time::sleep(interval).await;
                if let Err(e) = periodic_sync(&ctx, &mut prev_wg_counters, &mut peer_user_map).await
                {
                    error!(error = %e, "Periodic sync failed");
                }
            }
        }
    });

    // Neither task returns under normal operation. If one does, the daemon is
    // half-dead (no NOTIFY handling, or no stats/expiry), so fail loudly and let
    // systemd restart us rather than idling with a zero exit code.
    tokio::select! {
        r = listener_handle => Err(anyhow!("listener task exited unexpectedly: {r:?}")),
        r = periodic_handle => Err(anyhow!("periodic task exited unexpectedly: {r:?}")),
    }
}

/// Open a dedicated LISTEN connection subscribed to all daemon channels.
async fn connect_listener(pool: &DbPool) -> Result<PgListener> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen_all(LISTEN_CHANNELS).await?;
    info!(channels = ?LISTEN_CHANNELS, "Listening for notifications");
    Ok(listener)
}

/// Catch up on everything that may have happened while no LISTEN connection was
/// open: pending peers and (ephemeral) tc limits that a missed
/// `subscription_changed` would have updated.
async fn resync_after_reconnect(ctx: &SyncContext) {
    if let Err(e) = sync_peers(ctx).await {
        error!(error = %e, "Failed to sync peers after reconnect");
    }
    if let Err(e) = reapply_rate_limits(ctx).await {
        error!(error = %e, "Failed to reapply rate limits after reconnect");
    }
}

/// Listen for PostgreSQL notifications and sync immediately.
///
/// Uses `try_recv` rather than `recv`: sqlx reconnects transparently on a dropped
/// connection, but notifications sent while it was down are gone for good.
/// `try_recv` surfaces that as `Ok(None)`, which is our cue for a full resync.
async fn listen_for_changes(mut listener: PgListener, ctx: &SyncContext) -> Result<()> {
    loop {
        match listener.try_recv().await {
            Ok(Some(notification)) => {
                debug!(
                    channel = notification.channel(),
                    payload = ?notification.payload(),
                    "Received notification"
                );

                match notification.channel() {
                    "peer_changed" => {
                        if let Err(e) = sync_peers(ctx).await {
                            error!(error = %e, "Failed to sync peers");
                        }
                    }
                    "subscription_changed" => {
                        // Payload is user_id
                        if let Ok(user_id) = notification.payload().parse::<i64>()
                            && let Err(e) = update_user_rate_limit(ctx, user_id).await
                        {
                            error!(error = %e, user_id, "Failed to update rate limit");
                        }
                    }
                    _ => {}
                }
            }
            Ok(None) => {
                warn!("PgListener connection was lost and re-established; resyncing");
                resync_after_reconnect(ctx).await;
            }
            Err(e) => {
                error!(error = %e, "Listener error, reconnecting...");
                let mut backoff = Duration::from_secs(1);
                loop {
                    tokio::time::sleep(backoff).await;
                    match connect_listener(&ctx.pool).await {
                        Ok(new_listener) => {
                            listener = new_listener;
                            info!("PgListener reconnected successfully");
                            resync_after_reconnect(ctx).await;
                            break;
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

/// Public key → allowed-ips of every peer currently on an interface.
type LivePeers = HashMap<String, Vec<String>>;

/// Whether an active peer needs a `set peer` to match the DB: it is missing from the
/// interface, or is there with different allowed-ips (`add_peer` always writes `<ip>/32`).
fn peer_needs_set(live: &LivePeers, public_key: &str, assigned_ip: &str) -> bool {
    live.get(public_key)
        .is_none_or(|allowed| allowed.len() != 1 || allowed[0] != format!("{assigned_ip}/32"))
}

/// Put every `active` peer that is missing (or differs) back onto its protocol interface.
///
/// `sync_peers` only acts on `pending_add`/`pending_remove` rows, so after an
/// interface is recreated empty (reboot, manual recreation, or the kernel module
/// reloading) the peers still marked `active` in the DB would never be re-added
/// and silently stop working. The interface is read first (`wg/awg show dump`)
/// and only peers absent from it, or present with other allowed-ips, get a
/// `set peer` — so a restart against an interface that already matches issues
/// none. That matters for AmneziaWG, where every `awg set` re-runs the kernel
/// module's `jp_spec_setup` (see [`crate::wg::ensure_interface`]). If the dump
/// cannot be read, every peer is (re)added: `set peer` is idempotent.
async fn reconcile_active_peers(ctx: &SyncContext) -> Result<()> {
    let peers = sqlx::query!(
        r#"SELECT id, public_key AS "public_key!", assigned_ip AS "assigned_ip!",
                  protocol AS "protocol: Protocol"
           FROM peers WHERE sync_status = 'active'"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    // Interface name → what is on it right now. An interface that could not be read is
    // absent from the map, which makes every peer on it look missing.
    let mut live: HashMap<&str, LivePeers> = HashMap::new();
    for target in &ctx.targets {
        match crate::wg::get_peer_stats(target.tool, &target.interface) {
            Ok(stats) => {
                live.insert(
                    target.interface.as_str(),
                    stats
                        .into_iter()
                        .map(|s| (s.public_key, s.allowed_ips))
                        .collect(),
                );
            }
            Err(e) => warn!(
                interface = %target.interface,
                error = %e,
                "Could not read the interface's peers; re-adding every active peer on it"
            ),
        }
    }

    let mut reconciled = 0u32;
    let mut in_sync = 0u32;
    for peer in &peers {
        let Some(target) = ctx.target(peer.protocol) else {
            error!(
                peer_id = peer.id,
                protocol = peer.protocol.as_db_str(),
                "No configured interface for peer protocol — skipping reconcile"
            );
            continue;
        };
        if live
            .get(target.interface.as_str())
            .is_some_and(|on_iface| !peer_needs_set(on_iface, &peer.public_key, &peer.assigned_ip))
        {
            in_sync += 1;
            continue;
        }
        match crate::wg::add_peer(
            target.tool,
            &target.interface,
            &peer.public_key,
            &peer.assigned_ip,
        ) {
            Ok(()) => reconciled += 1,
            Err(e) => error!(peer_id = peer.id, error = %e, "Failed to reconcile active peer"),
        }
    }

    info!(
        total_active = peers.len(),
        in_sync, reconciled, "Reconciled active peers onto interfaces"
    );
    Ok(())
}

/// Re-apply tc rate limits for all active peers.
/// Called on startup after tc infrastructure is (re)created, since tc rules
/// are ephemeral and don't survive daemon restarts.
async fn reapply_rate_limits(ctx: &SyncContext) -> Result<()> {
    if !ctx.any_rate_limited() {
        return Ok(());
    }

    let peers = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip AS "assigned_ip!", p.protocol AS "protocol: Protocol",
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.sync_status = 'active'
        "#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let mut applied = 0u32;
    for peer in &peers {
        let Some(target) = ctx.target(peer.protocol) else {
            continue;
        };
        if !target.rate_limit_enabled {
            continue;
        }
        if let Some(speed_limit) = peer.speed_limit_mbps {
            match crate::tc::set_peer_limit(
                &target.interface,
                &peer.assigned_ip,
                speed_limit as u32,
            ) {
                Ok(()) => applied += 1,
                Err(e) => error!(peer_id = peer.id, error = %e, "Failed to reapply rate limit"),
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

/// Sync pending peer additions/removals with WireGuard / AmneziaWG.
/// Each peer is routed to its protocol's interface.
async fn sync_peers(ctx: &SyncContext) -> Result<()> {
    // Process pending additions
    let pending_add = sqlx::query!(
        r#"
        SELECT p.id, p.public_key AS "public_key!", p.assigned_ip AS "assigned_ip!", p.user_id,
               p.protocol AS "protocol: Protocol",
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.sync_status = 'pending_add'
        "#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for peer in pending_add {
        let Some(target) = ctx.target(peer.protocol) else {
            ctx.warn_unroutable_once(peer.id, peer.protocol);
            continue;
        };

        info!(peer_id = peer.id, ip = %peer.assigned_ip, interface = %target.interface, "Adding peer");

        if let Err(e) = crate::wg::add_peer(
            target.tool,
            &target.interface,
            &peer.public_key,
            &peer.assigned_ip,
        ) {
            error!(peer_id = peer.id, error = %e, "Failed to add peer");
            continue;
        }

        // The peer is only `active` once both the interface and tc agree. If the
        // limit can't be applied, take the peer back off the interface — it was
        // just added and would otherwise run unlimited until the retry — and leave
        // it `pending_add` for the next sync (add_peer / remove_peer /
        // add_peer_limit are all idempotent).
        if target.rate_limit_enabled
            && let Some(speed_limit) = peer.speed_limit_mbps
        {
            if let Err(e) =
                crate::tc::add_peer_limit(&target.interface, &peer.assigned_ip, speed_limit as u32)
            {
                error!(
                    peer_id = peer.id,
                    error = %e,
                    "Failed to apply rate limit; removing the peer again, stays pending_add"
                );
                if let Err(e) =
                    crate::wg::remove_peer(target.tool, &target.interface, &peer.public_key)
                {
                    error!(
                        peer_id = peer.id,
                        error = %e,
                        "Failed to remove the unlimited peer from the interface"
                    );
                }
                continue;
            }
            info!(peer_id = peer.id, speed_limit, "Rate limit applied");
        }

        match sqlx::query!(
            "UPDATE peers SET sync_status = 'active' WHERE id = $1",
            peer.id
        )
        .execute(&ctx.pool)
        .await
        {
            Ok(_) => info!(peer_id = peer.id, "Peer added successfully"),
            Err(e) => error!(peer_id = peer.id, error = %e, "Failed to mark peer active"),
        }
    }

    // Process pending removals
    let pending_remove = sqlx::query!(
        r#"SELECT id, public_key AS "public_key!", assigned_ip AS "assigned_ip!",
                  protocol AS "protocol: Protocol"
           FROM peers WHERE sync_status = 'pending_remove'"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for peer in pending_remove {
        let Some(target) = ctx.target(peer.protocol) else {
            // Never added on this host, so there is nothing to tear down; leaving the row
            // pending would keep its IP and key reserved forever.
            warn!(
                peer_id = peer.id,
                protocol = peer.protocol.as_db_str(),
                "No configured interface for peer protocol — nothing to tear down, marking removed"
            );
            mark_peer_removed(&ctx.pool, peer.id).await;
            continue;
        };

        info!(peer_id = peer.id, interface = %target.interface, "Removing peer");

        // Remove rate limit first (ignore errors - might not have one)
        if target.rate_limit_enabled {
            let _ = crate::tc::remove_peer_limit(&target.interface, &peer.assigned_ip);
        }

        if let Err(e) = crate::wg::remove_peer(target.tool, &target.interface, &peer.public_key) {
            error!(peer_id = peer.id, error = %e, "Failed to remove peer");
            continue;
        }

        mark_peer_removed(&ctx.pool, peer.id).await;
    }

    Ok(())
}

/// Flip a `pending_remove` row to `removed`, logging either way: a failed UPDATE just leaves
/// the peer for the next sync to retry.
async fn mark_peer_removed(pool: &DbPool, peer_id: i64) {
    match sqlx::query!(
        "UPDATE peers SET sync_status = 'removed' WHERE id = $1",
        peer_id
    )
    .execute(pool)
    .await
    {
        Ok(_) => info!(peer_id, "Peer removed successfully"),
        Err(e) => error!(peer_id, error = %e, "Failed to mark peer removed"),
    }
}

/// Periodic tasks: pick up pending peers whose NOTIFY was missed, update traffic
/// stats, check expired subscriptions.
async fn periodic_sync(
    ctx: &SyncContext,
    prev_wg_counters: &mut HashMap<String, Counters>,
    peer_user_map: &mut HashMap<String, (i64, i64)>,
) -> Result<()> {
    sync_peers(ctx).await?;
    // Refresh the label map BEFORE reading counters: a peer that just went active
    // would otherwise have its first tick's traffic computed (and its prev value
    // stored) while it is still missing from the map, losing that delta for good.
    match load_peer_user_map(&ctx.pool).await {
        Ok(map) => *peer_user_map = map,
        Err(e) => warn!(error = %e, "Failed to refresh peer→user map; using previous"),
    }
    update_traffic_stats(ctx, prev_wg_counters, peer_user_map).await?;
    check_expired_subscriptions(&ctx.pool).await?;
    Ok(())
}

/// Update rate limit for a user when their subscription changes
async fn update_user_rate_limit(ctx: &SyncContext, user_id: i64) -> Result<()> {
    if !ctx.any_rate_limited() {
        return Ok(());
    }

    // Get all active peers (any protocol) and current speed limit from plan
    let peers = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip AS "assigned_ip!", p.protocol AS "protocol: Protocol",
               pl.default_speed_limit_mbps AS speed_limit_mbps
        FROM peers p
        LEFT JOIN subscriptions s ON s.user_id = p.user_id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LEFT JOIN plans pl ON s.plan_id = pl.id
        WHERE p.user_id = $1 AND p.sync_status = 'active'
        "#,
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    if peers.is_empty() {
        debug!(
            user_id,
            "No active peers for user, skipping rate limit update"
        );
        return Ok(());
    }

    for peer in peers {
        let Some(target) = ctx.target(peer.protocol) else {
            continue;
        };
        if !target.rate_limit_enabled {
            continue;
        }
        match peer.speed_limit_mbps {
            Some(speed_limit) => {
                crate::tc::set_peer_limit(
                    &target.interface,
                    &peer.assigned_ip,
                    speed_limit as u32,
                )?;
                info!(
                    user_id,
                    peer_id = peer.id,
                    speed_limit,
                    "Updated rate limit"
                );
            }
            None => {
                // No speed limit (plan is unlimited or no active subscription)
                let _ = crate::tc::remove_peer_limit(&target.interface, &peer.assigned_ip);
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
    ctx: &SyncContext,
    prev_wg_counters: &mut HashMap<String, Counters>,
    peer_user_map: &HashMap<String, (i64, i64)>,
) -> Result<()> {
    // Collect stats from every protocol interface; public keys are unique across protocols,
    // so the prev-counter map and peer→user map stay keyed by public_key.
    let mut stats: Vec<PeerStat> = Vec::new();
    let mut all_interfaces_read = true;
    for target in &ctx.targets {
        match crate::wg::get_peer_stats(target.tool, &target.interface) {
            Ok(mut s) => stats.append(&mut s),
            Err(e) => {
                all_interfaces_read = false;
                debug!(interface = %target.interface, error = %e, "Failed to read peer stats")
            }
        }
    }

    // Peers removed from the interfaces (or with the interface gone) must not pile
    // up in the cache. Only prune when every interface answered: after a transient
    // read failure the next successful read would otherwise count all of a peer's
    // lifetime bytes as fresh delta.
    if all_interfaces_read {
        let seen: HashSet<&str> = stats.iter().map(|s| s.public_key.as_str()).collect();
        prev_wg_counters.retain(|k, _| seen.contains(k.as_str()));
    }

    let mut handshake_keys: Vec<String> = Vec::new();
    let mut handshake_times: Vec<DateTime<Utc>> = Vec::new();

    for stat in &stats {
        let prev = prev_wg_counters
            .get(&stat.public_key)
            .copied()
            .unwrap_or_default();

        // If a wg counter < previous, the interface was restarted — treat the current value as the delta
        let delta_of = |now: u64, before: u64| if now >= before { now - before } else { now };
        let delta_tx = delta_of(stat.tx_bytes, prev.tx_bytes);
        let delta_rx = delta_of(stat.rx_bytes, prev.rx_bytes);

        prev_wg_counters.insert(stat.public_key.clone(), Counters::from(stat));

        // Record traffic in Prometheus counters (keyed by user_id + peer_id)
        if (delta_tx > 0 || delta_rx > 0)
            && let Some(&(user_id, peer_id)) = peer_user_map.get(&stat.public_key)
        {
            let uid = user_id.to_string();
            let pid = peer_id.to_string();
            metrics::counter!("wg_tx_bytes_total", "user_id" => uid.clone(), "peer_id" => pid.clone())
                .increment(delta_tx);
            metrics::counter!("wg_rx_bytes_total", "user_id" => uid, "peer_id" => pid)
                .increment(delta_rx);
        }

        if let Some(handshake) = stat.last_handshake {
            handshake_keys.push(stat.public_key.clone());
            handshake_times.push(handshake);
        }
    }

    // One statement per tick instead of one per peer; rows whose handshake is
    // unchanged are skipped by IS DISTINCT FROM (traffic itself lives in VictoriaMetrics).
    if !handshake_keys.is_empty() {
        sqlx::query!(
            r#"
            UPDATE peers p
            SET last_handshake = v.last_handshake
            FROM UNNEST($1::text[], $2::timestamptz[]) AS v(public_key, last_handshake)
            WHERE p.public_key = v.public_key
              AND p.sync_status = 'active'
              AND p.last_handshake IS DISTINCT FROM v.last_handshake
            "#,
            &handshake_keys,
            &handshake_times,
        )
        .execute(&ctx.pool)
        .await?;
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

    // Find peers of users without a valid subscription. pending_add is included:
    // a peer stuck there (interface down at the time) must not be let through
    // once the interface is back if its subscription has meanwhile expired.
    let expired = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT p.id
        FROM peers p
        JOIN users u ON p.user_id = u.id
        WHERE p.sync_status IN ('active', 'pending_add')
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wg::parse_wg_dump;

    fn live_from(dump: &str) -> LivePeers {
        parse_wg_dump(dump)
            .unwrap()
            .into_iter()
            .map(|s| (s.public_key, s.allowed_ips))
            .collect()
    }

    #[test]
    fn matching_peers_need_no_set() {
        let live = live_from(crate::wg::tests::DUMP);
        assert!(!peer_needs_set(&live, "cGVlcjE=", "10.100.0.2"));
        assert!(!peer_needs_set(&live, "cGVlcjI=", "10.100.0.3"));
    }

    #[test]
    fn missing_or_differing_peers_need_a_set() {
        let live = live_from(crate::wg::tests::DUMP);
        // Not on the interface at all
        assert!(peer_needs_set(&live, "bm9wZQ==", "10.100.0.9"));
        // Present with another address
        assert!(peer_needs_set(&live, "cGVlcjE=", "10.100.0.7"));
        // Present with no allowed-ips
        assert!(peer_needs_set(&live, "cGVlcjM=", "10.100.0.4"));
        // Present with an extra network
        let extra = live_from(
            "k\tk\t51820\toff\npk\t(none)\t(none)\t10.100.0.2/32,10.100.1.0/24\t0\t0\t0\toff\n",
        );
        assert!(peer_needs_set(&extra, "pk", "10.100.0.2"));
    }

    #[test]
    fn empty_interface_needs_everything() {
        let live = live_from("k\tk\t51820\toff\n");
        assert!(peer_needs_set(&live, "cGVlcjE=", "10.100.0.2"));
    }
}
