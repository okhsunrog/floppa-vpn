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

use crate::admin::{rate_limit::RateLimiter, vm_client::VmClient};

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
    #[error("failed to build the HTTP client: {0}")]
    HttpClient(reqwest::Error),
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
    /// Outbound HTTP (Telegram file downloads, avatar URLs) with short timeouts — a hung
    /// upstream must not hold a request or a background task forever.
    pub http_client: reqwest::Client,
    pub vm: VmClient,
    telegram_login_states: Arc<RwLock<TtlMap<PendingTelegramLoginState>>>,
    telegram_login_codes: Arc<RwLock<TtlMap<PendingTelegramLoginCode>>>,
    /// Fixed-window counters for the unauthenticated auth endpoints.
    rate_limiter: Arc<RateLimiter>,
}

#[derive(Clone)]
struct PendingTelegramLoginState {
    /// Validated by `auth::parse_redirect_uri`.
    redirect_uri: url::Url,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
struct PendingTelegramLoginCode {
    /// The user the widget authenticated; the session and token are minted on exchange.
    user: auth::AuthUserInfo,
    expires_at: DateTime<Utc>,
    /// Set on first exchange: when, and the response that was issued. The code stays
    /// exchangeable for a short grace window afterwards so the client can retry when the
    /// response was lost mid-flight (app switch on mobile) — and gets the same token back.
    exchanged: Option<(DateTime<Utc>, auth::AuthResponse)>,
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
                .exchanged
                .as_ref()
                .is_none_or(|(consumed, _)| now - *consumed < LOGIN_CODE_EXCHANGE_GRACE)
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

/// Name of the security scheme the `security(("bearer" = []))` attributes on the handlers
/// refer to; declared here so the spec is self-consistent and generated clients know to send
/// `Authorization: Bearer <jwt>`.
const BEARER_SCHEME: &str = "bearer";

fn openapi_router() -> OpenApiRouter<AppState> {
    use utoipa::openapi::{
        ComponentsBuilder, InfoBuilder, OpenApiBuilder,
        security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    };

    OpenApiRouter::with_openapi(
        OpenApiBuilder::new()
            .info(
                InfoBuilder::new()
                    .title("Floppa VPN Admin API")
                    .description(Some("API for Floppa VPN admin panel and user management"))
                    .version(crate::VERSION)
                    .build(),
            )
            .components(Some(
                ComponentsBuilder::new()
                    .security_scheme(
                        BEARER_SCHEME,
                        SecurityScheme::Http(
                            HttpBuilder::new()
                                .scheme(HttpAuthScheme::Bearer)
                                .bearer_format("JWT")
                                .build(),
                        ),
                    )
                    .build(),
            ))
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
    .routes(routes!(user::get_my_sessions, user::revoke_all_my_sessions))
    .routes(routes!(user::delete_my_session))
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
    .routes(routes!(
        admin::list_user_sessions,
        admin::revoke_all_user_sessions
    ))
    .routes(routes!(admin::delete_user_session))
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
        crate::metrics::upgrade_required();
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
/// than `jwt_expiration_hours`. The refreshed token keeps the same session (`jti`), so
/// revoking that session kills every token it ever produced.
const TOKEN_REFRESH_AFTER_SECS: i64 = 24 * 3600;

/// How long a session may go unseen before its token has certainly expired: the token is
/// refreshed on activity and activity bumps `last_seen_at` (at most
/// [`SESSION_TOUCH_AFTER`](crate::admin::auth::SESSION_TOUCH_AFTER) late), so a session idle
/// longer than this cannot be used any more and is left out of session lists.
pub(crate) fn session_max_idle(state: &AppState) -> Duration {
    Duration::hours(state.auth_config.jwt_expiration_hours as i64)
        + crate::admin::auth::SESSION_TOUCH_AFTER
}

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

    // The session travels with the token. A legacy token (issued before sessions) gets one
    // here — the handler just accepted it — so an active old client becomes revocable
    // without logging in again.
    let session_id = match claims.jti {
        Some(session_id) => session_id,
        None => match floppa_core::services::create_session(
            &state.pool,
            claims.sub,
            floppa_core::SessionKind::Legacy,
        )
        .await
        {
            Ok(session_id) => session_id,
            Err(e) => {
                tracing::warn!(user_id = claims.sub, error = %e, "token refresh skipped");
                return response;
            }
        },
    };

    if let Ok(fresh) = crate::admin::auth::create_jwt(
        claims.sub,
        is_admin,
        session_id,
        &state.auth_secrets.jwt_secret,
        state.auth_config.jwt_expiration_hours,
    ) && let Ok(value) = axum::http::HeaderValue::from_str(&fresh)
    {
        response.headers_mut().insert(REFRESHED_TOKEN_HEADER, value);
        crate::metrics::token_refreshed();
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
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(StartupError::HttpClient)?;

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
            vm: VmClient::new(http_client.clone(), vm_url),
            http_client,
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
        // Outermost, so the 426 and refresh layers' own responses are counted too.
        .layer(middleware::from_fn(crate::metrics::http_metrics))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use floppa_core::config::{AuthSecrets, WireGuardConfig};
    use tower::ServiceExt;
    use uuid::Uuid;

    const JWT_SECRET: &str = "test-jwt-secret";

    fn test_state(pool: DbPool, min_client_version: Option<&str>) -> AppState {
        let config = Config {
            wireguard: WireGuardConfig {
                interface: "wg-test".into(),
                endpoint: "vpn.test.com:51820".into(),
                listen_port: None,
                client_subnet: "10.200.0.0/24".parse().unwrap(),
                server_ip: None,
                dns: vec!["8.8.8.8".into()],
                allowed_ips: "0.0.0.0/0".into(),
                rate_limit: None,
            },
            amneziawg: None,
            vless: None,
            bot: None,
            auth: None,
            allowed_origins: vec![],
            min_client_version: min_client_version.map(str::to_owned),
            metrics: None,
        };
        let secrets = Secrets {
            database_url: String::new(),
            wg_private_key: String::new(),
            awg_private_key: None,
            bot: None,
            auth: Some(AuthSecrets {
                jwt_secret: JWT_SECRET.into(),
                encryption_key: "00".repeat(32),
                admin_telegram_ids: vec![],
            }),
            vless: None,
        };
        AppState::new(
            pool,
            config,
            secrets,
            "server-public-key".into(),
            None,
            Bot::new("123456:test-token"),
        )
        .expect("test state is valid")
    }

    /// A token as the server would sign it; `session` is `None` for a legacy (pre-session)
    /// token.
    fn token(sub: i64, is_admin: bool, issued: DateTime<Utc>, session: Option<Uuid>) -> String {
        let claims = crate::admin::auth::Claims {
            sub,
            admin: is_admin,
            exp: (issued + Duration::days(30)).timestamp(),
            iat: issued.timestamp(),
            jti: session,
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(JWT_SECRET.as_bytes()),
        )
        .unwrap()
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    async fn request(
        router: &axum::Router,
        method: axum::http::Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn get(
        router: &axum::Router,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
        request(router, axum::http::Method::GET, uri, headers).await
    }

    /// An authenticated request made with `token`.
    async fn authed(
        router: &axum::Router,
        method: axum::http::Method,
        uri: &str,
        token: &str,
    ) -> axum::response::Response {
        request(
            router,
            method,
            uri,
            &[(header::AUTHORIZATION.as_str(), &bearer(token))],
        )
        .await
    }

    async fn json_body(resp: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(&to_bytes(resp.into_body(), 1 << 16).await.unwrap()).unwrap()
    }

    async fn seed_user(pool: &DbPool, telegram_id: i64, is_admin: bool) -> i64 {
        sqlx::query_scalar!(
            "INSERT INTO users (telegram_id, username, is_admin) VALUES ($1, 'u', $2) RETURNING id",
            telegram_id,
            is_admin,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn open_session(pool: &DbPool, user_id: i64) -> Uuid {
        floppa_core::services::create_session(pool, user_id, floppa_core::SessionKind::DeepLink)
            .await
            .unwrap()
    }

    fn refreshed_token(resp: &axum::response::Response) -> Option<String> {
        resp.headers()
            .get(REFRESHED_TOKEN_HEADER)
            .map(|v| v.to_str().unwrap().to_owned())
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn startup_rejects_misconfiguration(pool: DbPool) {
        let good = Config {
            wireguard: WireGuardConfig {
                interface: "wg".into(),
                endpoint: "e:1".into(),
                listen_port: None,
                client_subnet: "10.200.0.0/24".parse().unwrap(),
                server_ip: None,
                dns: vec![],
                allowed_ips: "0.0.0.0/0".into(),
                rate_limit: None,
            },
            amneziawg: None,
            vless: None,
            bot: None,
            auth: None,
            allowed_origins: vec![],
            min_client_version: Some("not-semver".into()),
            metrics: None,
        };
        let secrets = Secrets {
            database_url: String::new(),
            wg_private_key: String::new(),
            awg_private_key: None,
            bot: None,
            auth: None,
            vless: None,
        };
        let bot = Bot::new("123456:test-token");

        let err = AppState::new(
            pool.clone(),
            good.clone(),
            secrets.clone(),
            String::new(),
            None,
            bot.clone(),
        )
        .err()
        .expect("no [auth] secrets");
        assert!(matches!(err, StartupError::MissingAuthSecrets));

        let secrets = Secrets {
            auth: Some(AuthSecrets {
                jwt_secret: "s".into(),
                encryption_key: "not-hex".into(),
                admin_telegram_ids: vec![],
            }),
            ..secrets
        };
        let err = AppState::new(
            pool.clone(),
            good.clone(),
            secrets.clone(),
            String::new(),
            None,
            bot.clone(),
        )
        .err()
        .expect("bad encryption key");
        assert!(matches!(err, StartupError::InvalidEncryptionKey(_)));

        let secrets = Secrets {
            auth: Some(AuthSecrets {
                encryption_key: "00".repeat(32),
                ..secrets.auth.unwrap()
            }),
            ..secrets
        };
        let err = AppState::new(pool, good, secrets, String::new(), None, bot)
            .err()
            .expect("bad min_client_version");
        assert!(matches!(err, StartupError::InvalidMinClientVersion(v, _) if v == "not-semver"));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn outdated_clients_get_426(pool: DbPool) {
        let router = create_router(test_state(pool, Some("1.2.0")));

        let resp = get(&router, "/version", &[(CLIENT_VERSION_HEADER, "1.1.9")]).await;
        assert_eq!(resp.status(), StatusCode::UPGRADE_REQUIRED);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), 1 << 16).await.unwrap()).unwrap();
        assert_eq!(body["error"], "upgrade_required");
        assert_eq!(body["min_version"], "1.2.0");

        for (label, headers) in [
            ("exact minimum", vec![(CLIENT_VERSION_HEADER, "1.2.0")]),
            ("newer", vec![(CLIENT_VERSION_HEADER, "2.0.0")]),
            ("browser (no header)", vec![]),
            ("unparseable", vec![(CLIENT_VERSION_HEADER, "dev")]),
        ] {
            let resp = get(&router, "/version", &headers).await;
            assert_eq!(resp.status(), StatusCode::OK, "{label}");
        }
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn no_gate_without_min_client_version(pool: DbPool) {
        let router = create_router(test_state(pool, None));
        let resp = get(&router, "/version", &[(CLIENT_VERSION_HEADER, "0.0.1")]).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn stale_tokens_are_refreshed_from_the_database(pool: DbPool) {
        let user_id = seed_user(&pool, 1, false).await;
        let session = open_session(&pool, user_id).await;
        let router = create_router(test_state(pool.clone(), None));

        // Fresh token: authenticated, nothing to refresh.
        let resp = authed(
            &router,
            axum::http::Method::GET,
            "/me",
            &token(user_id, false, Utc::now(), Some(session)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(refreshed_token(&resp).is_none());

        // Old token, and the user was promoted since it was issued: the refreshed token must
        // carry the flag as stored, not as claimed — and stay on the same session.
        sqlx::query!("UPDATE users SET is_admin = true WHERE id = $1", user_id)
            .execute(&pool)
            .await
            .unwrap();
        let old = token(
            user_id,
            false,
            Utc::now() - Duration::days(2),
            Some(session),
        );
        let resp = authed(&router, axum::http::Method::GET, "/me", &old).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let fresh = refreshed_token(&resp).expect("refreshed token");
        let claims = crate::admin::auth::verify_jwt(&fresh, JWT_SECRET).unwrap();
        assert_eq!(claims.sub, user_id);
        assert!(claims.admin);
        assert!(claims.iat > (Utc::now() - Duration::minutes(1)).timestamp());
        assert_eq!(claims.jti, Some(session));

        // An old legacy token (no session) is accepted, and its refresh moves it onto a new
        // `legacy` session so it becomes revocable.
        let legacy = token(user_id, false, Utc::now() - Duration::days(2), None);
        let resp = authed(&router, axum::http::Method::GET, "/me", &legacy).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let claims =
            crate::admin::auth::verify_jwt(&refreshed_token(&resp).unwrap(), JWT_SECRET).unwrap();
        let migrated = claims.jti.expect("refreshed legacy token has a session");
        let kind = sqlx::query_scalar!(
            r#"SELECT kind AS "kind: floppa_core::SessionKind" FROM sessions
               WHERE id = $1 AND user_id = $2"#,
            migrated,
            user_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kind, floppa_core::SessionKind::Legacy);

        // A valid token whose user is gone: 401, and no refresh to keep it alive.
        let ghost = token(
            user_id + 1_000_000,
            false,
            Utc::now() - Duration::days(2),
            None,
        );
        let resp = authed(&router, axum::http::Method::GET, "/me", &ghost).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(refreshed_token(&resp).is_none());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn sessions_gate_tokens(pool: DbPool) {
        use axum::http::Method;

        let user_id = seed_user(&pool, 1, false).await;
        let other_id = seed_user(&pool, 2, false).await;
        let phone = open_session(&pool, user_id).await;
        let laptop = open_session(&pool, user_id).await;
        let others = open_session(&pool, other_id).await;
        let router = create_router(test_state(pool.clone(), None));
        let now = Utc::now();

        // The session list names the caller's own session.
        let resp = authed(
            &router,
            Method::GET,
            "/me/sessions",
            &token(user_id, false, now, Some(phone)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let list = json_body(resp).await;
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        let current: Vec<&serde_json::Value> =
            list.iter().filter(|s| s["current"] == true).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0]["id"], phone.to_string());
        assert_eq!(current[0]["kind"], "deep_link");

        // A jti that names no row, or another user's row, is refused.
        for (label, session) in [("missing", Uuid::new_v4()), ("someone else's", others)] {
            let resp = authed(
                &router,
                Method::GET,
                "/me",
                &token(user_id, false, now, Some(session)),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{label}");
        }

        // Signing out the laptop from the phone: the laptop's token dies, the phone's lives.
        let resp = authed(
            &router,
            Method::DELETE,
            &format!("/me/sessions/{laptop}"),
            &token(user_id, false, now, Some(phone)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = authed(
            &router,
            Method::GET,
            "/me",
            &token(user_id, false, now, Some(laptop)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(refreshed_token(&resp).is_none());
        let resp = authed(
            &router,
            Method::GET,
            "/me",
            &token(user_id, false, now, Some(phone)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        // Gone means gone: a second sign-out is a 404, and so is someone else's session.
        for session in [laptop, others] {
            let resp = authed(
                &router,
                Method::DELETE,
                &format!("/me/sessions/{session}"),
                &token(user_id, false, now, Some(phone)),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }

        // Legacy tokens are accepted until "sign out everywhere" moves the cutoff past them.
        let legacy = token(user_id, false, now - Duration::minutes(5), None);
        let resp = authed(&router, Method::GET, "/me", &legacy).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = authed(
            &router,
            Method::POST,
            "/me/sessions/revoke-all",
            &token(user_id, false, now, Some(phone)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        for (label, t) in [
            (
                "caller's own session",
                token(user_id, false, now, Some(phone)),
            ),
            ("legacy token", legacy),
        ] {
            let resp = authed(&router, Method::GET, "/me", &t).await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{label}");
        }
        // A session opened after the cutoff is unaffected by it, even with an older iat on
        // its token (the row, not the cutoff, governs session-backed tokens).
        let fresh = open_session(&pool, user_id).await;
        let resp = authed(
            &router,
            Method::GET,
            "/me",
            &token(user_id, false, now - Duration::minutes(5), Some(fresh)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        // The other user's legacy tokens are untouched.
        let resp = authed(
            &router,
            Method::GET,
            "/me",
            &token(other_id, false, now - Duration::minutes(5), None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn active_sessions_are_touched_at_most_hourly(pool: DbPool) {
        let user_id = seed_user(&pool, 1, false).await;
        let session = open_session(&pool, user_id).await;
        let router = create_router(test_state(pool.clone(), None));
        let t = token(user_id, false, Utc::now(), Some(session));
        let last_seen = |pool: DbPool| async move {
            sqlx::query_scalar!("SELECT last_seen_at FROM sessions WHERE id = $1", session)
                .fetch_one(&pool)
                .await
                .unwrap()
        };

        // Just created: a request does not write.
        let before = last_seen(pool.clone()).await;
        authed(&router, axum::http::Method::GET, "/me", &t).await;
        assert_eq!(last_seen(pool.clone()).await, before);

        // Stale by more than the touch interval: bumped.
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = NOW() - INTERVAL '2 hours' WHERE id = $1",
            session
        )
        .execute(&pool)
        .await
        .unwrap();
        authed(&router, axum::http::Method::GET, "/me", &t).await;
        assert!(last_seen(pool).await > before - Duration::minutes(1));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn admins_manage_other_users_sessions(pool: DbPool) {
        use axum::http::Method;

        let admin_id = seed_user(&pool, 1, true).await;
        let user_id = seed_user(&pool, 2, false).await;
        let admin_session = open_session(&pool, admin_id).await;
        let user_session = open_session(&pool, user_id).await;
        let router = create_router(test_state(pool.clone(), None));
        let now = Utc::now();
        let admin = token(admin_id, true, now, Some(admin_session));
        let user = token(user_id, false, now, Some(user_session));

        // Not for regular users.
        let resp = authed(
            &router,
            Method::GET,
            &format!("/users/{user_id}/sessions"),
            &user,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let resp = authed(
            &router,
            Method::GET,
            &format!("/users/{user_id}/sessions"),
            &admin,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let list = json_body(resp).await;
        assert_eq!(list[0]["id"], user_session.to_string());
        assert_eq!(list[0]["current"], false);
        let resp = authed(&router, Method::GET, "/users/999999/sessions", &admin).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // The session id must belong to the user in the path.
        let resp = authed(
            &router,
            Method::DELETE,
            &format!("/users/{user_id}/sessions/{admin_session}"),
            &admin,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = authed(
            &router,
            Method::DELETE,
            &format!("/users/{user_id}/sessions/{user_session}"),
            &admin,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = authed(&router, Method::GET, "/me", &user).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Sign out everywhere also kills the user's legacy tokens, and only theirs.
        let user_legacy = token(user_id, false, now - Duration::minutes(5), None);
        let admin_legacy = token(admin_id, true, now - Duration::minutes(5), None);
        let resp = authed(
            &router,
            Method::POST,
            &format!("/users/{user_id}/sessions/revoke-all"),
            &admin,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = authed(&router, Method::GET, "/me", &user_legacy).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = authed(&router, Method::GET, "/me", &admin_legacy).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
