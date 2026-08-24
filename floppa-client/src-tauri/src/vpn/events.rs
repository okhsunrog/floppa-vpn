//! The one event the tunnel emits.
//!
//! Push rather than poll, for a reason that matters on mobile specifically: a webview that has
//! been backgrounded has its timers throttled, so a frontend interval is not a reliable clock for
//! anything. The actor already owns a clock in Rust; this is how what it learns reaches the UI.
//!
//! One event carrying the whole snapshot, never a set of narrower ones. Delivering the phase and
//! the probe progress separately would reintroduce exactly the tearing this refactor removed: two
//! messages can be observed between one another, a single value cannot.

use crate::vpn::actor::types::TunnelState;
use serde::{Deserialize, Serialize};
use specta::Type;

/// Emitted whenever the actor publishes a new state.
///
/// The payload carries a `seq` that only ever increases, so a listener can drop anything not
/// strictly newer than what it already holds — which is what closes the race between seeding from
/// a direct read at startup and receiving the first pushed update.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TunnelStateChanged(pub TunnelState);
