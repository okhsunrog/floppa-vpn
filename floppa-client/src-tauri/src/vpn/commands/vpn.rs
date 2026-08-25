//! The tunnel commands.
//!
//! Every command here is a thin wrapper over the actor. None of them touches tunnel state, and
//! none of them can block on the tunnel: setting an intent returns as soon as the actor has
//! accepted it, and waiting for the result is a separate call the caller may drop.

use crate::vpn::actor::handle::{IntentRequest, TunnelHandle};
use crate::vpn::actor::types::{
    CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelParams, TunnelState,
};
use crate::vpn::protocol::Protocol;
use crate::vpn::store::ConfigError;
use tauri::State;

/// Ask for a tunnel.
///
/// Returns as soon as the actor accepts the intent — the epoch it returns identifies this request
/// for [`tunnel_await_cycle`]. There is deliberately no "busy" failure: with a single owner and a
/// write-only intent queue, there is no bad moment to ask.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_set_intent_up(
    order: Vec<Protocol>,
    params: TunnelParams,
    tunnel: State<'_, TunnelHandle>,
) -> Result<IntentAccepted, IntentError> {
    tunnel.set_intent(IntentRequest::Up { order, params }).await
}

/// Ask for no tunnel. Also the cancel button: an intent change is how an in-flight attempt is
/// stopped.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_set_intent_down(
    tunnel: State<'_, TunnelHandle>,
) -> Result<IntentAccepted, IntentError> {
    tunnel.set_intent(IntentRequest::Down).await
}

/// Wait for a request to reach a terminal outcome.
///
/// Safe to drop: dropping the future only discards the answer, it never cancels what the actor is
/// doing. A caller that asks after the fact still gets the answer, because recent outcomes are
/// retained.
#[tauri::command]
#[specta::specta]
pub async fn tunnel_await_cycle(
    epoch: IntentEpoch,
    tunnel: State<'_, TunnelHandle>,
) -> Result<CycleOutcome, IntentError> {
    tunnel.await_cycle(epoch).await
}

/// The current snapshot. A local read of the published state — no IPC, no lock.
#[tauri::command]
#[specta::specta]
pub fn tunnel_get_state(tunnel: State<'_, TunnelHandle>) -> TunnelState {
    tunnel.snapshot()
}

/// Store a config under its own protocol key.
///
/// Storing is not choosing: this does not change which protocol the next connect would use. The
/// previous behaviour of switching to whatever was imported last is what let a server sync
/// silently reorder the user's preference.
#[tauri::command]
#[specta::specta]
pub async fn import_config(
    raw: String,
    tunnel: State<'_, TunnelHandle>,
) -> Result<Protocol, ConfigError> {
    tunnel.import_config(raw).await
}

/// Forget every stored config.
///
/// Goes down and waits for the tunnel to actually be gone before wiping, rather than deciding from
/// a status snapshot — which is how a live adopted tunnel could survive being forgotten.
#[tauri::command]
#[specta::specta]
pub async fn clear_configs(tunnel: State<'_, TunnelHandle>) -> Result<(), IntentError> {
    tunnel.clear_configs().await
}

/// Forget which protocol last worked, so the next connect probes from the top of the order again.
#[tauri::command]
#[specta::specta]
pub async fn forget_preferred_protocol(tunnel: State<'_, TunnelHandle>) -> Result<(), ()> {
    tunnel.forget_preferred().await;
    Ok(())
}
