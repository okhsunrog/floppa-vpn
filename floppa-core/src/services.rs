//! Shared business logic used by both bot and admin.

use crate::error::{FloppaError, Result};
use crate::models::Protocol;
use crate::{Config, DbPool, encrypt_private_key};
use chrono::{Duration, Utc};
use ipnetwork::Ipv4Network;
use std::collections::HashSet;
use std::net::Ipv4Addr;

/// Result of user upsert operation.
#[derive(Debug)]
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
        ON CONFLICT (telegram_id) WHERE telegram_id IS NOT NULL DO UPDATE SET
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

    let trial_granted = grant_real_trial_if_unused(pool, row.id).await?;

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

/// Grant the one-time real trial (7-day "basic" plan) to a user if they haven't used it yet.
///
/// Atomically claims `trial_used_at`, so concurrent calls grant at most one trial.
/// Returns whether a trial was granted on this call. Keyed on `user_id` so it works for
/// both the Telegram signup path and the credential→Telegram link path.
pub async fn grant_real_trial_if_unused(pool: &DbPool, user_id: i64) -> Result<bool> {
    let claimed = sqlx::query!(
        "UPDATE users SET trial_used_at = NOW() WHERE id = $1 AND trial_used_at IS NULL",
        user_id,
    )
    .execute(pool)
    .await?;

    if claimed.rows_affected() != 1 {
        return Ok(false);
    }

    let basic_plan = sqlx::query!("SELECT id, trial_days FROM plans WHERE name = 'basic'")
        .fetch_optional(pool)
        .await?;

    let Some(plan) = basic_plan else {
        return Ok(false);
    };

    let days = plan.trial_days.unwrap_or(7) as i64;
    let now = Utc::now();
    let expires_at = now + Duration::days(days);
    sqlx::query!(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'trial')",
        user_id,
        plan.id,
        now,
        expires_at,
    )
    .execute(pool)
    .await?;

    Ok(true)
}

/// Grant a short "taster" trial. Does NOT consume `trial_used_at` (so the user can still
/// claim the real trial later via Telegram link). No-op if the 'taster' plan is missing.
pub async fn grant_taster_trial(pool: &DbPool, user_id: i64, minutes: i64) -> Result<()> {
    let taster_plan = sqlx::query!("SELECT id FROM plans WHERE name = 'taster'")
        .fetch_optional(pool)
        .await?;

    if let Some(plan) = taster_plan {
        let now = Utc::now();
        let expires_at = now + Duration::minutes(minutes);
        sqlx::query!(
            "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'taster')",
            user_id,
            plan.id,
            now,
            expires_at,
        )
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Validate a login and return `(normalized_uid_lowercase, display_form)`.
fn normalize_login(login: &str) -> Result<(String, String)> {
    let display = login.trim();
    if display.len() < 3 || display.len() > 64 {
        return Err(FloppaError::InvalidLogin("must be 3–64 characters".into()));
    }
    if !display
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return Err(FloppaError::InvalidLogin(
            "only letters, digits, and . _ - are allowed".into(),
        ));
    }
    Ok((display.to_lowercase(), display.to_string()))
}

/// Create a new credential (login + password) user with `telegram_id` NULL and grant a taster trial.
pub async fn create_credential_user(
    pool: &DbPool,
    login: &str,
    password: &str,
    taster_minutes: i64,
) -> Result<UpsertResult> {
    let (uid, display) = normalize_login(login)?;
    if password.len() < 8 {
        return Err(FloppaError::InvalidLogin(
            "password must be at least 8 characters".into(),
        ));
    }
    let secret_hash = crate::password::hash_password(password)?;

    let mut tx = pool.begin().await?;

    let user = sqlx::query!(
        "INSERT INTO users (telegram_id, username) VALUES (NULL, $1) \
         RETURNING id, username, first_name, last_name, photo_url, is_admin",
        display,
    )
    .fetch_one(&mut *tx)
    .await?;

    let ins = sqlx::query!(
        "INSERT INTO auth_identities (user_id, provider, provider_uid, secret_hash) VALUES ($1, 'password', $2, $3)",
        user.id,
        uid,
        secret_hash,
    )
    .execute(&mut *tx)
    .await;

    match ins {
        Ok(_) => {}
        Err(sqlx::Error::Database(db_err))
            if db_err.constraint() == Some("auth_identities_provider_provider_uid_key") =>
        {
            return Err(FloppaError::CredentialTaken);
        }
        Err(e) => return Err(e.into()),
    }

    tx.commit().await?;

    // Best-effort taster trial after commit (missing plan = no-op).
    grant_taster_trial(pool, user.id, taster_minutes).await?;

    Ok(UpsertResult {
        id: user.id,
        username: user.username,
        first_name: user.first_name,
        last_name: user.last_name,
        photo_url: user.photo_url,
        is_admin: user.is_admin,
        trial_granted: false,
    })
}

/// Authenticate a login + password. Returns the `users.id` on success.
///
/// Runs a password verification even when the login is not found (constant-time-ish),
/// to avoid leaking account existence via response timing. Returns `InvalidCredentials`
/// for both "no such login" and "wrong password".
pub async fn find_user_by_credential(pool: &DbPool, login: &str, password: &str) -> Result<i64> {
    let uid = login.trim().to_lowercase();
    let row = sqlx::query!(
        "SELECT id, user_id, secret_hash FROM auth_identities WHERE provider = 'password' AND provider_uid = $1",
        uid,
    )
    .fetch_optional(pool)
    .await?;

    let Some(r) = row else {
        crate::password::dummy_verify(password);
        return Err(FloppaError::InvalidCredentials);
    };

    let ok = r
        .secret_hash
        .as_deref()
        .map(|h| crate::password::verify_password(password, h))
        .unwrap_or(false);

    if !ok {
        return Err(FloppaError::InvalidCredentials);
    }

    let _ = sqlx::query!(
        "UPDATE auth_identities SET last_login_at = NOW() WHERE id = $1",
        r.id,
    )
    .execute(pool)
    .await;

    Ok(r.user_id)
}

/// Set (or change) the login+password credential for an existing user. Used by the backup-credential
/// nudge and the account page. Upserts the user's single `password` identity.
pub async fn set_credential_for_user(
    pool: &DbPool,
    user_id: i64,
    login: &str,
    password: &str,
) -> Result<()> {
    let (uid, _display) = normalize_login(login)?;
    if password.len() < 8 {
        return Err(FloppaError::InvalidLogin(
            "password must be at least 8 characters".into(),
        ));
    }
    let secret_hash = crate::password::hash_password(password)?;

    let res = sqlx::query!(
        r#"INSERT INTO auth_identities (user_id, provider, provider_uid, secret_hash)
           VALUES ($1, 'password', $2, $3)
           ON CONFLICT (user_id, provider) DO UPDATE SET provider_uid = $2, secret_hash = $3"#,
        user_id,
        uid,
        secret_hash,
    )
    .execute(pool)
    .await;

    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(db_err))
            if db_err.constraint() == Some("auth_identities_provider_provider_uid_key") =>
        {
            Err(FloppaError::CredentialTaken)
        }
        Err(e) => Err(e.into()),
    }
}

/// Attach a Telegram identity to an existing user that has no Telegram yet (branch A), filling in
/// any missing profile fields, and grant the one-time real trial. The caller must have already
/// verified that no OTHER row owns `tg_id`. Returns whether a real trial was granted on this call.
pub async fn attach_telegram_simple(
    pool: &DbPool,
    user_id: i64,
    tg_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
) -> Result<bool> {
    sqlx::query!(
        r#"
        UPDATE users SET
            telegram_id = $2,
            username   = COALESCE(username, $3),
            first_name = COALESCE(first_name, $4),
            last_name  = COALESCE(last_name, $5)
        WHERE id = $1
        "#,
        user_id,
        tg_id,
        username,
        first_name,
        last_name,
    )
    .execute(pool)
    .await?;

    grant_real_trial_if_unused(pool, user_id).await
}

/// Merge the established Telegram account `husk_id` INTO the current-session account `survivor_id`
/// (which must have no Telegram yet), then delete the husk. The survivor keeps its `id` so the
/// caller's JWT stays valid (no logout). Every `user_id` FK is re-pointed BEFORE the delete to
/// avoid `ON DELETE CASCADE` silently destroying data (and the RESTRICT on `payments`).
///
/// Returns `Ok(true)` on a successful merge, `Ok(false)` if the preconditions no longer hold
/// (a race — survivor already has a Telegram, or the husk lost it); the transaction makes no
/// changes in that case.
pub async fn merge_telegram_into_session(
    pool: &DbPool,
    survivor_id: i64,
    husk_id: i64,
) -> Result<bool> {
    if survivor_id == husk_id {
        return Ok(false);
    }

    let mut tx = pool.begin().await?;

    // Lock both rows and capture the husk's fields.
    let husk = sqlx::query!(
        r#"SELECT telegram_id, username, first_name, last_name, photo_url, language,
                  is_admin, trial_used_at, created_at
           FROM users WHERE id = $1 FOR UPDATE"#,
        husk_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let survivor = sqlx::query!(
        "SELECT telegram_id FROM users WHERE id = $1 FOR UPDATE",
        survivor_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    // Preconditions: survivor has no Telegram, husk still owns one.
    let Some(tg_id) = husk.telegram_id else {
        tx.rollback().await?;
        return Ok(false);
    };
    if survivor.telegram_id.is_some() {
        tx.rollback().await?;
        return Ok(false);
    }

    // 1. Reconcile user-level columns onto the survivor (LEAST ignores NULLs in Postgres, so an
    //    already-used trial on either side marks the merged account as trial-used).
    sqlx::query!(
        r#"UPDATE users SET
               is_admin      = is_admin OR $2,
               trial_used_at = LEAST(trial_used_at, $3),
               created_at    = LEAST(created_at, $4)
           WHERE id = $1"#,
        survivor_id,
        husk.is_admin,
        husk.trial_used_at,
        husk.created_at,
    )
    .execute(&mut *tx)
    .await?;

    // Revoke the husk's VLESS (fires the daemon notify) before it is deleted.
    sqlx::query!("UPDATE users SET vless_uuid = NULL WHERE id = $1", husk_id)
        .execute(&mut *tx)
        .await?;

    // 2. Free the husk's telegram_id before assigning it to the survivor (partial-unique).
    sqlx::query!("UPDATE users SET telegram_id = NULL WHERE id = $1", husk_id)
        .execute(&mut *tx)
        .await?;

    // 3. Move telegram_id + profile onto the survivor (COALESCE keeps the survivor's own values).
    sqlx::query!(
        r#"UPDATE users SET
               telegram_id = $2,
               username    = COALESCE(username, $3),
               first_name  = COALESCE(first_name, $4),
               last_name   = COALESCE(last_name, $5),
               photo_url   = COALESCE(photo_url, $6),
               language    = COALESCE(language, $7)
           WHERE id = $1"#,
        survivor_id,
        tg_id,
        husk.username,
        husk.first_name,
        husk.last_name,
        husk.photo_url,
        husk.language,
    )
    .execute(&mut *tx)
    .await?;

    // 4. Re-point every child FK husk → survivor BEFORE deleting the husk.
    // 4a. app_installations (UNIQUE(user_id, device_id)): re-point peers off doomed husk
    //     installations, drop the duplicates, then move the rest.
    sqlx::query!(
        r#"UPDATE peers p SET installation_id = s.id
           FROM app_installations h
           JOIN app_installations s ON s.user_id = $1 AND s.device_id = h.device_id
           WHERE h.user_id = $2 AND p.installation_id = h.id"#,
        survivor_id,
        husk_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"DELETE FROM app_installations h
           USING app_installations s
           WHERE h.user_id = $2 AND s.user_id = $1 AND h.device_id = s.device_id"#,
        survivor_id,
        husk_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE app_installations SET user_id = $1 WHERE user_id = $2",
        survivor_id,
        husk_id,
    )
    .execute(&mut *tx)
    .await?;

    // 4b–4d. peers (CASCADE), payments (RESTRICT), subscriptions + notification_log (CASCADE).
    sqlx::query!(
        "UPDATE peers SET user_id = $1 WHERE user_id = $2",
        survivor_id,
        husk_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE payments SET user_id = $1 WHERE user_id = $2",
        survivor_id,
        husk_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE subscriptions SET user_id = $1 WHERE user_id = $2",
        survivor_id,
        husk_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE notification_log SET user_id = $1 WHERE user_id = $2",
        survivor_id,
        husk_id
    )
    .execute(&mut *tx)
    .await?;

    // auth_identities + telegram_link_codes are intentionally NOT re-pointed: the husk's are
    // discarded via ON DELETE CASCADE (survivor's own login credentials win, by design).

    // 5. Delete the now-empty husk.
    sqlx::query!("DELETE FROM users WHERE id = $1", husk_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(true)
}

/// Server-side context needed for peer creation.
pub struct CreatePeerContext<'a> {
    pub pool: &'a DbPool,
    pub config: &'a Config,
    pub encryption_key: &'a [u8; 32],
    pub wg_public_key: &'a str,
    /// AmneziaWG server public key — required only when creating AmneziaWG peers.
    pub awg_public_key: Option<&'a str>,
}

/// Client-provided options when creating a peer.
#[derive(Default)]
pub struct CreatePeerOptions {
    pub installation_id: Option<i64>,
    /// Tunnel protocol. Defaults to AmneziaWG (the client default).
    pub protocol: Protocol,
}

/// Result of peer creation.
pub struct CreatePeerResult {
    pub id: i64,
    pub assigned_ip: String,
    pub private_key_plaintext: String,
    /// WireGuard .conf text
    pub config: String,
}

/// Create a new WireGuard peer for a user.
///
/// Checks subscription + peer limit, then generates x25519 keypair,
/// encrypts private key, allocates IP, and generates .conf.
///
/// Uses a transaction with FOR UPDATE to prevent concurrent peer limit violations.
pub async fn create_peer(
    ctx: &CreatePeerContext<'_>,
    user_id: i64,
    options: Option<CreatePeerOptions>,
) -> Result<CreatePeerResult> {
    let installation_id = options.as_ref().and_then(|o| o.installation_id);
    // No options → WireGuard (preserves the original single-protocol call sites).
    let protocol = options
        .as_ref()
        .map(|o| o.protocol)
        .unwrap_or(Protocol::WireGuard);

    // Resolve the subnet for this protocol up front (also validates AmneziaWG is configured).
    let subnet = match protocol {
        Protocol::WireGuard => ctx.config.wireguard.client_subnet.clone(),
        Protocol::AmneziaWg => ctx
            .config
            .amneziawg
            .as_ref()
            .ok_or(FloppaError::AmneziaWgNotConfigured)?
            .client_subnet
            .clone(),
    };

    // Transaction: check limit + allocate resources + insert peer atomically
    let mut tx = ctx.pool.begin().await?;

    // Lock the subscription row to serialize concurrent peer creations for this user
    let sub_info = sqlx::query!(
        r#"
        SELECT p.max_peers
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
    let max_peers = sub.max_peers;

    // A caller may only attach a peer to one of their own installations. Locking the row also
    // serializes concurrent peer creation for the same device, so the duplicate check below is
    // race-free even before the database unique index is considered.
    if let Some(id) = installation_id {
        let owned_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM app_installations WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;

        if owned_id.is_none() {
            return Err(FloppaError::InvalidInstallation(id));
        }

        let duplicate = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                SELECT 1 FROM peers
                WHERE installation_id = $1 AND protocol = $2
                  AND sync_status NOT IN ('removed', 'pending_remove')
            )"#,
        )
        .bind(id)
        .bind(protocol.as_db_str())
        .fetch_one(&mut *tx)
        .await?;

        if duplicate {
            return Err(FloppaError::PeerAlreadyExists {
                installation_id: id,
                protocol: protocol.as_db_str(),
            });
        }
    }

    // Slots are counted per-device: a client device (installation) is ONE slot no matter how many
    // protocol peers it holds (WireGuard + AmneziaWG share a slot), while each standalone exported
    // config (no installation) is its own slot.
    let slots_used = sqlx::query_scalar!(
        r#"
        SELECT (
            (SELECT COUNT(DISTINCT installation_id) FROM peers
             WHERE user_id = $1 AND sync_status != 'removed' AND installation_id IS NOT NULL)
          + (SELECT COUNT(*) FROM peers
             WHERE user_id = $1 AND sync_status != 'removed' AND installation_id IS NULL)
        )::int
        "#,
        user_id,
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(0);

    // Adding another protocol to a device that already has a peer is free (same slot). A standalone
    // config (no installation) always consumes a new slot.
    let consumes_new_slot = match installation_id {
        Some(id) => {
            let device_has_peer = sqlx::query_scalar!(
                r#"SELECT EXISTS(SELECT 1 FROM peers WHERE user_id = $1 AND installation_id = $2 AND sync_status != 'removed')"#,
                user_id,
                id,
            )
            .fetch_one(&mut *tx)
            .await?
            .unwrap_or(false);
            !device_has_peer
        }
        None => true,
    };

    if consumes_new_slot && slots_used >= max_peers {
        return Err(FloppaError::PeerLimitReached {
            current: slots_used,
            max: max_peers,
        });
    }

    let (private_key, public_key) = crate::wg_keys::generate_keypair()
        .map_err(|e| FloppaError::KeyGeneration(e.to_string()))?;

    let encrypted_private_key = encrypt_private_key(private_key.as_base64(), ctx.encryption_key)
        .map_err(|e| FloppaError::Encryption(e.to_string()))?;

    let assigned_ip = allocate_ip_tx(&mut tx, &subnet).await?;

    let peer_id = sqlx::query_scalar!(
        r#"
        INSERT INTO peers (user_id, public_key, private_key_encrypted, assigned_ip, sync_status, installation_id, protocol)
        VALUES ($1, $2, $3, $4, 'pending_add', $5, $6)
        RETURNING id
        "#,
        user_id,
        public_key.as_base64(),
        &encrypted_private_key,
        &assigned_ip,
        installation_id,
        protocol.as_db_str(),
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let config = match protocol {
        Protocol::WireGuard => generate_wg_config(
            private_key.as_base64(),
            &assigned_ip,
            ctx.config,
            ctx.wg_public_key,
        ),
        Protocol::AmneziaWg => {
            let awg = ctx
                .config
                .amneziawg
                .as_ref()
                .ok_or(FloppaError::AmneziaWgNotConfigured)?;
            let awg_pub = ctx
                .awg_public_key
                .ok_or(FloppaError::AmneziaWgNotConfigured)?;
            generate_awg_config(private_key.as_base64(), &assigned_ip, awg, awg_pub)
        }
    };

    Ok(CreatePeerResult {
        id: peer_id,
        assigned_ip,
        private_key_plaintext: private_key.as_base64().to_string(),
        config,
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
    // IP selection is read-then-insert. Serialize allocators for the same subnet across all users
    // so two transactions cannot select the same address and make one fail on the unique index.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(subnet)
        .execute(&mut **tx)
        .await?;
    allocate_ip_inner(&mut **tx, subnet).await
}

// Kept as runtime query because it uses a generic executor (pool or transaction)
async fn allocate_ip_inner<'e, E>(executor: E, subnet: &str) -> Result<String>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let network: Ipv4Network = subnet.parse().map_err(|_| FloppaError::NoAvailableIps)?;

    let assigned: Vec<Option<String>> =
        sqlx::query_scalar("SELECT assigned_ip FROM peers WHERE sync_status != 'removed'")
            .fetch_all(executor)
            .await?;

    let assigned_set: HashSet<Ipv4Addr> = assigned
        .iter()
        .filter_map(|ip| ip.as_ref()?.parse().ok())
        .collect();

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

/// Find an active peer by device_id + protocol for a given user (via app_installations JOIN).
///
/// A device may hold one active peer per protocol, so the protocol is part of the lookup.
pub async fn find_peer_by_device_id(
    pool: &DbPool,
    user_id: i64,
    device_id: &str,
    protocol: Protocol,
) -> Result<Option<i64>> {
    let peer_id = sqlx::query_scalar!(
        r#"
        SELECT p.id FROM peers p
        JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND ai.device_id = $2 AND p.protocol = $3
          AND p.sync_status NOT IN ('removed', 'pending_remove')
        "#,
        user_id,
        device_id,
        protocol.as_db_str(),
    )
    .fetch_optional(pool)
    .await?;

    Ok(peer_id)
}

/// Upsert an app installation record. Updates last_seen_at and optional fields on conflict.
pub async fn upsert_installation(
    pool: &DbPool,
    user_id: i64,
    device_id: &str,
    device_name: Option<&str>,
    platform: Option<&str>,
    app_version: Option<&str>,
) -> Result<crate::models::AppInstallation> {
    let row = sqlx::query_as!(
        crate::models::AppInstallation,
        r#"
        INSERT INTO app_installations (user_id, device_id, device_name, platform, app_version)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (user_id, device_id) DO UPDATE SET
            device_name = COALESCE($3, app_installations.device_name),
            platform = COALESCE($4, app_installations.platform),
            app_version = COALESCE($5, app_installations.app_version),
            last_seen_at = NOW()
        RETURNING id, user_id, device_id, device_name, platform, app_version, last_seen_at, created_at
        "#,
        user_id,
        device_id,
        device_name,
        platform,
        app_version,
    )
    .fetch_one(pool)
    .await?;

    Ok(row)
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

/// Generate an AmneziaWG client configuration string.
///
/// This is a standard AmneziaWG `.conf`: a WireGuard config plus the interface-wide
/// obfuscation params in `[Interface]`. The params are echoed verbatim from the server
/// config so both ends agree. The same text is parsed by the Tauri client (→ gotatun
/// `AwgConfig`) and importable into the official Amnezia client.
pub fn generate_awg_config(
    private_key: &str,
    assigned_ip: &str,
    awg: &crate::config::AmneziaWgConfig,
    awg_public_key: &str,
) -> String {
    let dns = awg.dns.join(", ");
    let o = &awg.obfuscation;

    let mut interface = format!(
        "[Interface]\nPrivateKey = {private_key}\nAddress = {assigned_ip}/32\nDNS = {dns}\nMTU = {mtu}\n",
        mtu = awg.mtu,
    );
    // Obfuscation params (AmneziaWG 2.0). H/S must match both ends; Jc/I are initiator-side.
    interface.push_str(&format!(
        "Jc = {}\nJmin = {}\nJmax = {}\n",
        o.jc, o.jmin, o.jmax
    ));
    interface.push_str(&format!(
        "S1 = {}\nS2 = {}\nS3 = {}\nS4 = {}\n",
        o.s1, o.s2, o.s3, o.s4
    ));
    interface.push_str(&format!(
        "H1 = {}\nH2 = {}\nH3 = {}\nH4 = {}\n",
        o.h1, o.h2, o.h3, o.h4
    ));
    for (n, val) in [(1, &o.i1), (2, &o.i2), (3, &o.i3), (4, &o.i4), (5, &o.i5)] {
        if !val.is_empty() {
            interface.push_str(&format!("I{n} = {val}\n"));
        }
    }

    format!(
        "{interface}\n[Peer]\nPublicKey = {awg_public_key}\nEndpoint = {endpoint}\nAllowedIPs = {allowed_ips}\nPersistentKeepalive = 25\n",
        endpoint = awg.endpoint,
        allowed_ips = awg.allowed_ips,
    )
}

/// Generate a VLESS+REALITY URI for a client.
///
/// `reality_public_key` comes from `Secrets.vless.reality_public_key`.
pub fn generate_vless_uri(uuid: &str, config: &Config, reality_public_key: &str) -> Result<String> {
    let vless = config
        .vless
        .as_ref()
        .ok_or(FloppaError::VlessNotConfigured)?;

    Ok(format!(
        "vless://{}@{}?encryption=none&flow={}&security=reality&sni={}&pbk={}&sid={}&type=tcp",
        uuid, vless.endpoint, vless.flow, vless.sni, reality_public_key, vless.short_id,
    ))
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
            amneziawg: None,
            vless: None,
            bot: None,
            auth: None,
            allowed_origins: vec![],
            min_client_version: None,
            metrics: None,
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
    fn test_generate_awg_config() {
        use crate::config::{AmneziaWgConfig, AwgObfuscation};
        let awg = AmneziaWgConfig {
            interface: "awg-test".into(),
            endpoint: "vpn.test.com:51821".into(),
            listen_port: None,
            client_subnet: "10.101.0.0/24".into(),
            server_ip: None,
            dns: vec!["1.1.1.1".into()],
            allowed_ips: "0.0.0.0/0, ::/0".into(),
            mtu: 1280,
            rate_limit: None,
            obfuscation: AwgObfuscation::default(),
        };
        let cfg = generate_awg_config("PRIV", "10.101.0.5", &awg, "AWGPUB");

        assert!(cfg.contains("PrivateKey = PRIV"));
        assert!(cfg.contains("Address = 10.101.0.5/32"));
        assert!(cfg.contains("MTU = 1280"));
        // AmneziaWG 2.0 obfuscation params present.
        assert!(cfg.contains("Jc = 6"));
        assert!(cfg.contains("S3 = 32")); // 2.0-only padding
        assert!(cfg.contains("H1 = 234567-345678"));
        assert!(cfg.contains("I1 = <b 0xc30000000108>"));
        // Empty signature slots are omitted.
        assert!(!cfg.contains("I2 ="));
        assert!(cfg.contains("PublicKey = AWGPUB"));
        assert!(cfg.contains("Endpoint = vpn.test.com:51821"));
        assert!(cfg.contains("PersistentKeepalive = 25"));
    }

    #[test]
    fn test_generate_wg_config_multiple_dns() {
        let mut config = test_config();
        config.wireguard.dns = vec!["8.8.8.8".into(), "1.1.1.1".into()];
        let result = generate_wg_config("KEY", "10.0.0.2", &config, "PUB");

        assert!(result.contains("DNS = 8.8.8.8, 1.1.1.1"));
    }

    // ── credential auth (login + password) ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_credential_user_and_login(pool: DbPool) {
        let res = create_credential_user(&pool, "Alice", "hunter2hunter", 120)
            .await
            .unwrap();
        assert!(!res.trial_granted);
        assert_eq!(res.username.as_deref(), Some("Alice")); // display preserves case

        // Login is case-insensitive on the normalized uid.
        let uid = find_user_by_credential(&pool, "alice", "hunter2hunter")
            .await
            .unwrap();
        assert_eq!(uid, res.id);

        // Wrong password → InvalidCredentials.
        let err = find_user_by_credential(&pool, "alice", "wrongpass1")
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::InvalidCredentials));

        // Unknown login → InvalidCredentials (not a distinct "not found").
        let err = find_user_by_credential(&pool, "nobody", "whatever1")
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::InvalidCredentials));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_credential_user_duplicate_login(pool: DbPool) {
        create_credential_user(&pool, "bob", "password123", 120)
            .await
            .unwrap();
        // Same login, different case → still taken.
        let err = create_credential_user(&pool, "BOB", "password123", 120)
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::CredentialTaken));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_credential_user_gets_taster_not_real_trial(pool: DbPool) {
        let res = create_credential_user(&pool, "carol", "password123", 120)
            .await
            .unwrap();

        // The one-time real trial is NOT consumed (so it can be claimed later via Telegram link).
        let trial_used: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar!("SELECT trial_used_at FROM users WHERE id = $1", res.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(trial_used.is_none());

        // A taster subscription exists.
        let taster_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1 AND source = 'taster'",
            res.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(taster_count, Some(1));

        // Now grant the real trial (as the Telegram-link path would) → succeeds once.
        assert!(grant_real_trial_if_unused(&pool, res.id).await.unwrap());
        assert!(!grant_real_trial_if_unused(&pool, res.id).await.unwrap());

        let trial_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1 AND source = 'trial'",
            res.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trial_count, Some(1));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_short_password_rejected(pool: DbPool) {
        let err = create_credential_user(&pool, "dave", "short", 120)
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::InvalidLogin(_)));
    }

    // ── Telegram link + merge ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_attach_telegram_grants_real_trial(pool: DbPool) {
        get_basic_plan_id(&pool).await;
        let user = create_credential_user(&pool, "newbie", "password123", 120)
            .await
            .unwrap();

        let granted =
            attach_telegram_simple(&pool, user.id, 55555, Some("tguser"), Some("Tg"), None)
                .await
                .unwrap();
        assert!(granted);

        let tg = sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", user.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(tg, Some(55555));

        let trial_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1 AND source = 'trial'",
            user.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trial_count, Some(1));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_merge_telegram_into_session(pool: DbPool) {
        let basic = get_basic_plan_id(&pool).await;

        // Survivor: the fresh credential account the user is logged into (no Telegram, taster only).
        let survivor = create_credential_user(&pool, "recover_me", "password123", 120)
            .await
            .unwrap();
        let survivor_inst_a = sqlx::query_scalar!(
            "INSERT INTO app_installations (user_id, device_id) VALUES ($1, 'devA') RETURNING id",
            survivor.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        // Husk: the established Telegram account with a used trial, a subscription, peers, a payment,
        // and two installations — one sharing device 'devA' with the survivor (must dedup).
        let husk = seed_user(&pool, 99999).await;
        sqlx::query!("UPDATE users SET trial_used_at = NOW() WHERE id = $1", husk)
            .execute(&pool)
            .await
            .unwrap();
        seed_subscription(&pool, husk, basic).await;
        let husk_inst_a = sqlx::query_scalar!(
            "INSERT INTO app_installations (user_id, device_id) VALUES ($1, 'devA') RETURNING id",
            husk
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO app_installations (user_id, device_id) VALUES ($1, 'devB')",
            husk
        )
        .execute(&pool)
        .await
        .unwrap();
        // A husk peer attached to the soon-to-be-deduped installation 'devA'.
        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, installation_id) VALUES ($1, 'PUBKEYHUSK', '10.0.0.50', $2)",
            husk, husk_inst_a
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO payments (user_id, plan_id, amount, invoice_payload, status) VALUES ($1, $2, 100, 'payload-1', 'completed')",
            husk, basic
        )
        .execute(&pool)
        .await
        .unwrap();

        let merged = merge_telegram_into_session(&pool, survivor.id, husk)
            .await
            .unwrap();
        assert!(merged);

        // Husk row is gone.
        let husk_exists =
            sqlx::query_scalar!("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)", husk)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(husk_exists, Some(false));

        // Survivor now owns the Telegram id and is marked trial-used (no re-trialing).
        let row = sqlx::query!(
            "SELECT telegram_id, trial_used_at FROM users WHERE id = $1",
            survivor.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.telegram_id, Some(99999));
        assert!(row.trial_used_at.is_some());

        // Payment survived and re-pointed (RESTRICT would have failed the delete otherwise).
        let payment_owner =
            sqlx::query_scalar!("SELECT user_id FROM payments WHERE invoice_payload = 'payload-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(payment_owner, survivor.id);

        // The husk peer re-pointed, and its installation was re-pointed to the survivor's 'devA'
        // (no FK violation, no duplicate (user_id, device_id)).
        let peer = sqlx::query!(
            "SELECT user_id, installation_id FROM peers WHERE public_key = 'PUBKEYHUSK'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(peer.user_id, survivor.id);
        assert_eq!(peer.installation_id, Some(survivor_inst_a));

        // Survivor has both installations: its own 'devA' (deduped) and the moved 'devB'.
        let devices: Vec<String> = sqlx::query_scalar!(
            "SELECT device_id FROM app_installations WHERE user_id = $1 ORDER BY device_id",
            survivor.id
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(devices, vec!["devA".to_string(), "devB".to_string()]);
        let _ = husk_inst_a; // deduped away

        // Survivor holds both subscriptions (its taster + the husk's basic).
        let sub_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1",
            survivor.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sub_count, Some(2));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_merge_aborts_when_survivor_already_linked(pool: DbPool) {
        // Survivor already has a Telegram (race) → merge is a no-op returning false.
        let survivor = seed_user(&pool, 111).await;
        let husk = seed_user(&pool, 222).await;
        let merged = merge_telegram_into_session(&pool, survivor, husk)
            .await
            .unwrap();
        assert!(!merged);
        // Both rows still exist, untouched.
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM users WHERE id IN ($1, $2)",
            survivor,
            husk
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, Some(2));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_set_credential_for_existing_user(pool: DbPool) {
        // A Telegram user sets a backup login+password.
        let tg_user = seed_user(&pool, 7001).await;
        set_credential_for_user(&pool, tg_user, "backup_login", "password123")
            .await
            .unwrap();
        assert_eq!(
            find_user_by_credential(&pool, "backup_login", "password123")
                .await
                .unwrap(),
            tg_user
        );

        // Changing it (upsert on user_id) replaces password + login.
        set_credential_for_user(&pool, tg_user, "backup_login", "newpassword9")
            .await
            .unwrap();
        assert!(
            find_user_by_credential(&pool, "backup_login", "password123")
                .await
                .is_err()
        );
        assert_eq!(
            find_user_by_credential(&pool, "backup_login", "newpassword9")
                .await
                .unwrap(),
            tg_user
        );

        // Another user can't take the same login.
        let other = seed_user(&pool, 7002).await;
        let err = set_credential_for_user(&pool, other, "backup_login", "password123")
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::CredentialTaken));
    }

    // ── upsert_user ──

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
    async fn test_allocate_ip_first_ip(pool: DbPool) {
        let ip = allocate_ip(&pool, "10.200.0.0/24").await.unwrap();
        assert_eq!(ip, "10.200.0.2"); // skips .0 (network) and .1 (gateway)
    }

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
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

    #[sqlx::test(migrations = "../migrations")]
    async fn test_allocate_ip_invalid_subnet(pool: DbPool) {
        let result = allocate_ip(&pool, "not-a-subnet").await;
        assert!(matches!(result, Err(FloppaError::NoAvailableIps)));
    }

    // ── create_peer ──

    fn test_ctx<'a>(pool: &'a DbPool, config: &'a Config) -> CreatePeerContext<'a> {
        static ENCRYPTION_KEY: [u8; 32] = [0x42u8; 32];
        CreatePeerContext {
            pool,
            config,
            encryption_key: &ENCRYPTION_KEY,
            wg_public_key: "dGVzdC1wdWJsaWMta2V5LWJhc2U2NC1lbmNvZGVkMTI=",
            awg_public_key: None,
        }
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_success(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);

        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let result = create_peer(&ctx, user_id, None).await.unwrap();

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

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_no_subscription(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);
        let user_id = seed_user(&pool, 11111).await;

        let result = create_peer(&ctx, user_id, None).await;
        assert!(matches!(result, Err(FloppaError::NoActiveSubscription)));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_limit_reached(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);

        // Plan with max_peers=1
        let plan_id = sqlx::query_scalar!(
            "INSERT INTO plans (name, display_name, max_peers) VALUES ('limited', 'Limited', 1) RETURNING id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        // Create first peer (should succeed)
        create_peer(&ctx, user_id, None).await.unwrap();

        // Second peer should fail
        let result = create_peer(&ctx, user_id, None).await;
        assert!(matches!(
            result,
            Err(FloppaError::PeerLimitReached { current: 1, max: 1 })
        ));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_per_device_slot_allows_second_protocol(pool: DbPool) {
        use crate::config::{AmneziaWgConfig, AwgObfuscation};
        // AmneziaWG-enabled config so the AWG peer can be created.
        let mut config = test_config();
        config.amneziawg = Some(AmneziaWgConfig {
            interface: "awg-test".into(),
            endpoint: "vpn.test.com:51821".into(),
            listen_port: None,
            client_subnet: "10.101.0.0/24".into(),
            server_ip: None,
            dns: vec!["1.1.1.1".into()],
            allowed_ips: "0.0.0.0/0, ::/0".into(),
            mtu: 1280,
            rate_limit: None,
            obfuscation: AwgObfuscation::default(),
        });
        static KEY: [u8; 32] = [0x42u8; 32];
        let ctx = CreatePeerContext {
            pool: &pool,
            config: &config,
            encryption_key: &KEY,
            wg_public_key: "dGVzdC1wdWJsaWMta2V5LWJhc2U2NC1lbmNvZGVkMTI=",
            awg_public_key: Some("dGVzdC1wdWJsaWMta2V5LWJhc2U2NC1lbmNvZGVkMTI="),
        };

        // max_peers = 1 (one device slot).
        let plan_id = sqlx::query_scalar!(
            "INSERT INTO plans (name, display_name, max_peers) VALUES ('limited', 'Limited', 1) RETURNING id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let user_id = seed_user(&pool, 22222).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let inst = upsert_installation(&pool, user_id, "dev-1", None, None, None)
            .await
            .unwrap();

        // WireGuard peer for the device → consumes the single slot.
        create_peer(
            &ctx,
            user_id,
            Some(CreatePeerOptions {
                installation_id: Some(inst.id),
                protocol: Protocol::WireGuard,
            }),
        )
        .await
        .unwrap();

        // AmneziaWG peer for the SAME device → allowed despite max_peers=1 (same slot).
        create_peer(
            &ctx,
            user_id,
            Some(CreatePeerOptions {
                installation_id: Some(inst.id),
                protocol: Protocol::AmneziaWg,
            }),
        )
        .await
        .unwrap();

        // A standalone exported config (no installation) now exceeds the limit.
        let result = create_peer(&ctx, user_id, None).await;
        assert!(matches!(
            result,
            Err(FloppaError::PeerLimitReached { current: 1, max: 1 })
        ));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_with_installation(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);

        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let installation = upsert_installation(
            &pool,
            user_id,
            "test-device-uuid",
            Some("Pixel 9"),
            Some("android"),
            None,
        )
        .await
        .unwrap();

        let options = Some(CreatePeerOptions {
            installation_id: Some(installation.id),
            protocol: Protocol::WireGuard,
        });

        let result = create_peer(&ctx, user_id, options).await.unwrap();

        let row = sqlx::query!("SELECT installation_id FROM peers WHERE id = $1", result.id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(row.installation_id, Some(installation.id));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_rejects_foreign_installation(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);
        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        let other_user_id = seed_user(&pool, 22222).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let foreign = upsert_installation(&pool, other_user_id, "foreign-device", None, None, None)
            .await
            .unwrap();

        let result = create_peer(
            &ctx,
            user_id,
            Some(CreatePeerOptions {
                installation_id: Some(foreign.id),
                protocol: Protocol::WireGuard,
            }),
        )
        .await;

        assert!(matches!(
            result,
            Err(FloppaError::InvalidInstallation(id)) if id == foreign.id
        ));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_rejects_duplicate_installation_protocol(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);
        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;
        let installation = upsert_installation(&pool, user_id, "dev-1", None, None, None)
            .await
            .unwrap();

        let options = || {
            Some(CreatePeerOptions {
                installation_id: Some(installation.id),
                protocol: Protocol::WireGuard,
            })
        };
        create_peer(&ctx, user_id, options()).await.unwrap();
        let result = create_peer(&ctx, user_id, options()).await;

        assert!(matches!(
            result,
            Err(FloppaError::PeerAlreadyExists {
                installation_id,
                protocol: "wireguard",
            }) if installation_id == installation.id
        ));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_concurrent_peer_creation_allocates_distinct_ips(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);
        let plan_id = get_basic_plan_id(&pool).await;
        let user_a = seed_user(&pool, 11111).await;
        let user_b = seed_user(&pool, 22222).await;
        seed_subscription(&pool, user_a, plan_id).await;
        seed_subscription(&pool, user_b, plan_id).await;

        let (peer_a, peer_b) = tokio::join!(
            create_peer(&ctx, user_a, None),
            create_peer(&ctx, user_b, None),
        );
        let peer_a = peer_a.unwrap();
        let peer_b = peer_b.unwrap();

        assert_ne!(peer_a.assigned_ip, peer_b.assigned_ip);
    }

    // ── find_peer_by_device_id ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_find_peer_by_device_id_found(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let installation = upsert_installation(&pool, user_id, "dev-123", None, None, None)
            .await
            .unwrap();

        let peer_id = sqlx::query_scalar!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, installation_id) VALUES ($1, 'key1', '10.0.0.2', 'active', $2) RETURNING id",
            user_id,
            installation.id,
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let result = find_peer_by_device_id(&pool, user_id, "dev-123", Protocol::AmneziaWg)
            .await
            .unwrap();
        assert_eq!(result, Some(peer_id));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_find_peer_by_device_id_not_found(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let result = find_peer_by_device_id(&pool, user_id, "nonexistent", Protocol::AmneziaWg)
            .await
            .unwrap();
        assert_eq!(result, None);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_find_peer_by_device_id_ignores_removed(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let installation = upsert_installation(&pool, user_id, "dev-123", None, None, None)
            .await
            .unwrap();

        sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, installation_id) VALUES ($1, 'key1', '10.0.0.2', 'removed', $2)",
            user_id,
            installation.id,
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = find_peer_by_device_id(&pool, user_id, "dev-123", Protocol::AmneziaWg)
            .await
            .unwrap();
        assert_eq!(result, None);
    }
}
