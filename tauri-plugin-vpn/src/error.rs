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

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
