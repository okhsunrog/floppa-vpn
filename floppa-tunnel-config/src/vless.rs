//! Tunnel defaults for a VLESS connection.
//!
//! A `vless://` URI describes the proxy (server, SNI, REALITY keys, flow) and nothing about the
//! TUN side, so both clients fill in the same address, resolver and MTU. VLESS carries no tunnel
//! address of its own — the proxy does not care what the local end is called — hence one fixed
//! private address for every client.

use ipnetwork::{IpNetwork, Ipv4Network};
use std::net::{IpAddr, Ipv4Addr};

/// The local TUN address.
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);

/// [`ADDRESS`] with its host prefix, as the `Address =` line of a `.conf` would carry it.
pub const ADDRESS_NETWORK: IpNetwork = match Ipv4Network::new_checked(ADDRESS, 32) {
    Some(network) => IpNetwork::V4(network),
    None => panic!("a /32 is always a valid prefix"),
};

/// The resolver used through the tunnel.
pub const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

/// The TUN MTU. Ethernet-sized: VLESS runs over TCP, which segments as needed, so there is no
/// per-packet overhead to leave room for the way WireGuard's UDP encapsulation has.
pub const MTU: u16 = 1500;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_what_the_clients_used_to_hard_code() {
        assert_eq!(ADDRESS_NETWORK.to_string(), "10.0.0.2/32");
        assert_eq!(DNS.to_string(), "1.1.1.1");
        assert_eq!(MTU, 1500);
    }
}
