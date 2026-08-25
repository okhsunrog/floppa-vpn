//! Shared business logic used by both bot and admin.

use crate::error::{FloppaError, Result};
use crate::models::{PeerSyncStatus, Protocol, SubscriptionSource};
use crate::{Config, DbPool, encrypt_private_key};
use chrono::{DateTime, Duration, Utc};
use ipnetwork::Ipv4Network;
use sqlx::{PgExecutor, PgTransaction};
use std::collections::HashSet;
use std::net::Ipv4Addr;
use tracing::warn;
use uuid::Uuid;

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

/// Upsert a Telegram user and auto-grant the one-time real trial if they haven't used one.
///
/// - Inserts or updates the user row (profile fields only fill in when provided).
/// - If `trial_used_at` is NULL, grants the "basic" plan for its `trial_minutes`
///   (see [`grant_real_trial_if_unused`]).
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
            username = COALESCE($2, users.username),
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

/// Grant a trial subscription on `plan_name` for the duration stored on that plan
/// (`trial_minutes`). When `consume_real_trial` is true, atomically claims the user's
/// one-time `trial_used_at` and no-ops if it was already used. Returns whether a
/// subscription was granted. No-op (returns false) if the plan is missing or has no
/// `trial_minutes` — in which case `trial_used_at` is left untouched.
///
/// Plan lookup, the `trial_used_at` claim and the INSERT run in one transaction, so a
/// failure between them can never burn the one-time trial without granting it.
async fn grant_trial(
    pool: &DbPool,
    user_id: i64,
    plan_name: &str,
    source: SubscriptionSource,
    consume_real_trial: bool,
) -> Result<bool> {
    let mut tx = pool.begin().await?;

    let plan = sqlx::query!(
        "SELECT id, trial_minutes FROM plans WHERE name = $1",
        plan_name
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(plan) = plan else {
        return Ok(false);
    };
    let Some(minutes) = plan.trial_minutes else {
        return Ok(false);
    };

    if consume_real_trial {
        let claimed = sqlx::query!(
            "UPDATE users SET trial_used_at = NOW() WHERE id = $1 AND trial_used_at IS NULL",
            user_id,
        )
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
            return Ok(false);
        }
    }

    let expires_at = Utc::now() + Duration::minutes(minutes as i64);
    insert_subscription(&mut tx, user_id, plan.id, Some(expires_at), source).await?;

    tx.commit().await?;
    Ok(true)
}

/// Insert a subscription starting now. `expires_at = None` means permanent. Returns its id.
///
/// Building block for the grant paths; it does NOT touch the user's other subscriptions — use
/// [`replace_active_subscription`] when the new one must supersede them.
pub async fn insert_subscription(
    tx: &mut PgTransaction<'_>,
    user_id: i64,
    plan_id: i32,
    expires_at: Option<DateTime<Utc>>,
    source: SubscriptionSource,
) -> Result<i64> {
    let id = sqlx::query_scalar!(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) \
         VALUES ($1, $2, NOW(), $3, $4) RETURNING id",
        user_id,
        plan_id,
        expires_at,
        source as _,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Close every currently active subscription of `user_id` (sets `expires_at = NOW()`) and insert
/// a new one on `plan_id` starting now. Returns the new subscription's id.
///
/// This is the "switch plan" primitive shared by Stars purchases, credit-funded switches and
/// admin grants: afterwards the user has exactly one active subscription. Runs inside the
/// caller's transaction so a payment record can be written atomically alongside it.
pub async fn replace_active_subscription(
    tx: &mut PgTransaction<'_>,
    user_id: i64,
    plan_id: i32,
    expires_at: Option<DateTime<Utc>>,
    source: SubscriptionSource,
) -> Result<i64> {
    sqlx::query!(
        "UPDATE subscriptions SET expires_at = NOW() \
         WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        user_id,
    )
    .execute(&mut **tx)
    .await?;
    insert_subscription(tx, user_id, plan_id, expires_at, source).await
}

/// How long an admin-granted subscription should last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionTerm {
    /// A fixed number of whole days from now.
    Days(u32),
    /// The plan's own `trial_minutes` (sub-day trials such as the taster live here).
    PlanDefault,
    /// No expiry.
    Permanent,
}

/// Why a [`SubscriptionTerm`] could not be turned into an expiry.
#[derive(Debug, thiserror::Error)]
pub enum SubscriptionTermError {
    #[error("plan {0} not found")]
    PlanNotFound(i32),
    #[error("no duration given and the plan has no trial duration")]
    NoDuration,
    #[error("duration is too long to represent")]
    DurationOutOfRange,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Resolve `term` against `plan_id` into an expiry (`None` = permanent), anchored at `now`.
///
/// The plan is looked up even for [`SubscriptionTerm::Days`]/[`Permanent`](SubscriptionTerm),
/// so an unknown plan is reported as such instead of surfacing later as a foreign-key error.
pub async fn resolve_subscription_expires(
    executor: impl PgExecutor<'_>,
    plan_id: i32,
    term: SubscriptionTerm,
    now: DateTime<Utc>,
) -> std::result::Result<Option<DateTime<Utc>>, SubscriptionTermError> {
    let plan_trial_minutes =
        sqlx::query_scalar!("SELECT trial_minutes FROM plans WHERE id = $1", plan_id)
            .fetch_optional(executor)
            .await?
            .ok_or(SubscriptionTermError::PlanNotFound(plan_id))?;

    let minutes = match term {
        SubscriptionTerm::Permanent => return Ok(None),
        SubscriptionTerm::Days(days) => i64::from(days) * 24 * 60,
        SubscriptionTerm::PlanDefault => {
            i64::from(plan_trial_minutes.ok_or(SubscriptionTermError::NoDuration)?)
        }
    };
    Duration::try_minutes(minutes)
        .and_then(|d| now.checked_add_signed(d))
        .map(Some)
        .ok_or(SubscriptionTermError::DurationOutOfRange)
}

/// Admin grant: supersede whatever the user has with `plan_id` until `expires_at`
/// (see [`replace_active_subscription`]). Returns the new subscription's id.
pub async fn grant_subscription(
    tx: &mut PgTransaction<'_>,
    user_id: i64,
    plan_id: i32,
    expires_at: Option<DateTime<Utc>>,
) -> Result<i64> {
    replace_active_subscription(
        tx,
        user_id,
        plan_id,
        expires_at,
        SubscriptionSource::AdminGrant,
    )
    .await
}

/// Grant the one-time real trial (the "basic" plan's `trial_minutes`) if not yet used.
///
/// Atomically claims `trial_used_at`, so concurrent calls grant at most one trial. Returns
/// whether a trial was granted on this call. Keyed on `user_id` so it works for both the
/// Telegram signup path and the credential→Telegram link path.
pub async fn grant_real_trial_if_unused(pool: &DbPool, user_id: i64) -> Result<bool> {
    grant_trial(pool, user_id, "basic", SubscriptionSource::Trial, true).await
}

/// Grant the short "taster" trial (the "taster" plan's `trial_minutes`). Does NOT consume
/// `trial_used_at`, so the user can still claim the real trial later via Telegram link.
/// No-op if the 'taster' plan is missing or has no `trial_minutes`.
pub async fn grant_taster_trial(pool: &DbPool, user_id: i64) -> Result<()> {
    grant_trial(pool, user_id, "taster", SubscriptionSource::Taster, false)
        .await
        .map(|_| ())
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
) -> Result<UpsertResult> {
    let (uid, display) = normalize_login(login)?;
    crate::password::validate_password(password)?;
    let secret_hash = crate::password::hash_password(password).await?;

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

    // Best-effort taster trial after commit (duration from the 'taster' plan; missing = no-op).
    // The account already exists, so a failure here must not turn into a registration error —
    // the client would retry and get "login taken" for an account it cannot log into yet.
    if let Err(e) = grant_taster_trial(pool, user.id).await {
        warn!(user_id = user.id, error = %e, "Failed to grant taster trial after registration");
    }

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
        crate::password::dummy_verify(password).await;
        return Err(FloppaError::InvalidCredentials);
    };

    let ok = match r.secret_hash.as_deref() {
        Some(phc) => crate::password::verify_password(password, phc).await,
        None => false,
    };

    if !ok {
        return Err(FloppaError::InvalidCredentials);
    }

    // Bookkeeping only: a failure here must not reject a correct login, but it should be visible.
    if let Err(e) = sqlx::query!(
        "UPDATE auth_identities SET last_login_at = NOW() WHERE id = $1",
        r.id,
    )
    .execute(pool)
    .await
    {
        warn!(identity_id = r.id, error = %e, "Failed to update last_login_at");
    }

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
    crate::password::validate_password(password)?;
    let secret_hash = crate::password::hash_password(password).await?;

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
    //    `is_admin` is deliberately NOT carried over: a merge link is minted by any credential
    //    account, so inheriting the husk's admin flag would turn a phished link into a privilege
    //    escalation. Admin rights come from config / an explicit admin action only.
    sqlx::query!(
        r#"UPDATE users SET
               trial_used_at = LEAST(trial_used_at, $2),
               created_at    = LEAST(created_at, $3)
           WHERE id = $1"#,
        survivor_id,
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
    //
    //     The typical merge happens on the SAME device (the user created a credential account
    //     on their phone, connected, then linked Telegram), so the survivor often already holds
    //     a live peer for the same (device, protocol). Re-pointing the husk's peer onto that
    //     installation would violate `peers_installation_protocol_active`; queue such husk peers
    //     for daemon removal first (the survivor's peer is the one the client actually uses).
    sqlx::query!(
        r#"UPDATE peers p SET sync_status = $3
           FROM app_installations h
           JOIN app_installations s ON s.user_id = $1 AND s.device_id = h.device_id
           WHERE h.user_id = $2 AND p.installation_id = h.id
             AND p.sync_status NOT IN ('removed', 'pending_remove')
             AND EXISTS (
                 SELECT 1 FROM peers sp
                 WHERE sp.installation_id = s.id AND sp.protocol = p.protocol
                   AND sp.sync_status NOT IN ('removed', 'pending_remove')
             )"#,
        survivor_id,
        husk_id,
        PeerSyncStatus::PendingRemove as _,
    )
    .execute(&mut *tx)
    .await?;
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

    // 6. Re-pointing rows by user_id fires no trigger (notify_subscription_changed only watches
    //    plan_id/expires_at), yet the survivor's peers must now be rate-limited by the plan it
    //    just inherited. Tell the daemon explicitly; the payload is the user_id it recomputes for.
    sqlx::query!(
        "SELECT pg_notify('subscription_changed', ($1::bigint)::text)",
        survivor_id
    )
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

/// Client-provided options when creating a peer. `Default` is a standalone (no installation)
/// AmneziaWG peer — AmneziaWG is the client default, see [`Protocol`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CreatePeerOptions {
    pub installation_id: Option<i64>,
    /// Tunnel protocol.
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
    options: CreatePeerOptions,
) -> Result<CreatePeerResult> {
    let CreatePeerOptions {
        installation_id,
        protocol,
    } = options;

    // Resolve everything protocol-specific up front, BEFORE any row is written: an AmneziaWG
    // peer must not be committed only to fail on a missing server key afterwards (the daemon
    // would then try to serve a peer nobody can connect to).
    let (subnet, server_ip, awg) = match protocol {
        Protocol::WireGuard => {
            let wg = &ctx.config.wireguard;
            (wg.client_subnet, wg.get_server_ip(), None)
        }
        Protocol::AmneziaWg => {
            let awg = ctx
                .config
                .amneziawg
                .as_ref()
                .ok_or(FloppaError::AmneziaWgNotConfigured)?;
            let awg_public_key = ctx
                .awg_public_key
                .ok_or(FloppaError::AmneziaWgNotConfigured)?;
            (
                awg.client_subnet,
                awg.get_server_ip(),
                Some((awg, awg_public_key)),
            )
        }
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
        .bind(protocol)
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

    let (private_key, public_key) = crate::wg_keys::generate_keypair()?;

    let encrypted_private_key = encrypt_private_key(private_key.as_base64(), ctx.encryption_key)?;

    let assigned_ip = allocate_ip_tx(&mut tx, subnet, &[server_ip])
        .await?
        .to_string();

    let peer_id = sqlx::query_scalar!(
        r#"
        INSERT INTO peers (user_id, public_key, private_key_encrypted, assigned_ip, sync_status, installation_id, protocol)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
        user_id,
        public_key.as_base64(),
        &encrypted_private_key,
        &assigned_ip,
        PeerSyncStatus::PendingAdd as _,
        installation_id,
        protocol as _,
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let config = match awg {
        None => generate_wg_config(
            private_key.as_base64(),
            &assigned_ip,
            ctx.config,
            ctx.wg_public_key,
        ),
        Some((awg, awg_public_key)) => {
            generate_awg_config(private_key.as_base64(), &assigned_ip, awg, awg_public_key)
        }
    };

    Ok(CreatePeerResult {
        id: peer_id,
        assigned_ip,
        private_key_plaintext: private_key.as_base64().to_string(),
        config,
    })
}

/// Lock-free allocation for the allocator's own tests. Production allocates only through
/// [`create_peer`], which serializes allocators with an advisory lock.
#[cfg(test)]
async fn allocate_ip(
    pool: &DbPool,
    subnet: Ipv4Network,
    reserved: &[Ipv4Addr],
) -> Result<Ipv4Addr> {
    allocate_ip_inner(pool, subnet, reserved).await
}

/// Allocate IP within a transaction.
async fn allocate_ip_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subnet: Ipv4Network,
    reserved: &[Ipv4Addr],
) -> Result<Ipv4Addr> {
    // IP selection is read-then-insert. Serialize allocators for the same subnet across all users
    // so two transactions cannot select the same address and make one fail on the unique index.
    // The lock key is the (network address, prefix) pair, so "10.100.0.0/24" and "10.100.0.5/24"
    // — the same network — contend on the same lock.
    sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
        .bind(u32::from(subnet.network()) as i32)
        .bind(i32::from(subnet.prefix()))
        .execute(&mut **tx)
        .await?;
    allocate_ip_inner(&mut **tx, subnet, reserved).await
}

/// Pick the lowest host address of `subnet` that is neither `reserved` (the server's own
/// address) nor held by a live peer. The network and broadcast addresses are never handed out.
// Kept as runtime query because it uses a generic executor (pool or transaction)
async fn allocate_ip_inner<'e, E>(
    executor: E,
    subnet: Ipv4Network,
    reserved: &[Ipv4Addr],
) -> Result<Ipv4Addr>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let assigned: Vec<Option<String>> =
        sqlx::query_scalar("SELECT assigned_ip FROM peers WHERE sync_status != 'removed'")
            .fetch_all(executor)
            .await?;

    let taken: HashSet<Ipv4Addr> = assigned
        .iter()
        .filter_map(|ip| ip.as_ref()?.parse().ok())
        .chain(reserved.iter().copied())
        .collect();

    subnet
        .iter()
        .filter(|ip| *ip != subnet.network() && *ip != subnet.broadcast())
        .find(|ip| !taken.contains(ip))
        .ok_or(FloppaError::NoAvailableIps)
}

/// Return the user's VLESS UUID, minting one if the user has none yet.
///
/// Get-or-create in a single statement: `COALESCE(vless_uuid, $new)` under the row lock the
/// UPDATE takes, so two concurrent first requests (the app and the bot, say) agree on one UUID
/// instead of the second overwriting the first. Assigning a UUID fires the `vless_user_changed`
/// notify that floppa-vless listens for.
pub async fn ensure_vless_uuid(executor: impl PgExecutor<'_>, user_id: i64) -> Result<Uuid> {
    let candidate = Uuid::new_v4().to_string();
    let stored = sqlx::query_scalar!(
        r#"UPDATE users SET vless_uuid = COALESCE(vless_uuid, $1)
           WHERE id = $2 RETURNING vless_uuid AS "vless_uuid!""#,
        candidate,
        user_id,
    )
    .fetch_one(executor)
    .await?;
    Ok(stored.parse()?)
}

/// Replace the user's VLESS UUID with a fresh one; the old one stops working as soon as
/// floppa-vless picks up the `vless_user_changed` notify. Returns `None` when the user has no
/// UUID to rotate (or does not exist) — nothing is minted in that case, use
/// [`ensure_vless_uuid`] for that.
pub async fn rotate_vless_uuid(
    executor: impl PgExecutor<'_>,
    user_id: i64,
) -> Result<Option<Uuid>> {
    let fresh = Uuid::new_v4().to_string();
    let stored = sqlx::query_scalar!(
        r#"UPDATE users SET vless_uuid = $1
           WHERE id = $2 AND vless_uuid IS NOT NULL RETURNING vless_uuid AS "vless_uuid!""#,
        fresh,
        user_id,
    )
    .fetch_optional(executor)
    .await?;
    stored.map(|u| u.parse()).transpose().map_err(Into::into)
}

/// Queue a peer for removal by the daemon: `active`/`pending_add` → `pending_remove`.
///
/// `owner` restricts the change to a peer of that user (the self-service path); `None` is the
/// admin path. Returns whether a row changed — `false` means the peer does not exist, belongs to
/// someone else, or is already on its way out (`pending_remove`/`removed`), all of which callers
/// treat as "nothing to do" rather than an error.
pub async fn mark_peer_for_removal(
    executor: impl PgExecutor<'_>,
    peer_id: i64,
    owner: Option<i64>,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"UPDATE peers SET sync_status = $1
           WHERE id = $2
             AND ($3::bigint IS NULL OR user_id = $3)
             AND sync_status IN ('active', 'pending_add')"#,
        PeerSyncStatus::PendingRemove as _,
        peer_id,
        owner,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Queue every live (`active`/`pending_add`) peer of `user_id` for removal. Returns how many
/// peers changed — `0` means the user had nothing the daemon still serves.
pub async fn mark_user_peers_for_removal(
    executor: impl PgExecutor<'_>,
    user_id: i64,
) -> Result<u64> {
    let result = sqlx::query!(
        r#"UPDATE peers SET sync_status = $1
           WHERE user_id = $2 AND sync_status IN ('active', 'pending_add')"#,
        PeerSyncStatus::PendingRemove as _,
        user_id,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}

/// Prepare an installation's peers for the installation row being deleted: its live peers are
/// queued for removal (a device that no longer exists must not keep a tunnel — or a peer slot),
/// and every peer that pointed at it, live or historical, is detached so the FK allows the
/// DELETE. Runs in the caller's transaction so the delete is atomic with it. Returns how many
/// peers were queued for removal.
pub async fn release_installation_peers(
    tx: &mut PgTransaction<'_>,
    installation_id: i64,
) -> Result<u64> {
    let queued = sqlx::query!(
        r#"UPDATE peers SET sync_status = $1
           WHERE installation_id = $2 AND sync_status IN ('active', 'pending_add')"#,
        PeerSyncStatus::PendingRemove as _,
        installation_id,
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    sqlx::query!(
        "UPDATE peers SET installation_id = NULL WHERE installation_id = $1",
        installation_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(queued)
}

/// A live peer together with the installation (device) it belongs to.
#[derive(Debug, Clone)]
pub struct DevicePeer {
    pub id: i64,
    pub assigned_ip: String,
    pub sync_status: PeerSyncStatus,
    pub protocol: Protocol,
    pub last_handshake: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub device_id: String,
    pub device_name: Option<String>,
}

/// Find the live (not removed / pending removal) peer a user's device holds for `protocol`.
///
/// A device may hold one live peer per protocol, so the protocol is part of the lookup.
pub async fn find_peer_by_device_id(
    executor: impl PgExecutor<'_>,
    user_id: i64,
    device_id: &str,
    protocol: Protocol,
) -> Result<Option<DevicePeer>> {
    let peer = sqlx::query_as!(
        DevicePeer,
        r#"
        SELECT p.id, p.assigned_ip, p.sync_status AS "sync_status: PeerSyncStatus",
               p.protocol AS "protocol: Protocol", p.last_handshake, p.created_at,
               ai.device_id, ai.device_name
        FROM peers p
        JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND ai.device_id = $2 AND p.protocol = $3
          AND p.sync_status NOT IN ('removed', 'pending_remove')
        "#,
        user_id,
        device_id,
        protocol as _,
    )
    .fetch_optional(executor)
    .await?;

    Ok(peer)
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
                client_subnet: "10.200.0.0/24".parse().unwrap(),
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
            client_subnet: "10.101.0.0/24".parse().unwrap(),
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

    // ── replace_active_subscription ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_replace_active_subscription_leaves_one_active(pool: DbPool) {
        let basic = get_basic_plan_id(&pool).await;
        let standard = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'standard'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, basic).await; // permanent
        seed_subscription(&pool, user_id, basic).await; // a second active one (legacy data)

        let mut tx = pool.begin().await.unwrap();
        let new_id = replace_active_subscription(
            &mut tx,
            user_id,
            standard,
            Some(Utc::now() + Duration::days(30)),
            SubscriptionSource::AdminGrant,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let active = sqlx::query!(
            r#"SELECT id, plan_id, source AS "source: SubscriptionSource" FROM subscriptions
               WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())"#,
            user_id
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, new_id);
        assert_eq!(active[0].plan_id, standard);
        assert_eq!(active[0].source, SubscriptionSource::AdminGrant);

        // The superseded ones are closed, not deleted.
        let total = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1",
            user_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(total, Some(3));
    }

    // ── credential auth (login + password) ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_credential_user_and_login(pool: DbPool) {
        let res = create_credential_user(&pool, "Alice", "hunter2hunter")
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
        create_credential_user(&pool, "bob", "password123")
            .await
            .unwrap();
        // Same login, different case → still taken.
        let err = create_credential_user(&pool, "BOB", "password123")
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::CredentialTaken));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_credential_user_gets_taster_not_real_trial(pool: DbPool) {
        let res = create_credential_user(&pool, "carol", "password123")
            .await
            .unwrap();

        // The one-time real trial is NOT consumed (so it can be claimed later via Telegram link).
        let trial_used: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar!("SELECT trial_used_at FROM users WHERE id = $1", res.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(trial_used.is_none());

        // A taster subscription exists (read back through the enum).
        let sources: Vec<SubscriptionSource> = sqlx::query_scalar!(
            r#"SELECT source AS "source: SubscriptionSource" FROM subscriptions WHERE user_id = $1"#,
            res.id
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(sources, vec![SubscriptionSource::Taster]);

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
        let err = create_credential_user(&pool, "dave", "short")
            .await
            .unwrap_err();
        assert!(matches!(err, FloppaError::InvalidPassword(_)));
    }

    // ── Telegram link + merge ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_attach_telegram_grants_real_trial(pool: DbPool) {
        get_basic_plan_id(&pool).await;
        let user = create_credential_user(&pool, "newbie", "password123")
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
        let survivor = create_credential_user(&pool, "recover_me", "password123")
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
        sqlx::query!(
            "UPDATE users SET trial_used_at = NOW(), is_admin = true WHERE id = $1",
            husk
        )
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

        // The daemon learns about the survivor's new plan through this channel.
        let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
            .await
            .unwrap();
        listener.listen("subscription_changed").await.unwrap();

        let merged = merge_telegram_into_session(&pool, survivor.id, husk)
            .await
            .unwrap();
        assert!(merged);

        let notification = tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv())
            .await
            .expect("merge must notify the daemon")
            .unwrap();
        assert_eq!(notification.payload(), survivor.id.to_string());

        // Husk row is gone.
        let husk_exists =
            sqlx::query_scalar!("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)", husk)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(husk_exists, Some(false));

        // Survivor now owns the Telegram id and is marked trial-used (no re-trialing), but does
        // NOT inherit the husk's admin flag.
        let row = sqlx::query!(
            "SELECT telegram_id, trial_used_at, is_admin FROM users WHERE id = $1",
            survivor.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.telegram_id, Some(99999));
        assert!(row.trial_used_at.is_some());
        assert!(!row.is_admin);

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
    async fn test_merge_same_device_same_protocol_keeps_survivor_peer(pool: DbPool) {
        let basic = get_basic_plan_id(&pool).await;

        // Both accounts hold an active AmneziaWG peer on the same device 'devA' — the common
        // "connect first, link Telegram later" flow on one phone.
        let survivor = create_credential_user(&pool, "same_phone", "password123")
            .await
            .unwrap();
        let survivor_inst = sqlx::query_scalar!(
            "INSERT INTO app_installations (user_id, device_id) VALUES ($1, 'devA') RETURNING id",
            survivor.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let survivor_peer = sqlx::query_scalar!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, installation_id, protocol) \
             VALUES ($1, 'PUBKEYSURV', '10.0.0.10', 'active', $2, 'amneziawg') RETURNING id",
            survivor.id,
            survivor_inst,
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let husk = seed_user(&pool, 99999).await;
        seed_subscription(&pool, husk, basic).await;
        let husk_inst = sqlx::query_scalar!(
            "INSERT INTO app_installations (user_id, device_id) VALUES ($1, 'devA') RETURNING id",
            husk
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let husk_peer = sqlx::query_scalar!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status, installation_id, protocol) \
             VALUES ($1, 'PUBKEYHUSK', '10.0.0.11', 'active', $2, 'amneziawg') RETURNING id",
            husk,
            husk_inst,
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let merged = merge_telegram_into_session(&pool, survivor.id, husk)
            .await
            .unwrap();
        assert!(merged);

        // Exactly one live peer remains for (devA, amneziawg): the survivor's own.
        let live: Vec<i64> = sqlx::query_scalar!(
            r#"SELECT id FROM peers
               WHERE installation_id = $1 AND protocol = 'amneziawg'
                 AND sync_status NOT IN ('removed', 'pending_remove')"#,
            survivor_inst
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(live, vec![survivor_peer]);

        // The husk's peer was handed to the daemon for removal (frees its IP), now owned by the
        // survivor and attached to the survivor's installation.
        let row = sqlx::query!(
            "SELECT user_id, installation_id, sync_status FROM peers WHERE id = $1",
            husk_peer
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.user_id, survivor.id);
        assert_eq!(row.installation_id, Some(survivor_inst));
        assert_eq!(row.sync_status, "pending_remove");
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
    async fn test_upsert_without_username_keeps_existing(pool: DbPool) {
        get_basic_plan_id(&pool).await;

        // A credential account that later linked Telegram keeps its login as `username`; a
        // /start from a Telegram profile without a public @username must not wipe it.
        let user = create_credential_user(&pool, "MyLogin", "password123")
            .await
            .unwrap();
        sqlx::query!("UPDATE users SET telegram_id = 4242 WHERE id = $1", user.id)
            .execute(&pool)
            .await
            .unwrap();

        let result = upsert_user(&pool, 4242, None, TelegramProfile::default(), false)
            .await
            .unwrap();
        assert_eq!(result.id, user.id);
        assert_eq!(result.username.as_deref(), Some("MyLogin"));

        // A real Telegram username still replaces it.
        let result = upsert_user(
            &pool,
            4242,
            Some("tg_name"),
            TelegramProfile::default(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.username.as_deref(), Some("tg_name"));
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

        // The one-time trial must not be burned when nothing was granted.
        let trial_used_at =
            sqlx::query_scalar!("SELECT trial_used_at FROM users WHERE id = $1", result.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(trial_used_at.is_none());
    }

    // ── allocate_ip ──

    fn net(s: &str) -> Ipv4Network {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_allocate_ip_first_ip(pool: DbPool) {
        let got = allocate_ip(&pool, net("10.200.0.0/24"), &[ip("10.200.0.1")])
            .await
            .unwrap();
        assert_eq!(got, ip("10.200.0.2")); // skips .0 (network) and .1 (server)
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_allocate_ip_reserves_configured_server_ip(pool: DbPool) {
        // Server sits on .1 by default, but an operator may put it elsewhere; the allocator must
        // never hand the server's own address to a client.
        let got = allocate_ip(&pool, net("10.200.0.0/24"), &[ip("10.200.0.2")])
            .await
            .unwrap();
        assert_eq!(got, ip("10.200.0.1"));

        // A subnet whose ip part is not the network address still allocates from the network.
        let got = allocate_ip(&pool, net("10.200.0.7/24"), &[ip("10.200.0.1")])
            .await
            .unwrap();
        assert_eq!(got, ip("10.200.0.2"));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_skips_configured_server_ip(pool: DbPool) {
        let mut config = test_config();
        config.wireguard.server_ip = Some(ip("10.200.0.2"));
        let ctx = test_ctx(&pool, &config);
        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let result = create_peer(&ctx, user_id, wg_opts()).await.unwrap();
        assert_eq!(result.assigned_ip, "10.200.0.1");
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

        let got = allocate_ip(&pool, net("10.200.0.0/24"), &[ip("10.200.0.1")])
            .await
            .unwrap();
        assert_eq!(got, ip("10.200.0.3"));
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

        let got = allocate_ip(&pool, net("10.200.0.0/24"), &[ip("10.200.0.1")])
            .await
            .unwrap();
        assert_eq!(got, ip("10.200.0.2")); // removed peer's IP is reusable
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

        let result = allocate_ip(&pool, net("10.200.0.0/30"), &[ip("10.200.0.1")]).await;
        assert!(matches!(result, Err(FloppaError::NoAvailableIps)));
    }

    // ── create_peer ──

    /// A standalone WireGuard peer — `test_config()` has no [amneziawg] section.
    fn wg_opts() -> CreatePeerOptions {
        CreatePeerOptions {
            installation_id: None,
            protocol: Protocol::WireGuard,
        }
    }

    #[test]
    fn default_peer_options_are_standalone_amneziawg() {
        let opts = CreatePeerOptions::default();
        assert_eq!(opts.installation_id, None);
        assert_eq!(opts.protocol, Protocol::AmneziaWg);
    }

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

        let result = create_peer(&ctx, user_id, wg_opts()).await.unwrap();

        assert_eq!(result.assigned_ip, "10.200.0.2");
        assert!(!result.private_key_plaintext.is_empty());
        assert!(result.config.contains("[Interface]"));
        assert!(result.config.contains("[Peer]"));

        // Verify peer in DB — decoded through the enum, so the stored form is the enum's form.
        let row = sqlx::query!(
            r#"SELECT sync_status AS "sync_status: PeerSyncStatus", protocol AS "protocol: Protocol"
               FROM peers WHERE id = $1"#,
            result.id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.sync_status, PeerSyncStatus::PendingAdd);
        assert_eq!(row.protocol, Protocol::WireGuard);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_enum_columns_are_check_constrained(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let bad_status = sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status) VALUES ($1, 'k', '10.0.0.2', 'pending')",
            user_id,
        )
        .execute(&pool)
        .await;
        assert!(
            matches!(bad_status, Err(sqlx::Error::Database(e)) if e.constraint() == Some("peers_sync_status_check"))
        );

        let bad_protocol = sqlx::query!(
            "INSERT INTO peers (user_id, public_key, assigned_ip, protocol) VALUES ($1, 'k', '10.0.0.2', 'vless')",
            user_id,
        )
        .execute(&pool)
        .await;
        assert!(
            matches!(bad_protocol, Err(sqlx::Error::Database(e)) if e.constraint() == Some("peers_protocol_check"))
        );

        let plan_id = get_basic_plan_id(&pool).await;
        let bad_source = sqlx::query!(
            "INSERT INTO subscriptions (user_id, plan_id, starts_at, source) VALUES ($1, $2, NOW(), 'gift')",
            user_id,
            plan_id,
        )
        .execute(&pool)
        .await;
        assert!(
            matches!(bad_source, Err(sqlx::Error::Database(e)) if e.constraint() == Some("subscriptions_source_check"))
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_peer_no_subscription(pool: DbPool) {
        let config = test_config();
        let ctx = test_ctx(&pool, &config);
        let user_id = seed_user(&pool, 11111).await;

        let result = create_peer(&ctx, user_id, wg_opts()).await;
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
        create_peer(&ctx, user_id, wg_opts()).await.unwrap();

        // Second peer should fail
        let result = create_peer(&ctx, user_id, wg_opts()).await;
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
            client_subnet: "10.101.0.0/24".parse().unwrap(),
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
            CreatePeerOptions {
                installation_id: Some(inst.id),
                protocol: Protocol::WireGuard,
            },
        )
        .await
        .unwrap();

        // AmneziaWG peer for the SAME device → allowed despite max_peers=1 (same slot).
        create_peer(
            &ctx,
            user_id,
            CreatePeerOptions {
                installation_id: Some(inst.id),
                protocol: Protocol::AmneziaWg,
            },
        )
        .await
        .unwrap();

        // A standalone exported config (no installation) now exceeds the limit.
        let result = create_peer(&ctx, user_id, wg_opts()).await;
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

        let options = CreatePeerOptions {
            installation_id: Some(installation.id),
            protocol: Protocol::WireGuard,
        };

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
            CreatePeerOptions {
                installation_id: Some(foreign.id),
                protocol: Protocol::WireGuard,
            },
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

        let options = || CreatePeerOptions {
            installation_id: Some(installation.id),
            protocol: Protocol::WireGuard,
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
            create_peer(&ctx, user_a, wg_opts()),
            create_peer(&ctx, user_b, wg_opts()),
        );
        let peer_a = peer_a.unwrap();
        let peer_b = peer_b.unwrap();

        assert_ne!(peer_a.assigned_ip, peer_b.assigned_ip);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_create_awg_peer_without_server_key_writes_nothing(pool: DbPool) {
        use crate::config::{AmneziaWgConfig, AwgObfuscation};
        let mut config = test_config();
        config.amneziawg = Some(AmneziaWgConfig {
            interface: "awg-test".into(),
            endpoint: "vpn.test.com:51821".into(),
            listen_port: None,
            client_subnet: "10.101.0.0/24".parse().unwrap(),
            server_ip: None,
            dns: vec!["1.1.1.1".into()],
            allowed_ips: "0.0.0.0/0, ::/0".into(),
            mtu: 1280,
            rate_limit: None,
            obfuscation: AwgObfuscation::default(),
        });
        // [amneziawg] is configured but the server has no awg_private_key → no public key.
        let ctx = test_ctx(&pool, &config);
        let plan_id = get_basic_plan_id(&pool).await;
        let user_id = seed_user(&pool, 11111).await;
        seed_subscription(&pool, user_id, plan_id).await;

        let result = create_peer(
            &ctx,
            user_id,
            CreatePeerOptions {
                installation_id: None,
                protocol: Protocol::AmneziaWg,
            },
        )
        .await;
        assert!(matches!(result, Err(FloppaError::AmneziaWgNotConfigured)));

        // The check happens before the transaction: no orphan peer row for the daemon to serve.
        let peers = sqlx::query_scalar!("SELECT COUNT(*) FROM peers WHERE user_id = $1", user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(peers, Some(0));
    }

    // ── ensure_vless_uuid ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_ensure_vless_uuid_is_stable_and_race_free(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        // Two concurrent first calls agree on one UUID.
        let (a, b) = tokio::join!(
            ensure_vless_uuid(&pool, user_id),
            ensure_vless_uuid(&pool, user_id),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a, b);

        // Later calls return the stored one; the column holds its hyphenated text form.
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(ensure_vless_uuid(&mut *tx, user_id).await.unwrap(), a);
        tx.commit().await.unwrap();
        let stored = sqlx::query_scalar!("SELECT vless_uuid FROM users WHERE id = $1", user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some(a.to_string().as_str()));

        // Unknown user → a database "no rows" error, not a minted UUID.
        assert!(matches!(
            ensure_vless_uuid(&pool, -1).await,
            Err(FloppaError::Database(sqlx::Error::RowNotFound))
        ));
    }

    // ── release_installation_peers ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_release_installation_peers_queues_live_and_detaches_all(pool: DbPool) {
        let user = seed_user(&pool, 11111).await;
        let inst = upsert_installation(&pool, user, "dev-1", None, None, None)
            .await
            .unwrap()
            .id;
        let insert = |key: &'static str, ip: &'static str, status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar!(
                    "INSERT INTO peers (user_id, installation_id, public_key, assigned_ip, \
                     sync_status) VALUES ($1, $2, $3, $4, $5) RETURNING id",
                    user,
                    inst,
                    key,
                    ip,
                    status,
                )
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let active = insert("k-active", "10.0.0.2", "active").await;
        let removed = insert("k-removed", "10.0.0.3", "removed").await;

        let mut tx = pool.begin().await.unwrap();
        assert_eq!(release_installation_peers(&mut tx, inst).await.unwrap(), 1);
        sqlx::query!("DELETE FROM app_installations WHERE id = $1", inst)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows = sqlx::query!(
            r#"SELECT id, sync_status AS "s: PeerSyncStatus", installation_id
               FROM peers WHERE user_id = $1 ORDER BY id"#,
            user
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows[0].id, active);
        assert_eq!(rows[0].s, PeerSyncStatus::PendingRemove);
        assert_eq!(rows[1].id, removed);
        assert_eq!(rows[1].s, PeerSyncStatus::Removed);
        assert!(rows.iter().all(|r| r.installation_id.is_none()));
    }

    // ── mark_peer_for_removal ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_mark_peer_for_removal_transitions(pool: DbPool) {
        let owner = seed_user(&pool, 11111).await;
        let stranger = seed_user(&pool, 22222).await;
        let insert = |key: &'static str, ip: &'static str, status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar!(
                    "INSERT INTO peers (user_id, public_key, assigned_ip, sync_status) \
                     VALUES ($1, $2, $3, $4) RETURNING id",
                    owner,
                    key,
                    ip,
                    status,
                )
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let active = insert("k-active", "10.0.0.2", "active").await;
        let pending_add = insert("k-pending", "10.0.0.3", "pending_add").await;
        let removed = insert("k-removed", "10.0.0.4", "removed").await;

        // Wrong owner → untouched.
        assert!(
            !mark_peer_for_removal(&pool, active, Some(stranger))
                .await
                .unwrap()
        );
        // Right owner → pending_remove; a second call is a no-op.
        assert!(
            mark_peer_for_removal(&pool, active, Some(owner))
                .await
                .unwrap()
        );
        assert!(
            !mark_peer_for_removal(&pool, active, Some(owner))
                .await
                .unwrap()
        );
        // Admin path (no owner) also covers peers the daemon has not added yet.
        assert!(
            mark_peer_for_removal(&pool, pending_add, None)
                .await
                .unwrap()
        );
        // Already removed / unknown → nothing to do.
        assert!(!mark_peer_for_removal(&pool, removed, None).await.unwrap());
        assert!(!mark_peer_for_removal(&pool, -1, None).await.unwrap());

        let statuses: Vec<PeerSyncStatus> = sqlx::query_scalar!(
            r#"SELECT sync_status AS "s: PeerSyncStatus" FROM peers WHERE user_id = $1 ORDER BY id"#,
            owner
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            statuses,
            vec![
                PeerSyncStatus::PendingRemove,
                PeerSyncStatus::PendingRemove,
                PeerSyncStatus::Removed
            ]
        );
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
            .unwrap()
            .expect("peer found");
        assert_eq!(result.id, peer_id);
        assert_eq!(result.device_id, "dev-123");
        assert_eq!(result.sync_status, PeerSyncStatus::Active);
        assert_eq!(result.protocol, Protocol::AmneziaWg);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_find_peer_by_device_id_not_found(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;

        let result = find_peer_by_device_id(&pool, user_id, "nonexistent", Protocol::AmneziaWg)
            .await
            .unwrap();
        assert!(result.is_none());
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
        assert!(result.is_none());
    }

    // ── resolve_subscription_expires ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_resolve_subscription_expires(pool: DbPool) {
        let basic = get_basic_plan_id(&pool).await;
        let basic_trial_minutes =
            sqlx::query_scalar!("SELECT trial_minutes FROM plans WHERE id = $1", basic)
                .fetch_one(&pool)
                .await
                .unwrap()
                .expect("seeded basic plan has a trial");
        let no_trial = sqlx::query_scalar!(
            "INSERT INTO plans (name, display_name, max_peers) VALUES ('flat', 'Flat', 1) RETURNING id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let now = Utc::now();
        let resolve = |plan: i32, term: SubscriptionTerm| {
            let pool = pool.clone();
            async move { resolve_subscription_expires(&pool, plan, term, now).await }
        };

        assert_eq!(
            resolve(basic, SubscriptionTerm::Days(3)).await.unwrap(),
            Some(now + Duration::days(3))
        );
        assert_eq!(
            resolve(basic, SubscriptionTerm::PlanDefault).await.unwrap(),
            Some(now + Duration::minutes(i64::from(basic_trial_minutes)))
        );
        assert_eq!(
            resolve(basic, SubscriptionTerm::Permanent).await.unwrap(),
            None
        );
        assert!(matches!(
            resolve(no_trial, SubscriptionTerm::PlanDefault).await,
            Err(SubscriptionTermError::NoDuration)
        ));
        // The plan is validated for every term, not just PlanDefault.
        assert!(matches!(
            resolve(-1, SubscriptionTerm::Permanent).await,
            Err(SubscriptionTermError::PlanNotFound(-1))
        ));
        // A huge day count is an error, not a panic.
        assert!(matches!(
            resolve(basic, SubscriptionTerm::Days(u32::MAX)).await,
            Err(SubscriptionTermError::DurationOutOfRange)
        ));
    }

    // ── VLESS UUID ──

    #[sqlx::test(migrations = "../migrations")]
    async fn test_rotate_vless_uuid_only_rotates_existing(pool: DbPool) {
        let user = seed_user(&pool, 11111).await;
        assert!(rotate_vless_uuid(&pool, user).await.unwrap().is_none());
        assert!(rotate_vless_uuid(&pool, -1).await.unwrap().is_none());

        let first = ensure_vless_uuid(&pool, user).await.unwrap();
        let rotated = rotate_vless_uuid(&pool, user)
            .await
            .unwrap()
            .expect("user has a uuid to rotate");
        assert_ne!(first, rotated);
        assert_eq!(ensure_vless_uuid(&pool, user).await.unwrap(), rotated);
    }
}
