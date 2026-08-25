use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use floppa_core::{
    FloppaError, PeerSyncStatus, Protocol, SubscriptionSource, decrypt_private_key, services,
};
use serde::{Deserialize, Serialize};
use teloxide::{prelude::*, types::InputFile};
use utoipa::ToSchema;

use crate::admin::{
    auth::AuthUser,
    error::ApiError,
    vm_client::{self, Traffic, TrafficMetric},
};

use super::AppState;

/// Protocol assumed when a request omits it: clients that predate the `protocol` field only
/// speak plain WireGuard, so AmneziaWG has to be asked for explicitly.
const LEGACY_REQUEST_PROTOCOL: Protocol = Protocol::WireGuard;

/// A rendered client config for one of the user's peers.
struct PeerConfig {
    assigned_ip: String,
    /// The `.conf` text.
    text: String,
}

/// Load one of `user_id`'s peers (404 otherwise), decrypt its key and render its config.
async fn load_my_peer_config(
    state: &AppState,
    user_id: i64,
    peer_id: i64,
) -> Result<PeerConfig, ApiError> {
    let peer = sqlx::query!(
        r#"
        SELECT private_key_encrypted, assigned_ip, protocol AS "protocol: Protocol"
        FROM peers
        WHERE id = $1 AND user_id = $2 AND sync_status != $3
        "#,
        peer_id,
        user_id,
        PeerSyncStatus::Removed as _,
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Peer not found"))?;

    let encrypted = peer.private_key_encrypted.as_deref().ok_or_else(|| {
        ApiError::internal(format!("Peer {peer_id} has no encrypted private key"))
    })?;
    let private_key = decrypt_private_key(encrypted, &state.encryption_key)
        .map_err(|e| ApiError::internal(format!("Decryption failed for peer {peer_id}: {e}")))?;

    let text = render_peer_config(state, peer.protocol, &private_key, &peer.assigned_ip)?;
    Ok(PeerConfig {
        assigned_ip: peer.assigned_ip,
        text,
    })
}

/// The REALITY public key, if VLESS is offered by this server (config + secrets both present).
fn require_vless(state: &AppState) -> Result<&str, ApiError> {
    match (&state.config.vless, &state.secrets.vless) {
        (Some(_), Some(secrets)) => Ok(&secrets.reality_public_key),
        _ => Err(FloppaError::VlessNotConfigured.into()),
    }
}

/// 402 unless the user currently has an active subscription.
async fn require_active_subscription(state: &AppState, user_id: i64) -> Result<(), ApiError> {
    let has_sub = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM subscriptions
                         WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW()))
           AS "exists!""#,
        user_id
    )
    .fetch_one(&state.pool)
    .await?;
    if has_sub {
        Ok(())
    } else {
        Err(FloppaError::NoActiveSubscription.into())
    }
}

/// Render a client config for a peer, branching on its stored protocol.
fn render_peer_config(
    state: &AppState,
    protocol: Protocol,
    private_key: &str,
    assigned_ip: &str,
) -> Result<String, ApiError> {
    match protocol {
        Protocol::AmneziaWg => {
            let awg = state
                .config
                .amneziawg
                .as_ref()
                .ok_or(floppa_core::FloppaError::AmneziaWgNotConfigured)?;
            let awg_pub = state
                .awg_public_key
                .as_deref()
                .ok_or(floppa_core::FloppaError::AmneziaWgNotConfigured)?;
            Ok(services::generate_awg_config(
                private_key,
                assigned_ip,
                awg,
                awg_pub,
            ))
        }
        Protocol::WireGuard => Ok(services::generate_wg_config(
            private_key,
            assigned_ip,
            &state.config,
            &state.wg_public_key,
        )),
    }
}

#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    id: i64,
    telegram_id: Option<i64>,
    /// True if a Telegram account is linked (can pay via Stars, gets bot notifications).
    telegram_linked: bool,
    /// True if the user has set a login+password credential (for the "set a backup login" nudge).
    has_credential: bool,
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
    source: SubscriptionSource,
    starts_at: chrono::DateTime<Utc>,
    expires_at: Option<chrono::DateTime<Utc>>,
    speed_limit_mbps: Option<i32>,
    max_peers: i32,
}

#[derive(Serialize, ToSchema)]
pub struct MyPeer {
    id: i64,
    assigned_ip: String,
    sync_status: PeerSyncStatus,
    protocol: Protocol,
    download_bytes: i64,
    upload_bytes: i64,
    last_handshake: Option<chrono::DateTime<Utc>>,
    created_at: chrono::DateTime<Utc>,
    device_name: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MyPeersResponse {
    /// Total WG traffic for this user (includes removed peers), last 30 days.
    wg_download_bytes: i64,
    wg_upload_bytes: i64,
    peers: Vec<MyPeer>,
    /// VLESS info (None if VLESS not configured on server)
    vless: Option<VlessInfo>,
    /// False when the metrics backend could not be queried: every byte counter in this
    /// response is then a placeholder zero, not a measurement.
    traffic_available: bool,
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
    /// Tunnel protocol. Defaults to WireGuard when omitted (pre-AmneziaWG clients).
    #[serde(default)]
    protocol: Option<Protocol>,
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
        r#"
        SELECT
            id, telegram_id, username, first_name, last_name, photo_url, is_admin,
            EXISTS(SELECT 1 FROM auth_identities ai WHERE ai.user_id = users.id) AS "has_credential!"
        FROM users WHERE id = $1
        "#,
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
            s.source AS "source: SubscriptionSource",
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
        telegram_linked: user.telegram_id.is_some(),
        telegram_id: user.telegram_id,
        has_credential: user.has_credential,
        username: user.username,
        first_name: user.first_name,
        last_name: user.last_name,
        photo_url: user.photo_url,
        is_admin: user.is_admin,
        subscription,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct SetCredentialRequest {
    login: String,
    password: String,
}

/// Set or change the login + password (backup access) for the current account.
#[utoipa::path(
    post,
    path = "/me/credentials",
    tag = "user",
    security(("bearer" = [])),
    request_body = SetCredentialRequest,
    responses(
        (status = 204, description = "Credential set"),
        (status = 400, body = ApiError, description = "Invalid login or password"),
        (status = 409, body = ApiError, description = "Login already taken"),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn set_my_credential(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<SetCredentialRequest>,
) -> Result<StatusCode, ApiError> {
    services::set_credential_for_user(&state.pool, auth.user_id, &req.login, &req.password).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct LinkStartResponse {
    code: String,
    deep_link: String,
    expires_at: chrono::DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct LinkPollResponse {
    linked: bool,
}

/// Start linking a Telegram account to the current account (returns a bot deep link + code).
#[utoipa::path(
    post,
    path = "/me/link/telegram/start",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = LinkStartResponse),
        (status = 409, body = ApiError, description = "Telegram already linked"),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn start_telegram_link(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<LinkStartResponse>, ApiError> {
    let already = sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", auth.user_id)
        .fetch_one(&state.pool)
        .await?;
    if already.is_some() {
        return Err(ApiError::conflict("Telegram is already linked"));
    }

    let username = state
        .config
        .bot
        .as_ref()
        .and_then(|b| b.username.as_deref())
        .ok_or_else(|| ApiError::internal("Bot username not configured"))?;

    // Opportunistic GC of expired codes, then mint a fresh one.
    let _ = sqlx::query!("DELETE FROM telegram_link_codes WHERE expires_at < NOW()")
        .execute(&state.pool)
        .await;
    let code = super::auth::generate_link_code();
    let expires_at = Utc::now() + chrono::Duration::minutes(10);
    sqlx::query!(
        "INSERT INTO telegram_link_codes (code, user_id, expires_at) VALUES ($1, $2, $3)",
        code,
        auth.user_id,
        expires_at,
    )
    .execute(&state.pool)
    .await?;

    let deep_link = format!("https://t.me/{username}?start=link_{code}");
    Ok(Json(LinkStartResponse {
        code,
        deep_link,
        expires_at,
    }))
}

/// Poll whether the Telegram link has completed (the app calls this after opening the deep link).
#[utoipa::path(
    get,
    path = "/me/link/telegram/poll",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, body = LinkPollResponse),
        (status = 401, body = ApiError, description = "Unauthorized"),
    )
)]
pub(super) async fn poll_telegram_link(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<LinkPollResponse>, ApiError> {
    let tg = sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", auth.user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(LinkPollResponse {
        linked: tg.is_some(),
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
        SELECT p.id, p.assigned_ip, p.sync_status AS "sync_status: PeerSyncStatus",
               p.protocol AS "protocol: Protocol", p.last_handshake, p.created_at,
               ai.device_name, ai.device_id AS "device_id?"
        FROM peers p
        LEFT JOIN app_installations ai ON p.installation_id = ai.id
        WHERE p.user_id = $1 AND p.sync_status != $2
        ORDER BY p.created_at DESC
        "#,
        auth.user_id,
        PeerSyncStatus::Removed as _,
    )
    .fetch_all(&state.pool)
    .await?;

    let peer_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let peer_traffic =
        vm_client::logged("peer_traffic", state.vm.peer_traffic(&peer_ids, 30).await);
    let wg_traffic = vm_client::logged(
        "user_traffic(wg)",
        state
            .vm
            .user_traffic(TrafficMetric::Wg, auth.user_id, 30)
            .await,
    );
    let mut traffic_available = peer_traffic.is_some() && wg_traffic.is_some();
    let peer_traffic = peer_traffic.unwrap_or_default();

    let peers: Vec<MyPeer> = rows
        .into_iter()
        .map(|r| {
            let Traffic { download, upload } = peer_traffic.get(&r.id).copied().unwrap_or_default();
            MyPeer {
                id: r.id,
                assigned_ip: r.assigned_ip,
                sync_status: r.sync_status,
                protocol: r.protocol,
                download_bytes: download,
                upload_bytes: upload,
                last_handshake: r.last_handshake,
                created_at: r.created_at,
                device_name: r.device_name,
                device_id: r.device_id, // LEFT JOIN → already Option
            }
        })
        .collect();

    // User-level WG traffic (includes removed peers)
    let Traffic {
        download: wg_download_bytes,
        upload: wg_upload_bytes,
    } = wg_traffic.unwrap_or_default();

    // VLESS info (only if server has VLESS configured)
    let vless = if state.config.vless.is_some() {
        let has_uuid = sqlx::query_scalar!(
            "SELECT vless_uuid IS NOT NULL FROM users WHERE id = $1",
            auth.user_id
        )
        .fetch_one(&state.pool)
        .await?
        .unwrap_or(false);

        let vless_traffic = vm_client::logged(
            "user_traffic(vless)",
            state
                .vm
                .user_traffic(TrafficMetric::Vless, auth.user_id, 30)
                .await,
        );
        traffic_available &= vless_traffic.is_some();
        let Traffic { download, upload } = vless_traffic.unwrap_or_default();

        Some(VlessInfo {
            has_uuid,
            download_bytes: download,
            upload_bytes: upload,
        })
    } else {
        None
    };

    Ok(Json(MyPeersResponse {
        wg_download_bytes,
        wg_upload_bytes,
        peers,
        vless,
        traffic_available,
    }))
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
        (status = 404, body = ApiError, description = "Installation not found"),
        (status = 409, body = ApiError, description = "Peer already exists for installation and protocol"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn create_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    body: Option<Json<CreatePeerRequest>>,
) -> Result<Json<CreatePeerResponse>, ApiError> {
    let ctx = services::CreatePeerContext {
        pool: &state.pool,
        config: &state.config,
        encryption_key: &state.encryption_key,
        wg_public_key: &state.wg_public_key,
        awg_public_key: state.awg_public_key.as_deref(),
    };

    let protocol = body
        .as_ref()
        .and_then(|Json(req)| req.protocol)
        .unwrap_or(LEGACY_REQUEST_PROTOCOL);

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

    let options = services::CreatePeerOptions {
        installation_id,
        protocol,
    };

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
    if !services::mark_peer_for_removal(&state.pool, peer_id, Some(auth.user_id)).await? {
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
    Ok(load_my_peer_config(&state, auth.user_id, peer_id)
        .await?
        .text)
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
    let config = load_my_peer_config(&state, auth.user_id, peer_id).await?;
    let filename = format!("floppa-vpn-{}.conf", config.assigned_ip);

    // Get user's telegram_id (None for credential-only accounts — they download the config directly)
    let telegram_id =
        sqlx::query_scalar!("SELECT telegram_id FROM users WHERE id = $1", auth.user_id)
            .fetch_one(&state.pool)
            .await?
            .ok_or_else(|| {
                ApiError::bad_request("No Telegram linked — download the config directly instead")
            })?;

    // Send config as document via Telegram bot
    let file = InputFile::memory(config.text.into_bytes()).file_name(filename);

    state
        .bot
        .send_document(ChatId(telegram_id), file)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("Failed to send config via Telegram: {e}")))?;

    Ok(StatusCode::OK)
}

#[derive(Deserialize, ToSchema)]
pub struct ByDeviceQuery {
    /// Tunnel protocol. Defaults to WireGuard when omitted (pre-AmneziaWG clients).
    #[serde(default)]
    protocol: Option<Protocol>,
}

/// Get a peer by device_id (+ protocol) for the current user
#[utoipa::path(
    get,
    path = "/me/peers/by-device/{device_id}",
    tag = "user",
    security(("bearer" = [])),
    params(
        ("device_id" = String, Path, description = "Device UUID"),
        ("protocol" = Option<Protocol>, Query, description = "Tunnel protocol"),
    ),
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
    Query(query): Query<ByDeviceQuery>,
) -> Result<Json<MyPeer>, ApiError> {
    let protocol = query.protocol.unwrap_or(LEGACY_REQUEST_PROTOCOL);
    let row = services::find_peer_by_device_id(&state.pool, auth.user_id, &device_id, protocol)
        .await?
        .ok_or_else(|| ApiError::not_found("No peer for this device"))?;

    let Traffic { download, upload } =
        vm_client::logged("peer_traffic", state.vm.peer_traffic(&[row.id], 30).await)
            .and_then(|m| m.get(&row.id).copied())
            .unwrap_or_default();

    Ok(Json(MyPeer {
        id: row.id,
        assigned_ip: row.assigned_ip,
        sync_status: row.sync_status,
        protocol: row.protocol,
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
    let reality_public_key = require_vless(&state)?;
    require_active_subscription(&state, auth.user_id).await?;

    let uuid = services::ensure_vless_uuid(&state.pool, auth.user_id).await?;
    let uri = services::generate_vless_uri(&uuid.to_string(), &state.config, reality_public_key)?;

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
    let reality_public_key = require_vless(&state)?;
    require_active_subscription(&state, auth.user_id).await?;

    // Rotate the existing UUID; a user who never had one simply gets their first.
    let uuid = match services::rotate_vless_uuid(&state.pool, auth.user_id).await? {
        Some(uuid) => uuid,
        None => services::ensure_vless_uuid(&state.pool, auth.user_id).await?,
    };
    let uri = services::generate_vless_uri(&uuid.to_string(), &state.config, reality_public_key)?;

    Ok(Json(VlessConfigResponse { uri }))
}
