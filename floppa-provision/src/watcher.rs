//! Repairing a peer the server deleted, with nobody looking at the app.
//!
//! This is what the move into `:vpn` was for. The actor already reconnects on its own: a tunnel
//! that dies is rebuilt, and a protocol whose peer has gone is stepped over in favour of one that
//! works. What it could not do was *fix* the peer that had gone, because fixing it means talking
//! to the server, and until now the only thing that talked to the server was a webview — which
//! Android freezes the moment the app goes into the background.
//!
//! So the sequence used to end here: phone in a pocket, peer deleted, ladder steps over AmneziaWG
//! onto WireGuard, connection carries on, and the dead peer stays dead until somebody opens the
//! app. On an account with one protocol there was nothing to step onto and no tunnel at all.
//!
//! # What it does
//!
//! It watches finished cycles. [`plan_outcome`](super::plan_outcome) reads each one and asks for
//! one of two things:
//!
//! - **Repair** — the cycle connected, over some *other* protocol. The dead one gets a new peer,
//!   quietly: the tunnel is up, so there is nothing to reconnect and nothing to tell anyone.
//! - **Reprovision** — the cycle connected over nothing. Same repair, and then a tunnel is asked
//!   for again, because one is owed.
//!
//! # Two things it must not do
//!
//! **It must not repair on a server it could not reach.** `PeerLookup::Unknown` is not `Missing`,
//! and creating a peer because the network was down is how an account burns through its peer
//! limit. The check lives in `floppa-api-client`; this only has to not undo it.
//!
//! **It must not ask twice.** A replacement peer that also fails to verify is not evidence that
//! it is missing — something else is wrong — and asking again would have the actor and the server
//! taking turns making peers until the plan's limit stopped them.

use floppa_api_client::{PeerProtocol, RepairOutcome, repair_peer};
use tracing::{debug, info, warn};

use crate::outcome::{OutcomePlan, plan_outcome};
use crate::server::{self, ActorSink};
use floppa_vpn_core::actor::Spawn;
use floppa_vpn_core::actor::handle::{IntentRequest, TunnelHandle};
use floppa_vpn_core::actor::types::{IntentView, TunnelParams, TunnelState};
use floppa_vpn_core::protocol::Protocol;

/// Start watching. Returns at once; the work happens on `spawn`.
///
/// Call it where the actor actually lives — the `:vpn` process on Android, the app process on
/// desktop — and nowhere else. Two watchers on one actor would both see the same dead peer and
/// both ask the server to replace it.
///
/// `app_version` is the running binary's own; this crate is linked into several and cannot read it
/// for itself. `&'static str` because it outlives the task, and because every caller has one to
/// hand: `env!("CARGO_PKG_VERSION")`.
pub fn watch(handle: TunnelHandle, spawn: Spawn, app_version: &'static str) {
    spawn(Box::pin(async move { run(handle, app_version).await }));
}

/// What the last live Up intent asked for, so a tunnel can be asked for again after a repair.
///
/// Remembered rather than read back, because the published state does not keep it: once the
/// actor gives up it demotes the intent, and a demoted intent has no order and no parameters.
/// What is remembered is what was seen while a tunnel was actually up — which is exactly the
/// case that needs this, a tunnel that was working and then could not be kept alive.
#[derive(Clone)]
struct LiveIntent {
    order: Vec<Protocol>,
    params: TunnelParams,
}

async fn run(handle: TunnelHandle, app_version: &str) {
    let mut states = handle.states();
    // The serial of the last outcome acted on. Every published state repeats the outcome its
    // cycle ended on, and a reconnect runs under the *same* intent — so the epoch cannot tell two
    // cycles apart and the serial is the only thing that can.
    let mut handled: Option<u64> = None;
    let mut just_reprovisioned = false;
    let mut live: Option<LiveIntent> = None;

    loop {
        if states.changed().await.is_err() {
            debug!("the actor is gone; nothing left to repair for");
            return;
        }
        let state: TunnelState = states.borrow_and_update().clone();

        if let Some(seen) = live_intent(&state) {
            live = Some(seen);
        }

        let Some(outcome) = state.last_outcome.clone() else {
            continue;
        };
        if handled == Some(state.outcome_serial) {
            continue;
        }
        handled = Some(state.outcome_serial);

        let plan = plan_outcome(&outcome);
        let asks_again = matches!(plan, OutcomePlan::Reprovision { .. });
        if asks_again && just_reprovisioned {
            info!("the peer was just replaced and still did not verify; not replacing it again");
            just_reprovisioned = false;
            continue;
        }
        just_reprovisioned = asks_again;

        match plan {
            OutcomePlan::Ignore => {}
            OutcomePlan::Repair { protocol } => {
                // Quiet by design: the tunnel is up. A repair that cannot be done costs nothing
                // that has not already been lost.
                if let Some(RepairOutcome::Recreated) = repair(&handle, protocol, app_version).await
                {
                    info!(%protocol, "a peer the ladder stepped over was replaced");
                }
            }
            OutcomePlan::Reprovision { protocol } => {
                if !matches!(
                    repair(&handle, protocol, app_version).await,
                    Some(RepairOutcome::Recreated)
                ) {
                    continue;
                }
                match live.clone() {
                    Some(intent) => {
                        info!(%protocol, "the peer was gone and has been replaced; asking for a tunnel again");
                        ask_again(&handle, protocol, intent).await;
                    }
                    // Nothing was ever up, so nothing is known about what to ask for. The peer is
                    // fixed either way, which is what the next connect — the user, the tile, an
                    // always-on start — will find.
                    None => info!(
                        %protocol,
                        "the peer was replaced; leaving the next connect to raise a tunnel"
                    ),
                }
            }
        }
    }
}

/// The order and parameters of a live Up intent, if this state shows one.
fn live_intent(state: &TunnelState) -> Option<LiveIntent> {
    if state.intent != IntentView::Up || state.intent_order.is_empty() {
        return None;
    }
    Some(LiveIntent {
        order: state.intent_order.clone(),
        params: state.params.clone()?,
    })
}

/// Ask for a tunnel again, repaired protocol first.
async fn ask_again(handle: &TunnelHandle, repaired: PeerProtocol, intent: LiveIntent) {
    let repaired = protocol_of(repaired);
    let mut order = vec![repaired];
    order.extend(intent.order.into_iter().filter(|p| *p != repaired));

    if let Err(e) = handle
        .set_intent(IntentRequest::Up {
            order,
            params: intent.params,
        })
        .await
    {
        warn!("the reconnect after a repair was refused: {e}");
    }
}

fn protocol_of(protocol: PeerProtocol) -> Protocol {
    match protocol {
        PeerProtocol::Wireguard => Protocol::WireGuard,
        PeerProtocol::Amneziawg => Protocol::AmneziaWg,
    }
}

/// Check the peer and replace it if it is gone. `None` when there was no way to even ask.
async fn repair(
    handle: &TunnelHandle,
    protocol: PeerProtocol,
    app_version: &str,
) -> Option<RepairOutcome> {
    let Some((api, identity)) = server::client(app_version) else {
        debug!("nobody is signed in on this device; the peer stays as it is");
        return None;
    };
    let outcome = repair_peer(&api, &ActorSink(handle.clone()), &identity, protocol).await;
    debug!(%protocol, ?outcome, "the peer was looked at");
    Some(outcome)
}
