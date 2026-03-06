//! Shared business logic used by both bot and admin.

use crate::error::{FloppaError, Result};
use crate::{Config, DbPool, encrypt_private_key};
use chrono::{Duration, Utc};
use ipnetwork::Ipv4Network;
use std::collections::HashSet;
use std::net::Ipv4Addr;

/// Result of user upsert operation.
pub struct UpsertResult {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub photo_url: Option<String>,
    pub is_admin: bool,
    /// Whether a trial subscription was auto-granted on this call.
    pub trial_granted: bool,
}

/// Profile fields from Telegram auth sources.
#[derive(Default)]
pub struct TelegramProfile<'a> {
    pub first_name: Option<&'a str>,
    pub last_name: Option<&'a str>,
    pub photo_url: Option<&'a str>,
}

/// Upsert a Telegram user and auto-grant a basic trial subscription if they haven't used one.
///
/// - Inserts or updates the user row.
/// - If `trial_used_at` is NULL, finds the "basic" plan and creates a 7-day subscription.
pub async fn upsert_user(
    pool: &DbPool,
    telegram_id: i64,
    username: Option<&str>,
    profile: TelegramProfile<'_>,
    is_admin_from_config: bool,
) -> Result<UpsertResult> {
    let row = sqlx::query!(
        r#"
        INSERT INTO users (telegram_id, username, first_name, last_name, photo_url, is_admin)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (telegram_id) DO UPDATE SET
            username = $2,
            first_name = COALESCE($3, users.first_name),
            last_name = COALESCE($4, users.last_name),
            photo_url = COALESCE($5, users.photo_url),
            is_admin = users.is_admin OR $6
        RETURNING id, username, first_name, last_name, photo_url, is_admin, trial_used_at
        "#,
        telegram_id,
        username,
        profile.first_name,
        profile.last_name,
        profile.photo_url,
        is_admin_from_config,
    )
    .fetch_one(pool)
    .await?;

    let mut trial_granted = false;

    if row.trial_used_at.is_none() {
        // Atomically claim trial — only one concurrent request can succeed
        let claimed = sqlx::query!(
            "UPDATE users SET trial_used_at = NOW() WHERE id = $1 AND trial_used_at IS NULL",
            row.id,
        )
        .execute(pool)
        .await?;

        if claimed.rows_affected() == 1 {
            let basic_plan = sqlx::query!("SELECT id, trial_days FROM plans WHERE name = 'basic'")
                .fetch_optional(pool)
                .await?;

            if let Some(plan) = basic_plan {
                let days = plan.trial_days.unwrap_or(7) as i64;
                let now = Utc::now();
                let expires_at = now + Duration::days(days);

                sqlx::query!(
                    "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'trial')",
                    row.id,
                    plan.id,
                    now,
                    expires_at,
                )
                .execute(pool)
                .await?;

                trial_granted = true;
            }
        }
    }

    Ok(UpsertResult {
        id: row.id,
        username: row.username,
        first_name: row.first_name,
        last_name: row.last_name,
        photo_url: row.photo_url,
        is_admin: row.is_admin,
        trial_granted,
    })
}

/// Optional device info when creating a peer from the client app.
pub struct CreatePeerOptions {
    pub device_name: Option<String>,
    pub device_id: Option<String>,
}

/// Result of peer creation.
pub struct CreatePeerResult {
    pub id: i64,
    pub assigned_ip: String,
    pub private_key_plaintext: String,
    pub config: String,
}

/// Create a new WireGuard peer for a user.
///
/// Checks subscription + peer limit, generates keypair, encrypts private key,
/// allocates IP, inserts peer, and returns the generated config.
/// Uses a transaction with FOR UPDATE to prevent concurrent peer limit violations.
pub async fn create_peer(
    pool: &DbPool,
    user_id: i64,
    config: &Config,
    encryption_key: &[u8; 32],
    wg_public_key: &str,
    options: Option<CreatePeerOptions>,
) -> Result<CreatePeerResult> {
    // Generate keypair and encrypt outside the transaction (CPU-bound)
    let (private_key, public_key) = crate::wg_keys::generate_keypair()
        .map_err(|e| FloppaError::KeyGeneration(e.to_string()))?;

    let encrypted_private_key = encrypt_private_key(private_key.as_base64(), encryption_key)
        .map_err(|e| FloppaError::Encryption(e.to_string()))?;

    let (device_name, device_id) = match &options {
        Some(opts) => (opts.device_name.as_deref(), opts.device_id.as_deref()),
        None => (None, None),
    };

    // Transaction: check limit + allocate IP + insert peer atomically
    let mut tx = pool.begin().await?;

    // Lock the subscription row to serialize concurrent peer creations for this user
    let sub_info = sqlx::query!(
        r#"
        SELECT p.max_peers, (SELECT COUNT(*) FROM peers WHERE user_id = $1 AND sync_status != 'removed')::int AS current_peers
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
        ORDER BY s.expires_at DESC NULLS FIRST
        LIMIT 1
        FOR UPDATE OF s
        "#,
        user_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let sub = sub_info.ok_or(FloppaError::NoActiveSubscription)?;
    let (max_peers, current_peers) = (sub.max_peers, sub.current_peers.unwrap_or(0));

    if current_peers >= max_peers {
        return Err(FloppaError::PeerLimitReached {
            current: current_peers,
            max: max_peers,
        });
    }

    // Allocate IP within the transaction
    let assigned_ip = allocate_ip_tx(&mut tx, &config.wireguard.client_subnet).await?;

    let peer_id = sqlx::query_scalar!(
        r#"
        INSERT INTO peers (user_id, public_key, private_key_encrypted, assigned_ip, sync_status, device_name, device_id)
        VALUES ($1, $2, $3, $4, 'pending_add', $5, $6)
        RETURNING id
        "#,
        user_id,
        public_key.as_base64(),
        &encrypted_private_key,
        &assigned_ip,
        device_name,
        device_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let wg_config =
        generate_wg_config(private_key.as_base64(), &assigned_ip, config, wg_public_key);

    Ok(CreatePeerResult {
        id: peer_id,
        assigned_ip,
        private_key_plaintext: private_key.as_base64().to_string(),
        config: wg_config,
    })
}

/// Allocate the next available IP address from the WireGuard subnet.
pub async fn allocate_ip(pool: &DbPool, subnet: &str) -> Result<String> {
    allocate_ip_inner(pool, subnet).await
}

/// Allocate IP within a transaction.
async fn allocate_ip_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subnet: &str,
) -> Result<String> {
    allocate_ip_inner(&mut **tx, subnet).await
}

// Kept as runtime query because it uses a generic executor (pool or transaction)
async fn allocate_ip_inner<'e, E>(executor: E, subnet: &str) -> Result<String>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let network: Ipv4Network = subnet.parse().map_err(|_| FloppaError::NoAvailableIps)?;

    let assigned: Vec<String> =
        sqlx::query_scalar("SELECT assigned_ip FROM peers WHERE sync_status != 'removed'")
            .fetch_all(executor)
            .await?;

    let assigned_set: HashSet<Ipv4Addr> =
        assigned.iter().filter_map(|ip| ip.parse().ok()).collect();

    // Skip network address and gateway (first two), exclude broadcast (last)
    for ip in network.iter().skip(2) {
        if ip == network.broadcast() {
            break;
        }
        if !assigned_set.contains(&ip) {
            return Ok(ip.to_string());
        }
    }

    Err(FloppaError::NoAvailableIps)
}

/// Find an active peer by device_id for a given user.
pub async fn find_peer_by_device_id(
    pool: &DbPool,
    user_id: i64,
    device_id: &str,
) -> Result<Option<i64>> {
    let peer_id = sqlx::query_scalar!(
        r#"
        SELECT id FROM peers
        WHERE user_id = $1 AND device_id = $2 AND sync_status NOT IN ('removed', 'pending_remove')
        "#,
        user_id,
        device_id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(peer_id)
}

/// Generate a WireGuard client configuration string.
pub fn generate_wg_config(
    private_key: &str,
    assigned_ip: &str,
    config: &Config,
    wg_public_key: &str,
) -> String {
    let dns = config.wireguard.dns.join(", ");
    format!(
        r#"[Interface]
PrivateKey = {}
Address = {}/32
DNS = {}

[Peer]
PublicKey = {}
Endpoint = {}
AllowedIPs = {}
PersistentKeepalive = 25
"#,
        private_key,
        assigned_ip,
        dns,
        wg_public_key,
        config.wireguard.endpoint,
        config.wireguard.allowed_ips
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WireGuardConfig;

    fn test_config() -> Config {
        Config {
            wireguard: WireGuardConfig {
                interface: "wg-test".into(),
                endpoint: "vpn.test.com:51820".into(),
                listen_port: None,
                client_subnet: "10.200.0.0/24".into(),
                server_ip: None,
                dns: vec!["8.8.8.8".into()],
                allowed_ips: "0.0.0.0/0, ::/0".into(),
                rate_limit: None,
            },
            bot: None,
            auth: None,
            allowed_origins: vec![],
            min_client_version: None,
        }
    }

    async fn get_basic_plan_id(pool: &DbPool) -> i32 {
        sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'basic'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn seed_user(pool: &DbPool, telegram_id: i64) -> i64 {
        sqlx::query_scalar!(
            "INSERT INTO users (telegram_id, username) VALUES ($1, 'testuser') RETURNING id",
            telegram_id,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn seed_subscription(pool: &DbPool, user_id: i64, plan_id: i32) {
        sqlx::query!(
            "INSERT INTO subscriptions (user_id, plan_id, starts_at) VALUES ($1, $2, NOW())",
            user_id,
            plan_id,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    // ── generate_wg_config (pure, no DB) ──

    #[test]
    fn test_generate_wg_config() {
        let config = test_config();
        let result = generate_wg_config("PRIVATE_KEY", "10.200.0.5", &config, "PUBLIC_KEY");

        assert!(result.contains("PrivateKey = PRIVATE_KEY"));
        assert!(result.contains("Address = 10.200.0.5/32"));
        assert!(result.contains("DNS = 8.8.8.8"));
        assert!(result.contains("PublicKey = PUBLIC_KEY"));
        assert!(result.contains("Endpoint = vpn.test.com:51820"));
        assert!(result.contains("AllowedIPs = 0.0.0.0/0, ::/0"));
        assert!(result.contains("PersistentKeepalive = 25"));
    }

    #[test]
    fn test_generate_wg_config_multiple_dns() {
        let mut config = test_config();
        config.wireguard.dns = vec!["8.8.8.8".into(), "1.1.1.1".into()];
        let result = generate_wg_config("KEY", "10.0.0.2", &config, "PUB");

        assert!(result.contains("DNS = 8.8.8.8, 1.1.1.1"));
    }

    // ── upsert_user ──

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_upsert_new_user_grants_trial(pool: DbPool) {
        get_basic_plan_id(&pool).await;

        let result = upsert_user(
            &pool,
            12345,
            Some("alice"),
            TelegramProfile {
                first_name: Some("Alice"),
                last_name: Some("Smith"),
                photo_url: None,
            },
            false,
        )
        .await
        .unwrap();

        assert!(result.trial_granted);
        assert_eq!(result.username.as_deref(), Some("alice"));
        assert_eq!(result.first_name.as_deref(), Some("Alice"));
        assert!(!result.is_admin);

        // Verify subscription was created
        let sub_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1",
            result.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sub_count, Some(1));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_upsert_existing_user_no_trial(pool: DbPool) {
        get_basic_plan_id(&pool).await;

        // First call — grants trial
        let first = upsert_user(
            &pool,
            12345,
            Some("alice"),
            TelegramProfile::default(),
            false,
        )
        .await
        .unwrap();
        assert!(first.trial_granted);

        // Second call — no trial
        let second = upsert_user(
            &pool,
            12345,
            Some("alice2"),
            TelegramProfile::default(),
            false,
        )
        .await
        .unwrap();
        assert!(!second.trial_granted);
        assert_eq!(second.username.as_deref(), Some("alice2"));
        assert_eq!(second.id, first.id);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_upsert_preserves_existing_profile_fields(pool: DbPool) {
        get_basic_plan_id(&pool).await;

        upsert_user(
            &pool,
            12345,
            Some("alice"),
            TelegramProfile {
                first_name: Some("Alice"),
                last_name: Some("Smith"),
                photo_url: Some("https://photo.url"),
            },
            false,
        )
        .await
        .unwrap();

        // Update with None fields — should preserve existing
        let result = upsert_user(
            &pool,
            12345,
            Some("alice"),
            TelegramProfile::default(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.first_name.as_deref(), Some("Alice"));
        assert_eq!(result.last_name.as_deref(), Some("Smith"));
        assert_eq!(result.photo_url.as_deref(), Some("https://photo.url"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_upsert_admin_flag_only_increases(pool: DbPool) {
        get_basic_plan_id(&pool).await;

        let r1 = upsert_user(&pool, 12345, Some("u"), TelegramProfile::default(), false)
            .await
            .unwrap();
        assert!(!r1.is_admin);

        let r2 = upsert_user(&pool, 12345, Some("u"), TelegramProfile::default(), true)
            .await
            .unwrap();
        assert!(r2.is_admin);

        // Calling with false should NOT revoke admin
        let r3 = upsert_user(&pool, 12345, Some("u"), TelegramProfile::default(), false)
            .await
            .unwrap();
        assert!(r3.is_admin);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_upsert_no_basic_plan_no_trial(pool: DbPool) {
        // Remove migration-seeded basic plan
        sqlx::query!("DELETE FROM plans WHERE name = 'basic'")
            .execute(&pool)
            .await
            .unwrap();

        let result = upsert_user(&pool, 12345, Some("u"), TelegramProfile::default(), false)
            .await
            .unwrap();
        assert!(!result.trial_granted);
    }

    // ── allocate_ip ──

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_allocate_ip_first_ip(pool: DbPool) {
        let ip = allocate_ip(&pool, "10.200.0.0/24").await.unwrap();
        assert_eq!(ip, "10.200.0.2"); // skips .0 (network) and .1 (gateway)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_allocate_ip_skips_assigned(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        // Manually insert a peer with .2
        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status) VALUES ($1, 'key1', '10.200.0.2', 'active')",
            user_id,
        )
        .execute(&pool)
        .await
        .unwrap();

        let ip = allocate_ip(&pool, "10.200.0.0/24").await.unwrap();
        assert_eq!(ip, "10.200.0.3");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_allocate_ip_reuses_removed(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status) VALUES ($1, 'key1', '10.200.0.2', 'removed')",
            user_id,
        )
        .execute(&pool)
        .await
        .unwrap();

        let ip = allocate_ip(&pool, "10.200.0.0/24").await.unwrap();
        assert_eq!(ip, "10.200.0.2"); // removed peer's IP is reusable
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_allocate_ip_subnet_full(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        // /30 subnet: 4 IPs total, skip .0 (network), .1 (gateway), .3 (broadcast) → only .2 usable
        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status) VALUES ($1, 'key1', '10.200.0.2', 'active')",
            user_id,
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = allocate_ip(&pool, "10.200.0.0/30").await;
        assert!(matches!(result, Err(FloppaError::NoAvailableIps)));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_allocate_ip_invalid_subnet(pool: DbPool) {
        let result = allocate_ip(&pool, "not-a-subnet").await;
        assert!(matches!(result, Err(FloppaError::NoAvailableIps)));
    }

    // ── create_peer ──

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_peer_success(pool: DbPool) {
        let config = test_config();
        let encryption_key = [0x42u8; 32];
        let wg_pub = "dGVzdC1wdWJsaWMta2V5LWJhc2U2NC1lbmNvZGVkMTI="; // dummy base64

        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let result = create_peer(&pool, user_id, &config, &encryption_key, wg_pub, None)
            .await
            .unwrap();

        assert_eq!(result.assigned_ip, "10.200.0.2");
        assert!(!result.private_key_plaintext.is_empty());
        assert!(result.config.contains("[Interface]"));
        assert!(result.config.contains("[Peer]"));

        // Verify peer in DB
        let status = sqlx::query_scalar!("SELECT sync_status FROM peers WHERE id = $1", result.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending_add");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_peer_no_subscription(pool: DbPool) {
        let config = test_config();
        let encryption_key = [0x42u8; 32];
        let user_id = seed_user(&pool, 11111).await;

        let result = create_peer(&pool, user_id, &config, &encryption_key, "PUB", None).await;
        assert!(matches!(result, Err(FloppaError::NoActiveSubscription)));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_peer_limit_reached(pool: DbPool) {
        let config = test_config();
        let encryption_key = [0x42u8; 32];

        // Plan with max_peers=1
        let plan_id = sqlx::query_scalar!(
            "INSERT INTO plans (name, display_name, max_peers, price_rub) VALUES ('limited', 'Limited', 1, 0) RETURNING id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        // Create first peer (should succeed)
        create_peer(&pool, user_id, &config, &encryption_key, "PUB", None)
            .await
            .unwrap();

        // Second peer should fail
        let result = create_peer(&pool, user_id, &config, &encryption_key, "PUB", None).await;
        assert!(matches!(
            result,
            Err(FloppaError::PeerLimitReached { current: 1, max: 1 })
        ));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_create_peer_with_device_info(pool: DbPool) {
        let config = test_config();
        let encryption_key = [0x42u8; 32];

        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let options = Some(CreatePeerOptions {
            device_name: Some("Pixel 9".into()),
            device_id: Some("test-device-uuid".into()),
        });

        let result = create_peer(&pool, user_id, &config, &encryption_key, "PUB", options)
            .await
            .unwrap();

        let row = sqlx::query!(
            "SELECT device_name, device_id FROM peers WHERE id = $1",
            result.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.device_name.as_deref(), Some("Pixel 9"));
        assert_eq!(row.device_id.as_deref(), Some("test-device-uuid"));
    }

    // ── find_peer_by_device_id ──

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_find_peer_by_device_id_found(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let peer_id = sqlx::query_scalar!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, device_id) VALUES ($1, 'key1', '10.0.0.2', 'active', 'dev-123') RETURNING id",
            user_id,
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let result = find_peer_by_device_id(&pool, user_id, "dev-123")
            .await
            .unwrap();
        assert_eq!(result, Some(peer_id));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_find_peer_by_device_id_not_found(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let result = find_peer_by_device_id(&pool, user_id, "nonexistent")
            .await
            .unwrap();
        assert_eq!(result, None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_find_peer_by_device_id_ignores_removed(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, device_id) VALUES ($1, 'key1', '10.0.0.2', 'removed', 'dev-123')",
            user_id,
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = find_peer_by_device_id(&pool, user_id, "dev-123")
            .await
            .unwrap();
        assert_eq!(result, None);
    }
}
