use serde::{Serialize, Serializer};

/// What a call into the plugin can fail with.
///
/// Only two things fail here: registering the Android plugin class, and the Kotlin side rejecting
/// or not answering an invoke. Kotlin reports by message, not by code, so the variants this enum
/// used to carry — "permission denied", "not prepared", "already running", "invalid config" and so
/// on — were never constructed anywhere; every failure arrived as a plugin-invoke error and the
/// typed variants only suggested a precision that did not exist.
///
/// Both variants exist only on Android; elsewhere nothing here can fail and the enum is empty.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Kotlin plugin class could not be registered with the Tauri runtime.
    #[cfg(target_os = "android")]
    #[error("failed to register the Android VPN plugin: {0}")]
    Register(tauri::plugin::mobile::PluginInvokeError),

    /// The Kotlin side rejected the command, or the invoke never completed.
    #[cfg(target_os = "android")]
    #[error("plugin command `{command}` failed: {source}")]
    PluginInvoke {
        command: &'static str,
        #[source]
        source: tauri::plugin::mobile::PluginInvokeError,
    },
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
