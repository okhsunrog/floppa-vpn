//! Keeping this device's peers on the server, from whichever process holds the tunnel actor.
//!
//! This sits above two crates that do not know about each other, and that is the whole reason it
//! exists. [`floppa_vpn_core`] runs a tunnel and knows nothing about who provisioned it;
//! [`floppa_api_client`] describes the server and knows nothing about tunnels. Joining "a connect
//! cycle ended this way" to "so ask the server for a new peer" needs both, and putting it in
//! either one would drag the other in behind it.
//!
//! It used to live inside the Tauri app, which was fine while the app was the only thing that held
//! an actor. It is not: on Android the actor is in `:vpn`, and on a desktop with the service
//! installed it is in the service — a process that has no webview and is not the Tauri crate at
//! all. So the parts that follow the actor live here, and the parts that describe a sync to a
//! person in their own language stay in the app.
//!
//! - [`identity`] — who this installation is: the device id the server tells machines apart by,
//!   and the name and version that go with it.
//! - [`session`] — who this device is to the server, in a file the process holding the actor can
//!   read. The token lives in a webview's `localStorage`, and the process that needs it most has
//!   no webview.
//! - [`server`] — a client authenticated as the signed-in user, and the actor's store as somewhere
//!   for a fetched config to land.
//! - [`outcome`] — reading a finished connect cycle as "a peer may have been deleted".
//! - [`watcher`] — acting on that, with nobody looking.
//!
//! # The app version is always passed in
//!
//! Nothing here reads `env!("CARGO_PKG_VERSION")`. This crate is linked into the app, into the
//! command-line client and into the service, and each of those has its own version — so reading it
//! here would tell the server this crate's version whichever program was actually asking.

pub mod identity;
pub mod outcome;
pub mod server;
pub mod session;
pub mod watcher;

pub use identity::{device_identity, device_name};
pub use outcome::{OutcomePlan, peer_protocol, plan_outcome};
pub use server::{ActorSink, client};
pub use session::ServerSession;
