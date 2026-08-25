//! Client-side tunnel configuration, shared by `floppa-cli` and `floppa-client`.
//!
//! The server hands a client one of three things: a WireGuard `.conf`, an AmneziaWG `.conf` (the
//! same format with obfuscation keys in `[Interface]`), or a `vless://` URI. This crate holds what
//! both clients need to turn the first two into a running tunnel, so the logic exists once:
//!
//! - [`conf`] — the strict `.conf` parser producing a typed [`TunnelConfig`], with
//!   [`ConfigParseError`] naming the line and key of anything it rejects;
//! - [`awg`] — [`AwgObfuscation`], the AmneziaWG 2.0 parameters, which the server also renders
//!   into client configs (so the type lives here and `floppa-core` re-exports it);
//! - [`route`] — the pure parts of routing: host routes, catch-all halves, `ip route` parsing;
//! - [`vless`] — the tunnel defaults a `vless://` URI does not carry;
//! - [`device`] (feature `gotatun`) — builders turning a [`TunnelConfig`] into gotatun's peer and
//!   AmneziaWG settings.
//!
//! Nothing here touches the network or the OS: resolving the endpoint, creating the TUN device and
//! running `ip`/`netsh` stay in the clients.

#![warn(missing_docs)]

pub mod awg;
pub mod conf;
#[cfg(feature = "gotatun")]
pub mod device;
pub mod route;
pub mod vless;

pub use awg::AwgObfuscation;
pub use conf::{
    ConfigParseError, DnsEntry, Endpoint, InterfaceConfig, KeyError, PeerConfig, PublicKey,
    SecretKey, TunnelConfig,
};
