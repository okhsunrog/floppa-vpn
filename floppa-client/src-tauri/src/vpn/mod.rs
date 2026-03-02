pub mod backend;
pub mod commands;
pub mod config;
pub mod platform;
pub mod protocol;
#[cfg(target_os = "android")]
pub mod rpc;
#[cfg(target_os = "android")]
pub mod rpc_server;
pub mod state;
pub mod tunnel;
#[cfg(target_os = "android")]
pub mod jni_entry;

pub use backend::{VpnBackend, create_backend};
pub use platform::{get_platform, Platform, PlatformImpl};
pub use state::VpnState;
