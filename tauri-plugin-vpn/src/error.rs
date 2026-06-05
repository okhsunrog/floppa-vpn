use serde::{Serialize, Serializer};

/// Plugin error types.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("VPN permission not granted")]
    PermissionDenied,

    #[error("VPN service not prepared. Call prepare_vpn first.")]
    NotPrepared,

    #[error("VPN is already running")]
    AlreadyRunning,

    #[error("VPN is not running")]
    NotRunning,

    #[error("Failed to establish VPN tunnel: {0}")]
    EstablishFailed(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Platform error: {0}")]
    Platform(String),

    #[error(transparent)]
    Tauri(#[from] tauri::Error),

    #[cfg(mobile)]
    #[error("Plugin invoke error: {0}")]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
}

impl Error {
    /// Stable machine-readable code for this error category. Mirrors the codes the
    /// Kotlin side passes to `invoke.reject(msg, code)` so the two stay in sync.
    pub fn code(&self) -> &'static str {
        match self {
            Error::PermissionDenied => "permission_denied",
            Error::NotPrepared => "not_prepared",
            Error::AlreadyRunning => "already_running",
            Error::NotRunning => "not_running",
            Error::EstablishFailed(_) => "establish_failed",
            Error::InvalidConfig(_) => "invalid_config",
            Error::Platform(_) => "platform",
            Error::Tauri(_) => "tauri",
            #[cfg(mobile)]
            Error::PluginInvoke(_) => "plugin_invoke",
        }
    }

    /// Map a Kotlin `invoke.reject(msg, code)` into a typed variant by its code,
    /// falling back to the opaque `PluginInvoke` for unknown/codeless rejects.
    #[cfg(mobile)]
    pub fn from_invoke(e: tauri::plugin::mobile::PluginInvokeError) -> Self {
        use tauri::plugin::mobile::PluginInvokeError;
        if let PluginInvokeError::InvokeRejected(resp) = &e {
            let msg = resp.message.clone().unwrap_or_default();
            match resp.code.as_deref() {
                Some("permission_denied") => return Error::PermissionDenied,
                Some("not_prepared") => return Error::NotPrepared,
                Some("already_running") => return Error::AlreadyRunning,
                Some("not_running") => return Error::NotRunning,
                Some("establish_failed") => return Error::EstablishFailed(msg),
                Some("invalid_config") => return Error::InvalidConfig(msg),
                Some("platform") => return Error::Platform(msg),
                _ => {}
            }
        }
        Error::PluginInvoke(e)
    }
}

impl Serialize for Error {
    /// Serialize as `{ code, message }` (like the client's `ConnectError`) so a
    /// consumer can branch on `code` instead of string-matching the message.
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Error", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type Result<T> = std::result::Result<T, Error>;
