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
    // User endpoints (authenticated)
    .routes(routes!(user::get_me))
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
