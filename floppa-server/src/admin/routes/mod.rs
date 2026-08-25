mod admin;
mod auth;
pub(crate) mod avatar;
mod plans;
mod user;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
};
use chrono::{DateTime, Duration, Utc};
use floppa_core::{AuthConfig, Config, DbPool, Secrets, config::AuthSecrets};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use teloxide::prelude::*;
use tokio::sync::RwLock;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub(crate) use crate::admin::error::ApiError;
use crate::admin::rate_limit::RateLimiter;

/// Request header the client app sends with its own semver version; the 426 middleware compares
/// it against `min_client_version`, and CORS must allow it.
pub const CLIENT_VERSION_HEADER: &str = "x-client-version";

/// Config/secrets problems that would otherwise surface as a 500 on the first request.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("secrets.toml: the [auth] section (jwt_secret, encryption_key) is required")]
    MissingAuthSecrets,
    #[error("secrets.toml: auth.encryption_key: {0}")]
    InvalidEncryptionKey(#[from] floppa_core::crypto::CryptoError),
    #[error("config.toml: min_client_version {0:?} is not a semver version: {1}")]
    InvalidMinClientVersion(String, semver::Error),
}

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub config: Config,
    pub secrets: Secrets,
    /// `config.auth` with the defaults filled in when the section is absent.
    pub auth_config: AuthConfig,
    /// `secrets.auth`, present by construction (see [`AppState::new`]).
    pub auth_secrets: AuthSecrets,
    /// `auth_secrets.encryption_key`, parsed once.
    pub encryption_key: [u8; 32],
    /// `config.min_client_version`, parsed once; `None` disables the 426 check.
    pub min_client_version: Option<semver::Version>,
    pub wg_public_key: String,
    /// AmneziaWG server public key (None if AmneziaWG is not configured).
    pub awg_public_key: Option<String>,
    pub bot: Bot,
    pub http_client: reqwest::Client,
    pub vm_url: String,
    telegram_login_states: Arc<RwLock<TtlMap<PendingTelegramLoginState>>>,
    telegram_login_codes: Arc<RwLock<TtlMap<PendingTelegramLoginCode>>>,
    /// Fixed-window counters for the unauthenticated auth endpoints.
    rate_limiter: Arc<RateLimiter>,
}

#[derive(Clone)]
struct PendingTelegramLoginState {
    redirect_uri: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
struct PendingTelegramLoginCode {
    auth_response: auth::AuthResponse,
    expires_at: DateTime<Utc>,
    /// Set on first exchange. The code stays exchangeable for a short grace window afterwards so
    /// the client can retry when the response was lost mid-flight (app switch on mobile).
    consumed_at: Option<DateTime<Utc>>,
}

/// An entry of a [`TtlMap`].
trait Expiring {
    fn expires_at(&self) -> DateTime<Utc>;
    /// Whether the entry is still usable at `now`. Defaults to "not yet expired".
    fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at() > now
    }
}

impl Expiring for PendingTelegramLoginState {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

impl Expiring for PendingTelegramLoginCode {
    fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Codes also die once their post-consumption grace window has passed.
    fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now
            && self
                .consumed_at
                .is_none_or(|consumed| now - consumed < LOGIN_CODE_EXCHANGE_GRACE)
    }
}

/// Retry window for exchanging an already-consumed login code (see
/// [`PendingTelegramLoginCode::consumed_at`]).
const LOGIN_CODE_EXCHANGE_GRACE: Duration = Duration::seconds(30);

/// Upper bound on live entries in each pending-login map. The entries are minted by
/// unauthenticated requests, so without a cap a flood of `/auth/telegram/start` calls could
/// grow the map until the process is OOM-killed; the rate limiter makes reaching this bound
/// impractical, the cap makes it harmless.
const PENDING_LOGIN_CAP: usize = 10_000;

/// A bounded in-memory map of nonce → pending login entry. Dead entries are dropped on every
/// access; when the map is full the entry closest to expiry goes first.
struct TtlMap<V> {
    inner: HashMap<String, V>,
    cap: usize,
}

impl<V: Expiring> TtlMap<V> {
    fn with_cap(cap: usize) -> Self {
        Self {
            inner: HashMap::new(),
            cap,
        }
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        self.inner.retain(|_, v| v.is_live(now));
    }

    fn insert(&mut self, now: DateTime<Utc>, key: String, value: V) {
        self.prune(now);
        if self.inner.len() >= self.cap
            && let Some(oldest) = self
                .inner
                .iter()
                .min_by_key(|(_, v)| v.expires_at())
                .map(|(k, _)| k.clone())
        {
            self.inner.remove(&oldest);
        }
        self.inner.insert(key, value);
    }

    fn remove(&mut self, now: DateTime<Utc>, key: &str) -> Option<V> {
        self.prune(now);
        self.inner.remove(key)
    }

    fn get_mut(&mut self, now: DateTime<Utc>, key: &str) -> Option<&mut V> {
        self.prune(now);
        self.inner.get_mut(key)
    }
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
    .routes(routes!(plans::list_public_plans))
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
    .routes(routes!(avatar::get_my_avatar))
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
    .routes(routes!(avatar::get_user_avatar))
    .routes(routes!(avatar::get_avatars_batch))
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
    // No version header = browser/admin panel, skip check
    if let Some(min_ver) = &state.min_client_version
        && let Some(client_header) = request.headers().get(CLIENT_VERSION_HEADER)
        && let Ok(client_str) = client_header.to_str()
        && let Ok(client_ver) = semver::Version::parse(client_str)
        && client_ver < *min_ver
    {
        return (
            StatusCode::UPGRADE_REQUIRED,
            Json(serde_json::json!({
                "error": "upgrade_required",
                "min_version": min_ver.to_string(),
                "message": "Please update the app to continue"
            })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Sliding session: once a valid token is older than this, any successful authed request
/// gets a fresh full-lifetime token in the `x-refreshed-token` response header. An active
/// user therefore never hits JWT expiry; re-login is only needed after being away longer
/// than `jwt_expiration_hours`.
const TOKEN_REFRESH_AFTER_SECS: i64 = 24 * 3600;

pub(crate) const REFRESHED_TOKEN_HEADER: &str = "x-refreshed-token";

async fn token_refresh_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let bearer = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);

    let mut response = next.run(request).await;

    if !response.status().is_success() {
        return response;
    }
    let Some(token) = bearer else {
        return response;
    };
    let Ok(claims) = crate::admin::auth::verify_jwt(&token, &state.auth_secrets.jwt_secret) else {
        return response;
    };
    if Utc::now().timestamp() - claims.iat < TOKEN_REFRESH_AFTER_SECS {
        return response;
    }

    // Re-issue only for a user that still exists, with the admin flag as stored now — the
    // refreshed token must not carry forward whatever the old one claimed.
    let is_admin = match crate::admin::auth::lookup_is_admin(&state.pool, claims.sub).await {
        Ok(Some(is_admin)) => is_admin,
        Ok(None) => return response,
        Err(e) => {
            tracing::warn!(user_id = claims.sub, error = %e, "token refresh skipped");
            return response;
        }
    };

    if let Ok(fresh) = crate::admin::auth::create_jwt(
        claims.sub,
        is_admin,
        &state.auth_secrets.jwt_secret,
        state.auth_config.jwt_expiration_hours,
    ) && let Ok(value) = axum::http::HeaderValue::from_str(&fresh)
    {
        response.headers_mut().insert(REFRESHED_TOKEN_HEADER, value);
    }
    response
}

impl AppState {
    /// Validate and derive everything the handlers need up front, so a misconfigured server
    /// fails on boot rather than with a 500 on the first request.
    ///
    /// `awg_public_key` is the AmneziaWG server public key when AmneziaWG is configured; the
    /// caller derives it (and fails) at startup rather than letting it silently vanish.
    pub fn new(
        pool: DbPool,
        config: Config,
        secrets: Secrets,
        wg_public_key: String,
        awg_public_key: Option<String>,
        bot: Bot,
    ) -> Result<Self, StartupError> {
        let auth_secrets = secrets
            .auth
            .clone()
            .ok_or(StartupError::MissingAuthSecrets)?;
        let encryption_key = auth_secrets.get_encryption_key()?;
        let auth_config = config.auth.clone().unwrap_or_default();
        let min_client_version = config
            .min_client_version
            .as_deref()
            .map(|raw| {
                semver::Version::parse(raw)
                    .map_err(|e| StartupError::InvalidMinClientVersion(raw.to_owned(), e))
            })
            .transpose()?;
        let vm_url = config
            .metrics
            .as_ref()
            .map(|m| m.victoria_metrics_url.clone())
            .unwrap_or_else(|| "http://127.0.0.1:8428".to_string());

        Ok(Self {
            pool,
            config,
            secrets,
            auth_config,
            auth_secrets,
            encryption_key,
            min_client_version,
            wg_public_key,
            awg_public_key,
            bot,
            http_client: reqwest::Client::new(),
            vm_url,
            telegram_login_states: Arc::new(RwLock::new(TtlMap::with_cap(PENDING_LOGIN_CAP))),
            telegram_login_codes: Arc::new(RwLock::new(TtlMap::with_cap(PENDING_LOGIN_CAP))),
            rate_limiter: Arc::new(RateLimiter::default()),
        })
    }
}

pub fn create_router(state: AppState) -> axum::Router {
    let (router, _openapi) = openapi_router().with_state(state.clone()).split_for_parts();
    router
        .layer(middleware::from_fn_with_state(
            state.clone(),
            version_check_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state,
            token_refresh_middleware,
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
    // Validate the plan even when the caller supplied an explicit duration or requested a permanent
    // subscription; otherwise the later FK failure is reported as an internal database error.
    let plan_trial =
        sqlx::query_scalar::<_, Option<i32>>("SELECT trial_minutes FROM plans WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("Plan not found"))?;

    if permanent {
        return Ok(None);
    }
    // The admin `days` override is in whole days; the plan's own default trial duration is
    // stored in minutes (`trial_minutes`) so it can express sub-day trials (e.g. taster).
    let minutes = if let Some(d) = days {
        d * 1440
    } else {
        match plan_trial {
            Some(trial_minutes) => trial_minutes as i64,
            None => {
                return Err(ApiError::bad_request(
                    "Days not specified and plan has no trial duration",
                ));
            }
        }
    };
    Ok(Some(now + Duration::minutes(minutes)))
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
    /// Whether AmneziaWG is offered by this server (the client defaults to it when available).
    amneziawg_available: bool,
    /// Whether VLESS+REALITY is offered by this server.
    vless_available: bool,
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
        amneziawg_available: state.awg_public_key.is_some(),
        vless_available: state.config.vless.is_some() && state.secrets.vless.is_some(),
    })
}
