use axum::{Json, http::StatusCode, response::IntoResponse};
use floppa_core::FloppaError;
use serde::Serialize;
use tracing::error;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct ApiError {
    pub error: String,
    pub message: String,
    #[serde(skip)]
    status: StatusCode,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        if self.status.is_server_error() {
            error!("{}: {}", self.error, self.message);
        }
        (
            self.status,
            Json(serde_json::json!({
                "error": self.error,
                "message": self.message,
            })),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self {
            error: "database_error".into(),
            message: format!("Database error: {e}"),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<FloppaError> for ApiError {
    fn from(e: FloppaError) -> Self {
        match e {
            FloppaError::NoActiveSubscription => Self {
                error: "no_active_subscription".into(),
                message: "No active subscription".into(),
                status: StatusCode::PAYMENT_REQUIRED,
            },
            FloppaError::PeerLimitReached { current, max } => Self {
                error: "peer_limit_reached".into(),
                message: format!("Peer limit reached: {current}/{max}"),
                status: StatusCode::FORBIDDEN,
            },
            FloppaError::PeerNotFound(id) => Self {
                error: "peer_not_found".into(),
                message: format!("Peer not found: id={id}"),
                status: StatusCode::NOT_FOUND,
            },
            FloppaError::UserNotFound(id) => Self {
                error: "user_not_found".into(),
                message: format!("User not found: telegram_id={id}"),
                status: StatusCode::NOT_FOUND,
            },
            FloppaError::NoAvailableIps => Self {
                error: "no_available_ips".into(),
                message: "No available IPs in subnet".into(),
                status: StatusCode::INTERNAL_SERVER_ERROR,
            },
            FloppaError::Database(e) => Self::from(e),
            FloppaError::SubscriptionExpired => Self {
                error: "subscription_expired".into(),
                message: "Subscription expired".into(),
                status: StatusCode::PAYMENT_REQUIRED,
            },
            FloppaError::Encryption(_)
            | FloppaError::KeyGeneration(_)
            | FloppaError::WireGuard(_)
            | FloppaError::Config(_) => Self {
                error: "internal_error".into(),
                message: format!("{e}"),
                status: StatusCode::INTERNAL_SERVER_ERROR,
            },
        }
    }
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            error: "not_found".into(),
            message: msg.into(),
            status: StatusCode::NOT_FOUND,
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            error: "bad_request".into(),
            message: msg.into(),
            status: StatusCode::BAD_REQUEST,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            error: "unauthorized".into(),
            message: "Unauthorized".into(),
            status: StatusCode::UNAUTHORIZED,
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            error: "conflict".into(),
            message: msg.into(),
            status: StatusCode::CONFLICT,
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            error: "internal_error".into(),
            message: msg.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            error: "bad_gateway".into(),
            message: msg.into(),
            status: StatusCode::BAD_GATEWAY,
        }
    }
}
