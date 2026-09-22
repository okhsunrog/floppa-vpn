//! The pure parts of routing a tunnel: which routes to add, and how to read the gateway they
//! must not disturb. Running `ip`, `netsh` or a privileged helper stays in each client.

use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use std::net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const fn v4(addr: Ipv4Addr, prefix: u8) -> IpNetwork {
    match Ipv4Network::new_checked(addr, prefix) {
        Some(network) => IpNetwork::V4(network),
        None => panic!("prefix out of range"),
    }
}

const fn v6(addr: Ipv6Addr, prefix: u8) -> IpNetwork {
    match Ipv6Network::new_checked(addr, prefix) {
        Some(network) => IpNetwork::V6(network),
        None => panic!("prefix out of range"),
    }
}

/// Everything, both families: the `AllowedIPs` a full-tunnel config carries, and what a config
/// without an `AllowedIPs` line means.
pub const CATCH_ALL: [IpNetwork; 2] = [v4(Ipv4Addr::UNSPECIFIED, 0), v6(Ipv6Addr::UNSPECIFIED, 0)];

/// The two halves of `0.0.0.0/0`. More specific than the system default route, so they win over
/// it without replacing it — and the default is still there when they are removed.
pub const CATCH_ALL_HALVES_V4: [IpNetwork; 2] = [
    v4(Ipv4Addr::UNSPECIFIED, 1),
    v4(Ipv4Addr::new(128, 0, 0, 0), 1),
];

/// The two halves of `::/0`.
pub const CATCH_ALL_HALVES_V6: [IpNetwork; 2] = [
    v6(Ipv6Addr::UNSPECIFIED, 1),
    v6(Ipv6Addr::new(0x8000, 0, 0, 0, 0, 0, 0, 0), 1),
];

/// Address ranges that belong to the device's local network and should remain reachable through
/// the physical interface when local-network access is enabled.
pub const LOCAL_NETWORKS: [IpNetwork; 6] = [
    v4(Ipv4Addr::new(10, 0, 0, 0), 8),
    v4(Ipv4Addr::new(172, 16, 0, 0), 12),
    v4(Ipv4Addr::new(192, 168, 0, 0), 16),
    v4(Ipv4Addr::new(169, 254, 0, 0), 16),
    v6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    v6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
];

/// The single-host route covering `endpoint`: `/32` or `/128` by address family. Pinned to the
/// physical gateway so the tunnel's own packets never enter the tunnel.
pub fn endpoint_route(endpoint: IpAddr) -> IpNetwork {
    match endpoint {
        IpAddr::V4(ip) => v4(ip, 32),
        IpAddr::V6(ip) => v6(ip, 128),
    }
}

/// The two `/1` halves of `network` if it is a `/0` of either family, `None` otherwise.
pub fn catch_all_halves(network: IpNetwork) -> Option<[IpNetwork; 2]> {
    match network {
        IpNetwork::V4(n) if n.prefix() == 0 => Some(CATCH_ALL_HALVES_V4),
        IpNetwork::V6(n) if n.prefix() == 0 => Some(CATCH_ALL_HALVES_V6),
        _ => None,
    }
}

/// The routes to install for `allowed_ips`: every `/0` split into its halves, everything else
/// as is, IPv6 dropped entirely when the host has it disabled.
pub fn split_default(allowed_ips: &[IpNetwork], include_ipv6: bool) -> Vec<IpNetwork> {
    allowed_ips
        .iter()
        .filter(|network| include_ipv6 || network.is_ipv4())
        .flat_map(|&network| match catch_all_halves(network) {
            Some(halves) => halves.to_vec(),
            None => vec![network],
        })
        .collect()
}

/// Build positive VPN routes while leaving private and link-local destinations outside them.
///
/// Android versions before API 33 have no `VpnService.Builder.excludeRoute`, so the portable way
/// to bypass LANs is to express the complement as positive routes. The same result is also useful
/// to platforms that install routes directly.
pub fn exclude_local_networks(allowed_ips: &[IpNetwork], include_ipv6: bool) -> Vec<IpNetwork> {
    let mut routes = allowed_ips
        .iter()
        .copied()
        .filter(|network| include_ipv6 || network.is_ipv4())
        .collect::<Vec<_>>();

    for excluded in LOCAL_NETWORKS {
        routes = routes
            .into_iter()
            .flat_map(|route| subtract_network(route, excluded))
            .collect();
    }
    routes
}

fn subtract_network(route: IpNetwork, excluded: IpNetwork) -> Vec<IpNetwork> {
    match (route, excluded) {
        (IpNetwork::V4(route), IpNetwork::V4(excluded)) => subtract_v4(route, excluded)
            .into_iter()
            .map(IpNetwork::V4)
            .collect(),
        (IpNetwork::V6(route), IpNetwork::V6(excluded)) => subtract_v6(route, excluded)
            .into_iter()
            .map(IpNetwork::V6)
            .collect(),
        (route, _) => vec![route],
    }
}

fn subtract_v4(route: Ipv4Network, excluded: Ipv4Network) -> Vec<Ipv4Network> {
    let route_start = u32::from(route.network());
    let route_end = u32::from(route.broadcast());
    let excluded_start = u32::from(excluded.network());
    let excluded_end = u32::from(excluded.broadcast());
    if route_end < excluded_start || excluded_end < route_start {
        return vec![route];
    }
    if excluded_start <= route_start && route_end <= excluded_end {
        return Vec::new();
    }

    let prefix = route.prefix() + 1;
    let half = 1_u32 << (32 - prefix);
    let left = Ipv4Network::new_checked(route.network(), prefix).expect("split prefix is valid");
    let right = Ipv4Network::new_checked(Ipv4Addr::from(route_start + half), prefix)
        .expect("split prefix is valid");
    let mut result = subtract_v4(left, excluded);
    result.extend(subtract_v4(right, excluded));
    result
}

fn subtract_v6(route: Ipv6Network, excluded: Ipv6Network) -> Vec<Ipv6Network> {
    let route_start = u128::from(route.network());
    let route_end = u128::from(route.broadcast());
    let excluded_start = u128::from(excluded.network());
    let excluded_end = u128::from(excluded.broadcast());
    if route_end < excluded_start || excluded_end < route_start {
        return vec![route];
    }
    if excluded_start <= route_start && route_end <= excluded_end {
        return Vec::new();
    }

    let prefix = route.prefix() + 1;
    let half = 1_u128 << (128 - prefix);
    let left = Ipv6Network::new_checked(route.network(), prefix).expect("split prefix is valid");
    let right = Ipv6Network::new_checked(Ipv6Addr::from(route_start + half), prefix)
        .expect("split prefix is valid");
    let mut result = subtract_v6(left, excluded);
    result.extend(subtract_v6(right, excluded));
    result
}

/// Pick the address to use from a resolved endpoint, preferring IPv4 when both exist: the host
/// route is easier to pin (every host has a v4 default route) and both clients behave the same.
pub fn pick_endpoint(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs.into_iter().collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

/// A default route's next hop: the gateway address and the interface it is reached on. The
/// interface matters for IPv6, where the gateway is almost always link-local (`fe80::…`) and a
/// route `via` it is rejected without a `dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gateway {
    /// The next-hop address.
    pub via: IpAddr,
    /// The interface the next hop is on.
    pub dev: String,
}

/// Why a line of `ip route show default` could not be read as a [`Gateway`].
///
/// A next hop that is not an address is an error, not a gateway: the string used to be passed on
/// to `ip route add` as-is and only rejected there.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayParseError {
    /// The word after `via` is not an IP address.
    #[error("unparseable default gateway {text:?}: {source}")]
    NextHop {
        /// The word as printed.
        text: String,
        /// Why it is not an address.
        source: AddrParseError,
    },
    /// A `via` route without a `dev`, which `ip` never prints but the parser should not guess at.
    #[error("default route {line:?} names no device")]
    MissingDevice {
        /// The line as printed.
        line: String,
    },
}

/// The first route in `ip route show default` output that goes `via` a gateway.
///
/// `ip` lists routes lowest metric first, so the first is the one traffic actually takes. A
/// default route without `via` (an on-link one, as a ppp or tun device gets) is skipped: it has no
/// gateway to pin an endpoint route to.
pub fn parse_default_route(output: &str) -> Result<Option<Gateway>, GatewayParseError> {
    output
        .lines()
        .find_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let after = |key: &str| {
                words
                    .iter()
                    .position(|&w| w == key)
                    .and_then(|i| words.get(i + 1).copied())
            };
            let via = after("via")?;
            Some(
                via.parse()
                    .map_err(|source| GatewayParseError::NextHop {
                        text: via.to_string(),
                        source,
                    })
                    .and_then(|via| {
                        let dev = after("dev").ok_or_else(|| GatewayParseError::MissingDevice {
                            line: line.trim().to_string(),
                        })?;
                        Ok(Gateway {
                            via,
                            dev: dev.to_string(),
                        })
                    }),
            )
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> IpNetwork {
        s.parse().unwrap()
    }

    #[test]
    fn endpoint_route_prefix_follows_address_family() {
        assert_eq!(
            endpoint_route("1.2.3.4".parse().unwrap()),
            net("1.2.3.4/32")
        );
        assert_eq!(
            endpoint_route("2001:db8::1".parse().unwrap()),
            net("2001:db8::1/128")
        );
    }

    #[test]
    fn the_catch_all_pairs_are_what_they_say() {
        assert_eq!(CATCH_ALL, [net("0.0.0.0/0"), net("::/0")]);
        assert_eq!(
            catch_all_halves(net("0.0.0.0/0")),
            Some([net("0.0.0.0/1"), net("128.0.0.0/1")])
        );
        assert_eq!(
            catch_all_halves(net("::/0")),
            Some([net("::/1"), net("8000::/1")])
        );
        assert_eq!(catch_all_halves(net("10.0.0.0/8")), None);
        assert_eq!(catch_all_halves(net("fd00::/8")), None);
    }

    #[test]
    fn split_default_splits_only_the_zero_prefixes_and_can_drop_ipv6() {
        assert_eq!(
            split_default(&[net("0.0.0.0/0")], false),
            vec![net("0.0.0.0/1"), net("128.0.0.0/1")]
        );
        let given = vec![net("10.0.0.0/8"), net("192.168.1.0/24")];
        assert_eq!(split_default(&given, false), given);

        let given = vec![net("0.0.0.0/0"), net("::/0"), net("fd00::/8")];
        assert_eq!(
            split_default(&given, false),
            vec![net("0.0.0.0/1"), net("128.0.0.0/1")]
        );
        assert_eq!(
            split_default(&given, true),
            vec![
                net("0.0.0.0/1"),
                net("128.0.0.0/1"),
                net("::/1"),
                net("8000::/1"),
                net("fd00::/8"),
            ]
        );
    }

    #[test]
    fn local_networks_are_absent_from_the_positive_route_complement() {
        let routes = exclude_local_networks(&CATCH_ALL, true);
        for local in LOCAL_NETWORKS {
            let probe = local.ip();
            assert!(
                !routes.iter().any(|route| route.contains(probe)),
                "{probe} is still covered by {routes:?}"
            );
        }
        for public in ["1.1.1.1", "8.8.8.8", "2001:4860:4860::8888"] {
            let public: IpAddr = public.parse().unwrap();
            assert!(routes.iter().any(|route| route.contains(public)));
        }
    }

    #[test]
    fn pick_endpoint_prefers_ipv4() {
        let v6: SocketAddr = "[2001:db8::1]:51820".parse().unwrap();
        let v4: SocketAddr = "1.2.3.4:51820".parse().unwrap();
        assert_eq!(pick_endpoint([v6, v4]), Some(v4));
        assert_eq!(pick_endpoint([v6]), Some(v6));
        assert_eq!(pick_endpoint([]), None);
    }

    #[test]
    fn default_route_parses_gateway_and_device() {
        // IPv4, with the trailing attributes `ip` prints
        assert_eq!(
            parse_default_route(
                "default via 192.168.1.1 dev wlan0 proto dhcp src 192.168.1.7 metric 600 \n"
            ),
            Ok(Some(Gateway {
                via: "192.168.1.1".parse().unwrap(),
                dev: "wlan0".into(),
            }))
        );
        // IPv6: link-local gateway, only usable together with its device
        assert_eq!(
            parse_default_route(
                "default via fe80::1 dev eth0 proto ra metric 1024 expires 1797sec hoplimit 64 pref medium\n"
            ),
            Ok(Some(Gateway {
                via: "fe80::1".parse().unwrap(),
                dev: "eth0".into(),
            }))
        );
        // Several default routes: the first `via` one wins (lowest metric); an on-link route
        // without `via` (a ppp/tun default) is skipped
        assert_eq!(
            parse_default_route(
                "default dev ppp0 scope link\ndefault via 10.0.0.1 dev eth0 metric 100\ndefault via 10.0.0.2 dev eth1 metric 200\n"
            ),
            Ok(Some(Gateway {
                via: "10.0.0.1".parse().unwrap(),
                dev: "eth0".into(),
            }))
        );
        assert_eq!(parse_default_route(""), Ok(None));
        assert_eq!(
            parse_default_route("default dev ppp0 scope link\n"),
            Ok(None)
        );
    }

    #[test]
    fn a_next_hop_that_is_not_an_address_is_an_error_not_a_gateway() {
        assert!(matches!(
            parse_default_route("default via garbage dev eth0"),
            Err(GatewayParseError::NextHop { text, .. }) if text == "garbage"
        ));
        assert!(matches!(
            parse_default_route("default via 10.0.0.1"),
            Err(GatewayParseError::MissingDevice { .. })
        ));
    }
}
