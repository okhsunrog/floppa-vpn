//! Turning "this device needs a peer" into one, the same way for every client.
//!
//! The CLI and the app want exactly this and used to each have their own version, which is how
//! they came to disagree about when a peer may be created and what an unanswered request means.
//!
//! # What is load-bearing
//!
//! **"No peer" and "could not ask" are different answers.** Only a `404` means the peer is gone.
//! A network failure read as "gone" makes an offline start create a duplicate peer, and makes a
//! reconnect replace a peer that was never missing — burning a slot from the account's limit each
//! time. [`crate::client::ProvisionApi::peer_by_device`] enforces the distinction in its type;
//! nothing here may collapse it.

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::client::{ApiErrorCode, ApiFailure, ProvisionApi};
use crate::schema::{CreatePeerRequest, PeerSyncStatus, Protocol, UpsertInstallationRequest};

impl PeerSyncStatus {
    /// Whether this peer is usable, or about to be.
    ///
    /// `PendingAdd` counts: the daemon is on its way to putting it on the interface, and a client
    /// that treated it as absent would ask for a second peer while the first was being created.
    /// `PendingRemove` does not: it is on its way off, and adopting it means connecting over a
    /// peer that will stop answering — which is exactly the failure the repair path exists to
    /// clean up after.
    pub fn is_live(self) -> bool {
        matches!(self, PeerSyncStatus::PendingAdd | PeerSyncStatus::Active)
    }
}

/// Who this device is to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Stable per install. The server keys a device's peers on it.
    pub device_id: String,
    /// What the account page shows. Cosmetic, and absent on a client that has no way to ask.
    pub device_name: Option<String>,
    /// `android` / `linux` / `windows`, as the running binary knows itself.
    pub platform: String,
    pub app_version: String,
}

/// Where a config goes once the server hands it over.
///
/// A trait, because the two clients keep configs in very different places — the app writes them
/// into the tunnel actor's store, the CLI holds one for the life of a process — and because a test
/// can then record what it was given.
#[async_trait]
pub trait ConfigSink: Send + Sync {
    /// Parse and store one config, in whatever form this client keeps them.
    ///
    /// Takes the raw text, WireGuard `.conf` or a `vless://` URI alike: which it is, is the
    /// sink's business.
    async fn import(&self, raw: String) -> Result<(), String>;

    /// Whether any usable config is stored at all.
    async fn has_any(&self) -> bool;
}

/// Why a sync did not finish the way it was meant to.
///
/// A tag rather than a sentence: one caller shows these in a user's own language from a locale
/// file, and only [`SyncError::CreateFailed`] carries text, because only it has something the
/// server said that is worth repeating.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("no active subscription")]
    NoSubscription,
    #[error("this account already has as many peers as its plan allows")]
    PeerLimitReached,
    #[error("the server would not create a peer: {detail}")]
    CreateFailed { detail: String },
}

/// How a sync ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncResult {
    /// Everything this device is entitled to is provisioned and stored.
    Ok,
    /// The server answered, and refused.
    Failed(SyncError),
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

/// Ask whether this device has a *usable* peer for `protocol`.
///
/// A peer the server is in the middle of removing answers as [`PeerLookup::Missing`], because for
/// every purpose a caller has it is: connecting over it means connecting over something that is
/// about to stop answering.
pub async fn lookup_peer(
    api: &dyn ProvisionApi,
    device_id: &str,
    protocol: Protocol,
) -> PeerLookup {
    match api.peer_by_device(device_id, protocol).await {
        Ok(Some(peer)) if peer.sync_status.is_live() => PeerLookup::Found(peer.id),
        Ok(Some(peer)) => {
            debug!(
                status = %peer.sync_status,
                "the {protocol} peer for this device is on its way out; treating it as gone"
            );
            PeerLookup::Missing
        }
        Ok(None) => PeerLookup::Missing,
        Err(e) => {
            debug!("could not ask about the {protocol} peer: {e}");
            PeerLookup::Unknown
        }
    }
}

/// This device's config for one protocol, as text — the peer created if it has none.
///
/// What [`sync_wg_family_peer`] does before it stores anything, and what a client with no config
/// store of its own (the CLI holds one for the life of a process) wants on its own.
pub async fn config_for_peer(
    api: &dyn ProvisionApi,
    identity: &DeviceIdentity,
    has_subscription: bool,
    protocol: Protocol,
    allow_create: bool,
) -> ConfigOutcome {
    match lookup_peer(api, &identity.device_id, protocol).await {
        PeerLookup::Found(id) => match api.peer_config(id).await {
            Ok(raw) => ConfigOutcome::Ready(raw),
            // A peer we have just been told exists, whose config we cannot fetch: the server is
            // there, but this call did not land. Nothing was concluded about the peer.
            Err(e) => {
                warn!("could not fetch the config for peer {id}: {e}");
                ConfigOutcome::Offline
            }
        },
        PeerLookup::Unknown => ConfigOutcome::Offline,
        PeerLookup::Missing => {
            if !allow_create {
                return ConfigOutcome::NotAsked;
            }
            if !has_subscription {
                return ConfigOutcome::Failed(SyncError::NoSubscription);
            }
            create_peer(api, identity, protocol).await
        }
    }
}

/// What came of asking for one protocol's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOutcome {
    Ready(String),
    /// There is no peer and none was to be created — the caller said not to.
    NotAsked,
    Offline,
    Failed(SyncError),
}

/// Fetch — and, when allowed, create — the peer for one protocol, storing its config.
///
/// `allow_create` is false for the secondary protocol on a server that does not offer it, so a
/// peer is never made for a protocol that cannot carry anything.
pub async fn sync_wg_family_peer(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
    has_subscription: bool,
    protocol: Protocol,
    allow_create: bool,
) -> SyncResult {
    match config_for_peer(api, identity, has_subscription, protocol, allow_create).await {
        ConfigOutcome::Ready(raw) => match sink.import(raw).await {
            Ok(()) => SyncResult::Ok,
            Err(detail) => {
                warn!("the server's {protocol} config did not import: {detail}");
                SyncResult::Failed(SyncError::CreateFailed { detail })
            }
        },
        ConfigOutcome::NotAsked => SyncResult::Ok,
        ConfigOutcome::Offline => SyncResult::Offline,
        ConfigOutcome::Failed(error) => SyncResult::Failed(error),
    }
}

async fn create_peer(
    api: &dyn ProvisionApi,
    identity: &DeviceIdentity,
    protocol: Protocol,
) -> ConfigOutcome {
    let request = CreatePeerRequest {
        device_id: Some(identity.device_id.clone()),
        device_name: identity.device_name.clone(),
        protocol: Some(protocol),
        ..Default::default()
    };
    match api.create_peer(&request).await {
        Ok(created) => {
            info!(peer = created.id, %protocol, ip = %created.assigned_ip, "a peer was created for this device");
            ConfigOutcome::Ready(created.config)
        }
        Err(ApiFailure::Unreachable(why)) => {
            debug!("the peer could not be created: {why}");
            ConfigOutcome::Offline
        }
        Err(ApiFailure::Refused(refusal)) => {
            warn!("the server refused to create a {protocol} peer: {refusal:?}");
            ConfigOutcome::Failed(if refusal.is(ApiErrorCode::NoActiveSubscription) {
                SyncError::NoSubscription
            } else if refusal.is(ApiErrorCode::PeerLimitReached) {
                SyncError::PeerLimitReached
            } else {
                SyncError::CreateFailed {
                    detail: refusal.detail(),
                }
            })
        }
    }
}

/// One full sync: register the installation, provision the peers, fetch the VLESS config.
///
/// Never propagates a transport error: everything that leaves the server's contents unknown is
/// [`SyncResult::Offline`], which is the only honest thing to say when nothing answered.
pub async fn sync_peers(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
) -> SyncResult {
    // First, because it decides whether the rest is worth attempting: it says both whether the
    // server is reachable and whether this account may have a peer at all. Reading a network
    // failure as "no subscription" is exactly the confusion this module exists to prevent, so an
    // unreachable server ends the sync here rather than further down.
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

    // AmneziaWG is the default where the server offers it, plain WireGuard otherwise. A server
    // that cannot be asked is treated as not offering it: WireGuard works everywhere AmneziaWG
    // does.
    let amneziawg_available = match api.public_config().await {
        Ok(config) => config.amneziawg_available,
        Err(e) => {
            debug!("could not read the server's public config: {e}");
            false
        }
    };
    let primary = if amneziawg_available {
        Protocol::Amneziawg
    } else {
        Protocol::Wireguard
    };

    // The primary must succeed: it is what a connect will use.
    let result = sync_wg_family_peer(api, sink, identity, has_subscription, primary, true).await;
    if !result.is_ok() {
        return result;
    }

    // The secondary is a bonus. A device is one slot against the account's peer limit whichever
    // protocols it holds, so holding both gives the user every switcher position for free — but
    // failing to get it must not fail the sync. It is only meaningful when AmneziaWG is offered:
    // when it is not, the primary is WireGuard and the secondary would be the AmneziaWG this
    // server does not have.
    let secondary = other(primary);
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
        debug!("the secondary {secondary} peer was not provisioned");
    }

    // VLESS is per-user and costs no peer slot. A server that does not offer it says so, which is
    // not a failure of ours; anything else is worth a line but must not fail the sync.
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

/// The other protocol. A device holds both where both are offered, so this is how the secondary is
/// named without repeating the pairing at every call site.
pub fn other(protocol: Protocol) -> Protocol {
    match protocol {
        Protocol::Wireguard => Protocol::Amneziawg,
        Protocol::Amneziawg => Protocol::Wireguard,
    }
}

/// What came of trying to repair a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairOutcome {
    /// The peer was gone, and a new one was provisioned.
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
/// The whole of a repair — what a caller does *afterwards* is what makes it a quiet background
/// repair or a reconnect, not anything done here.
pub async fn repair_peer(
    api: &dyn ProvisionApi,
    sink: &dyn ConfigSink,
    identity: &DeviceIdentity,
    protocol: Protocol,
) -> RepairOutcome {
    match lookup_peer(api, &identity.device_id, protocol).await {
        PeerLookup::Found(_) => {
            debug!("the {protocol} peer is still there");
            RepairOutcome::PeerExists
        }
        PeerLookup::Unknown => {
            debug!("the server could not be asked about the {protocol} peer");
            RepairOutcome::Unreachable
        }
        PeerLookup::Missing => {
            info!(%protocol, "the peer for this device is gone from the server; provisioning a new one");
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
