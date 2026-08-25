//! The Tauri command surface, one module per concern.
//!
//! - `vpn` — the tunnel: thin wrappers over the actor.
//! - `logs` — log configuration and diagnostic captures.
//! - `android` / `desktop` — everything whose implementation depends on the platform: device
//!   identity, the split-tunneling app list, Android permission prompts, insets, the status bar,
//!   and where an exported archive is saved. Exactly one of the two is compiled, and both define
//!   the same command names with the same signatures, so the registration in `lib.rs` and the
//!   generated bindings are platform-free. The desktop side answers the Android-only questions
//!   with their "not applicable" value (`Ok(true)`, an empty list, zero insets) rather than an
//!   error, because the UI treats those as "nothing to ask the user about".
//!
//! Every command name and signature is part of `bindings.ts`; a change here is a change there.

#[cfg(target_os = "android")]
mod android;
#[cfg(not(target_os = "android"))]
mod desktop;
mod logs;
mod vpn;

#[cfg(target_os = "android")]
pub use android::*;
#[cfg(not(target_os = "android"))]
pub use desktop::*;
pub use logs::*;
pub use vpn::*;

use serde::{Deserialize, Serialize};
use specta::Type;

/// Information about an installed app (for split tunneling UI)
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AppInfo {
    pub package_name: String,
    pub label: String,
    pub is_system: bool,
    pub icon: Option<String>,
}

/// Safe area insets (status bar, nav bar) in dp
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SafeAreaInsets {
    pub top: f64,
    pub bottom: f64,
}
