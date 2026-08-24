//! Windows platform implementation for VPN operations
//!
//! Uses netsh for network configuration:
//! - `netsh interface ip` for address configuration
//! - `netsh interface ip add route` for routing
//! - `netsh interface ip set dns` for DNS
//! - `route` command for endpoint host route

use super::{DnsSnapshot, Gateway, Platform, PlatformError, TunParams};
use crate::vpn::protocol::InterfaceName;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::os::windows::process::CommandExt;
use std::process::Command;
use tracing::{debug, info, warn};

const CREATE_NO_WINDOW: u32 = 0x08000000;

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
    fn run_netsh(&self, args: &[&str]) -> Result<String, PlatformError> {
        debug!("Running netsh: {args:?}");

        let output = Command::new("netsh")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| PlatformError::Unavailable(format!("failed to run netsh: {e}")))?;

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

    /// Get interface index by name, read fresh from the OS every time.
    fn get_interface_index(&self, iface: &str) -> Result<u32, PlatformError> {
        // Try to parse from netsh output
        let output = self.run_netsh(&["interface", "ip", "show", "interfaces"])?;

        for line in output.lines() {
            if line.contains(iface) {
                // Format: "    Idx     Met         MTU          State                Name"
                // Line:   "     12    4250        1500  connected     floppa0"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    if let Ok(idx) = parts[0].parse::<u32>() {
                        return Ok(idx);
                    }
                }
            }
        }

        Err(PlatformError::Failed(format!(
            "interface {iface} not found"
        )))
    }

    /// Get the default gateway IP from the routing table
    fn get_default_gateway() -> Result<Option<String>, PlatformError> {
        let output = Command::new("cmd")
            .args(["/C", "route", "print", "0.0.0.0"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| PlatformError::Failed(format!("failed to read default route: {e}")))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse "0.0.0.0    0.0.0.0    192.168.1.1    ..." from Active Routes section
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 && parts[0] == "0.0.0.0" && parts[1] == "0.0.0.0" {
                return Ok(Some(parts[2].to_string()));
            }
        }
        Ok(None)
    }
}

/// Flush the Windows DNS resolver cache
fn flush_dns_cache() {
    match Command::new("ipconfig")
        .arg("/flushdns")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) if output.status.success() => debug!("Flushed DNS cache"),
        Ok(output) => warn!(
            "ipconfig /flushdns failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(e) => warn!("Failed to run ipconfig /flushdns: {}", e),
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
        self.run_netsh(&["interface", "ip", "show", "interfaces"])
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
        self.run_netsh(&[
            "interface",
            "ip",
            "set",
            "address",
            &format!("name={iface}"),
            "source=static",
            &format!("addr={}", addr.ip()),
            &format!("mask={}", addr.mask()),
        ])
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

    async fn default_gateway(&self) -> Result<Option<Gateway>, PlatformError> {
        Ok(Self::get_default_gateway()?.map(Gateway))
    }

    async fn interface_index(&self, iface: &InterfaceName) -> Option<u32> {
        self.get_interface_index(iface.as_str()).ok()
    }

    async fn add_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        let gateway = gateway.ok_or_else(|| {
            PlatformError::Failed("no default gateway; cannot pin the endpoint route".to_string())
        })?;
        info!("Adding endpoint route: {endpoint} via {}", gateway.0);

        if endpoint.is_ipv4() {
            let output = Command::new("route")
                .args([
                    "add",
                    &endpoint.to_string(),
                    "mask",
                    "255.255.255.255",
                    &gateway.0,
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| PlatformError::Failed(format!("failed to add endpoint route: {e}")))?;
            if !output.status.success() {
                return Err(PlatformError::Failed(format!(
                    "route add failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(())
        } else {
            self.run_netsh(&[
                "interface",
                "ipv6",
                "add",
                "route",
                &format!("{endpoint}/128"),
                &format!("nexthop={}", gateway.0),
            ])
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
                args.push(gw.0.clone());
            }
            let output = Command::new("route")
                .args(&args)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| {
                    PlatformError::Failed(format!("failed to remove endpoint route: {e}"))
                })?;
            if !output.status.success() {
                // Already gone is the common case and is not a failure worth retrying.
                debug!(
                    "route delete reported: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        } else {
            let _ = self.run_netsh(&[
                "interface",
                "ipv6",
                "delete",
                "route",
                &format!("{endpoint}/128"),
            ]);
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
        let idx = if_index
            .or_else(|| self.get_interface_index(iface.as_str()).ok())
            .ok_or_else(|| {
                PlatformError::Failed(format!("could not get interface index for {iface}"))
            })?
            .to_string();

        info!("Adding {} routes via interface {iface}", routes.len());
        // The list arrives already split by `split_default`, so no /0 reaches netsh.
        for network in routes {
            let proto = if network.is_ipv4() { "ip" } else { "ipv6" };
            self.run_netsh(&[
                "interface",
                proto,
                "add",
                "route",
                &network.to_string(),
                &idx,
            ])?;
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
        let Some(idx) = if_index.or_else(|| self.get_interface_index(iface.as_str()).ok()) else {
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
            let _ = self.run_netsh(&[
                "interface",
                proto,
                "delete",
                "route",
                &network.to_string(),
                &idx,
            ]);
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
        let idx = if_index
            .or_else(|| self.get_interface_index(iface.as_str()).ok())
            .ok_or_else(|| {
                PlatformError::Failed(format!("could not get interface index for {iface}"))
            })?
            .to_string();

        info!("Configuring DNS servers: {servers:?}");
        let (v4, v6): (Vec<&IpAddr>, Vec<&IpAddr>) = servers.iter().partition(|s| s.is_ipv4());

        for (proto, list) in [("ipv4", v4), ("ipv6", v6)] {
            let Some((first, rest)) = list.split_first() else {
                continue;
            };
            self.run_netsh(&[
                "interface",
                proto,
                "set",
                "dnsservers",
                &format!("name={idx}"),
                "source=static",
                &format!("address={first}"),
                "validate=no",
            ])?;
            for server in rest {
                self.run_netsh(&[
                    "interface",
                    proto,
                    "add",
                    "dnsservers",
                    &format!("name={idx}"),
                    &format!("address={server}"),
                    "validate=no",
                ])?;
            }
        }

        flush_dns_cache();
        Ok(())
    }

    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        _snapshot: &DnsSnapshot,
        if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        info!("Restoring DNS configuration for {iface}");
        let Some(idx) = if_index.or_else(|| self.get_interface_index(iface.as_str()).ok()) else {
            debug!("Windows: no interface index for {iface}; DNS already released");
            return Ok(());
        };
        let idx = idx.to_string();

        for proto in ["ipv4", "ipv6"] {
            let _ = self.run_netsh(&[
                "interface",
                proto,
                "set",
                "dnsservers",
                &format!("name={idx}"),
                "source=dhcp",
            ]);
        }
        flush_dns_cache();
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
