use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use floppa_core::{FloppaError, decrypt_private_key, services};
use serde::{Deserialize, Serialize};
use teloxide::{prelude::*, types::InputFile};
use utoipa::ToSchema;

use crate::admin::auth::AuthUser;

use super::AppState;

#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    id: i64,
    telegram_id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
    subscription: Option<MySubscription>,
}

#[derive(Serialize, ToSchema)]
pub struct MySubscription {
    plan_name: String,
    plan_display_name: String,
    starts_at: chrono::DateTime<Utc>,
    expires_at: Option<chrono::DateTime<Utc>>,
    speed_limit_mbps: Option<i32>,
    traffic_limit_bytes: Option<i64>,
    max_peers: i32,
}

#[derive(Serialize, ToSchema)]
pub struct MyPeer {
    id: i64,
    assigned_ip: String,
    sync_status: String,
    tx_bytes: i64,
    rx_bytes: i64,
    traffic_used_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    created_at: chrono::DateTime<Utc>,
    device_name: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatePeerResponse {
    id: i64,
    assigned_ip: String,
    config: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreatePeerRequest {
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
}

/// Get current authenticated user info
#[utoipa::path(
    get,
    path = "/me",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = MeResponse),
        (status = 401, description = "Unauthorized"),
    )
)]
pub(super) async fn get_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, StatusCode> {
    let user = sqlx::query!(
        "SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Get active subscription with plan info
    let subscription = sqlx::query_as!(
        MySubscription,
        r#"
        SELECT
            p.name as plan_name,
            p.display_name as plan_display_name,
            s.starts_at,
            s.expires_at,
            p.default_speed_limit_mbps as speed_limit_mbps,
            p.default_traffic_limit_bytes as traffic_limit_bytes,
            p.max_peers
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LIMIT 1
        "#,
        auth.user_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(MeResponse {
        id: user.id,
        telegram_id: user.telegram_id,
        username: user.username,
        first_name: user.first_name,
        last_name: user.last_name,
        photo_url: user.photo_url,
        is_admin: user.is_admin,
        subscription,
    }))
}

/// List current user's peers
#[utoipa::path(
    get,
    path = "/me/peers",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<MyPeer>),
        (status = 401, description = "Unauthorized"),
    )
)]
pub(super) async fn get_my_peers(
    auth: AuthUser,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<MyPeer>>, StatusCode> {
    // Update client_version for all user's peers from X-Client-Version header
    if let Some(version) = headers
        .get("X-Client-Version")
        .and_then(|v| v.to_str().ok())
    {
        let _ = sqlx::query!(
            "UPDATE peers SET client_version = $1 WHERE user_id = $2 AND sync_status != 'removed'",
            version,
            auth.user_id
        )
        .execute(&state.pool)
        .await;
    }

    let peers: Vec<MyPeer> = sqlx::query_as!(
        MyPeer,
        r#"
        SELECT id, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, created_at, device_name, device_id
        FROM peers
        WHERE user_id = $1 AND sync_status != 'removed'
        ORDER BY created_at DESC
        "#,
        auth.user_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(peers))
}

/// Create a new WireGuard peer for the current user
#[utoipa::path(
    post,
    path = "/me/peers",
    tag = "user",
    security(("bearer" = [])),
    request_body(content = Option<CreatePeerRequest>, content_type = "application/json"),
    responses(
        (status = 200, body = CreatePeerResponse),
        (status = 401, description = "Unauthorized"),
        (status = 402, description = "No active subscription"),
        (status = 403, description = "Peer limit reached"),
        (status = 500, description = "Internal server error"),
    )
)]
pub(super) async fn create_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    body: Option<Json<CreatePeerRequest>>,
) -> Result<Json<CreatePeerResponse>, StatusCode> {
    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| {
            tracing::error!("Auth secrets required for encryption");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .get_encryption_key()
        .map_err(|e| {
            tracing::error!("Invalid encryption key: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let options = body.map(|Json(req)| services::CreatePeerOptions {
        device_name: req.device_name,
        device_id: req.device_id,
    });

    let result = services::create_peer(
        &state.pool,
        auth.user_id,
        &state.config,
        &encryption_key,
        &state.wg_public_key,
        options,
    )
    .await
    .map_err(|e| match e {
        FloppaError::NoActiveSubscription => StatusCode::PAYMENT_REQUIRED,
        FloppaError::PeerLimitReached { .. } => StatusCode::FORBIDDEN,
        other => {
            tracing::error!("Failed to create peer: {}", other);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    Ok(Json(CreatePeerResponse {
        id: result.id,
        assigned_ip: result.assigned_ip,
        config: result.config,
    }))
}

/// Delete a peer owned by the current user
#[utoipa::path(
    delete,
    path = "/me/peers/{id}",
    tag = "user",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "Peer ID")),
    responses(
        (status = 200, description = "Peer deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Peer not found"),
    )
)]
pub(super) async fn delete_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let result = sqlx::query!(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1 AND user_id = $2 AND sync_status = 'active'",
        peer_id,
        auth.user_id
    )
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::OK)
}

/// Get WireGuard config for a peer owned by the current user
#[utoipa::path(
    get,
    path = "/me/peers/{id}/config",
    tag = "user",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "Peer ID")),
    responses(
        (status = 200, description = "WireGuard config file", body = String),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Peer not found"),
    )
)]
pub(super) async fn get_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<String, StatusCode> {
    let peer = sqlx::query!(
        r#"
        SELECT private_key_encrypted, assigned_ip
        FROM peers
        WHERE id = $1 AND user_id = $2 AND sync_status != 'removed'
        "#,
        peer_id,
        auth.user_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Decrypt the private key using secrets
    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| {
            tracing::error!("Auth secrets required for decryption");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .get_encryption_key()
        .map_err(|e| {
            tracing::error!("Invalid encryption key: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let encrypted = peer.private_key_encrypted.as_deref().ok_or_else(|| {
        tracing::error!("Peer {} has no encrypted private key", peer_id);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let private_key = decrypt_private_key(encrypted, &encryption_key).map_err(|e| {
        tracing::error!("Decryption failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let config = services::generate_wg_config(
        &private_key,
        &peer.assigned_ip,
        &state.config,
        &state.wg_public_key,
    );
    Ok(config)
}

/// Send WireGuard config to user via Telegram bot
#[utoipa::path(
    post,
    path = "/me/peers/{id}/send-config",
    tag = "user",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "Peer ID")),
    responses(
        (status = 200, description = "Config sent via Telegram"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Peer not found"),
        (status = 502, description = "Failed to send via Telegram"),
    )
)]
pub(super) async fn send_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    // Get peer's encrypted key and IP
    let peer = sqlx::query!(
        r#"
        SELECT private_key_encrypted, assigned_ip
        FROM peers
        WHERE id = $1 AND user_id = $2 AND sync_status != 'removed'
        "#,
        peer_id,
        auth.user_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let assigned_ip = peer.assigned_ip.clone();

    // Decrypt and generate config
    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| {
            tracing::error!("Auth secrets required for decryption");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .get_encryption_key()
        .map_err(|e| {
            tracing::error!("Invalid encryption key: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let encrypted = peer.private_key_encrypted.as_deref().ok_or_else(|| {
        tracing::error!("Peer {} has no encrypted private key", peer_id);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let private_key = decrypt_private_key(encrypted, &encryption_key).map_err(|e| {
        tracing::error!("Decryption failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let config = services::generate_wg_config(
        &private_key,
        &assigned_ip,
        &state.config,
        &state.wg_public_key,
    );

    // Get user's telegram_id
    let telegram_id =
        sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", auth.user_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error fetching telegram_id: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    // Send config as document via Telegram bot
    let filename = format!("floppa-vpn-{assigned_ip}.conf");
    let file = InputFile::memory(config.into_bytes()).file_name(filename);

    state
        .bot
        .send_document(ChatId(telegram_id), file)
        .await
        .map_err(|e| {
            tracing::error!("Failed to send config via Telegram: {e}");
            StatusCode::BAD_GATEWAY
        })?;

    Ok(StatusCode::OK)
}

/// Get a peer by device_id for the current user
#[utoipa::path(
    get,
    path = "/me/peers/by-device/{device_id}",
    tag = "user",
    security(("bearer" = [])),
    params(("device_id" = String, Path, description = "Device UUID")),
    responses(
        (status = 200, body = MyPeer),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "No peer for this device"),
    )
)]
pub(super) async fn get_my_peer_by_device(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<MyPeer>, StatusCode> {
    let peer: MyPeer = sqlx::query_as!(
        MyPeer,
        r#"
        SELECT id, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, created_at, device_name, device_id
        FROM peers
        WHERE user_id = $1 AND device_id = $2 AND sync_status NOT IN ('removed', 'pending_remove')
        "#,
        auth.user_id,
        &device_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(peer))
}
