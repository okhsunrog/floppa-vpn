mod admin;
mod auth;
mod plans;
mod user;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use floppa_core::{Config, DbPool, Secrets};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use teloxide::prelude::*;
use tokio::sync::RwLock;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub(crate) use crate::admin::error::ApiError;

/// Fixed-window rate-limit buckets: key → (count, window_start).
type RateBuckets = Arc<RwLock<HashMap<String, (u32, chrono::DateTime<Utc>)>>>;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub config: Config,
    pub secrets: Secrets,
    pub wg_public_key: String,
    pub bot: Bot,
    pub http_client: reqwest::Client,
    pub vm_url: String,
    telegram_login_states: Arc<RwLock<HashMap<String, PendingTelegramLoginState>>>,
    telegram_login_codes: Arc<RwLock<HashMap<String, PendingTelegramLoginCode>>>,
    /// Fixed-window rate-limit counters for credential auth, keyed "register:<ip>" / "login:<ip>".
    rate_buckets: RateBuckets,
}

#[derive(Clone)]
struct PendingTelegramLoginState {
    redirect_uri: String,
    expires_at: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct PendingTelegramLoginCode {
    auth_response: auth::AuthResponse,
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
    .routes(routes!(auth::telegram_login))
    .routes(routes!(auth::start_telegram_deep_link_login))
    .routes(routes!(auth::telegram_deep_link_callback))
    .routes(routes!(auth::exchange_telegram_login_code))
    .routes(routes!(auth::telegram_mini_app_auth))
    .routes(routes!(auth::register_account))
    .routes(routes!(auth::login_account))
    // User endpoints (authenticated)
    .routes(routes!(user::get_me))
    .routes(routes!(user::set_my_credential))
    .routes(routes!(user::start_telegram_link))
    .routes(routes!(user::poll_telegram_link))
    .routes(routes!(user::upsert_my_installation))
    .routes(routes!(user::get_my_peers, user::create_my_peer))
    .routes(routes!(user::delete_my_peer))
    .routes(routes!(user::get_my_peer_config))
    .routes(routes!(user::send_my_peer_config))
    .routes(routes!(user::get_my_peer_by_device))
    .routes(routes!(
        user::get_my_vless_config,
        user::regenerate_my_vless_config
    ))
    // Admin endpoints
    .routes(routes!(admin::get_stats))
    .routes(routes!(admin::list_users, admin::create_user))
    .routes(routes!(admin::get_user))
    .routes(routes!(admin::set_user_credential))
    .routes(routes!(admin::set_subscription))
    .routes(routes!(admin::delete_subscription))
    .routes(routes!(admin::remove_peer))
    .routes(routes!(admin::list_peers))
    .routes(routes!(admin::delete_admin_peer))
    .routes(routes!(admin::list_vless_peers))
    .routes(routes!(admin::regenerate_admin_vless_config))
    .routes(routes!(admin::list_installations))
    .routes(routes!(admin::delete_installation))
    .routes(routes!(plans::list_plans, plans::create_plan))
    .routes(routes!(plans::update_plan, plans::delete_plan))
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

/// Extract the client IP from the leftmost X-Forwarded-For entry (server runs behind a proxy).
pub(super) fn client_ip(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Fixed-window rate limit. Returns Err(429) once `max` requests for `key` occur within `window`.
pub(super) async fn check_rate_limit(
    state: &AppState,
    key: String,
    max: u32,
    window: Duration,
) -> Result<(), ApiError> {
    let now = Utc::now();
    let mut buckets = state.rate_buckets.write().await;
    buckets.retain(|_, (_, start)| now - *start < window);
    let entry = buckets.entry(key).or_insert((0, now));
    if now - entry.1 >= window {
        *entry = (0, now);
    }
    entry.0 += 1;
    if entry.0 > max {
        return Err(ApiError::too_many_requests(
            "Too many attempts, please try again later",
        ));
    }
    Ok(())
}

pub fn create_router(
    pool: DbPool,
    config: Config,
    secrets: Secrets,
    wg_public_key: String,
    bot: Bot,
) -> axum::Router {
    let vm_url = config
        .metrics
        .as_ref()
        .map(|m| m.victoria_metrics_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:8428".to_string());

    let state = AppState {
        pool,
        config,
        secrets,
        wg_public_key,
        bot,
        http_client: reqwest::Client::new(),
        vm_url,
        telegram_login_states: Arc::new(RwLock::new(HashMap::new())),
        telegram_login_codes: Arc::new(RwLock::new(HashMap::new())),
        rate_buckets: Arc::new(RwLock::new(HashMap::new())),
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
) -> Result<Option<chrono::DateTime<Utc>>, ApiError> {
    if permanent {
        return Ok(None);
    }
    let days = if let Some(d) = days {
        d
    } else {
        let plan_trial = sqlx::query_scalar!("SELECT trial_days FROM plans WHERE id = $1", plan_id)
            .fetch_optional(pool)
            .await?;
        match plan_trial {
            None => return Err(ApiError::not_found("Plan not found")),
            Some(Some(trial_days)) => trial_days as i64,
            Some(None) => {
                return Err(ApiError::bad_request(
                    "Days not specified and plan has no trial_days",
                ));
            }
        }
    };
    Ok(Some(now + Duration::days(days)))
}

// Public endpoints

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
