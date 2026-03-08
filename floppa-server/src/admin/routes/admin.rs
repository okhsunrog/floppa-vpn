use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::admin::{auth::AdminUser, error::ApiError, vm_client};

use super::AppState;

#[derive(Serialize, ToSchema)]
pub struct Stats {
    total_users: i64,
    active_peers: i64,
    total_download_bytes: i64,
    total_upload_bytes: i64,
    active_subscriptions: i64,
    total_payments: i64,
    total_stars_revenue: i64,
}

/// Get system statistics (admin only)
#[utoipa::path(
    get,
    path = "/stats",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Stats),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn get_stats(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Stats>, ApiError> {
    let stats = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM users) as "total_users!",
            (SELECT COUNT(*) FROM peers WHERE sync_status = 'active') as "active_peers!",
            (SELECT COUNT(*) FROM subscriptions WHERE expires_at IS NULL OR expires_at > NOW()) as "active_subscriptions!",
            (SELECT COUNT(*) FROM payments WHERE status = 'completed') as "total_payments!",
            (SELECT COALESCE(SUM(amount), 0)::bigint FROM payments WHERE status = 'completed') as "total_stars_revenue!"
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let (total_download_bytes, total_upload_bytes) =
        vm_client::system_traffic(&state.http_client, &state.vm_url, 30)
            .await
            .unwrap_or((0, 0));

    Ok(Json(Stats {
        total_users: stats.total_users,
        active_peers: stats.active_peers,
        total_download_bytes,
        total_upload_bytes,
        active_subscriptions: stats.active_subscriptions,
        total_payments: stats.total_payments,
        total_stars_revenue: stats.total_stars_revenue,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct UserSummary {
    id: i64,
    telegram_id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
    created_at: chrono::DateTime<Utc>,
    active_plan: Option<String>,
    peer_count: i64,
    client_version: Option<String>,
    has_vless: bool,
}

/// List all users (admin only)
#[utoipa::path(
    get,
    path = "/users",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<UserSummary>),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn list_users(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserSummary>>, ApiError> {
    let users: Vec<UserSummary> = sqlx::query_as!(
        UserSummary,
        r#"
        SELECT
            u.id,
            u.telegram_id,
            u.username,
            u.first_name,
            u.last_name,
            u.photo_url,
            u.is_admin,
            u.created_at,
            (SELECT p.display_name FROM subscriptions s JOIN plans p ON s.plan_id = p.id WHERE s.user_id = u.id AND (s.expires_at IS NULL OR s.expires_at > NOW()) LIMIT 1) as active_plan,
            (SELECT COUNT(*) FROM peers p WHERE p.user_id = u.id AND p.sync_status != 'removed') as "peer_count!",
            (SELECT MAX(ai.app_version) FROM app_installations ai WHERE ai.user_id = u.id) as client_version,
            (u.vless_uuid IS NOT NULL) as "has_vless!"
        FROM users u
        ORDER BY u.created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(users))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateUserRequest {
    telegram_id: i64,
    #[serde(default)]
    username: Option<String>,
    /// Display name for the user (shown until they register and Telegram provides real name).
    #[serde(default)]
    first_name: Option<String>,
    plan_id: i32,
    /// Duration in days. Required unless `permanent` is true.
    #[serde(default)]
    days: Option<i64>,
    /// If true, creates a permanent subscription (no expiration date).
    #[serde(default)]
    permanent: bool,
}

#[derive(Serialize, ToSchema)]
pub struct CreateUserResponse {
    id: i64,
}

/// Pre-register a user by telegram_id and assign a subscription (admin only).
/// When the user later authenticates via Telegram, their account and subscription will be waiting.
#[utoipa::path(
    post,
    path = "/users",
    tag = "admin",
    security(("bearer" = [])),
    request_body = CreateUserRequest,
    responses(
        (status = 201, body = CreateUserResponse),
        (status = 400, body = ApiError, description = "Days not specified and plan has no trial_days"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Plan not found"),
        (status = 409, body = ApiError, description = "User with this telegram_id already exists"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn create_user(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();

    // Insert user row (fail if telegram_id already exists)
    let user_id = sqlx::query_scalar!(
        r#"
        INSERT INTO users (telegram_id, username, first_name, trial_used_at)
        VALUES ($1, $2, $3, NOW())
        RETURNING id
        "#,
        req.telegram_id,
        req.username.as_deref(),
        req.first_name.as_deref()
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e
            && db_err.constraint() == Some("users_telegram_id_key")
        {
            return ApiError::conflict("User with this telegram_id already exists");
        }
        ApiError::from(e)
    })?;

    let expires_at =
        super::resolve_subscription_expires(&state.pool, req.plan_id, req.days, req.permanent, now)
            .await?;

    // Create subscription
    sqlx::query!(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'admin_grant')",
        user_id,
        req.plan_id,
        now,
        expires_at
    )
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateUserResponse { id: user_id }),
    ))
}

#[derive(Serialize, ToSchema)]
pub struct UserDetail {
    id: i64,
    telegram_id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
    created_at: chrono::DateTime<Utc>,
    /// Total WG traffic for this user (includes removed peers), last 30 days.
    wg_download_bytes: i64,
    wg_upload_bytes: i64,
    peers: Vec<PeerDetail>,
    vless: Option<VlessAdminInfo>,
    subscriptions: Vec<SubscriptionDetail>,
}

#[derive(Serialize, ToSchema)]
pub struct VlessAdminInfo {
    has_uuid: bool,
    download_bytes: i64,
    upload_bytes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct PeerDetail {
    id: i64,
    public_key: String,
    assigned_ip: String,
    sync_status: String,
    download_bytes: i64,
    upload_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    device_name: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct SubscriptionDetail {
    id: i64,
    plan_id: i32,
    plan_name: String,
    plan_display_name: String,
    starts_at: chrono::DateTime<Utc>,
    expires_at: Option<chrono::DateTime<Utc>>,
    speed_limit_mbps: Option<i32>,
    max_peers: i32,
    is_active: bool,
    source: String,
}

/// Get user details (admin only)
#[utoipa::path(
    get,
    path = "/users/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, body = UserDetail),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "User not found"),
    )
)]
pub(super) async fn get_user(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<UserDetail>, ApiError> {
    let user = sqlx::query!(
        "SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin, created_at FROM users WHERE id = $1",
        id
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("User not found"))?;

    let peer_rows = sqlx::query!(
        r#"
        SELECT p.id, p.public_key, p.assigned_ip, p.sync_status, p.last_handshake,
               ai.device_name, ai.device_id AS "device_id?"
        FROM peers p
        LEFT JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND p.sync_status != 'removed'
        ORDER BY p.created_at DESC
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await?;

    let peer_ids: Vec<i64> = peer_rows.iter().map(|r| r.id).collect();
    let traffic = vm_client::peer_traffic(&state.http_client, &state.vm_url, &peer_ids, 30)
        .await
        .unwrap_or_default();

    let peers: Vec<PeerDetail> = peer_rows
        .into_iter()
        .map(|r| {
            let (download, upload) = traffic.get(&r.id).copied().unwrap_or((0, 0));
            PeerDetail {
                id: r.id,
                public_key: r.public_key,
                assigned_ip: r.assigned_ip,
                sync_status: r.sync_status,
                download_bytes: download,
                upload_bytes: upload,
                last_handshake: r.last_handshake,
                device_name: r.device_name,
                device_id: r.device_id,
            }
        })
        .collect();

    let subscriptions: Vec<SubscriptionDetail> = sqlx::query_as!(
        SubscriptionDetail,
        r#"
        SELECT s.id, s.plan_id, p.name as plan_name, p.display_name as plan_display_name,
               s.starts_at, s.expires_at,
               p.default_speed_limit_mbps as speed_limit_mbps,
               p.max_peers,
               (s.expires_at IS NULL OR s.expires_at > NOW()) as "is_active!",
               s.source
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1
        ORDER BY s.starts_at DESC
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await?;

    // User-level WG traffic (includes removed peers)
    let (wg_download_bytes, wg_upload_bytes) =
        vm_client::user_wg_traffic(&state.http_client, &state.vm_url, id, 30)
            .await
            .unwrap_or((0, 0));

    // VLESS info (only if server has VLESS configured)
    let vless = if state.config.vless.is_some() {
        let has_uuid =
            sqlx::query_scalar!("SELECT vless_uuid IS NOT NULL FROM users WHERE id = $1", id)
                .fetch_one(&state.pool)
                .await?
                .unwrap_or(false);

        let (download_bytes, upload_bytes) =
            vm_client::user_vless_traffic(&state.http_client, &state.vm_url, id, 30)
                .await
                .unwrap_or((0, 0));

        Some(VlessAdminInfo {
            has_uuid,
            download_bytes,
            upload_bytes,
        })
    } else {
        None
    };

    Ok(Json(UserDetail {
        id: user.id,
        telegram_id: user.telegram_id,
        username: user.username,
        first_name: user.first_name,
        last_name: user.last_name,
        photo_url: user.photo_url,
        is_admin: user.is_admin,
        created_at: user.created_at,
        wg_download_bytes,
        wg_upload_bytes,
        peers,
        vless,
        subscriptions,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct SetSubscriptionRequest {
    plan_id: i32,
    /// Duration in days. If omitted, uses the plan's trial_days (for trial plans).
    /// Use `permanent: true` to create a subscription with no expiration.
    #[serde(default)]
    days: Option<i64>,
    /// If true, creates a permanent subscription (no expiration date).
    #[serde(default)]
    permanent: bool,
}

/// Set (create or replace) a user's subscription (admin only).
/// If the user already has an active subscription, it will be expired first.
#[utoipa::path(
    put,
    path = "/users/{id}/subscription",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    request_body = SetSubscriptionRequest,
    responses(
        (status = 200, description = "Subscription set"),
        (status = 400, body = ApiError, description = "Days not specified and plan has no trial_days"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Plan not found"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn set_subscription(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SetSubscriptionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();

    let expires_at =
        super::resolve_subscription_expires(&state.pool, req.plan_id, req.days, req.permanent, now)
            .await?;

    let mut tx = state.pool.begin().await?;

    // Expire current active subscription (if any)
    sqlx::query!(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        id
    )
    .execute(&mut *tx)
    .await?;

    // Insert new subscription
    sqlx::query!(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'admin_grant')",
        id,
        req.plan_id,
        now,
        expires_at
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(StatusCode::OK)
}

/// Delete (expire) a user's active subscription (admin only)
#[utoipa::path(
    delete,
    path = "/users/{id}/subscription",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, description = "Subscription deleted"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "No active subscription found"),
    )
)]
pub(super) async fn delete_subscription(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let result = sqlx::query!(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("No active subscription found"));
    }

    Ok(StatusCode::OK)
}

/// Remove all active peers for a user (admin only)
#[utoipa::path(
    delete,
    path = "/users/{id}/peer",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, description = "Peers removed"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "No active peers found"),
    )
)]
pub(super) async fn remove_peer(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let result = sqlx::query!(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE user_id = $1 AND sync_status = 'active'",
        id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("No active peers found for this user"));
    }

    Ok(StatusCode::OK)
}

#[derive(Serialize, ToSchema)]
pub struct PeerSummary {
    id: i64,
    user_id: i64,
    username: Option<String>,
    assigned_ip: String,
    sync_status: String,
    download_bytes: i64,
    upload_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    device_name: Option<String>,
    device_id: Option<String>,
    client_version: Option<String>,
    plan_name: Option<String>,
    has_vless: bool,
}

/// List all peers (admin only)
#[utoipa::path(
    get,
    path = "/peers",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<PeerSummary>),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn list_peers(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PeerSummary>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.user_id, COALESCE(u.username, CONCAT_WS(' ', u.first_name, u.last_name)) AS username,
               p.assigned_ip, p.sync_status, p.last_handshake,
               ai.device_name, ai.device_id AS "device_id?", ai.app_version AS client_version,
               (SELECT pl.display_name FROM subscriptions s JOIN plans pl ON s.plan_id = pl.id WHERE s.user_id = u.id AND (s.expires_at IS NULL OR s.expires_at > NOW()) ORDER BY s.expires_at DESC NULLS FIRST LIMIT 1) AS plan_name,
               (u.vless_uuid IS NOT NULL) AS "has_vless!"
        FROM peers p
        JOIN users u ON p.user_id = u.id
        LEFT JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.sync_status != 'removed'
        ORDER BY p.last_handshake DESC NULLS LAST
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let peer_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let traffic = vm_client::peer_traffic(&state.http_client, &state.vm_url, &peer_ids, 30)
        .await
        .unwrap_or_default();

    let peers: Vec<PeerSummary> = rows
        .into_iter()
        .map(|r| {
            let (download, upload) = traffic.get(&r.id).copied().unwrap_or((0, 0));
            PeerSummary {
                id: r.id,
                user_id: r.user_id,
                username: r.username,
                assigned_ip: r.assigned_ip,
                sync_status: r.sync_status,
                download_bytes: download,
                upload_bytes: upload,
                last_handshake: r.last_handshake,
                device_name: r.device_name,
                device_id: r.device_id,
                client_version: r.client_version,
                plan_name: r.plan_name,
                has_vless: r.has_vless,
            }
        })
        .collect();

    Ok(Json(peers))
}

#[derive(Serialize, ToSchema)]
pub struct VlessPeerSummary {
    user_id: i64,
    username: Option<String>,
    device_name: Option<String>,
    app_version: Option<String>,
    plan_name: Option<String>,
    download_bytes: i64,
    upload_bytes: i64,
    has_wg: bool,
}

/// List all users with VLESS configs (admin only)
#[utoipa::path(
    get,
    path = "/vless-peers",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<VlessPeerSummary>),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn list_vless_peers(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<VlessPeerSummary>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT u.id,
               COALESCE(u.username, CONCAT_WS(' ', u.first_name, u.last_name)) AS username,
               latest_ai.device_name,
               latest_ai.app_version,
               (SELECT pl.display_name FROM subscriptions s JOIN plans pl ON s.plan_id = pl.id WHERE s.user_id = u.id AND (s.expires_at IS NULL OR s.expires_at > NOW()) ORDER BY s.expires_at DESC NULLS FIRST LIMIT 1) AS plan_name,
               EXISTS(SELECT 1 FROM peers p WHERE p.user_id = u.id AND p.sync_status NOT IN ('removed', 'pending_remove')) AS "has_wg!"
        FROM users u
        LEFT JOIN LATERAL (
            SELECT ai.device_name, ai.app_version
            FROM app_installations ai
            WHERE ai.user_id = u.id
            ORDER BY ai.last_seen_at DESC
            LIMIT 1
        ) latest_ai ON true
        WHERE u.vless_uuid IS NOT NULL
        ORDER BY u.id
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let traffic = vm_client::all_vless_traffic(&state.http_client, &state.vm_url, 30)
        .await
        .unwrap_or_default();

    let peers: Vec<VlessPeerSummary> = rows
        .into_iter()
        .map(|r| {
            let (download, upload) = traffic.get(&r.id).copied().unwrap_or((0, 0));
            VlessPeerSummary {
                user_id: r.id,
                username: r.username,
                device_name: r.device_name,
                app_version: r.app_version,
                plan_name: r.plan_name,
                download_bytes: download,
                upload_bytes: upload,
                has_wg: r.has_wg,
            }
        })
        .collect();

    Ok(Json(peers))
}

/// Delete a peer by ID (admin only)
#[utoipa::path(
    delete,
    path = "/peers/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "Peer ID")),
    responses(
        (status = 200, description = "Peer deleted"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Peer not found or not active"),
    )
)]
pub(super) async fn delete_admin_peer(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let result = sqlx::query!(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1 AND sync_status = 'active'",
        peer_id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Peer not found or not active"));
    }

    Ok(StatusCode::OK)
}

#[derive(Serialize, ToSchema)]
pub struct InstallationSummary {
    id: i64,
    user_id: i64,
    username: Option<String>,
    device_id: String,
    device_name: Option<String>,
    platform: Option<String>,
    app_version: Option<String>,
    last_seen_at: chrono::DateTime<Utc>,
    created_at: chrono::DateTime<Utc>,
    has_wg: bool,
    has_vless: bool,
}

/// List all app installations (admin only)
#[utoipa::path(
    get,
    path = "/installations",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<InstallationSummary>),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn list_installations(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<InstallationSummary>>, ApiError> {
    let rows: Vec<InstallationSummary> = sqlx::query_as!(
        InstallationSummary,
        r#"
        SELECT ai.id, ai.user_id,
               COALESCE(u.username, CONCAT_WS(' ', u.first_name, u.last_name)) AS username,
               ai.device_id, ai.device_name, ai.platform, ai.app_version,
               ai.last_seen_at, ai.created_at,
               EXISTS(SELECT 1 FROM peers p WHERE p.installation_id = ai.id AND p.sync_status NOT IN ('removed', 'pending_remove')) AS "has_wg!",
               (u.vless_uuid IS NOT NULL) AS "has_vless!"
        FROM app_installations ai
        JOIN users u ON ai.user_id = u.id
        ORDER BY ai.last_seen_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows))
}

/// Delete an app installation (admin only)
#[utoipa::path(
    delete,
    path = "/installations/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "Installation ID")),
    responses(
        (status = 200, description = "Installation deleted"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Installation not found"),
    )
)]
pub(super) async fn delete_installation(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let mut tx = state.pool.begin().await?;

    // Unlink peers from this installation
    sqlx::query!(
        "UPDATE peers SET installation_id = NULL WHERE installation_id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;

    let result = sqlx::query!("DELETE FROM app_installations WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Installation not found"));
    }

    tx.commit().await?;

    Ok(StatusCode::OK)
}

/// Regenerate VLESS UUID for a user (admin only). Old UUID stops working immediately.
#[utoipa::path(
    post,
    path = "/users/{id}/vless-config/regenerate",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, description = "VLESS config regenerated"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "User not found or has no VLESS config"),
    )
)]
pub(super) async fn regenerate_admin_vless_config(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(user_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let new_uuid = uuid::Uuid::new_v4().to_string();
    let result = sqlx::query!(
        "UPDATE users SET vless_uuid = $1 WHERE id = $2 AND vless_uuid IS NOT NULL",
        &new_uuid,
        user_id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("User not found or has no VLESS config"));
    }

    Ok(StatusCode::OK)
}
