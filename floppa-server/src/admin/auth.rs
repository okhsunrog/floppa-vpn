//! Authentication module for Telegram Login and JWT tokens

use axum::{extract::FromRequestParts, http::request::Parts};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::warn;
use utoipa::ToSchema;

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
/// Only `sub`, `exp` and `iat` are load-bearing: authorization is always re-read from the
/// database (see [`AuthUser`]), `admin` is informational. Tokens issued by older builds carry a
/// `username` claim too; serde ignores it, so they keep verifying.
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

/// Create a JWT token for an authenticated user
pub fn create_jwt(
    user_id: i64,
    is_admin: bool,
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

/// The authenticated user behind a request: a valid bearer JWT whose subject still exists.
///
/// The existence check is one indexed SELECT per authenticated request; it also refreshes
/// `is_admin` from the database, so a stale token can neither outlive its user (e.g. a husk
/// account deleted by a Telegram merge) nor keep admin rights that were revoked.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: i64,
    pub is_admin: bool,
}

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

        let Some(is_admin) = lookup_is_admin(&state.pool, claims.sub).await? else {
            warn!("User {} from JWT not found in DB", claims.sub);
            return Err(ApiError::unauthorized());
        };

        Ok(AuthUser {
            user_id: claims.sub,
            is_admin,
        })
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
        let token = create_jwt(123, true, secret, 24).unwrap();
        let claims = verify_jwt(&token, secret).unwrap();

        assert_eq!(claims.sub, 123);
        assert!(claims.admin);
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
        assert_eq!(verify_jwt(&token, secret).unwrap().sub, 7);
    }
}
