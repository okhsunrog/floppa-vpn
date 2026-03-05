use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::{Html, Redirect},
};
use chrono::{Duration, Utc};
use floppa_core::services;
use rand::random;
use serde::{Deserialize, Serialize};
use tracing::warn;
use utoipa::ToSchema;

use crate::admin::{
    auth::{
        MiniAppUser, TelegramAuthData, create_jwt, verify_telegram_auth, verify_telegram_mini_app,
    },
    error::ApiError,
};

use super::AppState;

#[derive(Clone, Serialize, ToSchema)]
pub struct AuthResponse {
    pub token: String,
    pub user: AuthUserInfo,
}

#[derive(Clone, Serialize, ToSchema)]
pub struct AuthUserInfo {
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramDeepLinkStartQuery {
    redirect_uri: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramDeepLinkCallbackQuery {
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
pub struct ExchangeTelegramLoginCodeRequest {
    code: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MiniAppAuthRequest {
    init_data: String,
}

fn generate_nonce() -> String {
    format!("{:032x}{:032x}", random::<u128>(), random::<u128>())
}

fn is_allowed_redirect_uri(uri: &str) -> bool {
    uri.starts_with("floppa://") || uri.starts_with("http://127.0.0.1:")
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
) -> Result<AuthResponse, ApiError> {
    let auth_secrets = state
        .secrets
        .auth
        .as_ref()
        .ok_or_else(|| ApiError::internal("Auth secrets not set"))?;

    let is_config_admin = auth_secrets.admin_telegram_ids.contains(&telegram_id);

    let result =
        services::upsert_user(&state.pool, telegram_id, username, profile, is_config_admin)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to upsert user: {e}")))?;

    let default_auth = floppa_core::AuthConfig::default();
    let auth_config = state.config.auth.as_ref().unwrap_or(&default_auth);

    let token = create_jwt(
        result.id,
        result.is_admin,
        result.username.clone(),
        &auth_secrets.jwt_secret,
        auth_config.jwt_expiration_hours,
    )
    .map_err(|e| ApiError::internal(format!("Failed to create JWT: {e}")))?;

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
) -> Result<AuthResponse, ApiError> {
    let bot_token = state
        .secrets
        .bot
        .as_ref()
        .map(|b| b.token.as_str())
        .ok_or_else(|| ApiError::internal("Bot token not configured in secrets"))?;

    if !verify_telegram_auth(&auth_data, bot_token) {
        return Err(ApiError::unauthorized());
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
        (status = 400, body = ApiError, description = "Invalid request"),
        (status = 500, body = ApiError, description = "Server misconfiguration"),
    )
)]
pub(super) async fn start_telegram_deep_link_login(
    State(state): State<AppState>,
    Query(query): Query<TelegramDeepLinkStartQuery>,
    headers: HeaderMap,
) -> Result<Html<String>, ApiError> {
    if !is_allowed_redirect_uri(&query.redirect_uri) {
        warn!(
            "Rejected deep-link auth start with invalid redirect URI: {}",
            query.redirect_uri
        );
        return Err(ApiError::bad_request("Invalid redirect URI"));
    }

    let bot_username = state
        .config
        .bot
        .as_ref()
        .and_then(|b| b.username.as_ref())
        .ok_or_else(|| ApiError::internal("Bot username not configured in config.toml"))?;

    let request_origin = detect_request_origin(&headers).ok_or_else(|| {
        warn!("Missing host headers for deep-link auth start");
        ApiError::bad_request("Missing host headers")
    })?;

    let now = Utc::now();
    let state_token = generate_nonce();
    {
        let mut login_states = state.telegram_login_states.write().await;
        login_states.retain(|_, value| value.expires_at > now);
        login_states.insert(
            state_token.clone(),
            super::PendingTelegramLoginState {
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
        (status = 400, body = ApiError, description = "Invalid or expired state"),
        (status = 401, body = ApiError, description = "Invalid Telegram auth payload"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn telegram_deep_link_callback(
    State(state): State<AppState>,
    Query(query): Query<TelegramDeepLinkCallbackQuery>,
) -> Result<Redirect, ApiError> {
    let now = Utc::now();
    let login_state = {
        let mut login_states = state.telegram_login_states.write().await;
        login_states.retain(|_, value| value.expires_at > now);
        login_states.remove(&query.state)
    }
    .ok_or_else(|| {
        warn!("Deep-link callback received with unknown or expired state");
        ApiError::bad_request("Invalid or expired state")
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
            super::PendingTelegramLoginCode {
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
        (status = 401, body = ApiError, description = "Invalid or expired code"),
    )
)]
pub(super) async fn exchange_telegram_login_code(
    State(state): State<AppState>,
    Json(request): Json<ExchangeTelegramLoginCodeRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let now = Utc::now();
    let pending = {
        let mut login_codes = state.telegram_login_codes.write().await;
        login_codes.retain(|_, value| value.expires_at > now);
        login_codes.remove(&request.code)
    }
    .ok_or_else(ApiError::unauthorized)?;

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
        (status = 401, body = ApiError, description = "Invalid Telegram auth data"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn telegram_login(
    State(state): State<AppState>,
    Json(auth_data): Json<TelegramAuthData>,
) -> Result<Json<AuthResponse>, ApiError> {
    let auth_response = authenticate_telegram_user(&state, auth_data).await?;
    Ok(Json(auth_response))
}

/// Authenticate via Telegram Mini App initData
#[utoipa::path(
    post,
    path = "/auth/telegram/mini-app",
    tag = "auth",
    request_body = MiniAppAuthRequest,
    responses(
        (status = 200, body = AuthResponse),
        (status = 401, body = ApiError, description = "Invalid Mini App initData"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn telegram_mini_app_auth(
    State(state): State<AppState>,
    Json(request): Json<MiniAppAuthRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let bot_token = state
        .secrets
        .bot
        .as_ref()
        .map(|b| b.token.as_str())
        .ok_or_else(|| ApiError::internal("Bot token not configured in secrets"))?;

    let mini_app_user: MiniAppUser = verify_telegram_mini_app(&request.init_data, bot_token)
        .ok_or_else(ApiError::unauthorized)?;

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
