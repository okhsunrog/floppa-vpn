use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect},
};
use chrono::{Duration, Utc};
use floppa_core::{Config, DbPool, FloppaError, Secrets, decrypt_private_key, services};
use rand::random;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use teloxide::{prelude::*, types::InputFile};
use tokio::sync::RwLock;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::admin::auth::{
    AdminUser, AuthUser, MiniAppUser, TelegramAuthData, create_jwt, verify_telegram_auth,
    verify_telegram_mini_app,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub config: Config,
    pub secrets: Secrets,
    pub wg_public_key: String,
    pub bot: Bot,
    telegram_login_states: Arc<RwLock<HashMap<String, PendingTelegramLoginState>>>,
    telegram_login_codes: Arc<RwLock<HashMap<String, PendingTelegramLoginCode>>>,
}

#[derive(Clone)]
struct PendingTelegramLoginState {
    redirect_uri: String,
    expires_at: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct PendingTelegramLoginCode {
    auth_response: AuthResponse,
    expires_at: chrono::DateTime<Utc>,
}

fn openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(
        utoipa::openapi::OpenApiBuilder::new()
            .info(
                utoipa::openapi::InfoBuilder::new()
                    .title("Floppa VPN Admin API")
                    .description(Some("API for Floppa VPN admin panel and user management"))
                    .version(crate::VERSION)
                    .build(),
            )
            .build(),
    )
    // Public endpoints
    .routes(routes!(get_version))
    .routes(routes!(get_public_config))
    .routes(routes!(telegram_login))
    .routes(routes!(start_telegram_deep_link_login))
    .routes(routes!(telegram_deep_link_callback))
    .routes(routes!(exchange_telegram_login_code))
    .routes(routes!(telegram_mini_app_auth))
    // User endpoints (authenticated)
    .routes(routes!(get_me))
    .routes(routes!(get_my_peers, create_my_peer))
    .routes(routes!(delete_my_peer))
    .routes(routes!(get_my_peer_config))
    .routes(routes!(send_my_peer_config))
    .routes(routes!(get_my_peer_by_device))
    // Admin endpoints
    .routes(routes!(get_stats))
    .routes(routes!(list_users, create_user))
    .routes(routes!(get_user))
    .routes(routes!(set_subscription))
    .routes(routes!(delete_subscription))
    .routes(routes!(remove_peer))
    .routes(routes!(list_peers))
    .routes(routes!(delete_admin_peer))
    .routes(routes!(list_plans, create_plan))
    .routes(routes!(update_plan, delete_plan))
}

/// Build just the OpenAPI spec (no DB or state required).
pub fn build_openapi() -> utoipa::openapi::OpenApi {
    let (_, openapi) = openapi_router().split_for_parts();
    openapi
}

async fn version_check_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    // No X-Client-Version header = browser/admin panel, skip check
    if let Some(min_version_str) = &state.config.min_client_version
        && let Some(client_header) = request.headers().get("X-Client-Version")
        && let Ok(client_str) = client_header.to_str()
        && let Ok(min_ver) = semver::Version::parse(min_version_str)
        && let Ok(client_ver) = semver::Version::parse(client_str)
        && client_ver < min_ver
    {
        return (
            StatusCode::UPGRADE_REQUIRED,
            Json(serde_json::json!({
                "error": "upgrade_required",
                "min_version": min_version_str,
                "message": "Please update the app to continue"
            })),
        )
            .into_response();
    }
    next.run(request).await
}

pub fn create_router(
    pool: DbPool,
    config: Config,
    secrets: Secrets,
    wg_public_key: String,
    bot: Bot,
) -> axum::Router {
    let state = AppState {
        pool,
        config,
        secrets,
        wg_public_key,
        bot,
        telegram_login_states: Arc::new(RwLock::new(HashMap::new())),
        telegram_login_codes: Arc::new(RwLock::new(HashMap::new())),
    };

    let (router, _openapi) = openapi_router().with_state(state.clone()).split_for_parts();
    router.layer(middleware::from_fn_with_state(
        state,
        version_check_middleware,
    ))
}

/// Resolve subscription expiration from request parameters.
/// Returns `None` for permanent subscriptions, `Some(expires_at)` otherwise.
async fn resolve_subscription_expires(
    pool: &DbPool,
    plan_id: i32,
    days: Option<i64>,
    permanent: bool,
    now: chrono::DateTime<Utc>,
) -> Result<Option<chrono::DateTime<Utc>>, StatusCode> {
    if permanent {
        return Ok(None);
    }
    let days = if let Some(d) = days {
        d
    } else {
        let plan_trial: Option<(Option<i32>,)> =
            sqlx::query_as("SELECT trial_days FROM plans WHERE id = $1")
                .bind(plan_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to fetch plan trial_days: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
        match plan_trial {
            None => return Err(StatusCode::NOT_FOUND),
            Some((Some(trial_days),)) => trial_days as i64,
            Some((None,)) => return Err(StatusCode::BAD_REQUEST),
        }
    };
    Ok(Some(now + Duration::days(days)))
}

// ============================================================================
// Public endpoints
// ============================================================================

#[derive(Serialize, ToSchema)]
struct VersionInfo {
    version: &'static str,
    git_hash: &'static str,
    build_time: &'static str,
}

#[utoipa::path(
    get,
    path = "/version",
    tag = "public",
    responses((status = 200, body = VersionInfo))
)]
async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: crate::VERSION,
        git_hash: crate::GIT_HASH,
        build_time: crate::BUILD_TIME,
    })
}

#[derive(Serialize, ToSchema)]
struct PublicConfig {
    telegram_bot_username: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
struct TelegramDeepLinkStartQuery {
    redirect_uri: String,
}

#[derive(Debug, Deserialize, ToSchema)]
struct TelegramDeepLinkCallbackQuery {
    state: String,
    id: i64,
    first_name: Option<String>,
    last_name: Option<String>,
    username: Option<String>,
    photo_url: Option<String>,
    auth_date: i64,
    hash: String,
}

#[derive(Debug, Deserialize, ToSchema)]
struct ExchangeTelegramLoginCodeRequest {
    code: String,
}

/// Get public configuration
#[utoipa::path(
    get,
    path = "/config",
    tag = "public",
    responses(
        (status = 200, body = PublicConfig),
    )
)]
async fn get_public_config(State(state): State<AppState>) -> Json<PublicConfig> {
    Json(PublicConfig {
        telegram_bot_username: state.config.bot.as_ref().and_then(|b| b.username.clone()),
    })
}

#[derive(Clone, Serialize, ToSchema)]
struct AuthResponse {
    token: String,
    user: AuthUserInfo,
}

#[derive(Clone, Serialize, ToSchema)]
struct AuthUserInfo {
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
}

fn generate_nonce() -> String {
    format!("{:032x}{:032x}", random::<u128>(), random::<u128>())
}

fn is_allowed_redirect_uri(uri: &str) -> bool {
    uri.starts_with("floppa://")
}

fn detect_request_origin(headers: &HeaderMap) -> Option<String> {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))?
        .to_str()
        .ok()?;

    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("https");

    Some(format!("{proto}://{host}"))
}

fn html_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Upsert a Telegram user and create a JWT auth response.
async fn upsert_and_create_jwt(
    state: &AppState,
    telegram_id: i64,
    username: Option<&str>,
    profile: services::TelegramProfile<'_>,
) -> Result<AuthResponse, StatusCode> {
    let auth_secrets = state.secrets.auth.as_ref().ok_or_else(|| {
        tracing::error!("Auth secrets not set");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let is_config_admin = auth_secrets.admin_telegram_ids.contains(&telegram_id);

    let result =
        services::upsert_user(&state.pool, telegram_id, username, profile, is_config_admin)
            .await
            .map_err(|e| {
                tracing::error!("Failed to upsert user: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let default_auth = floppa_core::AuthConfig::default();
    let auth_config = state.config.auth.as_ref().unwrap_or(&default_auth);

    let token = create_jwt(
        result.id,
        result.is_admin,
        result.username.clone(),
        &auth_secrets.jwt_secret,
        auth_config.jwt_expiration_hours,
    )
    .map_err(|e| {
        tracing::error!("Failed to create JWT: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(AuthResponse {
        token,
        user: AuthUserInfo {
            id: result.id,
            username: result.username,
            first_name: result.first_name,
            last_name: result.last_name,
            photo_url: result.photo_url,
            is_admin: result.is_admin,
        },
    })
}

async fn authenticate_telegram_user(
    state: &AppState,
    auth_data: TelegramAuthData,
) -> Result<AuthResponse, StatusCode> {
    let bot_token = state
        .secrets
        .bot
        .as_ref()
        .map(|b| b.token.as_str())
        .ok_or_else(|| {
            tracing::error!("Bot token not configured in secrets");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !verify_telegram_auth(&auth_data, bot_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    upsert_and_create_jwt(
        state,
        auth_data.id,
        auth_data.username.as_deref(),
        services::TelegramProfile {
            first_name: auth_data.first_name.as_deref(),
            last_name: auth_data.last_name.as_deref(),
            photo_url: auth_data.photo_url.as_deref(),
        },
    )
    .await
}

/// Render the Telegram login page for deep-link flow.
#[utoipa::path(
    get,
    path = "/auth/telegram/start",
    tag = "auth",
    params(
        ("redirect_uri" = String, Query, description = "Deep link URI, e.g. floppa://auth"),
    ),
    responses(
        (status = 200, description = "HTML login page"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Server misconfiguration"),
    )
)]
async fn start_telegram_deep_link_login(
    State(state): State<AppState>,
    Query(query): Query<TelegramDeepLinkStartQuery>,
    headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    if !is_allowed_redirect_uri(&query.redirect_uri) {
        tracing::warn!(
            "Rejected deep-link auth start with invalid redirect URI: {}",
            query.redirect_uri
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    let bot_username = state
        .config
        .bot
        .as_ref()
        .and_then(|b| b.username.as_ref())
        .ok_or_else(|| {
            tracing::error!("Bot username not configured in config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let request_origin = detect_request_origin(&headers).ok_or_else(|| {
        tracing::warn!("Missing host headers for deep-link auth start");
        StatusCode::BAD_REQUEST
    })?;

    let now = Utc::now();
    let state_token = generate_nonce();
    {
        let mut login_states = state.telegram_login_states.write().await;
        login_states.retain(|_, value| value.expires_at > now);
        login_states.insert(
            state_token.clone(),
            PendingTelegramLoginState {
                redirect_uri: query.redirect_uri.clone(),
                expires_at: now + Duration::minutes(10),
            },
        );
    }

    let callback_url = format!("{request_origin}/api/auth/telegram/callback?state={state_token}");
    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Floppa VPN Login</title>
  </head>
  <body style="font-family: sans-serif; margin: 24px; text-align: center;">
    <h1 style="margin-bottom: 8px;">Floppa VPN</h1>
    <p style="margin-top: 0; color: #666;">Continue with Telegram</p>
    <script async src="https://telegram.org/js/telegram-widget.js?22"
      data-telegram-login="{bot_username}"
      data-size="large"
      data-auth-url="{callback_url}"
      data-request-access="write">
    </script>
  </body>
</html>"#,
        bot_username = html_escape_attr(bot_username),
        callback_url = html_escape_attr(&callback_url),
    );

    Ok(Html(html))
}

/// Telegram widget callback for deep-link flow.
#[utoipa::path(
    get,
    path = "/auth/telegram/callback",
    tag = "auth",
    responses(
        (status = 307, description = "Redirect to deep link with temporary code"),
        (status = 400, description = "Invalid or expired state"),
        (status = 401, description = "Invalid Telegram auth payload"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn telegram_deep_link_callback(
    State(state): State<AppState>,
    Query(query): Query<TelegramDeepLinkCallbackQuery>,
) -> Result<Redirect, StatusCode> {
    let now = Utc::now();
    let login_state = {
        let mut login_states = state.telegram_login_states.write().await;
        login_states.retain(|_, value| value.expires_at > now);
        login_states.remove(&query.state)
    }
    .ok_or_else(|| {
        tracing::warn!("Deep-link callback received with unknown or expired state");
        StatusCode::BAD_REQUEST
    })?;

    let auth_data = TelegramAuthData {
        id: query.id,
        first_name: query.first_name,
        last_name: query.last_name,
        username: query.username,
        photo_url: query.photo_url,
        auth_date: query.auth_date,
        hash: query.hash,
    };
    let auth_response = authenticate_telegram_user(&state, auth_data).await?;

    let login_code = generate_nonce();
    {
        let mut login_codes = state.telegram_login_codes.write().await;
        login_codes.retain(|_, value| value.expires_at > now);
        login_codes.insert(
            login_code.clone(),
            PendingTelegramLoginCode {
                auth_response,
                expires_at: now + Duration::minutes(2),
            },
        );
    }

    let separator = if login_state.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let redirect_uri = format!(
        "{}{}code={}",
        login_state.redirect_uri, separator, login_code
    );
    Ok(Redirect::temporary(&redirect_uri))
}

/// Exchange one-time login code for JWT + user payload.
#[utoipa::path(
    post,
    path = "/auth/telegram/exchange-code",
    tag = "auth",
    request_body = ExchangeTelegramLoginCodeRequest,
    responses(
        (status = 200, body = AuthResponse),
        (status = 401, description = "Invalid or expired code"),
    )
)]
async fn exchange_telegram_login_code(
    State(state): State<AppState>,
    Json(request): Json<ExchangeTelegramLoginCodeRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let now = Utc::now();
    let pending = {
        let mut login_codes = state.telegram_login_codes.write().await;
        login_codes.retain(|_, value| value.expires_at > now);
        login_codes.remove(&request.code)
    }
    .ok_or(StatusCode::UNAUTHORIZED)?;

    if pending.expires_at <= now {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(Json(pending.auth_response))
}

/// Authenticate via Telegram Login Widget
#[utoipa::path(
    post,
    path = "/auth/telegram",
    tag = "auth",
    request_body = TelegramAuthData,
    responses(
        (status = 200, body = AuthResponse),
        (status = 401, description = "Invalid Telegram auth data"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn telegram_login(
    State(state): State<AppState>,
    Json(auth_data): Json<TelegramAuthData>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let auth_response = authenticate_telegram_user(&state, auth_data).await?;
    Ok(Json(auth_response))
}

#[derive(Debug, Deserialize, ToSchema)]
struct MiniAppAuthRequest {
    init_data: String,
}

/// Authenticate via Telegram Mini App initData
#[utoipa::path(
    post,
    path = "/auth/telegram/mini-app",
    tag = "auth",
    request_body = MiniAppAuthRequest,
    responses(
        (status = 200, body = AuthResponse),
        (status = 401, description = "Invalid Mini App initData"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn telegram_mini_app_auth(
    State(state): State<AppState>,
    Json(request): Json<MiniAppAuthRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let bot_token = state
        .secrets
        .bot
        .as_ref()
        .map(|b| b.token.as_str())
        .ok_or_else(|| {
            tracing::error!("Bot token not configured in secrets");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mini_app_user: MiniAppUser =
        verify_telegram_mini_app(&request.init_data, bot_token).ok_or(StatusCode::UNAUTHORIZED)?;

    let auth_response = upsert_and_create_jwt(
        &state,
        mini_app_user.id,
        mini_app_user.username.as_deref(),
        services::TelegramProfile {
            first_name: mini_app_user.first_name.as_deref(),
            last_name: mini_app_user.last_name.as_deref(),
            photo_url: None, // Mini App initData doesn't include photo_url
        },
    )
    .await?;

    Ok(Json(auth_response))
}

// ============================================================================
// User endpoints (authenticated user)
// ============================================================================

#[derive(Serialize, ToSchema)]
struct MeResponse {
    id: i64,
    telegram_id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
    subscription: Option<MySubscription>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct MySubscription {
    plan_name: String,
    plan_display_name: String,
    starts_at: chrono::DateTime<Utc>,
    expires_at: Option<chrono::DateTime<Utc>>,
    speed_limit_mbps: Option<i32>,
    traffic_limit_bytes: Option<i64>,
    max_peers: i32,
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
async fn get_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, StatusCode> {
    #[derive(sqlx::FromRow)]
    struct MeRow {
        id: i64,
        telegram_id: i64,
        username: Option<String>,
        first_name: Option<String>,
        last_name: Option<String>,
        photo_url: Option<String>,
        is_admin: bool,
    }

    let user: MeRow =
        sqlx::query_as("SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin FROM users WHERE id = $1")
            .bind(auth.user_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    // Get active subscription with plan info
    let subscription: Option<MySubscription> = sqlx::query_as(
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
    )
    .bind(auth.user_id)
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

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct MyPeer {
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
async fn get_my_peers(
    auth: AuthUser,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<MyPeer>>, StatusCode> {
    // Update client_version for all user's peers from X-Client-Version header
    if let Some(version) = headers
        .get("X-Client-Version")
        .and_then(|v| v.to_str().ok())
    {
        let _ = sqlx::query(
            "UPDATE peers SET client_version = $1 WHERE user_id = $2 AND sync_status != 'removed'",
        )
        .bind(version)
        .bind(auth.user_id)
        .execute(&state.pool)
        .await;
    }

    let peers: Vec<MyPeer> = sqlx::query_as(
        r#"
        SELECT id, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, created_at, device_name, device_id
        FROM peers
        WHERE user_id = $1 AND sync_status != 'removed'
        ORDER BY created_at DESC
        "#,
    )
    .bind(auth.user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    Ok(Json(peers))
}

#[derive(Serialize, ToSchema)]
struct CreatePeerResponse {
    id: i64,
    assigned_ip: String,
    config: String,
}

#[derive(Deserialize, ToSchema)]
struct CreatePeerRequest {
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
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
async fn create_my_peer(
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
async fn delete_my_peer(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let result = sqlx::query(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1 AND user_id = $2 AND sync_status = 'active'",
    )
    .bind(peer_id)
    .bind(auth.user_id)
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
async fn get_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<String, StatusCode> {
    let peer: (String, String) = sqlx::query_as(
        r#"
        SELECT private_key_encrypted, assigned_ip
        FROM peers
        WHERE id = $1 AND user_id = $2 AND sync_status != 'removed'
        "#,
    )
    .bind(peer_id)
    .bind(auth.user_id)
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

    let private_key = decrypt_private_key(&peer.0, &encryption_key).map_err(|e| {
        tracing::error!("Decryption failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let config =
        services::generate_wg_config(&private_key, &peer.1, &state.config, &state.wg_public_key);
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
async fn send_my_peer_config(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    // Get peer's encrypted key and IP
    let peer: (String, String) = sqlx::query_as(
        r#"
        SELECT private_key_encrypted, assigned_ip
        FROM peers
        WHERE id = $1 AND user_id = $2 AND sync_status != 'removed'
        "#,
    )
    .bind(peer_id)
    .bind(auth.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let assigned_ip = peer.1.clone();

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

    let private_key = decrypt_private_key(&peer.0, &encryption_key).map_err(|e| {
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
    let telegram_id: (i64,) = sqlx::query_as("SELECT telegram_id FROM users WHERE id = $1")
        .bind(auth.user_id)
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
        .send_document(ChatId(telegram_id.0), file)
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
async fn get_my_peer_by_device(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<MyPeer>, StatusCode> {
    let peer: MyPeer = sqlx::query_as(
        r#"
        SELECT id, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, created_at, device_name, device_id
        FROM peers
        WHERE user_id = $1 AND device_id = $2 AND sync_status NOT IN ('removed', 'pending_remove')
        "#,
    )
    .bind(auth.user_id)
    .bind(&device_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(peer))
}

// ============================================================================
// Admin endpoints
// ============================================================================

#[derive(Serialize, ToSchema)]
struct Stats {
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn get_stats(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Stats>, StatusCode> {
    let stats: (i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            (SELECT COUNT(*) FROM users),
            (SELECT COUNT(*) FROM peers WHERE sync_status = 'active'),
            (SELECT COALESCE(SUM(tx_bytes), 0)::bigint FROM peers),
            (SELECT COALESCE(SUM(rx_bytes), 0)::bigint FROM peers),
            (SELECT COUNT(*) FROM subscriptions WHERE expires_at IS NULL OR expires_at > NOW())
        "#,
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to fetch stats: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(Stats {
        total_users: stats.0,
        active_peers: stats.1,
        total_tx_bytes: stats.2,
        total_rx_bytes: stats.3,
        active_subscriptions: stats.4,
    }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct UserSummary {
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
}

/// List all users (admin only)
#[utoipa::path(
    get,
    path = "/users",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<UserSummary>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn list_users(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserSummary>>, StatusCode> {
    let users: Vec<UserSummary> = sqlx::query_as(
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
            (SELECT COUNT(*) FROM peers p WHERE p.user_id = u.id AND p.sync_status != 'removed') as peer_count
        FROM users u
        ORDER BY u.created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    Ok(Json(users))
}

#[derive(Deserialize, ToSchema)]
struct CreateUserRequest {
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
struct CreateUserResponse {
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
        (status = 400, description = "Days not specified and plan has no trial_days"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
        (status = 409, description = "User with this telegram_id already exists"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn create_user(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreateUserRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let now = Utc::now();

    // Insert user row (fail if telegram_id already exists)
    let user_id: (i64,) = sqlx::query_as(
        r#"
        INSERT INTO users (telegram_id, username, first_name, trial_used_at)
        VALUES ($1, $2, $3, NOW())
        RETURNING id
        "#,
    )
    .bind(req.telegram_id)
    .bind(req.username.as_deref())
    .bind(req.first_name.as_deref())
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e
            && db_err.constraint() == Some("users_telegram_id_key")
        {
            return StatusCode::CONFLICT;
        }
        tracing::error!("Failed to create user: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let expires_at =
        resolve_subscription_expires(&state.pool, req.plan_id, req.days, req.permanent, now)
            .await?;

    // Create subscription
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id.0)
    .bind(req.plan_id)
    .bind(now)
    .bind(expires_at)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to create subscription: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateUserResponse { id: user_id.0 }),
    ))
}

#[derive(Serialize, ToSchema)]
struct UserDetail {
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

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct PeerDetail {
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

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct SubscriptionDetail {
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    )
)]
async fn get_user(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<UserDetail>, StatusCode> {
    #[derive(sqlx::FromRow)]
    struct UserRow {
        id: i64,
        telegram_id: i64,
        username: Option<String>,
        first_name: Option<String>,
        last_name: Option<String>,
        photo_url: Option<String>,
        is_admin: bool,
        created_at: chrono::DateTime<Utc>,
    }

    let user: UserRow = sqlx::query_as(
        "SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin, created_at FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let peers: Vec<PeerDetail> = sqlx::query_as(
        r#"
        SELECT id, public_key, assigned_ip, sync_status, tx_bytes, rx_bytes, traffic_used_bytes, last_handshake, device_name, device_id
        FROM peers WHERE user_id = $1 AND sync_status != 'removed'
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let subscriptions: Vec<SubscriptionDetail> = sqlx::query_as(
        r#"
        SELECT s.id, s.plan_id, p.name as plan_name, p.display_name as plan_display_name,
               s.starts_at, s.expires_at,
               p.default_speed_limit_mbps as speed_limit_mbps,
               p.default_traffic_limit_bytes as traffic_limit_bytes,
               p.max_peers,
               (s.expires_at IS NULL OR s.expires_at > NOW()) as is_active
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1
        ORDER BY s.starts_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

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
struct SetSubscriptionRequest {
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
        (status = 400, description = "Days not specified and plan has no trial_days"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn set_subscription(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SetSubscriptionRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let now = Utc::now();

    let expires_at =
        resolve_subscription_expires(&state.pool, req.plan_id, req.days, req.permanent, now)
            .await?;

    let mut tx = state.pool.begin().await.map_err(|e| {
        tracing::error!("Failed to begin transaction: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Expire current active subscription (if any)
    sqlx::query(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to expire old subscription: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Insert new subscription
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(req.plan_id)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to create subscription: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tx.commit().await.map_err(|e| {
        tracing::error!("Failed to commit transaction: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "No active subscription found"),
    )
)]
async fn delete_subscription(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    let result = sqlx::query(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
    )
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to delete subscription: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn remove_peer(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    sqlx::query("UPDATE peers SET sync_status = 'pending_remove' WHERE user_id = $1 AND sync_status = 'active'")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    Ok(StatusCode::OK)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct PeerSummary {
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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn list_peers(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PeerSummary>>, StatusCode> {
    let peers: Vec<PeerSummary> = sqlx::query_as(
        r#"
        SELECT p.id, p.user_id, COALESCE(u.username, CONCAT_WS(' ', u.first_name, u.last_name)) AS username, p.assigned_ip, p.sync_status, p.tx_bytes, p.rx_bytes, p.last_handshake, p.device_name, p.device_id, p.client_version
        FROM peers p
        JOIN users u ON p.user_id = u.id
        WHERE p.sync_status != 'removed'
        ORDER BY p.last_handshake DESC NULLS LAST
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

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
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn delete_admin_peer(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(peer_id): Path<i64>,
) -> Result<impl IntoResponse, StatusCode> {
    sqlx::query(
        "UPDATE peers SET sync_status = 'pending_remove' WHERE id = $1 AND sync_status = 'active'",
    )
    .bind(peer_id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(StatusCode::OK)
}

// ============================================================================
// Plans management (admin)
// ============================================================================

#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct Plan {
    id: i32,
    name: String,
    display_name: String,
    default_speed_limit_mbps: Option<i32>,
    default_traffic_limit_bytes: Option<i64>,
    max_peers: i32,
    price_rub: i32,
    is_public: bool,
    trial_days: Option<i32>,
}

/// List all plans (admin only)
#[utoipa::path(
    get,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<Plan>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
async fn list_plans(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Plan>>, StatusCode> {
    let plans: Vec<Plan> = sqlx::query_as(
        "SELECT id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days FROM plans ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    Ok(Json(plans))
}

#[derive(Deserialize, ToSchema)]
struct CreatePlanRequest {
    name: String,
    display_name: String,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default)]
    default_traffic_limit_bytes: Option<i64>,
    #[serde(default = "default_max_peers")]
    max_peers: i32,
    #[serde(default)]
    price_rub: i32,
    #[serde(default = "default_is_public")]
    is_public: bool,
    #[serde(default)]
    trial_days: Option<i32>,
}

fn default_max_peers() -> i32 {
    1
}
fn default_is_public() -> bool {
    true
}

/// Create a new plan (admin only)
#[utoipa::path(
    post,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    request_body = CreatePlanRequest,
    responses(
        (status = 200, body = Plan),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 500, description = "Internal server error"),
    )
)]
async fn create_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreatePlanRequest>,
) -> Result<Json<Plan>, StatusCode> {
    let plan: Plan = sqlx::query_as(
        r#"
        INSERT INTO plans (name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days
        "#,
    )
    .bind(&req.name)
    .bind(&req.display_name)
    .bind(req.default_speed_limit_mbps)
    .bind(req.default_traffic_limit_bytes)
    .bind(req.max_peers)
    .bind(req.price_rub)
    .bind(req.is_public)
    .bind(req.trial_days)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to create plan: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(plan))
}

#[derive(Deserialize, ToSchema)]
struct UpdatePlanRequest {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default)]
    default_traffic_limit_bytes: Option<i64>,
    #[serde(default)]
    max_peers: Option<i32>,
    #[serde(default)]
    price_rub: Option<i32>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    trial_days: Option<i32>,
    #[serde(default)]
    clear_speed_limit: bool,
    #[serde(default)]
    clear_traffic_limit: bool,
    #[serde(default)]
    clear_trial_days: bool,
}

/// Update a plan (admin only)
#[utoipa::path(
    patch,
    path = "/plans/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i32, Path, description = "Plan ID")),
    request_body = UpdatePlanRequest,
    responses(
        (status = 200, body = Plan),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
    )
)]
async fn update_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(req): Json<UpdatePlanRequest>,
) -> Result<Json<Plan>, StatusCode> {
    let plan: Plan = sqlx::query_as(
        r#"
        UPDATE plans SET
            display_name = COALESCE($2, display_name),
            default_speed_limit_mbps = CASE WHEN $3 THEN NULL ELSE COALESCE($4, default_speed_limit_mbps) END,
            default_traffic_limit_bytes = CASE WHEN $5 THEN NULL ELSE COALESCE($6, default_traffic_limit_bytes) END,
            max_peers = COALESCE($7, max_peers),
            price_rub = COALESCE($8, price_rub),
            is_public = COALESCE($9, is_public),
            trial_days = CASE WHEN $10 THEN NULL ELSE COALESCE($11, trial_days) END
        WHERE id = $1
        RETURNING id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days
        "#,
    )
    .bind(id)
    .bind(&req.display_name)
    .bind(req.clear_speed_limit)
    .bind(req.default_speed_limit_mbps)
    .bind(req.clear_traffic_limit)
    .bind(req.default_traffic_limit_bytes)
    .bind(req.max_peers)
    .bind(req.price_rub)
    .bind(req.is_public)
    .bind(req.clear_trial_days)
    .bind(req.trial_days)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
                tracing::error!("DB error: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(plan))
}

/// Delete a plan (admin only). Fails if plan has subscriptions.
#[utoipa::path(
    delete,
    path = "/plans/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i32, Path, description = "Plan ID")),
    responses(
        (status = 204, description = "Plan deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
        (status = 409, description = "Plan has existing subscriptions"),
    )
)]
async fn delete_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, StatusCode> {
    // Don't allow deleting plans that have subscriptions
    let has_subs: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM subscriptions WHERE plan_id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if has_subs.0 > 0 {
        return Err(StatusCode::CONFLICT);
    }

    let result = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::NO_CONTENT)
}
