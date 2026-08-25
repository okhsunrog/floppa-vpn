//! The client-side VPN, independent of what is driving it.
//!
//! Everything here used to live inside the Tauri app, and `floppa-cli` had a smaller, separate
//! version of the same things: its own route and DNS handling, its own rollback, its own tunnel
//! setup — around a thousand lines describing the same job twice, with the two descriptions
//! disagreeing in the details that matter (which peer counts as usable, whether an unreachable
//! server means a peer is gone, whether a failed teardown is ever retried).
//!
//! So this is the whole of it, once:
//!
//! - [`actor`] — the connection state machine: intents, epochs, the protocol ladder, the reconnect
//!   budget, the unwind. Everything about *when* to connect and what to do when it fails.
//! - [`platform`] — routes, DNS and TUN devices, per operating system.
//! - [`backend`] and [`tunnel`] — the tunnels themselves: gotatun for WireGuard and AmneziaWG,
//!   shoes-lite for VLESS.
//! - [`rollback`] — the journal of undo steps, durable across a restart, so a machine is never
//!   left with a stale route or a VPN `/etc/resolv.conf`.
//! - [`store`] and [`state`] — the configs, and where they are kept.
//! - [`logging`] — one tracing setup, and the diagnostic captures on top of it.
//!
//! # What this crate may not depend on
//!
//! No `tauri`, and no `sqlx`. It compiles into the Android app, into the desktop app, and into a
//! command-line binary, and none of those may be assumed.

pub mod actor;
pub mod autostart;
pub mod backend;
pub mod config;
/// What the tunnel needs from whatever hosts it. Android-only: on desktop the ladder configures
/// the machine itself, and there is no service to ask.
#[cfg(target_os = "android")]
pub mod host;
pub mod logging;
pub mod platform;
pub mod private_file;
pub mod protocol;
/// The actor over a socket, both ends. Unix rather than Android, so the tests drive a real socket
/// on the host — and so a later desktop split reuses this rather than growing a second copy.
#[cfg(unix)]
pub mod remote;
pub mod rollback;
/// The wire itself. Unix-gated like both of its ends: on Windows nothing speaks it, and the
/// vocabulary the service trait imports would sit there unused.
#[cfg(unix)]
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

pub use backend::VpnBackend;
#[cfg(not(target_os = "android"))]
pub use backend::create_backend;
pub use platform::{Platform, PlatformImpl, get_platform};
pub use protocol::{InterfaceName, Preference, Protocol};
pub use state::ProtocolConfig;
