//! Authentication module for Telegram Login and JWT tokens

use axum::{extract::FromRequestParts, http::request::Parts};
use chrono::{DateTime, Duration, Utc};
use floppa_core::services;
use hmac::{Hmac, Mac};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::warn;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::admin::{error::ApiError, routes::AppState};

/// Data received from Telegram Login Widget
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramAuthData {
    pub id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub photo_url: Option<String>,
    pub auth_date: i64,
    pub hash: String,
}

/// JWT Claims.
///
/// Only `sub`, `exp`, `iat` and `jti` are load-bearing: authorization is always re-read from
/// the database (see [`AuthUser`]), `admin` is informational. Tokens issued by older builds
/// carry a `username` claim too; serde ignores it, so they keep verifying.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// User ID
    pub sub: i64,
    /// Was an admin when the token was issued (informational — never used for authorization)
    pub admin: bool,
    /// Expiration time (Unix timestamp)
    pub exp: i64,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// The `sessions` row this token belongs to. Absent on tokens issued before sessions
    /// existed ("legacy" tokens); those are accepted until they expire or the user's
    /// `tokens_valid_after` cutoff moves past their `iat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<Uuid>,
}

/// Verify Telegram Login Widget data
///
/// Algorithm from https://core.telegram.org/widgets/login#checking-authorization
pub fn verify_telegram_auth(data: &TelegramAuthData, bot_token: &str) -> bool {
    // Check auth_date is recent (within 24 hours)
    let now = Utc::now().timestamp();
    if now - data.auth_date > 86400 {
        warn!("Telegram auth data expired: auth_date={}", data.auth_date);
        return false;
    }

    // Build data-check-string (sorted key=value pairs, excluding hash)
    let mut pairs = Vec::new();
    pairs.push(format!("auth_date={}", data.auth_date));
    if let Some(ref first_name) = data.first_name {
        pairs.push(format!("first_name={}", first_name));
    }
    pairs.push(format!("id={}", data.id));
    if let Some(ref last_name) = data.last_name {
        pairs.push(format!("last_name={}", last_name));
    }
    if let Some(ref photo_url) = data.photo_url {
        pairs.push(format!("photo_url={}", photo_url));
    }
    if let Some(ref username) = data.username {
        pairs.push(format!("username={}", username));
    }
    pairs.sort();
    let data_check_string = pairs.join("\n");

    // secret_key = SHA256(bot_token)
    let secret_key = {
        use sha2::Digest;
        let mut hasher = Sha256::new();
        hasher.update(bot_token.as_bytes());
        hasher.finalize()
    };

    // hash = HMAC-SHA256(secret_key, data_check_string)
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&secret_key).expect("HMAC can take key of any size");
    mac.update(data_check_string.as_bytes());

    let provided_hash = match hex::decode(&data.hash) {
        Ok(h) => h,
        Err(_) => {
            warn!("Telegram auth hash is not valid hex");
            return false;
        }
    };

    if mac.verify_slice(&provided_hash).is_err() {
        warn!("Telegram auth hash mismatch");
        return false;
    }

    true
}

/// Data from Telegram Mini App initData
#[derive(Debug, Deserialize)]
pub struct MiniAppUser {
    pub id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

/// Verify Telegram Mini App initData
///
/// Algorithm from https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app
pub fn verify_telegram_mini_app(init_data: &str, bot_token: &str) -> Option<MiniAppUser> {
    let params: Vec<(String, String)> = form_urlencoded::parse(init_data.as_bytes())
        .map(|(k, v): (std::borrow::Cow<str>, std::borrow::Cow<str>)| {
            (k.into_owned(), v.into_owned())
        })
        .collect();

    let hash = params.iter().find(|(k, _)| k == "hash")?.1.clone();

    // Build data_check_string: sorted key=value pairs excluding hash, joined by \n
    let mut check_pairs: Vec<&(String, String)> =
        params.iter().filter(|(k, _)| k != "hash").collect();
    check_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let data_check_string: String = check_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");

    // secret_key = HMAC-SHA256("WebAppData", bot_token)
    let mut secret_mac =
        Hmac::<Sha256>::new_from_slice(b"WebAppData").expect("HMAC can take key of any size");
    secret_mac.update(bot_token.as_bytes());
    let secret_key = secret_mac.finalize().into_bytes();

    // computed_hash = HMAC-SHA256(secret_key, data_check_string)
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&secret_key).expect("HMAC can take key of any size");
    mac.update(data_check_string.as_bytes());

    let provided_hash = hex::decode(&hash).ok()?;
    if mac.verify_slice(&provided_hash).is_err() {
        warn!("Mini App initData hash mismatch");
        return None;
    }

    // Check auth_date is recent
    let auth_date: i64 = params
        .iter()
        .find(|(k, _)| k == "auth_date")?
        .1
        .parse()
        .ok()?;
    let now = Utc::now().timestamp();
    if now - auth_date > 86400 {
        warn!("Mini App initData expired: auth_date={auth_date}");
        return None;
    }

    // Parse user JSON
    let user_json = params.iter().find(|(k, _)| k == "user")?.1.clone();
    serde_json::from_str(&user_json).ok()
}

/// Create a JWT token for an authenticated user, bound to the session `session_id`.
pub fn create_jwt(
    user_id: i64,
    is_admin: bool,
    session_id: Uuid,
    secret: &str,
    expiration_hours: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let exp = now + Duration::hours(expiration_hours as i64);

    let claims = Claims {
        sub: user_id,
        admin: is_admin,
        exp: exp.timestamp(),
        iat: now.timestamp(),
        jti: Some(session_id),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Verify and decode a JWT token
pub fn verify_jwt(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}

/// The authenticated user behind a request: a valid bearer JWT whose subject still exists and
/// whose session (if the token has one) is live.
///
/// The check is one indexed SELECT per authenticated request (user row joined with the
/// token's session row); it also refreshes `is_admin` from the database, so a stale token can
/// neither outlive its user (e.g. a husk account deleted by a Telegram merge), nor keep admin
/// rights that were revoked, nor survive a "sign out" of its session.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: i64,
    pub is_admin: bool,
    /// The session behind the token; `None` for a legacy token (issued before sessions).
    pub session_id: Option<Uuid>,
}

/// Why a correctly signed, unexpired token is refused anyway. Logged, never sent to the client
/// (every case is a plain 401).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionRejection {
    #[error("user no longer exists")]
    UserGone,
    /// A legacy (no `jti`) token issued before the user's `tokens_valid_after`.
    #[error("legacy token predates the tokens_valid_after cutoff")]
    LegacyTokenCutOff,
    #[error("session row does not exist")]
    SessionMissing,
    #[error("session was revoked")]
    SessionRevoked,
    #[error("session belongs to another user")]
    SessionUserMismatch,
}

impl SessionRejection {
    /// Bounded label for the rejection counter.
    fn as_metric_label(self) -> &'static str {
        match self {
            Self::UserGone => "user_gone",
            Self::LegacyTokenCutOff => "legacy_cutoff",
            Self::SessionMissing => "session_missing",
            Self::SessionRevoked => "session_revoked",
            Self::SessionUserMismatch => "session_user_mismatch",
        }
    }
}

/// What the database says about a token's subject and session, as one row.
#[derive(Debug)]
struct TokenSubject {
    is_admin: bool,
    tokens_valid_after: Option<DateTime<Utc>>,
    /// The session the token names, when such a row exists at all.
    session: Option<SessionRow>,
}

#[derive(Debug)]
struct SessionRow {
    user_id: i64,
    revoked_at: Option<DateTime<Utc>>,
    last_seen_at: DateTime<Utc>,
}

/// `None` when the user row is gone. `session_id` is the token's `jti`; the session half of
/// the result is `None` both for a legacy token and for a `jti` that names no row.
async fn load_token_subject(
    pool: &floppa_core::DbPool,
    user_id: i64,
    session_id: Option<Uuid>,
) -> Result<Option<TokenSubject>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT u.is_admin, u.tokens_valid_after,
                  s.user_id AS "session_user?", s.revoked_at AS "session_revoked_at?",
                  s.last_seen_at AS "session_last_seen_at?"
           FROM users u
           LEFT JOIN sessions s ON s.id = $2
           WHERE u.id = $1"#,
        user_id,
        session_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| TokenSubject {
        is_admin: r.is_admin,
        tokens_valid_after: r.tokens_valid_after,
        session: match (r.session_user, r.session_last_seen_at) {
            (Some(user_id), Some(last_seen_at)) => Some(SessionRow {
                user_id,
                revoked_at: r.session_revoked_at,
                last_seen_at,
            }),
            _ => None,
        },
    }))
}

/// The authorization decision, separated from I/O so it can be tested exhaustively.
fn authorize(claims: &Claims, subject: Option<TokenSubject>) -> Result<AuthUser, SessionRejection> {
    let subject = subject.ok_or(SessionRejection::UserGone)?;
    match claims.jti {
        // A session-backed token lives and dies with its row.
        Some(_) => {
            let session = subject.session.ok_or(SessionRejection::SessionMissing)?;
            if session.user_id != claims.sub {
                return Err(SessionRejection::SessionUserMismatch);
            }
            if session.revoked_at.is_some() {
                return Err(SessionRejection::SessionRevoked);
            }
        }
        // A legacy token has no row to revoke; the per-user cutoff is the only handle on it.
        None => {
            if let Some(cutoff) = subject.tokens_valid_after
                && claims.iat < cutoff.timestamp()
            {
                return Err(SessionRejection::LegacyTokenCutOff);
            }
        }
    }
    Ok(AuthUser {
        user_id: claims.sub,
        is_admin: subject.is_admin,
        session_id: claims.jti,
    })
}

/// How stale a session's `last_seen_at` may get before an authenticated request bumps it.
/// Keeps the per-request cost at one SELECT; the UPDATE runs at most once an hour per session.
pub(crate) const SESSION_TOUCH_AFTER: Duration = Duration::hours(1);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Get Authorization header
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        let token = auth_header.ok_or_else(ApiError::unauthorized)?;

        let claims = verify_jwt(token, &state.auth_secrets.jwt_secret).map_err(|e| {
            warn!("JWT verification failed: {}", e);
            ApiError::unauthorized()
        })?;

        let subject = load_token_subject(&state.pool, claims.sub, claims.jti).await?;
        let stale_session = subject
            .as_ref()
            .and_then(|s| s.session.as_ref())
            .is_some_and(|s| Utc::now() - s.last_seen_at > SESSION_TOUCH_AFTER);

        let user = authorize(&claims, subject).map_err(|reason| {
            warn!(user_id = claims.sub, session_id = ?claims.jti, %reason, "token rejected");
            crate::metrics::token_rejected(reason.as_metric_label());
            ApiError::unauthorized()
        })?;

        if stale_session && let Some(session_id) = user.session_id {
            // Bookkeeping: a failure here must not fail the request.
            if let Err(e) = services::touch_session(&state.pool, session_id).await {
                warn!(%session_id, error = %e, "failed to bump session last_seen_at");
            }
        }

        Ok(user)
    }
}

/// `Some(is_admin)` for an existing user, `None` if the row is gone.
pub(crate) async fn lookup_is_admin(
    pool: &floppa_core::DbPool,
    user_id: i64,
) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!("SELECT is_admin FROM users WHERE id = $1", user_id)
        .fetch_optional(pool)
        .await
}

/// Admin user extractor - requires is_admin = true (as currently stored, not as in the JWT)
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin {
            warn!("User {} is not an admin", user.user_id);
            return Err(ApiError::forbidden("Not an admin"));
        }
        Ok(AdminUser(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_roundtrip() {
        let secret = "test-secret";
        let session = Uuid::new_v4();
        let token = create_jwt(123, true, session, secret, 24).unwrap();
        let claims = verify_jwt(&token, secret).unwrap();

        assert_eq!(claims.sub, 123);
        assert!(claims.admin);
        assert_eq!(claims.jti, Some(session));
    }

    fn claims(sub: i64, jti: Option<Uuid>, iat: DateTime<Utc>) -> Claims {
        Claims {
            sub,
            admin: false,
            exp: (iat + Duration::days(30)).timestamp(),
            iat: iat.timestamp(),
            jti,
        }
    }

    fn subject(cutoff: Option<DateTime<Utc>>, session: Option<SessionRow>) -> TokenSubject {
        TokenSubject {
            is_admin: true,
            tokens_valid_after: cutoff,
            session,
        }
    }

    fn live_session(user_id: i64) -> SessionRow {
        SessionRow {
            user_id,
            revoked_at: None,
            last_seen_at: Utc::now(),
        }
    }

    #[test]
    fn authorize_gates_on_user_session_and_cutoff() {
        let now = Utc::now();
        let jti = Uuid::new_v4();

        // Gone user: nothing else matters.
        assert_eq!(
            authorize(&claims(1, Some(jti), now), None).unwrap_err(),
            SessionRejection::UserGone
        );

        // Session-backed token: the row decides, and is_admin comes from the database.
        let user = authorize(
            &claims(1, Some(jti), now),
            Some(subject(None, Some(live_session(1)))),
        )
        .unwrap();
        assert_eq!(
            (user.user_id, user.is_admin, user.session_id),
            (1, true, Some(jti))
        );
        assert_eq!(
            authorize(&claims(1, Some(jti), now), Some(subject(None, None))).unwrap_err(),
            SessionRejection::SessionMissing
        );
        assert_eq!(
            authorize(
                &claims(1, Some(jti), now),
                Some(subject(None, Some(live_session(2))))
            )
            .unwrap_err(),
            SessionRejection::SessionUserMismatch
        );
        let revoked = SessionRow {
            revoked_at: Some(now),
            ..live_session(1)
        };
        assert_eq!(
            authorize(
                &claims(1, Some(jti), now),
                Some(subject(None, Some(revoked)))
            )
            .unwrap_err(),
            SessionRejection::SessionRevoked
        );
        // The cutoff is for legacy tokens only: a session-backed token older than it is fine.
        assert!(
            authorize(
                &claims(1, Some(jti), now - Duration::days(2)),
                Some(subject(
                    Some(now - Duration::days(1)),
                    Some(live_session(1))
                ))
            )
            .is_ok()
        );

        // Legacy token: accepted until the cutoff passes its iat.
        let legacy = authorize(&claims(1, None, now), Some(subject(None, None))).unwrap();
        assert_eq!(legacy.session_id, None);
        assert!(
            authorize(
                &claims(1, None, now),
                Some(subject(Some(now - Duration::days(1)), None))
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &claims(1, None, now - Duration::days(2)),
                Some(subject(Some(now - Duration::days(1)), None))
            )
            .unwrap_err(),
            SessionRejection::LegacyTokenCutOff
        );
    }

    fn hmac_hex(key: &[u8], data: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    const BOT_TOKEN: &str = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    fn widget_data(auth_date: i64) -> TelegramAuthData {
        // Widget: secret = SHA256(bot_token), data-check-string = sorted "k=v" lines.
        let secret = {
            use sha2::Digest;
            Sha256::digest(BOT_TOKEN.as_bytes())
        };
        let check = format!("auth_date={auth_date}\nfirst_name=Ann\nid=42\nusername=ann");
        TelegramAuthData {
            id: 42,
            first_name: Some("Ann".into()),
            last_name: None,
            username: Some("ann".into()),
            photo_url: None,
            auth_date,
            hash: hmac_hex(&secret, &check),
        }
    }

    #[test]
    fn widget_signature_accepts_only_the_signed_payload() {
        let now = Utc::now().timestamp();
        assert!(verify_telegram_auth(&widget_data(now), BOT_TOKEN));

        let mut tampered = widget_data(now);
        tampered.username = Some("admin".into());
        assert!(!verify_telegram_auth(&tampered, BOT_TOKEN));

        assert!(!verify_telegram_auth(&widget_data(now), "other-token"));

        let mut not_hex = widget_data(now);
        not_hex.hash = "zz".into();
        assert!(!verify_telegram_auth(&not_hex, BOT_TOKEN));

        // Signed correctly, but a day and a bit ago.
        assert!(!verify_telegram_auth(
            &widget_data(now - 86400 - 60),
            BOT_TOKEN
        ));
    }

    fn mini_app_init_data(auth_date: i64, user_json: &str) -> String {
        // Mini App: secret = HMAC("WebAppData", bot_token), same data-check-string rule over the
        // decoded query pairs.
        let secret = {
            let mut mac = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
            mac.update(BOT_TOKEN.as_bytes());
            mac.finalize().into_bytes()
        };
        let check = format!("auth_date={auth_date}\nquery_id=AAEx\nuser={user_json}");
        let hash = hmac_hex(&secret, &check);
        form_urlencoded::Serializer::new(String::new())
            .append_pair("query_id", "AAEx")
            .append_pair("user", user_json)
            .append_pair("auth_date", &auth_date.to_string())
            .append_pair("hash", &hash)
            .finish()
    }

    #[test]
    fn mini_app_signature_accepts_only_the_signed_payload() {
        let now = Utc::now().timestamp();
        let user = r#"{"id":42,"first_name":"Ann","username":"ann","language_code":"en"}"#;

        let ok = verify_telegram_mini_app(&mini_app_init_data(now, user), BOT_TOKEN)
            .expect("valid initData");
        assert_eq!(ok.id, 42);
        assert_eq!(ok.username.as_deref(), Some("ann"));

        assert!(verify_telegram_mini_app(&mini_app_init_data(now, user), "other").is_none());

        let forged = mini_app_init_data(now, user).replace("%22id%22%3A42", "%22id%22%3A43");
        assert_ne!(forged, mini_app_init_data(now, user));
        assert!(verify_telegram_mini_app(&forged, BOT_TOKEN).is_none());

        assert!(
            verify_telegram_mini_app(&mini_app_init_data(now - 86400 - 60, user), BOT_TOKEN)
                .is_none()
        );
        assert!(verify_telegram_mini_app("hash=abc", BOT_TOKEN).is_none());
    }

    /// Tokens minted before the `username` claim was dropped must keep verifying.
    #[test]
    fn jwt_with_legacy_username_claim_still_verifies() {
        let secret = "test-secret";
        let now = Utc::now().timestamp();
        let legacy = serde_json::json!({
            "sub": 7, "admin": false, "username": "old", "exp": now + 3600, "iat": now
        });
        let token = encode(
            &Header::default(),
            &legacy,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        let claims = verify_jwt(&token, secret).unwrap();
        assert_eq!(claims.sub, 7);
        assert_eq!(claims.jti, None);
    }
}
