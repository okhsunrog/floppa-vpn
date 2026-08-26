//! Repairing a peer the server deleted, with nobody looking at the app.
//!
//! This is what the whole move into `:vpn` was for. The actor already reconnects on its own: a
//! tunnel that dies is rebuilt, and a protocol whose peer has gone is stepped over in favour of
//! one that works. What it could not do was *fix* the peer that had gone, because fixing it means
//! talking to the server, and until now the only thing that talked to the server was a webview —
//! which Android freezes the moment the app goes into the background.
//!
//! So the sequence used to end here: phone in a pocket, peer deleted, ladder steps over AmneziaWG
//! onto WireGuard, connection carries on, and the dead peer stays dead until somebody opens the
//! app. If the account had only one protocol, there was nothing to step onto and no tunnel at all.
//!
//! # What it does and does not do
//!
//! It watches finished cycles. [`plan_outcome`](super::plan_outcome) reads each one, and there
//! are exactly two things it can ask for:
//!
//! - **Repair** — the cycle connected, over some *other* protocol. The dead one gets a new peer,
//!   quietly: the tunnel is up, so there is nothing to reconnect and nothing to tell anyone.
//! - **Reprovision** — the cycle connected over nothing. Same repair, and then the intent is
//!   raised again, because a tunnel is owed.
//!
//! It never repairs on a server it could not reach. `PeerLookup::Unknown` is not `Missing`, and
//! creating a peer because the network was down is how an account burns through its peer limit.

use std::sync::Arc;

use async_trait::async_trait;
use floppa_api_client::{ApiClient, ConfigSink, PeerProtocol, RepairOutcome, repair_peer};
use tracing::{debug, info, warn};

use super::session;
use super::{OutcomePlan, plan_outcome};
use crate::vpn::actor::Spawn;
use crate::vpn::actor::handle::{IntentRequest, TunnelHandle};
use crate::vpn::actor::types::{CycleOutcome, TunnelState};
use crate::vpn::config::config_dir;

/// Start watching. Returns immediately; the work happens on `spawn`.
pub fn watch(handle: TunnelHandle, spawn: Spawn) {
    let task = handle.clone();
    spawn(Box::pin(async move {
        run(task).await;
    }));
}

async fn run(handle: TunnelHandle) {
    let mut states = handle.states();
    // The serial of the last outcome acted on. Every published state repeats the outcome it
    // ended on, and a reconnect runs under the *same* intent — so the epoch cannot tell two
    // cycles apart and the serial is the only thing that can.
    let mut handled: Option<u64> = None;
    // Set after a reprovision, cleared by the cycle that follows it. Without it, a fresh peer
    // that also fails to verify asks for another one, and the actor and the server take turns
    // making peers until the account's limit stops them.
    let mut just_reprovisioned = false;

    loop {
        if states.changed().await.is_err() {
            debug!("the actor is gone; nothing left to repair for");
            return;
        }
        let state: TunnelState = states.borrow_and_update().clone();
        let Some(outcome) = state.last_outcome.clone() else {
            continue;
        };
        if handled == Some(state.outcome_serial) {
            continue;
        }
        handled = Some(state.outcome_serial);

        let plan = plan_outcome(&outcome);
        let reprovisioning = matches!(plan, OutcomePlan::Reprovision { .. });
        if reprovisioning && just_reprovisioned {
            info!("the peer was just replaced and still did not verify; not replacing it again");
            just_reprovisioned = false;
            continue;
        }
        just_reprovisioned = reprovisioning;

        match plan {
            OutcomePlan::Ignore => {}
            OutcomePlan::Repair { protocol } => {
                // Quiet by design: the tunnel is up. A repair that cannot be done costs nothing
                // that has not already been lost.
                match repair(&handle, protocol).await {
                    Some(RepairOutcome::Recreated) => {
                        info!(%protocol, "a peer the ladder stepped over was replaced")
                    }
                    Some(other) => debug!(%protocol, ?other, "nothing to repair"),
                    None => {}
                }
            }
            OutcomePlan::Reprovision { protocol } => {
                reprovision(&handle, protocol, &outcome).await;
            }
        }
    }
}

/// Check the peer and replace it if it is gone. `None` when there was no way to even ask.
async fn repair(handle: &TunnelHandle, protocol: PeerProtocol) -> Option<RepairOutcome> {
    let dir = match config_dir() {
        Ok(dir) => dir,
        Err(e) => {
            warn!("no config directory, so no session and no repair: {e}");
            return None;
        }
    };
    // Read per repair rather than held: the token is rewritten on every sliding refresh, and the
    // process that writes it is the other one.
    let Some(session) = session::load(&dir) else {
        debug!("nobody is signed in on this device; the peer stays as it is");
        return None;
    };
    let client = match ApiClient::new(&session.base_url, &session.token) {
        Ok(client) => client,
        Err(e) => {
            warn!("could not build an API client: {e}");
            return None;
        }
    };

    let sink = ActorSink(handle.clone());
    Some(repair_peer(&client, &sink, &session.identity(), protocol).await)
}

/// Repair, and then ask for a tunnel again — the cycle that led here ended without one.
async fn reprovision(handle: &TunnelHandle, protocol: PeerProtocol, outcome: &CycleOutcome) {
    match repair(handle, protocol).await {
        Some(RepairOutcome::Recreated) => {
            info!(%protocol, "the peer was gone and has been replaced; asking for a tunnel again");
        }
        Some(RepairOutcome::PeerExists) => {
            debug!(%protocol, "the peer is there, so a new one would not have helped");
            return;
        }
        Some(RepairOutcome::StillNoConfig) => {
            warn!(%protocol, "the peer was replaced but no usable config came back");
            return;
        }
        Some(RepairOutcome::Unreachable) | None => return,
    }

    // The order that failed, with the repaired protocol first: it is the one the user's settings
    // preferred, and it is the one that now has a working peer.
    let Some(order) = order_for(outcome, protocol) else {
        debug!("nothing to raise: the finished cycle named no order");
        return;
    };
    let params = match handle.snapshot().intent.params {
        Some(params) => params,
        None => {
            debug!("nothing to raise: the intent carries no parameters");
            return;
        }
    };
    if let Err(e) = handle.set_intent(IntentRequest::Up { order, params }).await {
        warn!("the reconnect after a repair was refused: {e}");
    }
}

/// The protocols to try, repaired one first.
fn order_for(
    outcome: &CycleOutcome,
    repaired: PeerProtocol,
) -> Option<Vec<crate::vpn::protocol::Protocol>> {
    use crate::vpn::protocol::Protocol;
    let repaired: Protocol = match repaired {
        PeerProtocol::Wireguard => Protocol::WireGuard,
        PeerProtocol::Amneziawg => Protocol::AmneziaWg,
    };
    let tried: Vec<Protocol> = match outcome {
        CycleOutcome::Exhausted { failures } => failures.iter().map(|f| f.protocol).collect(),
        CycleOutcome::LostGaveUp { protocol, .. } => vec![*protocol],
        _ => return None,
    };
    let mut order = vec![repaired];
    order.extend(tried.into_iter().filter(|p| *p != repaired));
    (!order.is_empty()).then_some(order)
}

/// The actor's config store, as somewhere for a fetched config to land.
struct ActorSink(TunnelHandle);

#[async_trait]
impl ConfigSink for ActorSink {
    async fn import(&self, raw: String) -> Result<(), String> {
        self.0
            .import_config(raw)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn has_any(&self) -> bool {
        !self.0.snapshot().configs.available.is_empty()
    }
}

/// So the watcher can be started from either process's setup without cloning by hand.
impl Clone for ActorSink {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[allow(dead_code)]
fn _assert_sink_is_object_safe(sink: Arc<dyn ConfigSink>) -> Arc<dyn ConfigSink> {
    sink
}
