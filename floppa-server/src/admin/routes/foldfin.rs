use super::AppState;
use crate::admin::{auth::AuthUser, error::ApiError};
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
};
use floppa_core::foldfin::{self, DeviceStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueRequest {
    /// Explicitly invalidate the old credential. A lost response requires rotation.
    #[serde(default)]
    rotate: bool,
}
#[derive(Serialize, ToSchema)]
pub struct IssueResponse {
    device_id: i64,
    revision: i64,
    /// Foldfin v2 document, including a secret returned only by this issuance call.
    config: serde_json::Value,
}

#[utoipa::path(post, path="/me/foldfin/config", tag="user", security(("bearer"=[])),
    request_body=IssueRequest,
    responses((status=201, body=IssueResponse), (status=409, body=ApiError), (status=401, body=ApiError), (status=402, body=ApiError), (status=403, body=ApiError), (status=503, body=ApiError)))]
pub(super) async fn issue(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(request): Json<IssueRequest>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let session = auth.session_id.ok_or_else(|| {
        ApiError::bad_request("Register this installation on a live API session first")
    })?;
    let result = foldfin::issue(&state.pool, auth.user_id, session, request.rotate).await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(IssueResponse {
            device_id: result.device_id,
            revision: result.revision,
            config: result.config,
        }),
    ))
}

#[utoipa::path(get, path="/me/foldfin/devices", tag="user", security(("bearer"=[])),
    responses((status=200, body=Vec<DeviceStatus>), (status=401, body=ApiError)))]
pub(super) async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<DeviceStatus>>, ApiError> {
    Ok(Json(foldfin::list(&state.pool, auth.user_id).await?))
}

#[utoipa::path(delete, path="/me/foldfin/devices/{id}", tag="user", security(("bearer"=[])),
    params(("id"=i64, Path, description="Foldfin device allocation")),
    responses((status=204), (status=404, body=ApiError), (status=401, body=ApiError)))]
pub(super) async fn revoke(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if !foldfin::revoke(&state.pool, auth.user_id, id).await? {
        return Err(ApiError::not_found("Foldfin device not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
