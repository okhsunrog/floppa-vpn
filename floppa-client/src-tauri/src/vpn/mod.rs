pub mod actor;
pub mod autostart;
pub mod backend;
pub mod commands;
pub mod config;
pub mod events;
/// What the tunnel needs from whatever hosts it. Android-only: on desktop the ladder configures
/// the machine itself, and there is no service to ask.
#[cfg(target_os = "android")]
pub mod host;
#[cfg(target_os = "android")]
pub mod jni_entry;
pub mod platform;
pub mod private_file;
pub mod protocol;
/// The actor over a socket, both ends. Unix rather than Android, so the tests drive a real socket
/// on the host — and so a later desktop split reuses this rather than growing a second copy.
#[cfg(unix)]
pub mod remote;
pub mod rollback;
pub mod rpc;
/// Unix-only rather than Android-only, so the accept loop's lifetime rule is tested on the host.
#[cfg(unix)]
pub mod rpc_listener;
#[cfg(unix)]
pub mod rpc_server;
/// Unix-only rather than Android-only, so what a generation reports about itself is tested on the
/// host.
#[cfg(unix)]
pub mod service_state;
pub mod state;
pub mod store;
pub mod tunnel;
pub mod wire;

pub use backend::{VpnBackend, create_backend};
pub use platform::{Platform, PlatformImpl, get_platform};
pub use protocol::{InterfaceName, Preference, Protocol};
pub use state::ProtocolConfig;
