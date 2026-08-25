//! Server-side avatar cache.
//!
//! Telegram profile photos live on Telegram's CDN, which is unreachable from clients in Russia
//! (and serves no CORS headers, so client-side caching can't populate either). The server — in a
//! datacenter where Telegram is reachable — downloads the photo and caches the bytes in
//! `user_avatars`, then serves them from our own origin.
//!
//! Download path: Bot API (`getUserProfilePhotos` → `getFile` → download) first, since it works
//! for every Telegram user (including Mini App / bot-`/start` users that have no `photo_url`);
//! falling back to downloading the stored `photo_url` directly.

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use teloxide::prelude::*;
use teloxide::types::UserId;
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::admin::{
    auth::{AdminUser, AuthUser},
    error::ApiError,
};

use super::AppState;

/// Re-download a user's avatar if it's older than this.
const AVATAR_TTL_DAYS: i64 = 7;

/// Largest avatar we are willing to download and cache. Telegram profile photos are a few
/// hundred KB; anything bigger is not an avatar.
const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;

/// Read a response body, giving up as soon as it exceeds [`MAX_AVATAR_BYTES`] — the body is
/// never buffered past the cap, whatever Content-Length claimed (or didn't).
async fn read_capped(mut resp: reqwest::Response) -> Option<Vec<u8>> {
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_AVATAR_BYTES as u64)
    {
        debug!(url = %resp.url(), "avatar: response larger than the cap, skipped");
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if bytes.len() + chunk.len() > MAX_AVATAR_BYTES {
            debug!(url = %resp.url(), "avatar: body exceeded the cap, skipped");
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

// =============================================================================
// Download + cache (background)
// =============================================================================

/// Spawn a background avatar refresh for a Telegram user when the cached copy is missing or stale.
/// Never blocks the caller (the auth path) and swallows all errors — avatars are best-effort.
pub fn spawn_refresh_if_stale(
    state: &AppState,
    user_id: i64,
    telegram_id: i64,
    photo_url: Option<String>,
) {
    let state = state.clone();
    tokio::spawn(async move {
        match sqlx::query_scalar!(
            "SELECT fetched_at FROM user_avatars WHERE user_id = $1",
            user_id
        )
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(fetched_at))
                if chrono::Utc::now() - fetched_at < chrono::Duration::days(AVATAR_TTL_DAYS) =>
            {
                // Cached copy is fresh — nothing to do.
            }
            Ok(_) => refresh_avatar(&state, user_id, telegram_id, photo_url.as_deref()).await,
            Err(e) => warn!(user_id, error = %e, "avatar: staleness check failed"),
        }
    });
}

/// Download (Bot API, then `photo_url` fallback) and store the avatar. Best-effort.
async fn refresh_avatar(state: &AppState, user_id: i64, telegram_id: i64, photo_url: Option<&str>) {
    let fetched = match fetch_via_bot(state, telegram_id).await {
        Some(bytes) => Some((bytes, "image/jpeg".to_string())),
        None => match photo_url {
            Some(url) => fetch_via_url(state, url).await,
            None => None,
        },
    };

    let Some((bytes, content_type)) = fetched else {
        debug!(user_id, telegram_id, "avatar: no photo available");
        return;
    };

    let etag = format!("\"{:x}\"", Sha256::digest(&bytes));
    if let Err(e) = sqlx::query!(
        r#"
        INSERT INTO user_avatars (user_id, blob, content_type, etag, fetched_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (user_id) DO UPDATE
            SET blob = $2, content_type = $3, etag = $4, fetched_at = NOW()
        "#,
        user_id,
        &bytes,
        content_type,
        etag,
    )
    .execute(&state.pool)
    .await
    {
        warn!(user_id, error = %e, "avatar: failed to store");
    } else {
        debug!(user_id, bytes = bytes.len(), "avatar: cached");
    }
}

/// Fetch the largest profile photo via the Bot API. Requires the user to be reachable by the bot.
async fn fetch_via_bot(state: &AppState, telegram_id: i64) -> Option<Vec<u8>> {
    let token = state.secrets.bot.as_ref()?.token.clone();
    let uid = UserId(telegram_id as u64);

    let photos = state.bot.get_user_profile_photos(uid).await.ok()?;
    // photos[0] is the most recent photo as a set of sizes; take the largest.
    let largest = photos
        .photos
        .into_iter()
        .next()?
        .into_iter()
        .max_by_key(|p| p.width * p.height)?;

    let file = state.bot.get_file(largest.file.id).await.ok()?;
    let url = format!("https://api.telegram.org/file/bot{token}/{}", file.path);
    let resp = state.http_client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    read_capped(resp).await
}

/// Fallback: download the stored `photo_url` directly from the server.
async fn fetch_via_url(state: &AppState, url: &str) -> Option<(Vec<u8>, String)> {
    let resp = state.http_client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|ct| ct.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = read_capped(resp).await?;
    Some((bytes, content_type))
}

/// Trigger a (stale-gated) avatar refresh for the given users — looks up their telegram_id +
/// photo_url and spawns a background fetch. Used to populate avatars on demand for users that were
/// already logged in before this feature existed (the auth-time trigger only fires on a fresh login).
async fn trigger_refresh_for_users(state: &AppState, user_ids: &[i64]) {
    if user_ids.is_empty() {
        return;
    }
    let rows = sqlx::query!(
        "SELECT id, telegram_id, photo_url FROM users WHERE id = ANY($1) AND telegram_id IS NOT NULL",
        user_ids,
    )
    .fetch_all(&state.pool)
    .await;

    if let Ok(rows) = rows {
        for r in rows {
            if let Some(tg) = r.telegram_id {
                spawn_refresh_if_stale(state, r.id, tg, r.photo_url);
            }
        }
    }
}

// =============================================================================
// Serving
// =============================================================================

/// Build a cacheable binary avatar response (handles `If-None-Match` → 304).
async fn serve_avatar(state: &AppState, user_id: i64, headers: &HeaderMap) -> Response {
    let row = match sqlx::query!(
        "SELECT blob, content_type, etag FROM user_avatars WHERE user_id = $1",
        user_id
    )
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return ApiError::not_found("No avatar cached").into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    };

    // Conditional request: client already has this exact image.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|inm| inm.split(',').any(|t| t.trim() == row.etag))
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, row.etag)]).into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, row.content_type),
            (header::ETAG, row.etag),
            // Per-user authed resource → private cache; revalidate via ETag.
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
        ],
        Body::from(row.blob),
    )
        .into_response()
}

/// Get the current user's cached avatar.
#[utoipa::path(
    get,
    path = "/me/avatar",
    tag = "user",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Avatar image bytes", content_type = "image/jpeg"),
        (status = 304, description = "Not modified"),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 404, body = ApiError, description = "No avatar cached"),
    )
)]
pub(super) async fn get_my_avatar(
    auth: AuthUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // Populate/refresh on demand (stale-gated) so already-logged-in users get an avatar without
    // re-authenticating. Spawned in the background; the client retries the 404 shortly after.
    trigger_refresh_for_users(&state, &[auth.user_id]).await;
    serve_avatar(&state, auth.user_id, &headers).await
}

/// Get a user's cached avatar (admin).
#[utoipa::path(
    get,
    path = "/admin/users/{id}/avatar",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, description = "Avatar image bytes", content_type = "image/jpeg"),
        (status = 304, description = "Not modified"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "No avatar cached"),
    )
)]
pub(super) async fn get_user_avatar(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(user_id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    serve_avatar(&state, user_id, &headers).await
}

#[derive(Deserialize, ToSchema)]
pub struct AvatarBatchRequest {
    user_ids: Vec<i64>,
}

/// Batch-fetch cached avatars as data URLs (admin). Returns only users that have a cached avatar,
/// keyed by user id (string). Avoids one request per row in the admin user list.
#[utoipa::path(
    post,
    path = "/admin/avatars",
    tag = "admin",
    security(("bearer" = [])),
    request_body = AvatarBatchRequest,
    responses(
        (status = 200, description = "Map of user id → data URL", body = HashMap<String, String>),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn get_avatars_batch(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<AvatarBatchRequest>,
) -> Result<Json<HashMap<String, String>>, ApiError> {
    // Cap to avoid pathological requests.
    let ids: Vec<i64> = req.user_ids.into_iter().take(200).collect();
    let rows = sqlx::query!(
        "SELECT user_id, blob, content_type FROM user_avatars WHERE user_id = ANY($1)",
        &ids,
    )
    .fetch_all(&state.pool)
    .await?;

    let map: HashMap<String, String> = rows
        .into_iter()
        .map(|r| {
            let data_url = format!(
                "data:{};base64,{}",
                r.content_type,
                BASE64_STANDARD.encode(&r.blob)
            );
            (r.user_id.to_string(), data_url)
        })
        .collect();

    // Populate avatars for requested users we don't have yet (stale-gated, background), so the
    // admin list fills in on subsequent loads without requiring each user to re-authenticate.
    let missing: Vec<i64> = ids
        .into_iter()
        .filter(|id| !map.contains_key(&id.to_string()))
        .collect();
    trigger_refresh_for_users(&state, &missing).await;

    Ok(Json(map))
}
