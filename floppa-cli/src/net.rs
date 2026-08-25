//! Routing for a connection: the host route pinning the VPN endpoint to the physical gateway,
//! the routes through the TUN interface, and their symmetric teardown.
//!
//! Shared by the WireGuard/AmneziaWG and VLESS paths; the interface address/MTU is the tunnel's
//! own business (gotatun's TUN is configured here by `tunnel`, shoes-lite configures its own).

use anyhow::{Result, anyhow, bail};
use floppa_tunnel_config::route::{self, Gateway};
use ipnetwork::IpNetwork;
use std::net::IpAddr;
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

/// The `via <gw> dev <dev>` tail of an `ip route` command.
fn gateway_args(gateway: &Gateway) -> [String; 4] {
    [
        "via".into(),
        gateway.via.to_string(),
        "dev".into(),
        gateway.dev.clone(),
    ]
}

/// The default gateway of `endpoint`'s address family (the host route must use the same one).
fn default_gateway(endpoint: IpAddr) -> Result<Option<Gateway>> {
    let family = if endpoint.is_ipv4() { "-4" } else { "-6" };
    let output = Command::new("ip")
        .args([family, "route", "show", "default"])
        .output()?;
    Ok(route::parse_default_route(&String::from_utf8_lossy(
        &output.stdout,
    ))?)
}

/// Host route that keeps the endpoint reachable through the physical gateway once the catch-all
/// routes point at the tunnel. Deleted with the same `via`/`dev` it was added with, so a route
/// the system installed after a roaming event is left alone.
struct EndpointRoute {
    destination: IpNetwork,
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
            let destination = route.destination.to_string();
            let gateway = gateway_args(&route.gateway);
            let mut args = vec!["route", "del", &destination];
            args.extend(gateway.iter().map(String::as_str));
            if let Err(e) = run_ip(&args) {
                errors.push(e);
            }
        }
        collect_errors(errors)
    }
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
    let destination = route::endpoint_route(endpoint);
    let destination_text = destination.to_string();
    let gateway_text = gateway_args(&gateway);
    let mut args = vec!["route", "replace", &destination_text];
    args.extend(gateway_text.iter().map(String::as_str));
    run_ip(&args)?;
    eprintln!(
        "Endpoint route: {destination} via {} dev {}",
        gateway.via, gateway.dev
    );
    applied.endpoint_route = Some(EndpointRoute {
        destination,
        gateway,
    });

    for &network in allowed_ips {
        match route::catch_all_halves(network) {
            Some(halves) if network.is_ipv4() => {
                for half in halves {
                    applied.add_route(&half.to_string())?;
                }
            }
            Some(halves) => {
                // IPv6 may be disabled on the host; the tunnel still works for IPv4.
                for half in halves {
                    if let Err(e) = applied.add_route(&half.to_string()) {
                        eprintln!("Skipping IPv6 route {half}: {e:#}");
                    }
                }
            }
            None => applied.add_route(&network.to_string())?,
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
    fn gateway_args_carry_the_device() {
        let gw = Gateway {
            via: "fe80::1".parse().unwrap(),
            dev: "eth0".into(),
        };
        assert_eq!(gateway_args(&gw), ["via", "fe80::1", "dev", "eth0"]);
    }
}
