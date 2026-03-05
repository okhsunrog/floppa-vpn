pub mod backend;
pub mod commands;
pub mod config;
#[cfg(target_os = "android")]
pub mod jni_entry;
pub mod platform;
#[cfg(target_os = "android")]
pub mod rpc;
#[cfg(target_os = "android")]
pub mod rpc_server;
pub mod state;
pub mod tunnel;

pub use backend::{VpnBackend, create_backend};
pub use platform::{Platform, PlatformImpl, get_platform};
pub use state::VpnState;
