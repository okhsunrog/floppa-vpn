//! Windows platform implementation for VPN operations
//!
//! Uses netsh for network configuration:
//! - `netsh interface ip` for address configuration
//! - `netsh interface ip add route` for routing
//! - `netsh interface ip set dns` for DNS
//! - `route` command for endpoint host route

use super::{DnsSnapshot, Gateway, IpFamily, Platform, PlatformError, TunParams};
use crate::protocol::InterfaceName;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, info, warn};

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// How long an *undo* may wait on netsh/route. Apply calls are unbounded; an undo runs during
/// unwind and on exit, where a hung command used to hang the whole teardown.
const UNDO_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `command`, bounded by `timeout` when given. The child is killed on timeout.
async fn run_bounded(
    command: &mut Command,
    what: &str,
    timeout: Option<Duration>,
) -> Result<std::process::Output, PlatformError> {
    command.creation_flags(CREATE_NO_WINDOW).kill_on_drop(true);
    let spawned = command.output();
    match timeout {
        Some(limit) => tokio::time::timeout(limit, spawned).await.map_err(|_| {
            PlatformError::Failed(format!("{what} timed out after {}s", limit.as_secs()))
        })?,
        None => spawned.await,
    }
    .map_err(|e| PlatformError::Unavailable(format!("failed to run {what}: {e}")))
}

/// Windows platform implementation.
///
/// Holds no undo state. The previous version cached `interface_index`, but Wintun assigns a NEW
/// adapter index every time an adapter is created — so a cached index from an earlier attempt
/// pointed at an unrelated system interface, and routes and DNS were applied to it. The index is
/// now read fresh per attempt and carried in the rollback step that used it.
pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn new() -> Self {
        Self
    }

    /// Run a netsh command.
    ///
    /// Note the deliberate change: success is decided by the EXIT STATUS alone. The previous
    /// version also scanned stdout for the English substrings "error" and "failed", so on a
    /// localized Windows a successful command was reported as a failure — and, worse, a genuinely
    /// failed one on an English system reported failure after the change had been applied. Either
    /// way "returned Err" never implied "nothing happened", which is exactly why every rollback
    /// step is pushed before it is applied.
    async fn run_netsh(
        &self,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<String, PlatformError> {
        debug!("Running netsh: {args:?}");

        let output = run_bounded(Command::new("netsh").args(args), "netsh", timeout).await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status.success() {
            return Ok(stdout);
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        // netsh exits 1 for "requires elevation" among many other things; there is no dedicated
        // code, so elevation is detected by the caller running as a non-admin, not guessed here.
        Err(PlatformError::Failed(format!("netsh failed: {detail}")))
    }

    /// The index carried in the rollback step, or a fresh lookup by name.
    async fn index_or_lookup(
        &self,
        iface: &InterfaceName,
        if_index: Option<u32>,
    ) -> Result<u32, PlatformError> {
        match if_index {
            Some(idx) => Ok(idx),
            None => self.get_interface_index(iface.as_str()).await,
        }
    }

    /// Get interface index by name, read fresh from the OS every time.
    async fn get_interface_index(&self, iface: &str) -> Result<u32, PlatformError> {
        let output = self
            .run_netsh(&["interface", "ip", "show", "interfaces"], None)
            .await?;
        parse_interface_index(&output, iface)
    }

    /// Get the default gateway for `family` from the routing table.
    async fn get_default_gateway(family: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        let args: &[&str] = match family {
            IpFamily::V4 => &["print", "-4", "0.0.0.0"],
            IpFamily::V6 => &["print", "-6", "::/0"],
        };
        let output = run_bounded(Command::new("route").args(args), "route print", None).await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(match family {
            IpFamily::V4 => parse_default_gateway_v4(&stdout),
            IpFamily::V6 => parse_default_gateway_v6(&stdout),
        })
    }
}

/// The index of the interface named exactly `iface` in `netsh interface ip show interfaces`.
///
/// Exact, not `contains`: `floppa0` is a substring of `floppa01`, and a stale adapter with a
/// longer name matched first and received the routes.
fn parse_interface_index(output: &str, iface: &str) -> Result<u32, PlatformError> {
    // Format: "    Idx     Met         MTU          State                Name"
    // Line:   "     12    4250        1500  connected     floppa0"
    // The name is everything after the fourth column; it may contain spaces.
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(idx) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        if rest.len() >= 4 && rest[3..].join(" ") == iface {
            return Ok(idx);
        }
    }
    Err(PlatformError::Failed(format!(
        "interface {iface} not found"
    )))
}

/// The IPv4 default gateway with the lowest metric.
///
/// `route print` lists every 0.0.0.0/0 entry, and with more than one active default (Wi-Fi plus
/// Ethernet, a second VPN) the first line is not the one in use. Previously the first line won.
fn parse_default_gateway_v4(stdout: &str) -> Option<Gateway> {
    // "0.0.0.0    0.0.0.0    192.168.1.1    192.168.1.10    25"
    stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 || parts[0] != "0.0.0.0" || parts[1] != "0.0.0.0" {
                return None;
            }
            let gateway: Gateway = parts[2].parse().ok()?;
            let metric: u32 = parts[4].parse().ok()?;
            Some((metric, gateway))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}

/// The IPv6 default gateway with the lowest metric.
fn parse_default_gateway_v6(stdout: &str) -> Option<Gateway> {
    // " 12    281 ::/0    fe80::1"   (If  Metric  Network Destination  Gateway)
    stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 || parts[2] != "::/0" {
                return None;
            }
            let metric: u32 = parts[1].parse().ok()?;
            let gateway: Gateway = parts[3].parse().ok()?;
            Some((metric, gateway))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gateway)| gateway)
}

/// Flush the Windows DNS resolver cache
async fn flush_dns_cache() {
    match run_bounded(
        Command::new("ipconfig").arg("/flushdns"),
        "ipconfig /flushdns",
        Some(UNDO_TIMEOUT),
    )
    .await
    {
        Ok(output) if output.status.success() => debug!("Flushed DNS cache"),
        Ok(output) => warn!(
            "ipconfig /flushdns failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(e) => warn!("Failed to run ipconfig /flushdns: {e}"),
    }
}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Platform for WindowsPlatform {
    fn tun_params(&self) -> TunParams {
        TunParams {
            manage_device: true,
            fwmark: None,
            wintun_file: wintun_path(),
        }
    }

    async fn preflight(&self) -> Result<(), PlatformError> {
        // netsh needs an elevated token; probe with a harmless read so the failure surfaces
        // before anything is mutated rather than midway up the ladder.
        self.run_netsh(&["interface", "ip", "show", "interfaces"], None)
            .await
            .map(|_| ())
    }

    async fn prepare_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        // Close any stale Wintun adapter left by a crash or a force-kill. Without this the
        // adapter lingers and the next session fails to create one with the same name.
        let Some(path) = wintun_path() else {
            warn!("wintun.dll not found next to the executable");
            return Ok(());
        };
        match unsafe { wintun_bindings::load_from_path(&path) } {
            Ok(wintun) => {
                if let Ok(adapter) = wintun_bindings::Adapter::open(&wintun, iface.as_str()) {
                    info!("Found stale Wintun adapter '{iface}', closing it");
                    drop(adapter); // WintunCloseAdapter runs in Drop
                }
            }
            Err(e) => warn!("Failed to load wintun.dll for stale adapter cleanup: {e}"),
        }
        Ok(())
    }

    async fn release_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        // The adapter is destroyed when the backend drops its DeviceHandle, which the
        // StartBackend step below this one on the stack already does.
        debug!("Windows: adapter for {iface} is released with the backend");
        Ok(())
    }

    async fn configure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        info!("Configuring address {addr} on interface {iface}");
        self.run_netsh(
            &[
                "interface",
                "ip",
                "set",
                "address",
                &format!("name={iface}"),
                "source=static",
                &format!("addr={}", addr.ip()),
                &format!("mask={}", addr.mask()),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn deconfigure_address(
        &self,
        iface: &InterfaceName,
        _addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        // No-op ONLY because Step::StartBackend sits below this on the stack: dropping the
        // Wintun adapter takes its addresses with it. If the ladder is ever reordered so that
        // the address outlives the adapter, this must start deleting the address explicitly.
        debug!("Windows: address on {iface} is released with the adapter");
        Ok(())
    }

    async fn default_gateway(&self, family: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        Self::get_default_gateway(family).await
    }

    async fn interface_index(&self, iface: &InterfaceName) -> Option<u32> {
        self.get_interface_index(iface.as_str()).await.ok()
    }

    async fn add_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        let gateway = gateway.ok_or_else(|| {
            PlatformError::Failed("no default gateway; cannot pin the endpoint route".to_string())
        })?;
        info!("Adding endpoint route: {endpoint} via {gateway}");

        if endpoint.is_ipv4() {
            let output = run_bounded(
                Command::new("route").args([
                    "add",
                    &endpoint.to_string(),
                    "mask",
                    "255.255.255.255",
                    &gateway.to_string(),
                ]),
                "route add",
                None,
            )
            .await?;
            if !output.status.success() {
                return Err(PlatformError::Failed(format!(
                    "route add failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(())
        } else {
            self.run_netsh(
                &[
                    "interface",
                    "ipv6",
                    "add",
                    "route",
                    &format!("{endpoint}/128"),
                    &format!("nexthop={gateway}"),
                ],
                None,
            )
            .await
            .map(|_| ())
        }
    }

    async fn remove_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        info!("Removing endpoint route: {endpoint}");
        if endpoint.is_ipv4() {
            let mut args = vec!["delete".to_string(), endpoint.to_string()];
            // Scope the delete to the gateway the route was added with, so a route installed by
            // something else to the same destination is left alone.
            if let Some(gw) = gateway {
                args.push("mask".into());
                args.push("255.255.255.255".into());
                args.push(gw.to_string());
            }
            let output = run_bounded(
                Command::new("route").args(&args),
                "route delete",
                Some(UNDO_TIMEOUT),
            )
            .await?;
            if !output.status.success() {
                // Already gone is the common case and is not a failure worth retrying.
                debug!(
                    "route delete reported: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        } else {
            let _ = self
                .run_netsh(
                    &[
                        "interface",
                        "ipv6",
                        "delete",
                        "route",
                        &format!("{endpoint}/128"),
                    ],
                    Some(UNDO_TIMEOUT),
                )
                .await;
            Ok(())
        }
    }

    async fn add_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if routes.is_empty() {
            return Ok(());
        }
        let idx = self.index_or_lookup(iface, if_index).await?.to_string();

        info!("Adding {} routes via interface {iface}", routes.len());
        // The list arrives already split by `split_default`, so no /0 reaches netsh.
        for network in routes {
            let proto = if network.is_ipv4() { "ip" } else { "ipv6" };
            self.run_netsh(
                &[
                    "interface",
                    proto,
                    "add",
                    "route",
                    &network.to_string(),
                    &idx,
                ],
                None,
            )
            .await?;
        }
        Ok(())
    }

    async fn remove_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if routes.is_empty() {
            return Ok(());
        }
        let Ok(idx) = self.index_or_lookup(iface, if_index).await else {
            // The adapter is already gone, and its routes went with it.
            debug!("Windows: no interface index for {iface}; routes already released");
            return Ok(());
        };
        let idx = idx.to_string();

        info!("Removing {} routes via interface {iface}", routes.len());
        // Exactly the routes that were added — not the hardcoded four /1s the previous version
        // deleted, which left any non-default AllowedIP installed forever.
        for network in routes {
            let proto = if network.is_ipv4() { "ip" } else { "ipv6" };
            let _ = self
                .run_netsh(
                    &[
                        "interface",
                        proto,
                        "delete",
                        "route",
                        &network.to_string(),
                        &idx,
                    ],
                    Some(UNDO_TIMEOUT),
                )
                .await;
        }
        Ok(())
    }

    async fn capture_dns(
        &self,
        _iface: &InterfaceName,
        _if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        // Windows restores by handing the interface back to DHCP; there is nothing to carry.
        Ok(DnsSnapshot::Dhcp)
    }

    async fn configure_dns(
        &self,
        iface: &InterfaceName,
        servers: &[IpAddr],
        if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if servers.is_empty() {
            info!("No DNS servers to configure");
            return Ok(());
        }
        let idx = self.index_or_lookup(iface, if_index).await?.to_string();

        info!("Configuring DNS servers: {servers:?}");
        let (v4, v6): (Vec<&IpAddr>, Vec<&IpAddr>) = servers.iter().partition(|s| s.is_ipv4());

        for (proto, list) in [("ipv4", v4), ("ipv6", v6)] {
            let Some((first, rest)) = list.split_first() else {
                continue;
            };
            self.run_netsh(
                &[
                    "interface",
                    proto,
                    "set",
                    "dnsservers",
                    &format!("name={idx}"),
                    "source=static",
                    &format!("address={first}"),
                    "validate=no",
                ],
                None,
            )
            .await?;
            for server in rest {
                self.run_netsh(
                    &[
                        "interface",
                        proto,
                        "add",
                        "dnsservers",
                        &format!("name={idx}"),
                        &format!("address={server}"),
                        "validate=no",
                    ],
                    None,
                )
                .await?;
            }
        }

        flush_dns_cache().await;
        Ok(())
    }

    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        _snapshot: &DnsSnapshot,
        if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        info!("Restoring DNS configuration for {iface}");
        let Ok(idx) = self.index_or_lookup(iface, if_index).await else {
            debug!("Windows: no interface index for {iface}; DNS already released");
            return Ok(());
        };
        let idx = idx.to_string();

        for proto in ["ipv4", "ipv6"] {
            let _ = self
                .run_netsh(
                    &[
                        "interface",
                        proto,
                        "set",
                        "dnsservers",
                        &format!("name={idx}"),
                        "source=dhcp",
                    ],
                    Some(UNDO_TIMEOUT),
                )
                .await;
        }
        flush_dns_cache().await;
        Ok(())
    }

    async fn ipv6_enabled(&self) -> bool {
        true
    }
}

/// `wintun.dll` ships next to the executable.
fn wintun_path() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wintun.dll")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERFACES: &str = "\n\
Idx     Met         MTU          State                Name\n\
---  ----------  ----------  ------------  ---------------------------\n\
  1          75  4294967295  connected     Loopback Pseudo-Interface 1\n\
 17          25        1500  connected     floppa01\n\
 12        4250        1500  connected     floppa0\n";

    #[test]
    fn interface_index_matches_the_exact_name() {
        assert_eq!(parse_interface_index(INTERFACES, "floppa0").unwrap(), 12);
        assert_eq!(parse_interface_index(INTERFACES, "floppa01").unwrap(), 17);
        assert_eq!(
            parse_interface_index(INTERFACES, "Loopback Pseudo-Interface 1").unwrap(),
            1
        );
        assert!(parse_interface_index(INTERFACES, "floppa").is_err());
    }

    #[test]
    fn v4_gateway_is_the_lowest_metric() {
        let out = "\
Active Routes:\n\
Network Destination        Netmask          Gateway       Interface  Metric\n\
          0.0.0.0          0.0.0.0      10.0.0.1        10.0.0.5     55\n\
          0.0.0.0          0.0.0.0   192.168.1.1    192.168.1.10     25\n\
        127.0.0.0        255.0.0.0         On-link       127.0.0.1    331\n";
        assert_eq!(
            parse_default_gateway_v4(out),
            Some("192.168.1.1".parse().unwrap())
        );
    }

    #[test]
    fn v6_gateway_is_the_lowest_metric() {
        let out = "\
Active Routes:\n\
 If Metric Network Destination      Gateway\n\
 12    281 ::/0                     fe80::1\n\
 17     55 ::/0                     fe80::2\n\
  1    331 ::1/128                  On-link\n";
        assert_eq!(
            parse_default_gateway_v6(out),
            Some("fe80::2".parse().unwrap())
        );
    }
}
