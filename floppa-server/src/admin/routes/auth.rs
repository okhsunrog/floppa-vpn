use std::net::SocketAddr;

use axum::{
    Json,
    extract::{ConnectInfo, Query, State},
    http::HeaderMap,
    response::Html,
};
use chrono::{Duration, Utc};
use floppa_core::{SessionKind, services};
use rand::random;
use serde::{Deserialize, Serialize};
use tracing::warn;
use utoipa::ToSchema;

use crate::admin::{
    auth::{
        MiniAppUser, TelegramAuthData, create_jwt, verify_telegram_auth, verify_telegram_mini_app,
    },
    error::ApiError,
    rate_limit::{RateLimitScope, client_ip},
};

use super::AppState;

#[derive(Clone, Serialize, ToSchema)]
pub struct AuthResponse {
    pub token: String,
    pub user: AuthUserInfo,
}

/// The user half of an [`AuthResponse`]; also what every login path resolves before a JWT
/// is signed.
#[derive(Clone, Serialize, ToSchema)]
pub struct AuthUserInfo {
    id: i64,
    /// Linked Telegram account, `None` for credential-only accounts.
    telegram_id: Option<i64>,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    photo_url: Option<String>,
    is_admin: bool,
}

impl AuthUserInfo {
    fn from_upsert(result: services::UpsertResult, telegram_id: Option<i64>) -> Self {
        Self {
            id: result.id,
            telegram_id,
            username: result.username,
            first_name: result.first_name,
            last_name: result.last_name,
            photo_url: result.photo_url,
            is_admin: result.is_admin,
        }
    }
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

pub(super) fn generate_nonce() -> String {
    format!("{:032x}{:032x}", random::<u128>(), random::<u128>())
}

/// Telegram caps deep-link `start` payloads at 64 chars and inline-button `callback_data` at
/// 64 bytes; the code travels as both `link_<code>` and `link_merge:<code>`, so it must stay
/// ≤ 53 chars. 128 bits is ample for a single-use code with a 10-minute TTL.
pub(super) fn generate_link_code() -> String {
    format!("{:032x}", random::<u128>())
}

/// Parse a deep-link `redirect_uri` and accept only the app's own scheme (`floppa://…`) or the
/// desktop loopback listener (`http://127.0.0.1:<port>/…`); anything else is `None`.
fn parse_redirect_uri(uri: &str) -> Option<url::Url> {
    let url = url::Url::parse(uri).ok()?;
    let allowed = match url.scheme() {
        "floppa" => true,
        "http" => url.host_str() == Some("127.0.0.1") && url.port().is_some(),
        _ => false,
    };
    allowed.then_some(url)
}

/// Render `value` as a JavaScript string literal (quotes included) that is safe inside a
/// `<script>` block: JSON escaping handles quotes, backslashes and control characters, and
/// `<`, `>`, `&` plus the U+2028/2029 line terminators are escaped on top so neither
/// `</script>` nor a line break can end the literal early.
fn js_string_literal(value: &str) -> String {
    serde_json::to_string(value)
        .expect("a str always serializes")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
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

/// Open a session for an already-resolved user and sign the JWT that names it. Single point
/// that issues tokens: every login path (Telegram widget, Mini App, deep link, credential)
/// converges here, so every token is revocable through its session.
async fn build_auth_response(
    state: &AppState,
    user: AuthUserInfo,
    kind: SessionKind,
) -> Result<AuthResponse, ApiError> {
    let session_id = services::create_session(&state.pool, user.id, kind).await?;
    let token = create_jwt(
        user.id,
        user.is_admin,
        session_id,
        &state.auth_secrets.jwt_secret,
        state.auth_config.jwt_expiration_hours,
    )
    .map_err(|e| ApiError::internal(format!("Failed to create JWT: {e}")))?;

    Ok(AuthResponse { token, user })
}

/// Upsert a Telegram user; the caller decides when to open the session (see
/// [`build_auth_response`]).
async fn upsert_telegram_user(
    state: &AppState,
    telegram_id: i64,
    username: Option<&str>,
    profile: services::TelegramProfile<'_>,
) -> Result<AuthUserInfo, ApiError> {
    let is_config_admin = state.auth_secrets.admin_telegram_ids.contains(&telegram_id);

    let result =
        services::upsert_user(&state.pool, telegram_id, username, profile, is_config_admin)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to upsert user: {e}")))?;

    // Cache the user's Telegram avatar server-side (async, best-effort) — Telegram's CDN is
    // unreachable from clients in Russia, so we serve avatars from our own origin.
    super::avatar::spawn_refresh_if_stale(state, result.id, telegram_id, result.photo_url.clone());

    Ok(AuthUserInfo::from_upsert(result, Some(telegram_id)))
}

/// Verify a Login Widget payload and upsert the user behind it.
async fn authenticate_telegram_user(
    state: &AppState,
    auth_data: TelegramAuthData,
) -> Result<AuthUserInfo, ApiError> {
    let bot_token = state
        .secrets
        .bot
        .as_ref()
        .map(|b| b.token.as_str())
        .ok_or_else(|| ApiError::internal("Bot token not configured in secrets"))?;

    if !verify_telegram_auth(&auth_data, bot_token) {
        return Err(ApiError::unauthorized());
    }

    upsert_telegram_user(
        state,
        auth_data.id,
        auth_data.username.as_deref(),
        services::TelegramProfile {
            first_name: auth_data.first_name.as_deref(),
            last_name: auth_data.last_name.as_deref(),
            photo_url: auth_data.photo_url.as_deref(),
            language: None, // the Login Widget does not report the client language
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
        (status = 429, body = ApiError, description = "Too many attempts"),
        (status = 500, body = ApiError, description = "Server misconfiguration"),
    )
)]
pub(super) async fn start_telegram_deep_link_login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<TelegramDeepLinkStartQuery>,
    headers: HeaderMap,
) -> Result<Html<String>, ApiError> {
    state.rate_limiter.check(
        RateLimitScope::TelegramStart,
        client_ip(&headers, peer).to_string(),
        state.auth_config.telegram_login_rate_limit_per_15min,
        Duration::minutes(15),
    )?;

    let redirect_uri = parse_redirect_uri(&query.redirect_uri).ok_or_else(|| {
        warn!(
            "Rejected deep-link auth start with invalid redirect URI: {}",
            query.redirect_uri
        );
        ApiError::bad_request("Invalid redirect URI")
    })?;

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
    state.telegram_login_states.write().await.insert(
        now,
        state_token.clone(),
        super::PendingTelegramLoginState {
            redirect_uri,
            expires_at: now + Duration::minutes(10),
        },
    );

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
/// Returns an HTML landing page that auto-opens the app via deep link,
/// with a manual button and copy-code fallback for browsers that block custom schemes.
#[utoipa::path(
    get,
    path = "/auth/telegram/callback",
    tag = "auth",
    responses(
        (status = 200, description = "HTML page that redirects to deep link"),
        (status = 400, body = ApiError, description = "Invalid or expired state"),
        (status = 401, body = ApiError, description = "Invalid Telegram auth payload"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn telegram_deep_link_callback(
    State(state): State<AppState>,
    Query(query): Query<TelegramDeepLinkCallbackQuery>,
) -> Result<Html<String>, ApiError> {
    let now = Utc::now();
    let login_state = state
        .telegram_login_states
        .write()
        .await
        .remove(now, &query.state)
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
    let user = authenticate_telegram_user(&state, auth_data).await?;

    // The session is opened when the app exchanges the code, not here: a code that is never
    // exchanged (page closed mid-flow) must not leave a phantom device in the user's list.
    let login_code = generate_nonce();
    state.telegram_login_codes.write().await.insert(
        now,
        login_code.clone(),
        super::PendingTelegramLoginCode {
            user,
            expires_at: now + Duration::minutes(2),
            exchanged: None,
        },
    );

    let mut deep_link = login_state.redirect_uri;
    deep_link.query_pairs_mut().append_pair("code", &login_code);
    let deep_link = deep_link.to_string();

    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="color-scheme" content="light dark" />
    <title>Floppa VPN — Login</title>
    <style>
      * {{ margin: 0; padding: 0; box-sizing: border-box; }}
      body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
             color: #111827; background: #f5f5f5; min-height: 100vh; display: flex;
             align-items: center; justify-content: center; padding: 24px; }}
      .card {{ background: #fff; border-radius: 12px; padding: 32px 24px; max-width: 420px;
               width: 100%; text-align: center; box-shadow: 0 2px 12px rgba(0,0,0,0.08); }}
      h1 {{ font-size: 22px; margin-bottom: 4px; }}
      .hint {{ color: #6b7280; font-size: 14px; margin-bottom: 20px; }}
      .btn {{ display: block; width: 100%; padding: 12px; border: none; border-radius: 8px;
              font-size: 16px; font-weight: 600; cursor: pointer; text-decoration: none;
              text-align: center; margin-bottom: 12px; }}
      .btn-primary {{ background: #16a34a; color: #fff; }}
      .btn-primary:active {{ background: #15803d; }}
      .divider {{ border: none; border-top: 1px solid #e5e7eb; margin: 16px 0; }}
      .code-label {{ color: #6b7280; font-size: 13px; margin-bottom: 8px; }}
      .code-box {{ background: #f3f4f6; border: 1px solid #d1d5db; border-radius: 8px;
                   padding: 12px 16px; font-family: 'SF Mono', Monaco, Consolas, monospace;
                   font-size: 13px; word-break: break-all; color: #374151;
                   text-align: left; margin-bottom: 12px; user-select: all; }}
      .btn-copy {{ display: inline-flex; align-items: center; gap: 8px; padding: 8px 20px;
                   background: transparent; border: 1px solid #d1d5db; border-radius: 8px;
                   color: #374151; font-size: 14px; font-weight: 500; cursor: pointer; }}
      .btn-copy:active {{ background: #f3f4f6; }}
      .btn-copy svg {{ width: 16px; height: 16px; }}
      .copied {{ color: #16a34a; font-size: 13px; margin-top: 8px; min-height: 20px; }}
      @media (prefers-color-scheme: dark) {{
        body {{ background: #111; color: #f3f4f6; }}
        .card {{ background: #1f2937; box-shadow: 0 2px 12px rgba(0,0,0,0.3); }}
        .hint {{ color: #9ca3af; }}
        .btn-primary {{ background: #22c55e; color: #052e16; }}
        .btn-primary:active {{ background: #16a34a; }}
        .divider {{ border-color: #374151; }}
        .code-label {{ color: #9ca3af; }}
        .code-box {{ background: #111827; border-color: #374151; color: #d1d5db; }}
        .btn-copy {{ border-color: #4b5563; color: #d1d5db; }}
        .btn-copy:active {{ background: #374151; }}
        .copied {{ color: #4ade80; }}
      }}
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Floppa VPN</h1>
      <p class="hint">Opening the app&hellip;</p>
      <a class="btn btn-primary" id="open" href="{deep_link_attr}">Open Floppa VPN</a>
      <hr class="divider" />
      <p class="code-label">Paste this into the app:</p>
      <div class="code-box" id="code-box">{code}</div>
      <button class="btn-copy" id="copy" onclick="copyCode()">
        <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24"
             stroke-width="1.5" stroke="currentColor">
          <path stroke-linecap="round" stroke-linejoin="round"
                d="M15.75 17.25v3.375c0 .621-.504 1.125-1.125
                   1.125h-9.75a1.125 1.125 0 0 1-1.125-1.125V7.875c0-.621.504-1.125
                   1.125-1.125H6.75a9.06 9.06 0 0 1 1.5.124m7.5 10.376h3.375c.621
                   0 1.125-.504 1.125-1.125V11.25c0-4.46-3.243-8.161-7.5-8.876a9.06
                   9.06 0 0 0-1.5-.124H9.375c-.621 0-1.125.504-1.125 1.125v3.5m7.5
                   10.375H9.375a1.125 1.125 0 0 1-1.125-1.125v-9.25m0
                   0a2.625 2.625 0 1 1 5.25 0" />
        </svg>
        Copy Code
      </button>
      <p class="copied" id="copied"></p>
    </div>
    <script>
      window.location.href = {deep_link_js};

      function copyCode() {{
        navigator.clipboard.writeText({code_js}).then(function() {{
          document.getElementById("copied").textContent = "Copied!";
        }}, function() {{
          var t = document.createElement("textarea");
          t.value = {code_js};
          document.body.appendChild(t);
          t.select();
          document.execCommand("copy");
          document.body.removeChild(t);
          document.getElementById("copied").textContent = "Copied!";
        }});
      }}
    </script>
  </body>
</html>"#,
        deep_link_attr = html_escape_attr(&deep_link),
        deep_link_js = js_string_literal(&deep_link),
        code = html_escape_attr(&login_code),
        code_js = js_string_literal(&login_code),
    );

    Ok(Html(html))
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
        (status = 429, body = ApiError, description = "Too many attempts"),
    )
)]
pub(super) async fn exchange_telegram_login_code(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ExchangeTelegramLoginCodeRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    // Exchanges share the deep-link cap mainly so a lost code cannot be guessed at line rate
    // (it is 256 random bits anyway).
    state.rate_limiter.check(
        RateLimitScope::ExchangeCode,
        client_ip(&headers, peer).to_string(),
        state.auth_config.telegram_login_rate_limit_per_15min,
        Duration::minutes(15),
    )?;

    let now = Utc::now();
    // A consumed code stays exchangeable for a short grace window (`TtlMap` drops it after)
    // so the client can retry when the first response was lost mid-flight, e.g. the webview
    // got suspended during the browser → app switch on mobile. The retry gets the very same
    // token: the session is opened once, on the first exchange. The map lock is held across
    // that one INSERT so two racing exchanges cannot open two sessions.
    let mut login_codes = state.telegram_login_codes.write().await;
    let pending = login_codes
        .get_mut(now, &request.code)
        .ok_or_else(ApiError::unauthorized)?;
    let auth_response = match &pending.exchanged {
        Some((_, auth_response)) => auth_response.clone(),
        None => {
            let auth_response =
                build_auth_response(&state, pending.user.clone(), SessionKind::DeepLink).await?;
            pending.exchanged = Some((now, auth_response.clone()));
            auth_response
        }
    };

    Ok(Json(auth_response))
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
    let user = authenticate_telegram_user(&state, auth_data).await?;
    Ok(Json(
        build_auth_response(&state, user, SessionKind::TelegramWidget).await?,
    ))
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

    let user = upsert_telegram_user(
        &state,
        mini_app_user.id,
        mini_app_user.username.as_deref(),
        services::TelegramProfile {
            first_name: mini_app_user.first_name.as_deref(),
            last_name: mini_app_user.last_name.as_deref(),
            photo_url: None, // Mini App initData doesn't include photo_url
            language: None,
        },
    )
    .await?;

    Ok(Json(
        build_auth_response(&state, user, SessionKind::MiniApp).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AccountRegisterRequest {
    login: String,
    password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AccountLoginRequest {
    login: String,
    password: String,
}

/// Fetch the display fields of an existing user by id.
async fn fetch_auth_user_info(state: &AppState, user_id: i64) -> Result<AuthUserInfo, ApiError> {
    let user = sqlx::query_as!(
        AuthUserInfo,
        "SELECT id, telegram_id, username, first_name, last_name, photo_url, is_admin \
         FROM users WHERE id = $1",
        user_id
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(user)
}

/// Register a new account with a login + password (no Telegram). Grants a short taster trial
/// (duration comes from the 'taster' plan's `trial_minutes`).
#[utoipa::path(
    post,
    path = "/auth/account/register",
    tag = "auth",
    request_body = AccountRegisterRequest,
    responses(
        (status = 200, body = AuthResponse),
        (status = 400, body = ApiError, description = "Invalid login or password"),
        (status = 409, body = ApiError, description = "Login already taken"),
        (status = 429, body = ApiError, description = "Too many attempts"),
    )
)]
pub(super) async fn register_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AccountRegisterRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    state.rate_limiter.check(
        RateLimitScope::Register,
        client_ip(&headers, peer).to_string(),
        state.auth_config.register_rate_limit_per_hour,
        Duration::hours(1),
    )?;

    let result = services::create_credential_user(&state.pool, &req.login, &req.password).await?;
    let user = AuthUserInfo::from_upsert(result, None);
    Ok(Json(
        build_auth_response(&state, user, SessionKind::Credential).await?,
    ))
}

/// Log in with a login + password.
#[utoipa::path(
    post,
    path = "/auth/account/login",
    tag = "auth",
    request_body = AccountLoginRequest,
    responses(
        (status = 200, body = AuthResponse),
        (status = 401, body = ApiError, description = "Invalid login or password"),
        (status = 429, body = ApiError, description = "Too many attempts"),
    )
)]
pub(super) async fn login_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<AccountLoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let window = Duration::minutes(15);
    // Per IP against one client trying many accounts; per login name against many addresses
    // trying one account. Logins are matched case-insensitively, so key on the lowercase form.
    state.rate_limiter.check(
        RateLimitScope::LoginIp,
        client_ip(&headers, peer).to_string(),
        state.auth_config.login_ip_rate_limit_per_15min,
        window,
    )?;
    state.rate_limiter.check(
        RateLimitScope::LoginName,
        req.login.trim().to_lowercase(),
        state.auth_config.login_rate_limit_per_15min,
        window,
    )?;

    let user_id = services::find_user_by_credential(&state.pool, &req.login, &req.password).await?;
    let user = fetch_auth_user_info(&state, user_id).await?;
    Ok(Json(
        build_auth_response(&state, user, SessionKind::Credential).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{generate_link_code, js_string_literal, parse_redirect_uri};

    #[test]
    fn redirect_uri_allowlist() {
        assert!(parse_redirect_uri("floppa://auth").is_some());
        assert!(parse_redirect_uri("floppa://auth/cb?x=1").is_some());
        assert!(parse_redirect_uri("http://127.0.0.1:43123/callback").is_some());
        for bad in [
            "http://127.0.0.1/callback",
            "http://localhost:43123/callback",
            "https://127.0.0.1:43123/callback",
            "https://evil.example/floppa://auth",
            "javascript:alert(1)",
            "not a url",
        ] {
            assert!(parse_redirect_uri(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn js_string_literal_cannot_break_out_of_a_script_block() {
        let lit = js_string_literal("floppa://a?\"</script><script>alert(1)</script>&\u{2028}x");
        assert!(lit.starts_with('"') && lit.ends_with('"'));
        assert!(!lit.contains('<') && !lit.contains('>') && !lit.contains('&'));
        assert!(!lit.contains('\u{2028}'));
        assert!(!lit[1..lit.len() - 1].contains("\"</"));
        assert_eq!(js_string_literal("plain"), "\"plain\"");
    }

    /// Telegram rejects deep-link `start` payloads over 64 chars and inline-button
    /// `callback_data` over 64 bytes; the link code travels as both `link_<code>`
    /// and `link_merge:<code>`.
    #[test]
    fn link_code_fits_telegram_limits() {
        let code = generate_link_code();
        assert!(format!("link_{code}").len() <= 64);
        assert!(format!("link_merge:{code}").len() <= 64);
        assert!(code.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
