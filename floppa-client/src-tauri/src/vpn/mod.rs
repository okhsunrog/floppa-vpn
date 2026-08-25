//! The app's VPN surface: the shared core, plus the parts that only exist inside a Tauri app.
//!
//! Everything about *the tunnel* — the actor, the backends, the platform layer, the rollback
//! journal, the config store — lives in `floppa-vpn-core`, where `floppa-cli` uses the same copy.
//! What is left here is what needs Tauri or the Android plugin to exist at all:
//!
//! - [`commands`] — the command surface that crosses into TypeScript.
//! - [`events`] — the tunnel state, forwarded to the webview.
//! - [`process`] — making the `:vpn` process exist, which only the plugin's activity can do.
//! - [`jni_entry`] — what Kotlin calls, and what calls Kotlin back.
//!
//! Re-exported wholesale rather than referenced through the crate name, so every `crate::vpn::…`
//! path in this app keeps meaning what it meant.

pub use floppa_vpn_core::*;

pub mod commands;
pub mod events;
/// The JNI implementation of the core's `ServiceHost`. It lives beside [`jni_entry`] rather than
/// in the core crate because everything it does is call into it, and the bridge has to stay in
/// the binary that Kotlin loads.
#[cfg(target_os = "android")]
pub mod host {
    pub use floppa_vpn_core::host::*;
    pub mod service;
}
#[cfg(target_os = "android")]
pub mod jni_entry;
/// Making the process that holds the actor exist. Android-only: it is the plugin's context and
/// activity that can do it, and neither exists in `:vpn`.
#[cfg(target_os = "android")]
pub mod process;
