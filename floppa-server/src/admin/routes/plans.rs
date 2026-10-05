use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::admin::{auth::AdminUser, error::ApiError};

use super::AppState;

#[derive(Serialize, ToSchema)]
pub struct PlanFields {
    id: i32,
    name: String,
    display_name: String,
    default_speed_limit_mbps: Option<i32>,
    max_peers: i32,
    is_public: bool,
    trial_minutes: Option<i32>,
    price_stars: Option<i32>,
    period_days: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct Plan {
    #[serde(flatten)]
    fields: PlanFields,
    region_ids: Vec<String>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AdminRegion {
    id: String,
    display_name: String,
    is_active: bool,
}

#[utoipa::path(get, path = "/regions", tag = "admin", security(("bearer" = [])),
    responses((status = 200, body = Vec<AdminRegion>), (status = 403, body = ApiError)))]
pub(super) async fn list_regions(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<AdminRegion>>, ApiError> {
    Ok(Json(
        sqlx::query_as("SELECT id, display_name, is_active FROM regions ORDER BY sort_order, id")
            .fetch_all(&state.pool)
            .await?,
    ))
}

async fn with_regions(fields: PlanFields, conn: &mut sqlx::PgConnection) -> Result<Plan, ApiError> {
    let region_ids = sqlx::query_scalar(
        "SELECT region_id FROM plan_regions WHERE plan_id = $1 ORDER BY region_id",
    )
    .bind(fields.id)
    .fetch_all(conn)
    .await?;
    Ok(Plan { fields, region_ids })
}

/// Europe is the daemon's fallback exit, so it must remain available on every plan.
/// Inactive regions may be retained, but cannot be newly granted.
async fn replace_regions(
    conn: &mut sqlx::PgConnection,
    plan_id: i32,
    region_ids: &[String],
) -> Result<(), ApiError> {
    if !region_ids.iter().any(|id| id == "europe") {
        return Err(ApiError::bad_request(
            "Europe is required as the fallback region",
        ));
    }
    let invalid: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM unnest($1::text[]) AS requested(id) \
         WHERE NOT EXISTS (SELECT 1 FROM regions r WHERE r.id = requested.id \
         AND (r.is_active OR EXISTS (SELECT 1 FROM plan_regions pr \
         WHERE pr.plan_id = $2 AND pr.region_id = r.id))))",
    )
    .bind(region_ids)
    .bind(plan_id)
    .fetch_one(&mut *conn)
    .await?;
    if invalid {
        return Err(ApiError::bad_request("Unknown or inactive region"));
    }
    sqlx::query("DELETE FROM plan_regions WHERE plan_id = $1")
        .bind(plan_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO plan_regions (plan_id, region_id) SELECT DISTINCT $1, id FROM unnest($2::text[]) AS requested(id)")
        .bind(plan_id).bind(region_ids).execute(&mut *conn).await?;
    // Notify each affected subscriber after the transaction commits.
    sqlx::query("SELECT pg_notify('subscription_changed', user_id::text) FROM current_subscriptions WHERE plan_id = $1")
        .bind(plan_id).execute(conn).await?;
    Ok(())
}

#[derive(Deserialize, ToSchema)]
pub struct CreatePlanRequest {
    #[serde(default)]
    region_ids: Option<Vec<String>>,
    name: String,
    display_name: String,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default = "default_max_peers")]
    max_peers: i32,
    #[serde(default = "default_is_public")]
    is_public: bool,
    #[serde(default)]
    trial_minutes: Option<i32>,
    #[serde(default)]
    price_stars: Option<i32>,
    #[serde(default)]
    period_days: Option<i32>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePlanRequest {
    #[serde(default)]
    region_ids: Option<Vec<String>>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    default_speed_limit_mbps: Option<i32>,
    #[serde(default)]
    max_peers: Option<i32>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    trial_minutes: Option<i32>,
    #[serde(default)]
    price_stars: Option<i32>,
    #[serde(default)]
    period_days: Option<i32>,
    #[serde(default)]
    clear_speed_limit: bool,
    #[serde(default)]
    clear_trial_minutes: bool,
    #[serde(default)]
    clear_price_stars: bool,
    #[serde(default)]
    clear_period_days: bool,
}

fn default_max_peers() -> i32 {
    1
}
fn default_is_public() -> bool {
    true
}

/// Public-facing view of a plan (no internal `name`/`is_public`). Served unauthenticated
/// to the landing page and Info tab so users can see pricing without logging in.
#[derive(Serialize, ToSchema)]
pub struct PublicPlan {
    id: i32,
    display_name: String,
    default_speed_limit_mbps: Option<i32>,
    max_peers: i32,
    trial_minutes: Option<i32>,
    price_stars: Option<i32>,
    period_days: Option<i32>,
}

/// List public plans (no auth) — only plans flagged `is_public`. Used by the landing page
/// and the in-app Info tab to display tariffs.
#[utoipa::path(
    get,
    path = "/plans/public",
    tag = "public",
    responses(
        (status = 200, body = Vec<PublicPlan>),
    )
)]
pub(super) async fn list_public_plans(
    State(state): State<AppState>,
) -> Result<Json<Vec<PublicPlan>>, ApiError> {
    let plans: Vec<PublicPlan> = sqlx::query_as!(
        PublicPlan,
        "SELECT id, display_name, default_speed_limit_mbps, max_peers, trial_minutes, price_stars, period_days \
         FROM plans WHERE is_public = true ORDER BY price_stars ASC NULLS FIRST, id ASC"
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(plans))
}

/// List all plans (admin only)
#[utoipa::path(
    get,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    responses(
        (status = 200, body = Vec<Plan>),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
    )
)]
pub(super) async fn list_plans(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Plan>>, ApiError> {
    let plans: Vec<PlanFields> = sqlx::query_as!(
        PlanFields,
        "SELECT id, name, display_name, default_speed_limit_mbps, max_peers, is_public, trial_minutes, price_stars, period_days FROM plans ORDER BY id"
    )
    .fetch_all(&state.pool)
    .await?;

    let mut conn = state.pool.acquire().await?;
    let mut result = Vec::with_capacity(plans.len());
    for plan in plans {
        result.push(with_regions(plan, &mut conn).await?);
    }
    Ok(Json(result))
}

/// Create a new plan (admin only)
#[utoipa::path(
    post,
    path = "/plans",
    tag = "admin",
    security(("bearer" = [])),
    request_body = CreatePlanRequest,
    responses(
        (status = 201, body = Plan),
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 500, body = ApiError, description = "Internal server error"),
    )
)]
pub(super) async fn create_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(req): Json<CreatePlanRequest>,
) -> Result<(StatusCode, Json<Plan>), ApiError> {
    if let Some(stars) = req.price_stars
        && stars <= 0
    {
        return Err(ApiError::bad_request("price_stars must be positive"));
    }
    if let Some(days) = req.period_days
        && days < 1
    {
        return Err(ApiError::bad_request("period_days must be at least 1"));
    }

    let mut tx = state.pool.begin().await?;
    let plan: PlanFields = sqlx::query_as!(
        PlanFields,
        r#"
        INSERT INTO plans (name, display_name, default_speed_limit_mbps, max_peers, is_public, trial_minutes, price_stars, period_days)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, name, display_name, default_speed_limit_mbps, max_peers, is_public, trial_minutes, price_stars, period_days
        "#,
        &req.name,
        &req.display_name,
        req.default_speed_limit_mbps,
        req.max_peers,
        req.is_public,
        req.trial_minutes,
        req.price_stars,
        req.period_days
    )
    .fetch_one(&mut *tx)
    .await?;
    if let Some(region_ids) = &req.region_ids {
        replace_regions(&mut tx, plan.id, region_ids).await?;
    }
    let plan = with_regions(plan, &mut tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(plan)))
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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Plan not found"),
    )
)]
pub(super) async fn update_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
    Json(req): Json<UpdatePlanRequest>,
) -> Result<Json<Plan>, ApiError> {
    if let Some(stars) = req.price_stars
        && stars <= 0
    {
        return Err(ApiError::bad_request("price_stars must be positive"));
    }
    if let Some(days) = req.period_days
        && days < 1
    {
        return Err(ApiError::bad_request("period_days must be at least 1"));
    }

    let mut tx = state.pool.begin().await?;
    let plan: PlanFields = sqlx::query_as!(
        PlanFields,
        r#"
        UPDATE plans SET
            display_name = COALESCE($2, display_name),
            default_speed_limit_mbps = CASE WHEN $3 THEN NULL ELSE COALESCE($4, default_speed_limit_mbps) END,
            max_peers = COALESCE($5, max_peers),
            is_public = COALESCE($6, is_public),
            trial_minutes = CASE WHEN $7 THEN NULL ELSE COALESCE($8, trial_minutes) END,
            price_stars = CASE WHEN $9 THEN NULL ELSE COALESCE($10, price_stars) END,
            period_days = CASE WHEN $11 THEN NULL ELSE COALESCE($12, period_days) END
        WHERE id = $1
        RETURNING id, name, display_name, default_speed_limit_mbps, max_peers, is_public, trial_minutes, price_stars, period_days
        "#,
        id,
        req.display_name.as_deref(),
        req.clear_speed_limit,
        req.default_speed_limit_mbps,
        req.max_peers,
        req.is_public,
        req.clear_trial_minutes,
        req.trial_minutes,
        req.clear_price_stars,
        req.price_stars,
        req.clear_period_days,
        req.period_days
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("Plan not found"))?;
    if let Some(region_ids) = &req.region_ids {
        replace_regions(&mut tx, plan.id, region_ids).await?;
    }
    let plan = with_regions(plan, &mut tx).await?;
    tx.commit().await?;
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
        (status = 401, body = ApiError, description = "Unauthorized"),
        (status = 403, body = ApiError, description = "Not an admin"),
        (status = 404, body = ApiError, description = "Plan not found"),
        (status = 409, body = ApiError, description = "Plan has existing subscriptions"),
    )
)]
pub(super) async fn delete_plan(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, ApiError> {
    // Don't allow deleting plans that have subscriptions
    let has_subs = sqlx::query_scalar!("SELECT COUNT(*) FROM subscriptions WHERE plan_id = $1", id)
        .fetch_one(&state.pool)
        .await?;

    if has_subs.unwrap_or(0) > 0 {
        return Err(ApiError::conflict(
            "Plan has existing subscriptions and cannot be deleted",
        ));
    }

    let result = sqlx::query!("DELETE FROM plans WHERE id = $1", id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Plan not found"));
    }

    Ok(StatusCode::NO_CONTENT)
}
