//! The client side of the Floppa VPN server API — the one copy of it.
//!
//! Both clients in this repository talk to the same server, and until this crate existed both
//! described it separately: `floppa-cli` had its own request bodies, its own `Protocol` enum and
//! its own idea of which HTTP statuses meant what, and the Tauri client was about to grow a third
//! set beside the TypeScript one. Three descriptions of one contract drift, and the way they
//! drift is a field the server renamed and a client that keeps sending the old name until somebody
//! notices in production.
//!
//! So:
//! - [`schema`] is **generated** from `floppa-web-shared/openapi.json`, the same document that
//!   generates the TypeScript client, by `just openapi`. Never edited by hand.
//! - [`client`] is the one HTTP client over those types, with failures typed by what a caller can
//!   do about them.
//! - [`provision`] is the logic that turns "this device needs a peer" into one: shared, because
//!   the CLI and the app need exactly the same thing and used to disagree about the details.
//!
//! # What this crate may not depend on
//!
//! It is compiled into the Android app, so: no `sqlx`, no `tauri`, nothing that resolves names or
//! opens tunnels. It describes the server and talks to it, and that is all.

pub mod client;
pub mod provision;
pub mod schema;

pub use client::{ApiClient, ApiErrorCode, ApiFailure, ProvisionApi, Refusal};
pub use provision::{
    ConfigSink, DeviceIdentity, PeerLookup, RepairOutcome, SyncError, SyncResult, lookup_peer,
    repair_peer, sync_peers, sync_wg_family_peer,
};
/// The protocols that are backed by a per-device peer row.
///
/// The server's own `Protocol` — it has exactly the two, because VLESS is per-user and has no
/// peer. A client that also knows about VLESS keeps its own wider enum and converts here.
pub use schema::Protocol as PeerProtocol;
