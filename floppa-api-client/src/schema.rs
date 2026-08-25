//! The server's API types, generated from `floppa-web-shared/openapi.json`.
//!
//! DO NOT EDIT. Regenerate with `just openapi`, which rebuilds the OpenAPI document from the
//! server's own annotations and then rewrites this file from it. Editing it by hand reintroduces
//! exactly the drift it exists to prevent.
#![allow(clippy::all)]

/// Error types.
pub mod error {
    /// Error from a `TryFrom` or `FromStr` implementation.
    pub struct ConversionError(::std::borrow::Cow<'static, str>);
    impl ::std::error::Error for ConversionError {}
    impl ::std::fmt::Display for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Display::fmt(&self.0, f)
        }
    }
    impl ::std::fmt::Debug for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Debug::fmt(&self.0, f)
        }
    }
    impl From<&'static str> for ConversionError {
        fn from(value: &'static str) -> Self {
            Self(value.into())
        }
    }
    impl From<String> for ConversionError {
        fn from(value: String) -> Self {
            Self(value.into())
        }
    }
}
///`AccountLoginRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "login",
///    "password"
///  ],
///  "properties": {
///    "login": {
///      "type": "string"
///    },
///    "password": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct AccountLoginRequest {
    pub login: ::std::string::String,
    pub password: ::std::string::String,
}
///`AccountRegisterRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "login",
///    "password"
///  ],
///  "properties": {
///    "login": {
///      "type": "string"
///    },
///    "password": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct AccountRegisterRequest {
    pub login: ::std::string::String,
    pub password: ::std::string::String,
}
/**The JSON error body every failing endpoint returns.

4xx messages are meant for the client and may describe the problem. 5xx messages are fixed
strings: the details (config paths, crypto/DB errors, upstream responses) are logged exactly
once, at the point where the error is mapped, and never sent to the client.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "The JSON error body every failing endpoint returns.\n\n4xx messages are meant for the client and may describe the problem. 5xx messages are fixed\nstrings: the details (config paths, crypto/DB errors, upstream responses) are logged exactly\nonce, at the point where the error is mapped, and never sent to the client.",
///  "type": "object",
///  "required": [
///    "error",
///    "message"
///  ],
///  "properties": {
///    "error": {
///      "type": "string"
///    },
///    "message": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct ApiError {
    pub error: ::std::string::String,
    pub message: ::std::string::String,
}
///`AuthResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "token",
///    "user"
///  ],
///  "properties": {
///    "token": {
///      "type": "string"
///    },
///    "user": {
///      "$ref": "#/definitions/AuthUserInfo"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct AuthResponse {
    pub token: ::std::string::String,
    pub user: AuthUserInfo,
}
/**The user half of an [`AuthResponse`]; also what every login path resolves before a JWT
is signed.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "The user half of an [`AuthResponse`]; also what every login path resolves before a JWT\nis signed.",
///  "type": "object",
///  "required": [
///    "id",
///    "is_admin"
///  ],
///  "properties": {
///    "first_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "is_admin": {
///      "type": "boolean"
///    },
///    "last_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "photo_url": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "telegram_id": {
///      "description": "Linked Telegram account, `None` for credential-only accounts.",
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct AuthUserInfo {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    pub id: i64,
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub photo_url: ::std::option::Option<::std::string::String>,
    ///Linked Telegram account, `None` for credential-only accounts.
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub telegram_id: ::std::option::Option<i64>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`AvatarBatchRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "user_ids"
///  ],
///  "properties": {
///    "user_ids": {
///      "type": "array",
///      "items": {
///        "type": "integer",
///        "format": "int64"
///      }
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct AvatarBatchRequest {
    pub user_ids: ::std::vec::Vec<i64>,
}
///`CreatePeerRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "properties": {
///    "device_id": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "installation_id": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "protocol": {
///      "oneOf": [
///        {
///          "type": "null"
///        },
///        {
///          "description": "Tunnel protocol. Defaults to WireGuard when omitted (pre-AmneziaWG clients).",
///          "$ref": "#/definitions/Protocol"
///        }
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct CreatePeerRequest {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_id: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub installation_id: ::std::option::Option<i64>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub protocol: ::std::option::Option<Protocol>,
}
impl ::std::default::Default for CreatePeerRequest {
    fn default() -> Self {
        Self {
            device_id: Default::default(),
            device_name: Default::default(),
            installation_id: Default::default(),
            protocol: Default::default(),
        }
    }
}
///`CreatePeerResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "assigned_ip",
///    "config",
///    "id"
///  ],
///  "properties": {
///    "assigned_ip": {
///      "type": "string"
///    },
///    "config": {
///      "type": "string"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct CreatePeerResponse {
    pub assigned_ip: ::std::string::String,
    pub config: ::std::string::String,
    pub id: i64,
}
///`CreatePlanRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "display_name",
///    "name"
///  ],
///  "properties": {
///    "default_speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "display_name": {
///      "type": "string"
///    },
///    "is_public": {
///      "type": "boolean"
///    },
///    "max_peers": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "name": {
///      "type": "string"
///    },
///    "period_days": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "price_stars": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "trial_minutes": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct CreatePlanRequest {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub default_speed_limit_mbps: ::std::option::Option<i32>,
    pub display_name: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub is_public: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub max_peers: ::std::option::Option<i32>,
    pub name: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub period_days: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub price_stars: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub trial_minutes: ::std::option::Option<i32>,
}
///`CreateUserRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "plan_id",
///    "telegram_id"
///  ],
///  "properties": {
///    "days": {
///      "description": "Duration in days. Required unless `permanent` is true or the plan has a trial duration.",
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32",
///      "minimum": 0.0
///    },
///    "first_name": {
///      "description": "Display name for the user (shown until they register and Telegram provides real name).",
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "permanent": {
///      "description": "If true, creates a permanent subscription (no expiration date).",
///      "type": "boolean"
///    },
///    "plan_id": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "telegram_id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct CreateUserRequest {
    ///Duration in days. Required unless `permanent` is true or the plan has a trial duration.
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub days: ::std::option::Option<i32>,
    ///Display name for the user (shown until they register and Telegram provides real name).
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    ///If true, creates a permanent subscription (no expiration date).
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub permanent: ::std::option::Option<bool>,
    pub plan_id: i32,
    pub telegram_id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`CreateUserResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "id"
///  ],
///  "properties": {
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct CreateUserResponse {
    pub id: i64,
}
///`ExchangeTelegramLoginCodeRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "code"
///  ],
///  "properties": {
///    "code": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct ExchangeTelegramLoginCodeRequest {
    pub code: ::std::string::String,
}
///`FloppaApi`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "title": "FloppaApi"
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
#[serde(transparent)]
pub struct FloppaApi(pub ::serde_json::Value);
impl ::std::ops::Deref for FloppaApi {
    type Target = ::serde_json::Value;
    fn deref(&self) -> &::serde_json::Value {
        &self.0
    }
}
impl ::std::convert::From<FloppaApi> for ::serde_json::Value {
    fn from(value: FloppaApi) -> Self {
        value.0
    }
}
impl ::std::convert::From<::serde_json::Value> for FloppaApi {
    fn from(value: ::serde_json::Value) -> Self {
        Self(value)
    }
}
///`InstallationResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "created_at",
///    "device_id",
///    "id",
///    "last_seen_at"
///  ],
///  "properties": {
///    "app_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "device_id": {
///      "type": "string"
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_seen_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "platform": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct InstallationResponse {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub app_version: ::std::option::Option<::std::string::String>,
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    pub device_id: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub id: i64,
    pub last_seen_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub platform: ::std::option::Option<::std::string::String>,
}
///`InstallationSummary`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "created_at",
///    "device_id",
///    "has_vless",
///    "has_wg",
///    "id",
///    "last_seen_at",
///    "user_id"
///  ],
///  "properties": {
///    "app_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "device_id": {
///      "type": "string"
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "has_vless": {
///      "type": "boolean"
///    },
///    "has_wg": {
///      "type": "boolean"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_seen_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "platform": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "user_id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct InstallationSummary {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub app_version: ::std::option::Option<::std::string::String>,
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    pub device_id: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub has_vless: bool,
    pub has_wg: bool,
    pub id: i64,
    pub last_seen_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub platform: ::std::option::Option<::std::string::String>,
    pub user_id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`LinkPollResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "linked"
///  ],
///  "properties": {
///    "linked": {
///      "type": "boolean"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct LinkPollResponse {
    pub linked: bool,
}
///`LinkStartResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "code",
///    "deep_link",
///    "expires_at"
///  ],
///  "properties": {
///    "code": {
///      "type": "string"
///    },
///    "deep_link": {
///      "type": "string"
///    },
///    "expires_at": {
///      "type": "string",
///      "format": "date-time"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct LinkStartResponse {
    pub code: ::std::string::String,
    pub deep_link: ::std::string::String,
    pub expires_at: ::chrono::DateTime<::chrono::offset::Utc>,
}
///`MeResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "has_credential",
///    "id",
///    "is_admin",
///    "telegram_linked"
///  ],
///  "properties": {
///    "first_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "has_credential": {
///      "description": "True if the user has set a login+password credential (for the \"set a backup login\" nudge).",
///      "type": "boolean"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "is_admin": {
///      "type": "boolean"
///    },
///    "last_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "photo_url": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "subscription": {
///      "oneOf": [
///        {
///          "type": "null"
///        },
///        {
///          "$ref": "#/definitions/MySubscription"
///        }
///      ]
///    },
///    "telegram_id": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "telegram_linked": {
///      "description": "True if a Telegram account is linked (can pay via Stars, gets bot notifications).",
///      "type": "boolean"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct MeResponse {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    ///True if the user has set a login+password credential (for the "set a backup login" nudge).
    pub has_credential: bool,
    pub id: i64,
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub photo_url: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub subscription: ::std::option::Option<MySubscription>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub telegram_id: ::std::option::Option<i64>,
    ///True if a Telegram account is linked (can pay via Stars, gets bot notifications).
    pub telegram_linked: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`MiniAppAuthRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "init_data"
///  ],
///  "properties": {
///    "init_data": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct MiniAppAuthRequest {
    pub init_data: ::std::string::String,
}
///`MyPeer`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "assigned_ip",
///    "created_at",
///    "download_bytes",
///    "id",
///    "protocol",
///    "sync_status",
///    "upload_bytes"
///  ],
///  "properties": {
///    "assigned_ip": {
///      "type": "string"
///    },
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "device_id": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_handshake": {
///      "type": [
///        "string",
///        "null"
///      ],
///      "format": "date-time"
///    },
///    "protocol": {
///      "$ref": "#/definitions/Protocol"
///    },
///    "sync_status": {
///      "$ref": "#/definitions/PeerSyncStatus"
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct MyPeer {
    pub assigned_ip: ::std::string::String,
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_id: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub download_bytes: i64,
    pub id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_handshake: ::std::option::Option<::chrono::DateTime<::chrono::offset::Utc>>,
    pub protocol: Protocol,
    pub sync_status: PeerSyncStatus,
    pub upload_bytes: i64,
}
///`MyPeersResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "peers",
///    "traffic_available",
///    "wg_download_bytes",
///    "wg_upload_bytes"
///  ],
///  "properties": {
///    "peers": {
///      "type": "array",
///      "items": {
///        "$ref": "#/definitions/MyPeer"
///      }
///    },
///    "traffic_available": {
///      "description": "False when the metrics backend could not be queried: every byte counter in this\nresponse is then a placeholder zero, not a measurement.",
///      "type": "boolean"
///    },
///    "vless": {
///      "oneOf": [
///        {
///          "type": "null"
///        },
///        {
///          "description": "VLESS info (None if VLESS not configured on server)",
///          "$ref": "#/definitions/VlessInfo"
///        }
///      ]
///    },
///    "wg_download_bytes": {
///      "description": "Total WG traffic for this user (includes removed peers), last 30 days.",
///      "type": "integer",
///      "format": "int64"
///    },
///    "wg_upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct MyPeersResponse {
    pub peers: ::std::vec::Vec<MyPeer>,
    /**False when the metrics backend could not be queried: every byte counter in this
    response is then a placeholder zero, not a measurement.*/
    pub traffic_available: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub vless: ::std::option::Option<VlessInfo>,
    ///Total WG traffic for this user (includes removed peers), last 30 days.
    pub wg_download_bytes: i64,
    pub wg_upload_bytes: i64,
}
///`MySubscription`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "max_peers",
///    "plan_display_name",
///    "plan_name",
///    "source",
///    "starts_at"
///  ],
///  "properties": {
///    "expires_at": {
///      "type": [
///        "string",
///        "null"
///      ],
///      "format": "date-time"
///    },
///    "max_peers": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "plan_display_name": {
///      "type": "string"
///    },
///    "plan_name": {
///      "type": "string"
///    },
///    "source": {
///      "$ref": "#/definitions/SubscriptionSource"
///    },
///    "speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "starts_at": {
///      "type": "string",
///      "format": "date-time"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct MySubscription {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub expires_at: ::std::option::Option<::chrono::DateTime<::chrono::offset::Utc>>,
    pub max_peers: i32,
    pub plan_display_name: ::std::string::String,
    pub plan_name: ::std::string::String,
    pub source: SubscriptionSource,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub speed_limit_mbps: ::std::option::Option<i32>,
    pub starts_at: ::chrono::DateTime<::chrono::offset::Utc>,
}
///`PeerDetail`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "assigned_ip",
///    "download_bytes",
///    "id",
///    "protocol",
///    "public_key",
///    "sync_status",
///    "upload_bytes"
///  ],
///  "properties": {
///    "assigned_ip": {
///      "type": "string"
///    },
///    "device_id": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_handshake": {
///      "type": [
///        "string",
///        "null"
///      ],
///      "format": "date-time"
///    },
///    "protocol": {
///      "$ref": "#/definitions/Protocol"
///    },
///    "public_key": {
///      "type": "string"
///    },
///    "sync_status": {
///      "$ref": "#/definitions/PeerSyncStatus"
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct PeerDetail {
    pub assigned_ip: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_id: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub download_bytes: i64,
    pub id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_handshake: ::std::option::Option<::chrono::DateTime<::chrono::offset::Utc>>,
    pub protocol: Protocol,
    pub public_key: ::std::string::String,
    pub sync_status: PeerSyncStatus,
    pub upload_bytes: i64,
}
///`PeerSummary`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "assigned_ip",
///    "download_bytes",
///    "has_vless",
///    "id",
///    "protocol",
///    "sync_status",
///    "upload_bytes",
///    "user_id"
///  ],
///  "properties": {
///    "assigned_ip": {
///      "type": "string"
///    },
///    "client_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_id": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "has_vless": {
///      "type": "boolean"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_handshake": {
///      "type": [
///        "string",
///        "null"
///      ],
///      "format": "date-time"
///    },
///    "plan_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "protocol": {
///      "$ref": "#/definitions/Protocol"
///    },
///    "sync_status": {
///      "$ref": "#/definitions/PeerSyncStatus"
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "user_id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct PeerSummary {
    pub assigned_ip: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub client_version: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_id: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub download_bytes: i64,
    pub has_vless: bool,
    pub id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_handshake: ::std::option::Option<::chrono::DateTime<::chrono::offset::Utc>>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub plan_name: ::std::option::Option<::std::string::String>,
    pub protocol: Protocol,
    pub sync_status: PeerSyncStatus,
    pub upload_bytes: i64,
    pub user_id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
/**Peer synchronization status with WireGuard interface.

Stored in `peers.sync_status` (TEXT, CHECK-constrained by migration 0014); bind it in
`query!` macros as `$n` with `PeerSyncStatus::Active as _`.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Peer synchronization status with WireGuard interface.\n\nStored in `peers.sync_status` (TEXT, CHECK-constrained by migration 0014); bind it in\n`query!` macros as `$n` with `PeerSyncStatus::Active as _`.",
///  "type": "string",
///  "enum": [
///    "pending_add",
///    "active",
///    "pending_remove",
///    "removed"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum PeerSyncStatus {
    #[serde(rename = "pending_add")]
    PendingAdd,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "pending_remove")]
    PendingRemove,
    #[serde(rename = "removed")]
    Removed,
}
impl ::std::fmt::Display for PeerSyncStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::PendingAdd => f.write_str("pending_add"),
            Self::Active => f.write_str("active"),
            Self::PendingRemove => f.write_str("pending_remove"),
            Self::Removed => f.write_str("removed"),
        }
    }
}
impl ::std::str::FromStr for PeerSyncStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "pending_add" => Ok(Self::PendingAdd),
            "active" => Ok(Self::Active),
            "pending_remove" => Ok(Self::PendingRemove),
            "removed" => Ok(Self::Removed),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for PeerSyncStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for PeerSyncStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for PeerSyncStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///`Plan`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "display_name",
///    "id",
///    "is_public",
///    "max_peers",
///    "name"
///  ],
///  "properties": {
///    "default_speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "display_name": {
///      "type": "string"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "is_public": {
///      "type": "boolean"
///    },
///    "max_peers": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "name": {
///      "type": "string"
///    },
///    "period_days": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "price_stars": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "trial_minutes": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub default_speed_limit_mbps: ::std::option::Option<i32>,
    pub display_name: ::std::string::String,
    pub id: i32,
    pub is_public: bool,
    pub max_peers: i32,
    pub name: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub period_days: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub price_stars: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub trial_minutes: ::std::option::Option<i32>,
}
/**VPN tunnel protocol. WireGuard and AmneziaWG share the peers table (keypair + IP);
AmneziaWG adds interface-wide obfuscation and runs on its own server interface.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "VPN tunnel protocol. WireGuard and AmneziaWG share the peers table (keypair + IP);\nAmneziaWG adds interface-wide obfuscation and runs on its own server interface.",
///  "type": "string",
///  "enum": [
///    "wireguard",
///    "amneziawg"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum Protocol {
    #[serde(rename = "wireguard")]
    Wireguard,
    #[serde(rename = "amneziawg")]
    Amneziawg,
}
impl ::std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Wireguard => f.write_str("wireguard"),
            Self::Amneziawg => f.write_str("amneziawg"),
        }
    }
}
impl ::std::str::FromStr for Protocol {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "wireguard" => Ok(Self::Wireguard),
            "amneziawg" => Ok(Self::Amneziawg),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for Protocol {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for Protocol {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for Protocol {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///`PublicConfig`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "amneziawg_available",
///    "vless_available"
///  ],
///  "properties": {
///    "amneziawg_available": {
///      "description": "Whether AmneziaWG is offered by this server (the client defaults to it when available).",
///      "type": "boolean"
///    },
///    "telegram_bot_username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "vless_available": {
///      "description": "Whether VLESS+REALITY is offered by this server.",
///      "type": "boolean"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct PublicConfig {
    ///Whether AmneziaWG is offered by this server (the client defaults to it when available).
    pub amneziawg_available: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub telegram_bot_username: ::std::option::Option<::std::string::String>,
    ///Whether VLESS+REALITY is offered by this server.
    pub vless_available: bool,
}
/**Public-facing view of a plan (no internal `name`/`is_public`). Served unauthenticated
to the landing page and Info tab so users can see pricing without logging in.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Public-facing view of a plan (no internal `name`/`is_public`). Served unauthenticated\nto the landing page and Info tab so users can see pricing without logging in.",
///  "type": "object",
///  "required": [
///    "display_name",
///    "id",
///    "max_peers"
///  ],
///  "properties": {
///    "default_speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "display_name": {
///      "type": "string"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "max_peers": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "period_days": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "price_stars": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "trial_minutes": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct PublicPlan {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub default_speed_limit_mbps: ::std::option::Option<i32>,
    pub display_name: ::std::string::String,
    pub id: i32,
    pub max_peers: i32,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub period_days: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub price_stars: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub trial_minutes: ::std::option::Option<i32>,
}
///One live login of a user, as shown in "Devices & sessions".
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "One live login of a user, as shown in \"Devices & sessions\".",
///  "type": "object",
///  "required": [
///    "created_at",
///    "current",
///    "id",
///    "kind",
///    "last_seen_at"
///  ],
///  "properties": {
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "current": {
///      "description": "True for the session the request itself was made with.",
///      "type": "boolean"
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "id": {
///      "type": "string",
///      "format": "uuid"
///    },
///    "installation_id": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "kind": {
///      "description": "Which login path opened it.",
///      "$ref": "#/definitions/SessionKind"
///    },
///    "label": {
///      "description": "Device description recorded when the app registered its installation on this session.",
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "last_seen_at": {
///      "description": "Bumped by authenticated requests, at most once an hour.",
///      "type": "string",
///      "format": "date-time"
///    },
///    "platform": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    ///True for the session the request itself was made with.
    pub current: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub id: ::uuid::Uuid,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub installation_id: ::std::option::Option<i64>,
    ///Which login path opened it.
    pub kind: SessionKind,
    ///Device description recorded when the app registered its installation on this session.
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub label: ::std::option::Option<::std::string::String>,
    ///Bumped by authenticated requests, at most once an hour.
    pub last_seen_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub platform: ::std::option::Option<::std::string::String>,
}
/**Which login path minted an API session; `sessions.kind` (TEXT, CHECK-constrained by
migration 0018). Bind as `SessionKind::DeepLink as _`.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Which login path minted an API session; `sessions.kind` (TEXT, CHECK-constrained by\nmigration 0018). Bind as `SessionKind::DeepLink as _`.",
///  "type": "string",
///  "enum": [
///    "telegram_widget",
///    "mini_app",
///    "deep_link",
///    "credential",
///    "legacy"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum SessionKind {
    #[serde(rename = "telegram_widget")]
    TelegramWidget,
    #[serde(rename = "mini_app")]
    MiniApp,
    #[serde(rename = "deep_link")]
    DeepLink,
    #[serde(rename = "credential")]
    Credential,
    #[serde(rename = "legacy")]
    Legacy,
}
impl ::std::fmt::Display for SessionKind {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::TelegramWidget => f.write_str("telegram_widget"),
            Self::MiniApp => f.write_str("mini_app"),
            Self::DeepLink => f.write_str("deep_link"),
            Self::Credential => f.write_str("credential"),
            Self::Legacy => f.write_str("legacy"),
        }
    }
}
impl ::std::str::FromStr for SessionKind {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "telegram_widget" => Ok(Self::TelegramWidget),
            "mini_app" => Ok(Self::MiniApp),
            "deep_link" => Ok(Self::DeepLink),
            "credential" => Ok(Self::Credential),
            "legacy" => Ok(Self::Legacy),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for SessionKind {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for SessionKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for SessionKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///`SetCredentialRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "login",
///    "password"
///  ],
///  "properties": {
///    "login": {
///      "type": "string"
///    },
///    "password": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct SetCredentialRequest {
    pub login: ::std::string::String,
    pub password: ::std::string::String,
}
///`SetSubscriptionRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "plan_id"
///  ],
///  "properties": {
///    "days": {
///      "description": "Duration in days. If omitted, uses the plan's trial duration (for trial plans).\nUse `permanent: true` to create a subscription with no expiration.",
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32",
///      "minimum": 0.0
///    },
///    "permanent": {
///      "description": "If true, creates a permanent subscription (no expiration date).",
///      "type": "boolean"
///    },
///    "plan_id": {
///      "type": "integer",
///      "format": "int32"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct SetSubscriptionRequest {
    /**Duration in days. If omitted, uses the plan's trial duration (for trial plans).
    Use `permanent: true` to create a subscription with no expiration.*/
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub days: ::std::option::Option<i32>,
    ///If true, creates a permanent subscription (no expiration date).
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub permanent: ::std::option::Option<bool>,
    pub plan_id: i32,
}
///`SetUserCredentialRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "login",
///    "password"
///  ],
///  "properties": {
///    "login": {
///      "type": "string"
///    },
///    "password": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct SetUserCredentialRequest {
    pub login: ::std::string::String,
    pub password: ::std::string::String,
}
///`Stats`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "active_peers",
///    "active_subscriptions",
///    "total_download_bytes",
///    "total_payments",
///    "total_stars_revenue",
///    "total_upload_bytes",
///    "total_users",
///    "traffic_available"
///  ],
///  "properties": {
///    "active_peers": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "active_subscriptions": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "total_download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "total_payments": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "total_stars_revenue": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "total_upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "total_users": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "traffic_available": {
///      "description": "False when the metrics backend could not be queried (byte counters are then zero).",
///      "type": "boolean"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct Stats {
    pub active_peers: i64,
    pub active_subscriptions: i64,
    pub total_download_bytes: i64,
    pub total_payments: i64,
    pub total_stars_revenue: i64,
    pub total_upload_bytes: i64,
    pub total_users: i64,
    ///False when the metrics backend could not be queried (byte counters are then zero).
    pub traffic_available: bool,
}
///`SubscriptionDetail`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "id",
///    "is_active",
///    "max_peers",
///    "plan_display_name",
///    "plan_id",
///    "plan_name",
///    "source",
///    "starts_at"
///  ],
///  "properties": {
///    "expires_at": {
///      "type": [
///        "string",
///        "null"
///      ],
///      "format": "date-time"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "is_active": {
///      "type": "boolean"
///    },
///    "max_peers": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "plan_display_name": {
///      "type": "string"
///    },
///    "plan_id": {
///      "type": "integer",
///      "format": "int32"
///    },
///    "plan_name": {
///      "type": "string"
///    },
///    "source": {
///      "$ref": "#/definitions/SubscriptionSource"
///    },
///    "speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "starts_at": {
///      "type": "string",
///      "format": "date-time"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct SubscriptionDetail {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub expires_at: ::std::option::Option<::chrono::DateTime<::chrono::offset::Utc>>,
    pub id: i64,
    pub is_active: bool,
    pub max_peers: i32,
    pub plan_display_name: ::std::string::String,
    pub plan_id: i32,
    pub plan_name: ::std::string::String,
    pub source: SubscriptionSource,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub speed_limit_mbps: ::std::option::Option<i32>,
    pub starts_at: ::chrono::DateTime<::chrono::offset::Utc>,
}
/**How a subscription came to exist. Stored in `subscriptions.source` (TEXT, CHECK-constrained
by migration 0014); bind it in `query!` macros as `$n` with `SubscriptionSource::Trial as _`.*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "How a subscription came to exist. Stored in `subscriptions.source` (TEXT, CHECK-constrained\nby migration 0014); bind it in `query!` macros as `$n` with `SubscriptionSource::Trial as _`.",
///  "type": "string",
///  "enum": [
///    "trial",
///    "taster",
///    "purchase",
///    "admin_grant"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum SubscriptionSource {
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "taster")]
    Taster,
    #[serde(rename = "purchase")]
    Purchase,
    #[serde(rename = "admin_grant")]
    AdminGrant,
}
impl ::std::fmt::Display for SubscriptionSource {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Trial => f.write_str("trial"),
            Self::Taster => f.write_str("taster"),
            Self::Purchase => f.write_str("purchase"),
            Self::AdminGrant => f.write_str("admin_grant"),
        }
    }
}
impl ::std::str::FromStr for SubscriptionSource {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "trial" => Ok(Self::Trial),
            "taster" => Ok(Self::Taster),
            "purchase" => Ok(Self::Purchase),
            "admin_grant" => Ok(Self::AdminGrant),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for SubscriptionSource {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for SubscriptionSource {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for SubscriptionSource {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///Data received from Telegram Login Widget
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Data received from Telegram Login Widget",
///  "type": "object",
///  "required": [
///    "auth_date",
///    "hash",
///    "id"
///  ],
///  "properties": {
///    "auth_date": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "first_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "hash": {
///      "type": "string"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "last_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "photo_url": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct TelegramAuthData {
    pub auth_date: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    pub hash: ::std::string::String,
    pub id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub photo_url: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`UpdatePlanRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "properties": {
///    "clear_period_days": {
///      "type": "boolean"
///    },
///    "clear_price_stars": {
///      "type": "boolean"
///    },
///    "clear_speed_limit": {
///      "type": "boolean"
///    },
///    "clear_trial_minutes": {
///      "type": "boolean"
///    },
///    "default_speed_limit_mbps": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "display_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "is_public": {
///      "type": [
///        "boolean",
///        "null"
///      ]
///    },
///    "max_peers": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "period_days": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "price_stars": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    },
///    "trial_minutes": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int32"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct UpdatePlanRequest {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub clear_period_days: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub clear_price_stars: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub clear_speed_limit: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub clear_trial_minutes: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub default_speed_limit_mbps: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub display_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub is_public: ::std::option::Option<bool>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub max_peers: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub period_days: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub price_stars: ::std::option::Option<i32>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub trial_minutes: ::std::option::Option<i32>,
}
impl ::std::default::Default for UpdatePlanRequest {
    fn default() -> Self {
        Self {
            clear_period_days: Default::default(),
            clear_price_stars: Default::default(),
            clear_speed_limit: Default::default(),
            clear_trial_minutes: Default::default(),
            default_speed_limit_mbps: Default::default(),
            display_name: Default::default(),
            is_public: Default::default(),
            max_peers: Default::default(),
            period_days: Default::default(),
            price_stars: Default::default(),
            trial_minutes: Default::default(),
        }
    }
}
///`UpsertInstallationRequest`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "device_id"
///  ],
///  "properties": {
///    "app_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_id": {
///      "type": "string"
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "platform": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct UpsertInstallationRequest {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub app_version: ::std::option::Option<::std::string::String>,
    pub device_id: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub platform: ::std::option::Option<::std::string::String>,
}
///`UserDetail`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "created_at",
///    "id",
///    "is_admin",
///    "peers",
///    "subscriptions",
///    "traffic_available",
///    "wg_download_bytes",
///    "wg_upload_bytes"
///  ],
///  "properties": {
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "first_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "is_admin": {
///      "type": "boolean"
///    },
///    "last_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "peers": {
///      "type": "array",
///      "items": {
///        "$ref": "#/definitions/PeerDetail"
///      }
///    },
///    "photo_url": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "subscriptions": {
///      "type": "array",
///      "items": {
///        "$ref": "#/definitions/SubscriptionDetail"
///      }
///    },
///    "telegram_id": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "traffic_available": {
///      "description": "False when the metrics backend could not be queried: every byte counter in this\nresponse is then a placeholder zero, not a measurement.",
///      "type": "boolean"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "vless": {
///      "oneOf": [
///        {
///          "type": "null"
///        },
///        {
///          "$ref": "#/definitions/VlessAdminInfo"
///        }
///      ]
///    },
///    "wg_download_bytes": {
///      "description": "Total WG traffic for this user (includes removed peers), last 30 days.",
///      "type": "integer",
///      "format": "int64"
///    },
///    "wg_upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct UserDetail {
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    pub id: i64,
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_name: ::std::option::Option<::std::string::String>,
    pub peers: ::std::vec::Vec<PeerDetail>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub photo_url: ::std::option::Option<::std::string::String>,
    pub subscriptions: ::std::vec::Vec<SubscriptionDetail>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub telegram_id: ::std::option::Option<i64>,
    /**False when the metrics backend could not be queried: every byte counter in this
    response is then a placeholder zero, not a measurement.*/
    pub traffic_available: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub vless: ::std::option::Option<VlessAdminInfo>,
    ///Total WG traffic for this user (includes removed peers), last 30 days.
    pub wg_download_bytes: i64,
    pub wg_upload_bytes: i64,
}
///`UserSummary`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "created_at",
///    "has_vless",
///    "id",
///    "is_admin",
///    "peer_count"
///  ],
///  "properties": {
///    "active_plan": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "client_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "created_at": {
///      "type": "string",
///      "format": "date-time"
///    },
///    "first_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "has_vless": {
///      "type": "boolean"
///    },
///    "id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "is_admin": {
///      "type": "boolean"
///    },
///    "last_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "peer_count": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "photo_url": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "telegram_id": {
///      "type": [
///        "integer",
///        "null"
///      ],
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct UserSummary {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub active_plan: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub client_version: ::std::option::Option<::std::string::String>,
    pub created_at: ::chrono::DateTime<::chrono::offset::Utc>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub first_name: ::std::option::Option<::std::string::String>,
    pub has_vless: bool,
    pub id: i64,
    pub is_admin: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub last_name: ::std::option::Option<::std::string::String>,
    pub peer_count: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub photo_url: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub telegram_id: ::std::option::Option<i64>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
///`VersionInfo`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "build_time",
///    "git_hash",
///    "version"
///  ],
///  "properties": {
///    "build_time": {
///      "type": "string"
///    },
///    "git_hash": {
///      "type": "string"
///    },
///    "version": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct VersionInfo {
    pub build_time: ::std::string::String,
    pub git_hash: ::std::string::String,
    pub version: ::std::string::String,
}
///`VlessAdminInfo`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "download_bytes",
///    "has_uuid",
///    "upload_bytes"
///  ],
///  "properties": {
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "has_uuid": {
///      "type": "boolean"
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct VlessAdminInfo {
    pub download_bytes: i64,
    pub has_uuid: bool,
    pub upload_bytes: i64,
}
///`VlessConfigResponse`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "uri"
///  ],
///  "properties": {
///    "uri": {
///      "type": "string"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct VlessConfigResponse {
    pub uri: ::std::string::String,
}
///`VlessInfo`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "download_bytes",
///    "has_uuid",
///    "upload_bytes"
///  ],
///  "properties": {
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "has_uuid": {
///      "description": "Whether the user has generated a VLESS UUID",
///      "type": "boolean"
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct VlessInfo {
    pub download_bytes: i64,
    ///Whether the user has generated a VLESS UUID
    pub has_uuid: bool,
    pub upload_bytes: i64,
}
///`VlessPeerSummary`
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "type": "object",
///  "required": [
///    "download_bytes",
///    "has_wg",
///    "upload_bytes",
///    "user_id"
///  ],
///  "properties": {
///    "app_version": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "device_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "download_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "has_wg": {
///      "type": "boolean"
///    },
///    "plan_name": {
///      "type": [
///        "string",
///        "null"
///      ]
///    },
///    "upload_bytes": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "user_id": {
///      "type": "integer",
///      "format": "int64"
///    },
///    "username": {
///      "type": [
///        "string",
///        "null"
///      ]
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, PartialEq)]
pub struct VlessPeerSummary {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub app_version: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub device_name: ::std::option::Option<::std::string::String>,
    pub download_bytes: i64,
    pub has_wg: bool,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub plan_name: ::std::option::Option<::std::string::String>,
    pub upload_bytes: i64,
    pub user_id: i64,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub username: ::std::option::Option<::std::string::String>,
}
