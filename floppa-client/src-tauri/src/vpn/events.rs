//! The events this process pushes into the webview: one for the tunnel, two for the tray.
//!
//! Push rather than poll, for a reason that matters on mobile specifically: a webview that has
//! been backgrounded has its timers throttled, so a frontend interval is not a reliable clock for
//! anything. The actor already owns a clock in Rust; this is how what it learns reaches the UI.
//!
//! One event carrying the whole snapshot, never a set of narrower ones. Delivering the phase and
//! the probe progress separately would reintroduce exactly the tearing this refactor removed: two
//! messages can be observed between one another, a single value cannot.
//!
//! The tray's two are the opposite shape and carry nothing at all — see them below.

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

/// The tray's one action was clicked.
///
/// Carries nothing on purpose: connect and disconnect are the same row, and which of the two it
/// means is decided where the button's own label is decided. Rust would have to keep a second copy
/// of that rule to put an answer in here, and a second copy is how the label and the action come
/// to disagree.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TrayToggleRequested;

/// The window's close button was pressed, and nothing has closed.
///
/// Desktop only. What closing means is a setting — quit, or carry on in the tray — so the close is
/// prevented and the question asked here. See `crate::tray`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct WindowCloseRequested;
