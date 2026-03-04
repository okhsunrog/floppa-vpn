use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::admin::auth::AdminUser;

use super::AppState;

#[derive(Serialize, ToSchema)]
pub struct Plan {
    id: i32,
    name: String,
    display_name: String,
    default_speed_limit_mbps: Option<i32>,
    default_traffic_limit_bytes: Option<i64>,
    max_peers: i32,
    price_rub: i32,
    is_public: bool,
    trial_days: Option<i32>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreatePlanRequest {
    name: String,
    display_name: String,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default)]
    default_traffic_limit_bytes: Option<i64>,
    #[serde(default = "default_max_peers")]
    max_peers: i32,
    #[serde(default)]
    price_rub: i32,
    #[serde(default = "default_is_public")]
    is_public: bool,
    #[serde(default)]
    trial_days: Option<i32>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePlanRequest {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default)]
    default_traffic_limit_bytes: Option<i64>,
    #[serde(default)]
    max_peers: Option<i32>,
    #[serde(default)]
    price_rub: Option<i32>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    trial_days: Option<i32>,
    #[serde(default)]
    clear_speed_limit: bool,
    #[serde(default)]
    clear_traffic_limit: bool,
    #[serde(default)]
    clear_trial_days: bool,
}

fn default_max_peers() -> i32 {
    1
}
fn default_is_public() -> bool {
    true
}

/// List all plans (admin only)
#[utoipa::path(
    get,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<Plan>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
    )
)]
pub(super) async fn list_plans(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Plan>>, StatusCode> {
    let plans: Vec<Plan> = sqlx::query_as!(
        Plan,
        "SELECT id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days FROM plans ORDER BY id"
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(plans))
}

/// Create a new plan (admin only)
#[utoipa::path(
    post,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    request_body = CreatePlanRequest,
    responses(
        (status = 200, body = Plan),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 500, description = "Internal server error"),
    )
)]
pub(super) async fn create_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreatePlanRequest>,
) -> Result<Json<Plan>, StatusCode> {
    let plan: Plan = sqlx::query_as!(
        Plan,
        r#"
        INSERT INTO plans (name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days
        "#,
        &req.name,
        &req.display_name,
        req.default_speed_limit_mbps,
        req.default_traffic_limit_bytes,
        req.max_peers,
        req.price_rub,
        req.is_public,
        req.trial_days
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to create plan: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(plan))
}

/// Update a plan (admin only)
#[utoipa::path(
    patch,
    path = "/plans/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i32, Path, description = "Plan ID")),
    request_body = UpdatePlanRequest,
    responses(
        (status = 200, body = Plan),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
    )
)]
pub(super) async fn update_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(req): Json<UpdatePlanRequest>,
) -> Result<Json<Plan>, StatusCode> {
    let plan: Plan = sqlx::query_as!(
        Plan,
        r#"
        UPDATE plans SET
            display_name = COALESCE($2, display_name),
            default_speed_limit_mbps = CASE WHEN $3 THEN NULL ELSE COALESCE($4, default_speed_limit_mbps) END,
            default_traffic_limit_bytes = CASE WHEN $5 THEN NULL ELSE COALESCE($6, default_traffic_limit_bytes) END,
            max_peers = COALESCE($7, max_peers),
            price_rub = COALESCE($8, price_rub),
            is_public = COALESCE($9, is_public),
            trial_days = CASE WHEN $10 THEN NULL ELSE COALESCE($11, trial_days) END
        WHERE id = $1
        RETURNING id, name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, price_rub, is_public, trial_days
        "#,
        id,
        req.display_name.as_deref(),
        req.clear_speed_limit,
        req.default_speed_limit_mbps,
        req.clear_traffic_limit,
        req.default_traffic_limit_bytes,
        req.max_peers,
        req.price_rub,
        req.is_public,
        req.clear_trial_days,
        req.trial_days
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(plan))
}

/// Delete a plan (admin only). Fails if plan has subscriptions.
#[utoipa::path(
    delete,
    path = "/plans/{id}",
    tag = "admin",
    security(("bearer" = [])),
    params(("id" = i32, Path, description = "Plan ID")),
    responses(
        (status = 204, description = "Plan deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "Plan not found"),
        (status = 409, description = "Plan has existing subscriptions"),
    )
)]
pub(super) async fn delete_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, StatusCode> {
    // Don't allow deleting plans that have subscriptions
    let has_subs = sqlx::query_scalar!("SELECT COUNT(*) FROM subscriptions WHERE plan_id = $1", id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if has_subs.unwrap_or(0) > 0 {
        return Err(StatusCode::CONFLICT);
    }

    let result = sqlx::query!("DELETE FROM plans WHERE id = $1", id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::NO_CONTENT)
}
