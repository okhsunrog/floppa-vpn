//! Server-side peer provisioning: asking the server for this device's peers, and repairing them.
//!
//! This used to live in the frontend (`usePeerProvisioning.ts`), which worked exactly as long as
//! somebody was looking at the app. The tunnel does not need anybody to be looking any more — the
//! actor reconnects from `:vpn` with the UI process frozen — but a peer *deleted on the server*
//! could only ever be noticed and recreated by a webview. So the ladder would step over the dead
//! protocol, carry the connection on another, and leave the dead peer dead until the user next
//! opened the app. Here, it does not have to.
//!
//! # What is load-bearing
//!
//! **"No peer" and "could not ask" are different answers.** Only a `404` means the peer is gone.
//! A network failure that reads as "gone" makes an offline start create a duplicate peer, and
//! makes a reconnect re-provision a peer that was never missing — burning a peer slot each time.
//! [`api::ProvisionApi::peer_by_device`] enforces the distinction in its type; nothing here may
//! collapse it.
//!
//! **A repair is not a reconnect.** [`plan_outcome`] tells the two apart: a cycle that connected
//! over a *different* protocol owes the dead one a new peer and nothing else — no error, no
//! reconnect, the tunnel is up. A cycle that connected over nothing owes the user both.

pub mod api;
pub mod creds;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use specta::Type;
use tracing::{debug, info, warn};

use crate::vpn::actor::types::{AttemptError, CycleOutcome};
use crate::vpn::protocol::Protocol;
use api::{
    ApiErrorCode, ApiFailure, CreatePeerRequest, ProvisionApi, UpsertInstallationRequest,
    WgProtocol,
};

/// Who this device is to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub device_name: Option<String>,
    pub platform: String,
    pub app_version: String,
}

/// Where a config goes once the server hands it over.
///
/// A trait rather than the actor handle itself, so the logic below can be driven by a fake that
/// records what it was given. The real implementation is the actor: importing a config is a write
/// to the config store, and the store lives with the actor.
#[async_trait]
pub trait ConfigSink: Send + Sync {
    /// Parse and store one config, whatever protocol it turns out to be.
    async fn import(&self, raw: String) -> Result<Protocol, String>;

    /// Whether any usable config is stored at all.
    async fn has_any(&self) -> bool;
}

/// Why a sync did not finish the way it was meant to.
///
/// Carried to the UI as a tag rather than a sentence: the words a user reads are in the locale
/// files, in their language, and a `detail` is appended only where the server said something
/// worth repeating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncError {
    /// No subscription, so the server will not make a peer.
    NoSubscription,
    /// This account already holds as many peers as its plan allows.
    PeerLimitReached,
    /// Anything else the server refused with.
    CreateFailed { detail: String },
}

/// How a sync ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SyncResult {
    /// Everything this device is entitled to is provisioned and stored.
    Ok,
    /// The server answered and refused.
    Failed { error: SyncError },
    /// The server could not be reached. Nothing was learned, so nothing was changed.
    Offline,
}

impl SyncResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, SyncResult::Ok)
    }
}

/// What the server said about a peer this device may or may not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerLookup {
    /// It exists, and this is its id.
    Found(i64),
    /// The server said there is no such peer.
    Missing,
    /// The server could not be asked. **Not** the same as [`PeerLookup::Missing`].
    Unknown,
}

/// Ask whether this device has a peer for `protocol`.
pub async fn lookup_peer(
    api: &dyn ProvisionApi,
    device_id: &str,
    protocol: WgProtocol,
) -> PeerLookup {
    match api.peer_by_device(device_id, protocol).await {
        Ok(Some(peer)) => PeerLookup::Found(peer.id),
        Ok(None) => PeerLookup::Missing,
        Err(e) => {
            debug!("could not ask about the {} peer: {e}", protocol.as_str());
            PeerLookup::Unknown
        }
    }
}

/// Fetch — and, when allowed, create — the peer for one wg-family protocol, storing its config.
///
/// `allow_create` is false for the secondary protocol when the server does not offer it, so a
/// bonus peer is never conjured for a protocol that cannot work.
pub async fn sync_wg_family_peer(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
    has_subscription: bool,
    protocol: WgProtocol,
    allow_create: bool,
) -> SyncResult {
    match lookup_peer(api, &identity.device_id, protocol).await {
        PeerLookup::Found(id) => match api.peer_config(id).await {
            Ok(raw) => match sink.import(raw).await {
                Ok(_) => SyncResult::Ok,
                Err(detail) => {
                    warn!(
                        "the server's {} config did not import: {detail}",
                        protocol.as_str()
                    );
                    SyncResult::Failed {
                        error: SyncError::CreateFailed { detail },
                    }
                }
            },
            // A peer we have just been told exists, whose config we cannot fetch: the server is
            // there but this call did not land. Nothing was learned about the peer.
            Err(e) => {
                warn!("could not fetch the config for peer {id}: {e}");
                SyncResult::Offline
            }
        },
        PeerLookup::Unknown => SyncResult::Offline,
        PeerLookup::Missing => {
            if !allow_create {
                return SyncResult::Ok;
            }
            if !has_subscription {
                return SyncResult::Failed {
                    error: SyncError::NoSubscription,
                };
            }
            create_peer(api, sink, identity, protocol).await
        }
    }
}

async fn create_peer(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
    protocol: WgProtocol,
) -> SyncResult {
    let request = CreatePeerRequest {
        device_id: identity.device_id.clone(),
        device_name: identity.device_name.clone(),
        protocol,
    };
    match api.create_peer(&request).await {
        Ok(created) => {
            info!(
                peer = created.id,
                protocol = protocol.as_str(),
                "a peer was created for this device"
            );
            match sink.import(created.config).await {
                Ok(_) => SyncResult::Ok,
                Err(detail) => SyncResult::Failed {
                    error: SyncError::CreateFailed { detail },
                },
            }
        }
        Err(ApiFailure::Unreachable(why)) => {
            debug!("the peer could not be created: {why}");
            SyncResult::Offline
        }
        Err(ApiFailure::Refused(refusal)) => {
            let error = if refusal.is(ApiErrorCode::NoActiveSubscription) {
                SyncError::NoSubscription
            } else if refusal.is(ApiErrorCode::PeerLimitReached) {
                SyncError::PeerLimitReached
            } else {
                SyncError::CreateFailed {
                    detail: if refusal.message.is_empty() {
                        format!("HTTP {}", refusal.status)
                    } else {
                        refusal.message.clone()
                    },
                }
            };
            warn!(
                "the server refused to create a {} peer: {refusal:?}",
                protocol.as_str()
            );
            SyncResult::Failed { error }
        }
    }
}

/// One full sync: register the installation, provision the wg-family peers, fetch VLESS.
///
/// Never panics and never propagates a transport error: everything that leaves the server's
/// contents unknown is [`SyncResult::Offline`], which is the only honest thing to say when
/// nothing answered.
pub async fn sync_peers(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
) -> SyncResult {
    // First, because it is the one call that decides whether the rest is worth making: it says
    // whether the server is reachable *and* whether this account may have a peer at all. Reading
    // a network failure as "no subscription" would be exactly the confusion this module exists to
    // avoid, so an unreachable server ends the sync here.
    let has_subscription = match api.me().await {
        Ok(me) => me.subscription.is_some(),
        Err(e) => {
            debug!("the sync stops before it starts: {e}");
            return SyncResult::Offline;
        }
    };

    // Best-effort: the installation record is how the account page lists devices, and a sync that
    // failed only at that has still done everything a tunnel needs.
    let installation = UpsertInstallationRequest {
        device_id: identity.device_id.clone(),
        device_name: identity.device_name.clone(),
        platform: Some(identity.platform.clone()),
        app_version: Some(identity.app_version.clone()),
    };
    if let Err(e) = api.upsert_installation(&installation).await {
        debug!("the installation record was not updated: {e}");
    }

    // AmneziaWG is the default when the server offers it, plain WireGuard otherwise. A server we
    // cannot ask is treated as not offering it: WireGuard works everywhere AmneziaWG does.
    let amneziawg_available = match api.public_config().await {
        Ok(config) => config.amneziawg_available,
        Err(e) => {
            debug!("could not read the server's public config: {e}");
            false
        }
    };
    let primary = if amneziawg_available {
        WgProtocol::Amneziawg
    } else {
        WgProtocol::Wireguard
    };

    // The primary must succeed: it is what a connect will use.
    let result = sync_wg_family_peer(api, sink, identity, has_subscription, primary, true).await;
    if !result.is_ok() {
        return result;
    }

    // The secondary is a bonus. A device is one peer-limit slot whichever protocols it holds, so
    // holding both gives the user every switcher position for free — but failing to get it must
    // not fail the sync. It is only ever meaningful when AmneziaWG is offered: when it is not,
    // the primary is WireGuard and the secondary would be the AmneziaWG this server does not have.
    let secondary = primary.other();
    let bonus = sync_wg_family_peer(
        api,
        sink,
        identity,
        has_subscription,
        secondary,
        amneziawg_available,
    )
    .await;
    if !bonus.is_ok() {
        debug!(
            "the secondary {} peer was not provisioned",
            secondary.as_str()
        );
    }

    // VLESS is per-user and consumes no peer slot. A server that does not offer it says so, which
    // is not a failure of ours; anything else is worth a line but must not fail the sync.
    match api.vless_config().await {
        Ok(config) => {
            if let Err(detail) = sink.import(config.uri).await {
                warn!("the VLESS config did not import: {detail}");
            }
        }
        Err(ApiFailure::Refused(r)) if r.is(ApiErrorCode::VlessNotConfigured) => {
            debug!("this server does not offer VLESS");
        }
        Err(e) => debug!("the VLESS config is unavailable: {e}"),
    }

    SyncResult::Ok
}

// ---------------------------------------------------------------------------
// Reacting to a finished cycle
// ---------------------------------------------------------------------------

/// What a finished cycle asks of provisioning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomePlan {
    /// Nothing to do here. The cycle either succeeded outright or failed for a reason no peer
    /// would fix — whoever shows errors deals with it.
    Ignore,
    /// A protocol failed to verify on a cycle that *did* connect over another one. Its peer may
    /// have been deleted; check, and recreate it if so. Quiet: the tunnel is up.
    Repair { protocol: WgProtocol },
    /// A protocol failed to verify on a cycle that connected over nothing. Same check, and if the
    /// peer was indeed gone, a reconnect is owed once a new one exists.
    Reprovision { protocol: WgProtocol },
}

/// Decide what a finished cycle means for the peers on the server.
///
/// The one thing this cannot decide is *why* an attempt failed — that arrives typed from the
/// actor. `VerifyFailed` for a wg-family protocol is the signal that its peer may be gone, and it
/// is found by name: assuming it was "whichever protocol was tried last" is wrong the moment the
/// order has more than one entry.
pub fn plan_outcome(outcome: &CycleOutcome) -> OutcomePlan {
    match outcome {
        // Connected is not "nothing went wrong": the ladder tries protocols in order, so a peer
        // deleted under AmneziaWG shows up as a verification failure a second before WireGuard
        // carries the connection. That peer is worth repairing now rather than on some later
        // connect that has no fallback left.
        CycleOutcome::Connected { failures, .. } => match first_verify_failure(failures) {
            Some(protocol) => OutcomePlan::Repair { protocol },
            None => OutcomePlan::Ignore,
        },
        CycleOutcome::Exhausted { failures } => match first_verify_failure(failures) {
            Some(protocol) => OutcomePlan::Reprovision { protocol },
            None => OutcomePlan::Ignore,
        },
        // A tunnel that was up and then died: whatever was carrying it is the candidate. VLESS
        // has no per-device peer, so it never is one.
        CycleOutcome::LostGaveUp { protocol, .. } => match WgProtocol::try_from(*protocol) {
            Ok(protocol) => OutcomePlan::Reprovision { protocol },
            Err(()) => OutcomePlan::Ignore,
        },
        CycleOutcome::UnwindFailed | CycleOutcome::Cancelled | CycleOutcome::Down => {
            OutcomePlan::Ignore
        }
    }
}

/// The first protocol in `failures` that failed *verification*, and has a peer to lose.
fn first_verify_failure(
    failures: &[crate::vpn::actor::types::AttemptFailure],
) -> Option<WgProtocol> {
    failures
        .iter()
        .find(|f| matches!(f.error, AttemptError::VerifyFailed))
        .and_then(|f| WgProtocol::try_from(f.protocol).ok())
}

/// What came of acting on a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairOutcome {
    /// The peer was gone and a new one was provisioned.
    Recreated,
    /// The peer was gone, and provisioning a new one did not produce a usable config.
    StillNoConfig,
    /// The peer is there. Whatever went wrong is not a missing peer.
    PeerExists,
    /// The server could not be asked, so nothing was concluded and nothing was changed.
    Unreachable,
}

/// Check whether the peer for `protocol` is gone, and replace it if it is.
///
/// This is the whole of a repair, and the whole of the server side of a re-provision — what
/// separates them is what the caller does afterwards, not what is done here.
pub async fn repair_peer(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
    protocol: WgProtocol,
) -> RepairOutcome {
    match lookup_peer(api, &identity.device_id, protocol).await {
        PeerLookup::Found(_) => {
            debug!("the {} peer is still there", protocol.as_str());
            RepairOutcome::PeerExists
        }
        PeerLookup::Unknown => {
            debug!(
                "the server could not be asked about the {} peer",
                protocol.as_str()
            );
            RepairOutcome::Unreachable
        }
        PeerLookup::Missing => {
            info!(
                protocol = protocol.as_str(),
                "the peer for this device is gone from the server; provisioning a new one"
            );
            if !sync_peers(api, sink, identity).await.is_ok() {
                return RepairOutcome::Unreachable;
            }
            if sink.has_any().await {
                RepairOutcome::Recreated
            } else {
                RepairOutcome::StillNoConfig
            }
        }
    }
}

#[cfg(test)]
mod tests;
