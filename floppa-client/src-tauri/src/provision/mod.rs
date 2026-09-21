//! What this app adds to the shared provisioning in `floppa-provision`.
//!
//! The server talk, the peer logic and the API types are in `floppa-api-client`. Everything that
//! follows the *actor* — the session file both of this app's processes read, the reading of a
//! finished connect cycle as "a peer may have been deleted", and the watcher that acts on it — is
//! in `floppa-provision`, because the process holding the actor is not always this one: on Android
//! it is `:vpn`, and on a desktop with the service installed it is the service.
//!
//! What is left here is the one thing that is genuinely the app's: how a sync is described to a
//! person. Re-exported wholesale rather than referenced through the crate name, so every
//! `crate::provision::…` path in this app keeps meaning what it meant.

pub use floppa_provision::*;

use floppa_api_client::{SyncError, SyncResult};
use serde::{Deserialize, Serialize};
use specta::Type;

/// How a sync ended, in the words the connection card needs.
///
/// A translation of [`SyncResult`] rather than the thing itself, and deliberately: what the card
/// wants is a tag it can look up in a locale file, in the user's language. `floppa-api-client`
/// describes the server, not this app's vocabulary for talking to a person — and it has no
/// `specta`, which is the practical half of the same point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SyncOutcome {
    /// Everything this device is entitled to is provisioned and stored.
    Ok,
    /// The server answered, and refused.
    Failed { error: SyncFailure },
    /// Nothing was learned and nothing was changed — no server, or nobody signed in.
    Offline,
}

/// Why a sync was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncFailure {
    NoSubscription,
    PeerLimitReached,
    /// Anything else. `detail` is the server's own words, for a card that has no better ones.
    CreateFailed {
        detail: String,
    },
}

impl From<SyncResult> for SyncOutcome {
    fn from(result: SyncResult) -> Self {
        match result {
            SyncResult::Ok => SyncOutcome::Ok,
            SyncResult::Offline => SyncOutcome::Offline,
            SyncResult::Failed(error) => SyncOutcome::Failed {
                error: match error {
                    SyncError::NoSubscription => SyncFailure::NoSubscription,
                    SyncError::PeerLimitReached => SyncFailure::PeerLimitReached,
                    SyncError::CreateFailed { detail } => SyncFailure::CreateFailed { detail },
                },
            },
        }
    }
}
