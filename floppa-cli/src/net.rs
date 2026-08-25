//! Routing for a connection: the host route pinning the VPN endpoint to the physical gateway,
//! the routes through the TUN interface, and their symmetric teardown.
//!
//! Shared by the WireGuard/AmneziaWG and VLESS paths; the interface address/MTU is the tunnel's
//! own business (gotatun's TUN is configured here by `tunnel`, shoes-lite configures its own).

use anyhow::{Result, anyhow, bail};
use ipnetwork::IpNetwork;
use std::net::{IpAddr, SocketAddr};
use std::process::Command;

pub fn run_ip(args: &[&str]) -> Result<()> {
    let output = Command::new("ip").args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!("ip {} failed: {}", args.join(" "), stderr.trim()))
    }
}

/// A default route's next hop: the gateway address and the interface it is reached on. The
/// interface matters for IPv6, where the gateway is almost always link-local (`fe80::…`) and a
/// route `via` it is rejected without a `dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gateway {
    via: String,
    dev: String,
}

impl Gateway {
    /// The `via <gw> dev <dev>` tail of an `ip route` command.
    fn args(&self) -> [&str; 4] {
        ["via", &self.via, "dev", &self.dev]
    }
}

/// The first route in `ip route show default` output that has both a `via` and a `dev`.
fn parse_default_route(output: &str) -> Option<Gateway> {
    output.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let after = |key: &str| {
            words
                .iter()
                .position(|&w| w == key)
                .and_then(|i| words.get(i + 1))
                .map(|s| s.to_string())
        };
        Some(Gateway {
            via: after("via")?,
            dev: after("dev")?,
        })
    })
}

/// The default gateway of `endpoint`'s address family (the host route must use the same one).
fn default_gateway(endpoint: IpAddr) -> Result<Option<Gateway>> {
    let family = if endpoint.is_ipv4() { "-4" } else { "-6" };
    let output = Command::new("ip")
        .args([family, "route", "show", "default"])
        .output()?;
    Ok(parse_default_route(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Host route that keeps the endpoint reachable through the physical gateway once the catch-all
/// routes point at the tunnel. Deleted with the same `via`/`dev` it was added with, so a route
/// the system installed after a roaming event is left alone.
struct EndpointRoute {
    destination: String,
    gateway: Gateway,
}

/// Everything `configure_routes` added, in the form needed to remove it again.
pub struct AppliedNetworking {
    interface: String,
    endpoint_route: Option<EndpointRoute>,
    /// Destinations routed `dev <interface>`, in the order they were added.
    routes: Vec<String>,
}

impl AppliedNetworking {
    fn new(interface: &str) -> Self {
        Self {
            interface: interface.to_string(),
            endpoint_route: None,
            routes: Vec::new(),
        }
    }

    fn add_route(&mut self, destination: &str) -> Result<()> {
        run_ip(&["route", "replace", destination, "dev", &self.interface])?;
        self.routes.push(destination.to_string());
        Ok(())
    }

    /// Remove everything that was added, in reverse order. Every step runs even if an earlier
    /// one fails; the errors are collected. A second call is a no-op.
    pub fn teardown(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for destination in self.routes.drain(..).rev() {
            if let Err(e) = run_ip(&["route", "del", &destination, "dev", &self.interface]) {
                errors.push(e);
            }
        }
        if let Some(route) = self.endpoint_route.take() {
            let mut args = vec!["route", "del", &route.destination];
            args.extend(route.gateway.args());
            if let Err(e) = run_ip(&args) {
                errors.push(e);
            }
        }
        collect_errors(errors)
    }
}

/// Host route for `endpoint`: `/32` or `/128` by address family.
fn endpoint_route(endpoint: IpAddr) -> String {
    let prefix = if endpoint.is_ipv4() { 32 } else { 128 };
    format!("{endpoint}/{prefix}")
}

/// Pick the address to use from a resolved endpoint, preferring IPv4 when both exist: the host
/// route is easier to pin (every host has a v4 default route) and it matches the desktop client.
pub fn pick_endpoint(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs.into_iter().collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

/// Pin `endpoint` to the default gateway, then route `allowed_ips` through `interface`.
/// A `/0` network becomes the two half-routes so it wins over the existing default route.
/// Without a default gateway nothing is applied: catch-all routes with an unpinned endpoint
/// would send the tunnel's own packets into the tunnel.
/// On failure, whatever was already applied is removed before the error is returned.
pub fn configure_routes(
    endpoint: IpAddr,
    allowed_ips: &[IpNetwork],
    interface: &str,
) -> Result<AppliedNetworking> {
    let mut applied = AppliedNetworking::new(interface);
    if let Err(e) = apply_routes(&mut applied, endpoint, allowed_ips) {
        if let Err(cleanup) = applied.teardown() {
            eprintln!("Failed to undo partial route setup: {cleanup:#}");
        }
        return Err(e);
    }
    Ok(applied)
}

fn apply_routes(
    applied: &mut AppliedNetworking,
    endpoint: IpAddr,
    allowed_ips: &[IpNetwork],
) -> Result<()> {
    let Some(gateway) = default_gateway(endpoint)? else {
        bail!("No default gateway for {endpoint}; cannot pin the endpoint route");
    };
    let destination = endpoint_route(endpoint);
    let mut args = vec!["route", "replace", &destination];
    args.extend(gateway.args());
    run_ip(&args)?;
    eprintln!(
        "Endpoint route: {destination} via {} dev {}",
        gateway.via, gateway.dev
    );
    applied.endpoint_route = Some(EndpointRoute {
        destination,
        gateway,
    });

    for network in allowed_ips {
        if network.prefix() == 0 {
            if network.is_ipv4() {
                applied.add_route("0.0.0.0/1")?;
                applied.add_route("128.0.0.0/1")?;
            } else {
                // IPv6 may be disabled on the host; the tunnel still works for IPv4.
                for destination in ["::/1", "8000::/1"] {
                    if let Err(e) = applied.add_route(destination) {
                        eprintln!("Skipping IPv6 route {destination}: {e:#}");
                    }
                }
            }
        } else {
            applied.add_route(&network.to_string())?;
        }
    }
    Ok(())
}

/// Fold the errors of independent teardown steps into one, so a failing step never hides the
/// ones after it.
pub fn collect_errors(errors: Vec<anyhow::Error>) -> Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors
        .iter()
        .map(|e| format!("{e:#}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(anyhow!(joined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_route_prefix_follows_address_family() {
        assert_eq!(endpoint_route("1.2.3.4".parse().unwrap()), "1.2.3.4/32");
        assert_eq!(
            endpoint_route("2001:db8::1".parse().unwrap()),
            "2001:db8::1/128"
        );
    }

    #[test]
    fn default_route_parses_gateway_and_device() {
        // IPv4, with the trailing attributes `ip` prints
        assert_eq!(
            parse_default_route(
                "default via 192.168.1.1 dev wlan0 proto dhcp src 192.168.1.7 metric 600 \n"
            ),
            Some(Gateway {
                via: "192.168.1.1".into(),
                dev: "wlan0".into(),
            })
        );
        // IPv6: link-local gateway, only usable together with its device
        assert_eq!(
            parse_default_route(
                "default via fe80::1 dev eth0 proto ra metric 1024 expires 1797sec hoplimit 64 pref medium\n"
            ),
            Some(Gateway {
                via: "fe80::1".into(),
                dev: "eth0".into(),
            })
        );
        // Several default routes: the first complete one wins; an on-link route without `via`
        // (a ppp/tun default) is skipped
        assert_eq!(
            parse_default_route(
                "default dev ppp0 scope link\ndefault via 10.0.0.1 dev eth0 metric 100\n"
            ),
            Some(Gateway {
                via: "10.0.0.1".into(),
                dev: "eth0".into(),
            })
        );
        assert_eq!(parse_default_route(""), None);
        assert_eq!(parse_default_route("default dev ppp0 scope link\n"), None);
    }

    #[test]
    fn gateway_args_carry_the_device() {
        let gw = Gateway {
            via: "fe80::1".into(),
            dev: "eth0".into(),
        };
        assert_eq!(gw.args(), ["via", "fe80::1", "dev", "eth0"]);
    }

    #[test]
    fn pick_endpoint_prefers_ipv4() {
        let v6: SocketAddr = "[2001:db8::1]:51820".parse().unwrap();
        let v4: SocketAddr = "1.2.3.4:51820".parse().unwrap();
        assert_eq!(pick_endpoint([v6, v4]), Some(v4));
        assert_eq!(pick_endpoint([v6]), Some(v6));
        assert_eq!(pick_endpoint([]), None);
    }
}
