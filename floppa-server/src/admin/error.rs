use std::fmt::Display;

use axum::{Json, http::StatusCode, response::IntoResponse};
use floppa_core::{FloppaError, services::SubscriptionTermError};
use serde::Serialize;
use tracing::error;
use utoipa::ToSchema;

/// The JSON error body every failing endpoint returns.
///
/// 4xx messages are meant for the client and may describe the problem. 5xx messages are fixed
/// strings: the details (config paths, crypto/DB errors, upstream responses) are logged exactly
/// once, at the point where the error is mapped, and never sent to the client.
#[derive(Serialize, ToSchema)]
pub struct ApiError {
    pub error: String,
    pub message: String,
    #[serde(skip)]
    status: StatusCode,
}

const INTERNAL_ERROR_MESSAGE: &str = "Internal server error";
const BAD_GATEWAY_MESSAGE: &str = "An upstream service failed";

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
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
        Self::server_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Internal database error",
            e,
        )
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
            FloppaError::InvalidInstallation(id) => Self {
                error: "invalid_installation".into(),
                message: format!("Installation not found: id={id}"),
                status: StatusCode::NOT_FOUND,
            },
            FloppaError::PeerAlreadyExists {
                installation_id,
                protocol,
            } => Self {
                error: "peer_already_exists".into(),
                message: format!(
                    "An active {protocol} peer already exists for installation {installation_id}"
                ),
                status: StatusCode::CONFLICT,
            },
            FloppaError::NoAvailableIps => Self::server_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "no_available_ips",
                "No available IPs in subnet",
                e,
            ),
            FloppaError::Database(e) => Self::from(e),
            FloppaError::VlessNotConfigured => Self {
                error: "vless_not_configured".into(),
                message: "VLESS is not configured on this server".into(),
                status: StatusCode::BAD_REQUEST,
            },
            FloppaError::AmneziaWgNotConfigured => Self {
                error: "amneziawg_not_configured".into(),
                message: "AmneziaWG is not configured on this server".into(),
                status: StatusCode::BAD_REQUEST,
            },
            FloppaError::CredentialTaken => Self {
                error: "login_taken".into(),
                message: "This login is already taken".into(),
                status: StatusCode::CONFLICT,
            },
            FloppaError::InvalidCredentials => Self {
                error: "invalid_credentials".into(),
                message: "Invalid login or password".into(),
                status: StatusCode::UNAUTHORIZED,
            },
            FloppaError::InvalidLogin(msg) => Self {
                error: "invalid_login".into(),
                message: msg,
                status: StatusCode::BAD_REQUEST,
            },
            FloppaError::InvalidPassword(reason) => Self {
                error: "invalid_password".into(),
                message: reason.to_string(),
                status: StatusCode::BAD_REQUEST,
            },
            FloppaError::Crypto(_)
            | FloppaError::Key(_)
            | FloppaError::PasswordHash(_)
            | FloppaError::BlockingTask(_)
            | FloppaError::MalformedUuid(_)
            | FloppaError::Config(_) => Self::internal(e),
        }
    }
}

impl From<SubscriptionTermError> for ApiError {
    fn from(e: SubscriptionTermError) -> Self {
        match e {
            SubscriptionTermError::PlanNotFound(_) => Self::not_found("Plan not found"),
            SubscriptionTermError::NoDuration => {
                Self::bad_request("Days not specified and plan has no trial duration")
            }
            SubscriptionTermError::DurationOutOfRange => Self::bad_request("Duration is too long"),
            SubscriptionTermError::Database(e) => Self::from(e),
        }
    }
}

impl ApiError {
    /// A 5xx: logs `detail` here — the single place a server error is logged — and answers with
    /// the fixed `message`.
    fn server_error(
        status: StatusCode,
        error: &'static str,
        message: &'static str,
        detail: impl Display,
    ) -> Self {
        error!(status = status.as_u16(), error, "{detail}");
        crate::metrics::server_error(error);
        Self {
            error: error.into(),
            message: message.into(),
            status,
        }
    }

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

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            error: "forbidden".into(),
            message: msg.into(),
            status: StatusCode::FORBIDDEN,
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            error: "conflict".into(),
            message: msg.into(),
            status: StatusCode::CONFLICT,
        }
    }

    /// 500. `detail` is logged, not returned.
    pub fn internal(detail: impl Display) -> Self {
        Self::server_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            INTERNAL_ERROR_MESSAGE,
            detail,
        )
    }

    /// 502. `detail` is logged, not returned.
    pub fn bad_gateway(detail: impl Display) -> Self {
        Self::server_error(
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
            BAD_GATEWAY_MESSAGE,
            detail,
        )
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self {
            error: "too_many_requests".into(),
            message: msg.into(),
            status: StatusCode::TOO_MANY_REQUESTS,
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_never_echo_their_detail() {
        let e = ApiError::internal("secret: /etc/floppa-vpn/secrets.toml");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.message, INTERNAL_ERROR_MESSAGE);

        let e = ApiError::bad_gateway("telegram said no");
        assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(e.message, BAD_GATEWAY_MESSAGE);

        let e = ApiError::from(FloppaError::Config(
            floppa_core::config::ConfigError::InvalidKey("dump of the key".into()),
        ));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.message.contains("dump of the key"));
    }

    #[test]
    fn client_errors_keep_their_message() {
        let e = ApiError::from(FloppaError::PeerLimitReached { current: 3, max: 3 });
        assert_eq!(e.status(), StatusCode::FORBIDDEN);
        assert_eq!(e.message, "Peer limit reached: 3/3");
    }
}
