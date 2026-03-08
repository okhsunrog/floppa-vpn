use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use floppa_core::{decrypt_private_key, services};
use serde::{Deserialize, Serialize};
use teloxide::{prelude::*, types::InputFile};
use utoipa::ToSchema;

use crate::admin::{auth::AuthUser, error::ApiError, vm_client};

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
    max_peers: i32,
}

#[derive(Serialize, ToSchema)]
pub struct MyPeer {
    id: i64,
    assigned_ip: String,
    sync_status: String,
    download_bytes: i64,
    upload_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    created_at: chrono::DateTime<Utc>,
    device_name: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MyPeersResponse {
    peers: Vec<MyPeer>,
    /// VLESS info (None if VLESS not configured on server)
    vless: Option<VlessInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct VlessInfo {
    /// Whether the user has generated a VLESS UUID
    has_uuid: bool,
    download_bytes: i64,
    upload_bytes: i64,
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
    #[serde(default)]
    installation_id: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct VlessConfigResponse {
    uri: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpsertInstallationRequest {
    device_id: String,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    app_version: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct InstallationResponse {
    id: i64,
    device_id: String,
    device_name: Option<String>,
    platform: Option<String>,
    app_version: Option<String>,
    last_seen_at: chrono::DateTime<Utc>,
    created_at: chrono::DateTime<Utc>,
}

/// Get current authenticated user info
#[utoipa::path(
    get,
    path = "/me",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = MeResponse),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn get_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, ApiError> {
    let user = sqlx::query!(
        "SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.pool)
    .await?;

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
            p.max_peers
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
        LIMIT 1
        "#,
        auth.user_id
    )
    .fetch_optional(&state.pool)
    .await?;

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

/// Upsert an app installation (device registration)
#[utoipa::path(
    post,
    path = "/me/installations",
    tag = "user",
    security(("bearer" = [])),
    request_body = UpsertInstallationRequest,
    responses(
        (status = 200, body = InstallationResponse),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn upsert_my_installation(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpsertInstallationRequest>,
) -> Result<Json<InstallationResponse>, ApiError> {
    let installation = services::upsert_installation(
        &state.pool,
        auth.user_id,
        &req.device_id,
        req.device_name.as_deref(),
        req.platform.as_deref(),
        req.app_version.as_deref(),
    )
    .await?;

    Ok(Json(InstallationResponse {
        id: installation.id,
        device_id: installation.device_id,
        device_name: installation.device_name,
        platform: installation.platform,
        app_version: installation.app_version,
        last_seen_at: installation.last_seen_at,
        created_at: installation.created_at,
    }))
}

/// List current user's peers and VLESS info
#[utoipa::path(
    get,
    path = "/me/peers",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = MyPeersResponse),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn get_my_peers(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MyPeersResponse>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip, p.sync_status, p.last_handshake, p.created_at,
               ai.device_name, ai.device_id AS "device_id?"
        FROM peers p
        LEFT JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND p.sync_status != 'removed'
        ORDER BY p.created_at DESC
        "#,
        auth.user_id
    )
    .fetch_all(&state.pool)
    .await?;

    let peer_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let traffic = vm_client::peer_traffic(&state.http_client, &state.vm_url, &peer_ids, 30)
        .await
        .unwrap_or_default();

    let peers: Vec<MyPeer> = rows
        .into_iter()
        .map(|r| {
            let (download, upload) = traffic.get(&r.id).copied().unwrap_or((0, 0));
            MyPeer {
                id: r.id,
                assigned_ip: r.assigned_ip,
                sync_status: r.sync_status,
                download_bytes: download,
                upload_bytes: upload,
                last_handshake: r.last_handshake,
                created_at: r.created_at,
                device_name: r.device_name,
                device_id: r.device_id, // LEFT JOIN → already Option
            }
        })
        .collect();

    // VLESS info (only if server has VLESS configured)
    let vless = if state.config.vless.is_some() {
        let has_uuid = sqlx::query_scalar!(
            "SELECT vless_uuid IS NOT NULL FROM users WHERE id = $1",
            auth.user_id
        )
        .fetch_one(&state.pool)
        .await?
        .unwrap_or(false);

        let (download_bytes, upload_bytes) =
            vm_client::user_vless_traffic(&state.http_client, &state.vm_url, auth.user_id, 30)
                .await
                .unwrap_or((0, 0));

        Some(VlessInfo {
            has_uuid,
            download_bytes,
            upload_bytes,
        })
    } else {
        None
    };

    Ok(Json(MyPeersResponse { peers, vless }))
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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 402, body = ApiError, description = "No active subscription"),
        (status = 403, body = ApiError, description = "Peer limit reached"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn create_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    body: Option<Json<CreatePeerRequest>>,
) -> Result<Json<CreatePeerResponse>, ApiError> {
    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::internal("Auth secrets required for encryption"))?
        .get_encryption_key()
        .map_err(|e| ApiError::internal(format!("Invalid encryption key: {e}")))?;

    let ctx = services::CreatePeerContext {
        pool: &state.pool,
        config: &state.config,
        encryption_key: &encryption_key,
        wg_public_key: &state.wg_public_key,
    };

    // Resolve installation_id: use explicit field, or auto-upsert from legacy device_id/device_name
    let installation_id = if let Some(Json(ref req)) = body {
        if let Some(id) = req.installation_id {
            Some(id)
        } else if let Some(ref device_id) = req.device_id {
            let inst = services::upsert_installation(
                &state.pool,
                auth.user_id,
                device_id,
                req.device_name.as_deref(),
                None,
                None,
            )
            .await?;
            Some(inst.id)
        } else {
            None
        }
    } else {
        None
    };

    let options = installation_id.map(|id| services::CreatePeerOptions {
        installation_id: Some(id),
    });

    let result = services::create_peer(&ctx, auth.user_id, options).await?;

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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 404, body = ApiError, description = "Peer not found"),
    )
)]
pub(super) async fn delete_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let result = sqlx::query!(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1 AND user_id = $2 AND sync_status IN ('active', 'pending_add')",
        peer_id,
        auth.user_id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Peer not found"));
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
        (status = 200, description = "WireGuard .conf", body = String),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 404, body = ApiError, description = "Peer not found"),
    )
)]
pub(super) async fn get_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<String, ApiError> {
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
    .await?
    .ok_or_else(|| ApiError::not_found("Peer not found"))?;

    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::internal("Auth secrets required for decryption"))?
        .get_encryption_key()
        .map_err(|e| ApiError::internal(format!("Invalid encryption key: {e}")))?;

    let encrypted = peer.private_key_encrypted.as_deref().ok_or_else(|| {
        ApiError::internal(format!("Peer {peer_id} has no encrypted private key"))
    })?;
    let private_key = decrypt_private_key(encrypted, &encryption_key)
        .map_err(|e| ApiError::internal(format!("Decryption failed: {e}")))?;

    Ok(services::generate_wg_config(
        &private_key,
        &peer.assigned_ip,
        &state.config,
        &state.wg_public_key,
    ))
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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 404, body = ApiError, description = "Peer not found"),
        (status = 502, body = ApiError, description = "Failed to send via Telegram"),
    )
)]
pub(super) async fn send_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
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
    .await?
    .ok_or_else(|| ApiError::not_found("Peer not found"))?;

    let encryption_key = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::internal("Auth secrets required for decryption"))?
        .get_encryption_key()
        .map_err(|e| ApiError::internal(format!("Invalid encryption key: {e}")))?;

    let encrypted = peer.private_key_encrypted.as_deref().ok_or_else(|| {
        ApiError::internal(format!("Peer {peer_id} has no encrypted private key"))
    })?;
    let private_key = decrypt_private_key(encrypted, &encryption_key)
        .map_err(|e| ApiError::internal(format!("Decryption failed: {e}")))?;

    let wg_config = services::generate_wg_config(
        &private_key,
        &peer.assigned_ip,
        &state.config,
        &state.wg_public_key,
    );
    let filename = format!("floppa-vpn-{}.conf", peer.assigned_ip);

    // Get user's telegram_id
    let telegram_id =
        sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", auth.user_id)
            .fetch_one(&state.pool)
            .await?;

    // Send config as document via Telegram bot
    let file = InputFile::memory(wg_config.into_bytes()).file_name(filename);

    state
        .bot
        .send_document(ChatId(telegram_id), file)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("Failed to send config via Telegram: {e}")))?;

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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 404, body = ApiError, description = "No peer for this device"),
    )
)]
pub(super) async fn get_my_peer_by_device(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<MyPeer>, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.assigned_ip, p.sync_status, p.last_handshake, p.created_at,
               ai.device_name, ai.device_id
        FROM peers p
        JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND ai.device_id = $2 AND p.sync_status NOT IN ('removed', 'pending_remove')
        "#,
        auth.user_id,
        &device_id
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("No peer for this device"))?;

    let (download, upload) =
        vm_client::peer_traffic(&state.http_client, &state.vm_url, &[row.id], 30)
            .await
            .ok()
            .and_then(|m| m.get(&row.id).copied())
            .unwrap_or((0, 0));

    Ok(Json(MyPeer {
        id: row.id,
        assigned_ip: row.assigned_ip,
        sync_status: row.sync_status,
        download_bytes: download,
        upload_bytes: upload,
        last_handshake: row.last_handshake,
        created_at: row.created_at,
        device_name: row.device_name,
        device_id: Some(row.device_id),
    }))
}

/// Get VLESS config for the current user (generates UUID on first call)
#[utoipa::path(
    get,
    path = "/me/vless-config",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = VlessConfigResponse),
        (status = 400, body = ApiError, description = "VLESS not configured"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 402, body = ApiError, description = "No active subscription"),
    )
)]
pub(super) async fn get_my_vless_config(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<VlessConfigResponse>, ApiError> {
    // Verify VLESS is configured on the server
    let reality_public_key = state
        .secrets
        .vless
        .as_ref()
        .map(|v| v.reality_public_key.as_str())
        .ok_or_else(|| ApiError::bad_request("VLESS is not configured on this server"))?;

    if state.config.vless.is_none() {
        return Err(ApiError::bad_request(
            "VLESS is not configured on this server",
        ));
    }

    // Check active subscription
    let has_sub = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM subscriptions WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW()))",
        auth.user_id
    )
    .fetch_one(&state.pool)
    .await?;

    if has_sub != Some(true) {
        return Err(ApiError::from(
            floppa_core::FloppaError::NoActiveSubscription,
        ));
    }

    // Get or generate VLESS UUID
    let vless_uuid =
        sqlx::query_scalar!("SELECT vless_uuid FROM users WHERE id = $1", auth.user_id)
            .fetch_one(&state.pool)
            .await?;

    let uuid = match vless_uuid {
        Some(uuid) => uuid,
        None => {
            let new_uuid = uuid::Uuid::new_v4().to_string();
            sqlx::query!(
                "UPDATE users SET vless_uuid = $1 WHERE id = $2",
                &new_uuid,
                auth.user_id
            )
            .execute(&state.pool)
            .await?;
            new_uuid
        }
    };

    let uri = services::generate_vless_uri(&uuid, &state.config, reality_public_key)
        .map_err(|e| ApiError::internal(format!("Failed to generate VLESS URI: {e}")))?;

    Ok(Json(VlessConfigResponse { uri }))
}

/// Regenerate VLESS UUID for the current user (old UUID stops working immediately)
#[utoipa::path(
    post,
    path = "/me/vless-config/regenerate",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = VlessConfigResponse),
        (status = 400, body = ApiError, description = "VLESS not configured"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 402, body = ApiError, description = "No active subscription"),
    )
)]
pub(super) async fn regenerate_my_vless_config(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<VlessConfigResponse>, ApiError> {
    let reality_public_key = state
        .secrets
        .vless
        .as_ref()
        .map(|v| v.reality_public_key.as_str())
        .ok_or_else(|| ApiError::bad_request("VLESS is not configured on this server"))?;

    if state.config.vless.is_none() {
        return Err(ApiError::bad_request(
            "VLESS is not configured on this server",
        ));
    }

    // Check active subscription
    let has_sub = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM subscriptions WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW()))",
        auth.user_id
    )
    .fetch_one(&state.pool)
    .await?;

    if has_sub != Some(true) {
        return Err(ApiError::from(
            floppa_core::FloppaError::NoActiveSubscription,
        ));
    }

    let new_uuid = uuid::Uuid::new_v4().to_string();
    sqlx::query!(
        "UPDATE users SET vless_uuid = $1 WHERE id = $2",
        &new_uuid,
        auth.user_id
    )
    .execute(&state.pool)
    .await?;

    let uri = services::generate_vless_uri(&new_uuid, &state.config, reality_public_key)
        .map_err(|e| ApiError::internal(format!("Failed to generate VLESS URI: {e}")))?;

    Ok(Json(VlessConfigResponse { uri }))
}
