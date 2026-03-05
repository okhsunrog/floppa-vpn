use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::admin::{auth::AdminUser, error::ApiError};

use super::AppState;

#[derive(Serialize, ToSchema)]
pub struct Stats {
    total_users: i64,
    active_peers: i64,
    total_tx_bytes: i64,
    total_rx_bytes: i64,
    active_subscriptions: i64,
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
            (SELECT COALESCE(SUM(tx_bytes), 0)::bigint FROM peers) as "total_tx_bytes!",
            (SELECT COALESCE(SUM(rx_bytes), 0)::bigint FROM peers) as "total_rx_bytes!",
            (SELECT COUNT(*) FROM subscriptions WHERE expires_at IS NULL OR expires_at > NOW()) as "active_subscriptions!"
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(Stats {
        total_users: stats.total_users,
        active_peers: stats.active_peers,
        total_tx_bytes: stats.total_tx_bytes,
        total_rx_bytes: stats.total_rx_bytes,
        active_subscriptions: stats.active_subscriptions,
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
            (SELECT MAX(p.client_version) FROM peers p WHERE p.user_id = u.id AND p.sync_status != 'removed') as client_version
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
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at) VALUES ($1, $2, $3, $4)",
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
    peers: Vec<PeerDetail>,
    subscriptions: Vec<SubscriptionDetail>,
}

#[derive(Serialize, ToSchema)]
pub struct PeerDetail {
    id: i64,
    public_key: String,
    assigned_ip: String,
    sync_status: String,
    tx_bytes: i64,
    rx_bytes: i64,
    traffic_used_bytes: i64,
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
    traffic_limit_bytes: Option<i64>,
    max_peers: i32,
    is_active: bool,
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

    let peers: Vec<PeerDetail> = sqlx::query_as!(
        PeerDetail,
        r#"
        SELECT id, public_key, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, device_name, device_id
        FROM peers WHERE user_id = $1 AND sync_status != 'removed'
        ORDER BY created_at DESC
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await?;

    let subscriptions: Vec<SubscriptionDetail> = sqlx::query_as!(
        SubscriptionDetail,
        r#"
        SELECT s.id, s.plan_id, p.name as plan_name, p.display_name as plan_display_name,
               s.starts_at, s.expires_at,
               p.default_speed_limit_mbps as speed_limit_mbps,
               p.default_traffic_limit_bytes as traffic_limit_bytes,
               p.max_peers,
               (s.expires_at IS NULL OR s.expires_at > NOW()) as "is_active!"
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1
        ORDER BY s.starts_at DESC
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(UserDetail {
        id: user.id,
        telegram_id: user.telegram_id,
        username: user.username,
        first_name: user.first_name,
        last_name: user.last_name,
        photo_url: user.photo_url,
        is_admin: user.is_admin,
        created_at: user.created_at,
        peers,
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
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at) VALUES ($1, $2, $3, $4)",
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
    tx_bytes: i64,
    rx_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    device_name: Option<String>,
    device_id: Option<String>,
    client_version: Option<String>,
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
    let peers: Vec<PeerSummary> = sqlx::query_as!(
        PeerSummary,
        r#"
        SELECT p.id, p.user_id, COALESCE(u.username, CONCAT_WS(' ', u.first_name, u.last_name)) AS username, p.assigned_ip, p.sync_status, p.tx_bytes, p.rx_bytes, p.last_handshake, p.device_name, p.device_id, p.client_version
        FROM peers p
        JOIN users u ON p.user_id = u.id
        WHERE p.sync_status != 'removed'
        ORDER BY p.last_handshake DESC NULLS LAST
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

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
