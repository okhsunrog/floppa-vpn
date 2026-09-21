//! What `--protocol` accepts.
//!
//! Three, where the server's own [`PeerProtocol`] has two: VLESS is provisioned per user and has
//! no peer row, so the server never names it in a peer request. The clap names are the wire
//! strings, so a typo is a usage error rather than a request the server would coerce into some
//! default.

use floppa_api_client::PeerProtocol;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Protocol {
    #[value(name = "wireguard")]
    WireGuard,
    #[value(name = "amneziawg")]
    AmneziaWg,
    #[value(name = "vless")]
    Vless,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::WireGuard => "wireguard",
            Protocol::AmneziaWg => "amneziawg",
            Protocol::Vless => "vless",
        }
    }

    /// The server's protocol, where the server has one for it. `None` for VLESS.
    pub fn peer(self) -> Option<PeerProtocol> {
        match self {
            Protocol::WireGuard => Some(PeerProtocol::Wireguard),
            Protocol::AmneziaWg => Some(PeerProtocol::Amneziawg),
            Protocol::Vless => None,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
