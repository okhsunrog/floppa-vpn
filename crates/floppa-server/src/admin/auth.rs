//! Authentication module for Telegram Login and JWT tokens

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use utoipa::ToSchema;

use crate::admin::routes::AppState;

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

/// JWT Claims
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// User ID
    pub sub: i64,
    /// Is admin
    pub admin: bool,
    /// Username (for display)
    pub username: Option<String>,
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
        tracing::warn!("Telegram auth data expired: auth_date={}", data.auth_date);
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
    let result = mac.finalize();
    let expected_hash = hex::encode(result.into_bytes());

    if expected_hash != data.hash {
        tracing::warn!("Telegram auth hash mismatch");
        return false;
    }

    true
}

/// Data from Telegram Mini App initData
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
    let computed_hash = hex::encode(mac.finalize().into_bytes());

    if computed_hash != hash {
        tracing::warn!("Mini App initData hash mismatch");
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
        tracing::warn!("Mini App initData expired: auth_date={auth_date}");
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
    username: Option<String>,
    secret: &str,
    expiration_hours: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let exp = now + Duration::hours(expiration_hours as i64);

    let claims = Claims {
        sub: user_id,
        admin: is_admin,
        username,
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

/// Authenticated user extracted from request
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuthUser {
    pub user_id: i64,
    pub is_admin: bool,
    pub username: Option<String>,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

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

        let token = auth_header.ok_or(StatusCode::UNAUTHORIZED)?;

        // Get JWT secret from secrets
        let secret = state
            .secrets
            .auth
            .as_ref()
            .map(|a| a.jwt_secret.as_str())
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

        // Verify token
        let claims = verify_jwt(token, secret).map_err(|e| {
            tracing::warn!("JWT verification failed: {}", e);
            StatusCode::UNAUTHORIZED
        })?;

        Ok(AuthUser {
            user_id: claims.sub,
            is_admin: claims.admin,
            username: claims.username,
        })
    }
}

/// Admin user extractor - requires is_admin = true
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;

        // Always verify admin status against DB (JWT may be stale)
        let is_admin: Option<(bool,)> = sqlx::query_as("SELECT is_admin FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to verify admin status: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        match is_admin {
            Some((true,)) => Ok(AdminUser(user)),
            Some((false,)) => {
                tracing::warn!(
                    "User {} has admin JWT but is_admin=false in DB",
                    user.user_id
                );
                Err(StatusCode::FORBIDDEN)
            }
            None => {
                tracing::warn!("User {} from JWT not found in DB", user.user_id);
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_roundtrip() {
        let secret = "test-secret";
        let token = create_jwt(123, true, Some("testuser".into()), secret, 24).unwrap();
        let claims = verify_jwt(&token, secret).unwrap();

        assert_eq!(claims.sub, 123);
        assert!(claims.admin);
        assert_eq!(claims.username, Some("testuser".into()));
    }
}
