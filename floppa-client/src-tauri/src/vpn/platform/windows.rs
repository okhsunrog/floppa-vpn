//! Windows platform implementation for VPN operations
//!
//! Uses netsh for network configuration:
//! - `netsh interface ip` for address configuration
//! - `netsh interface ip add route` for routing
//! - `netsh interface ip set dns` for DNS
//! - `route` command for endpoint host route

use super::{Platform, TunParams};
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::os::windows::process::CommandExt;
use std::process::Command;
use tracing::{debug, info, warn};

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Windows platform implementation
pub struct WindowsPlatform {
    /// Interface index for the TUN device (set after configuration)
    interface_index: std::sync::Mutex<Option<u32>>,
    /// Saved endpoint IP for route cleanup
    saved_endpoint_ip: std::sync::Mutex<Option<IpAddr>>,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        Self {
            interface_index: std::sync::Mutex::new(None),
            saved_endpoint_ip: std::sync::Mutex::new(None),
        }
    }

    /// Run netsh command and return result
    fn run_netsh(&self, args: &[&str]) -> Result<String, String> {
        debug!("Running netsh: {:?}", args);

        let output = Command::new("netsh")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Failed to run netsh: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            // netsh sometimes returns success even with errors in stdout
            if !stderr.is_empty() {
                Err(format!("netsh failed: {}", stderr))
            } else if stdout.contains("error") || stdout.contains("failed") {
                Err(format!("netsh failed: {}", stdout))
            } else {
                Ok(stdout)
            }
        }
    }

    /// Get interface index by name
    fn get_interface_index(&self, iface: &str) -> Result<u32, String> {
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

        Err(format!("Interface {} not found", iface))
    }

    /// Get the default gateway IP from the routing table
    fn get_default_gateway() -> Result<Option<String>, String> {
        let output = Command::new("cmd")
            .args(["/C", "route", "print", "0.0.0.0"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Failed to get default route: {}", e))?;
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
        let wintun_file = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("wintun.dll")));

        TunParams {
            manage_device: true,
            fwmark: None,
            wintun_file,
        }
    }

    async fn prepare_tun(&self, iface: &str) -> Result<(), String> {
        // Clean up any stale Wintun adapter left behind by a crash, force-kill,
        // or a previous failed TUN creation (e.g. adapter created but session failed).
        let wintun_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("wintun.dll")));

        if let Some(ref path) = wintun_path {
            match unsafe { wintun_bindings::load_from_path(path) } {
                Ok(wintun) => {
                    match wintun_bindings::Adapter::open(&wintun, iface) {
                        Ok(adapter) => {
                            info!("Found stale Wintun adapter '{iface}', closing it");
                            drop(adapter); // WintunCloseAdapter is called in Drop
                        }
                        Err(_) => {
                            // No stale adapter — nothing to clean up
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to load wintun.dll for stale adapter cleanup: {e}");
                }
            }
        }

        Ok(())
    }

    async fn configure_address(&self, iface: &str, addr: IpNetwork) -> Result<(), String> {
        info!("Configuring address {} on interface {}", addr, iface);

        let ip = addr.ip().to_string();
        let mask = addr.mask().to_string();

        // Set static IP address
        // netsh interface ip set address name="interface" source=static addr=x.x.x.x mask=y.y.y.y
        self.run_netsh(&[
            "interface",
            "ip",
            "set",
            "address",
            &format!("name={}", iface),
            "source=static",
            &format!("addr={}", ip),
            &format!("mask={}", mask),
        ])?;

        // Store interface index for later use
        if let Ok(idx) = self.get_interface_index(iface) {
            *self.interface_index.lock().unwrap() = Some(idx);
        }

        Ok(())
    }

    async fn add_endpoint_route(&self, endpoint_ip: IpAddr) -> Result<(), String> {
        let gateway =
            Self::get_default_gateway()?.ok_or_else(|| "No default gateway found".to_string())?;

        info!("Adding endpoint route: {} via {}", endpoint_ip, gateway);

        let output = if endpoint_ip.is_ipv4() {
            Command::new("route")
                .args([
                    "add",
                    &endpoint_ip.to_string(),
                    "mask",
                    "255.255.255.255",
                    &gateway,
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
        } else {
            // IPv6: netsh interface ipv6 add route <ip>/128 nexthop=<gateway>
            Command::new("netsh")
                .args([
                    "interface",
                    "ipv6",
                    "add",
                    "route",
                    &format!("{}/128", endpoint_ip),
                    &format!("nexthop={}", gateway),
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
        }
        .map_err(|e| format!("Failed to add endpoint route: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("route add failed: {}", stderr));
        }

        *self.saved_endpoint_ip.lock().unwrap() = Some(endpoint_ip);
        Ok(())
    }

    async fn remove_endpoint_route(&self) -> Result<(), String> {
        if let Some(endpoint_ip) = self.saved_endpoint_ip.lock().unwrap().take() {
            info!("Removing endpoint route: {}", endpoint_ip);
            if endpoint_ip.is_ipv4() {
                let _ = Command::new("route")
                    .args(["delete", &endpoint_ip.to_string()])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            } else {
                let _ = Command::new("netsh")
                    .args([
                        "interface",
                        "ipv6",
                        "delete",
                        "route",
                        &format!("{}/128", endpoint_ip),
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
        }
        Ok(())
    }

    async fn add_routes(&self, iface: &str, allowed_ips: &[IpNetwork]) -> Result<(), String> {
        info!(
            "Adding {} routes via interface {}",
            allowed_ips.len(),
            iface
        );

        // Get interface index
        let if_index = self
            .interface_index
            .lock()
            .unwrap()
            .or_else(|| self.get_interface_index(iface).ok())
            .ok_or_else(|| format!("Could not get interface index for {}", iface))?;

        for network in allowed_ips {
            // For default route (0.0.0.0/0 or ::/0), split into two /1 routes
            if network.prefix() == 0 {
                if network.is_ipv4() {
                    // Split into 0.0.0.0/1 and 128.0.0.0/1
                    self.run_netsh(&[
                        "interface",
                        "ip",
                        "add",
                        "route",
                        "0.0.0.0/1",
                        &if_index.to_string(),
                    ])?;
                    self.run_netsh(&[
                        "interface",
                        "ip",
                        "add",
                        "route",
                        "128.0.0.0/1",
                        &if_index.to_string(),
                    ])?;
                } else {
                    // IPv6: split into ::/1 and 8000::/1
                    self.run_netsh(&[
                        "interface",
                        "ipv6",
                        "add",
                        "route",
                        "::/1",
                        &if_index.to_string(),
                    ])?;
                    self.run_netsh(&[
                        "interface",
                        "ipv6",
                        "add",
                        "route",
                        "8000::/1",
                        &if_index.to_string(),
                    ])?;
                }
            } else {
                // Regular route
                let proto = if network.is_ipv4() { "ip" } else { "ipv6" };
                self.run_netsh(&[
                    "interface",
                    proto,
                    "add",
                    "route",
                    &network.to_string(),
                    &if_index.to_string(),
                ])?;
            }
        }

        Ok(())
    }

    async fn remove_routes(&self, iface: &str) -> Result<(), String> {
        info!("Removing routes via interface {}", iface);

        let if_index = self
            .interface_index
            .lock()
            .unwrap()
            .or_else(|| self.get_interface_index(iface).ok());

        if let Some(idx) = if_index {
            let idx_str = idx.to_string();
            // Remove split routes - ignore errors as they may not exist
            let _ = self.run_netsh(&["interface", "ip", "delete", "route", "0.0.0.0/1", &idx_str]);
            let _ = self.run_netsh(&[
                "interface",
                "ip",
                "delete",
                "route",
                "128.0.0.0/1",
                &idx_str,
            ]);
            let _ = self.run_netsh(&["interface", "ipv6", "delete", "route", "::/1", &idx_str]);
            let _ = self.run_netsh(&["interface", "ipv6", "delete", "route", "8000::/1", &idx_str]);
        }

        Ok(())
    }

    async fn configure_dns(&self, iface: &str, servers: &[IpAddr]) -> Result<(), String> {
        if servers.is_empty() {
            info!("No DNS servers to configure");
            return Ok(());
        }

        info!("Configuring DNS servers: {:?}", servers);

        // Get interface index
        let if_index = self
            .interface_index
            .lock()
            .unwrap()
            .or_else(|| self.get_interface_index(iface).ok())
            .ok_or_else(|| format!("Could not get interface index for {}", iface))?;

        let idx_str = if_index.to_string();

        // Separate IPv4 and IPv6 DNS servers
        let (ipv4_servers, ipv6_servers): (Vec<&IpAddr>, Vec<&IpAddr>) =
            servers.iter().partition(|s| s.is_ipv4());

        // Configure IPv4 DNS
        if !ipv4_servers.is_empty() {
            // Set first server
            self.run_netsh(&[
                "interface",
                "ipv4",
                "set",
                "dnsservers",
                &format!("name={}", idx_str),
                "source=static",
                &format!("address={}", ipv4_servers[0]),
                "validate=no",
            ])?;

            // Add additional servers
            for server in ipv4_servers.iter().skip(1) {
                self.run_netsh(&[
                    "interface",
                    "ipv4",
                    "add",
                    "dnsservers",
                    &format!("name={}", idx_str),
                    &format!("address={}", server),
                    "validate=no",
                ])?;
            }
        }

        // Configure IPv6 DNS
        if !ipv6_servers.is_empty() {
            // Set first server
            self.run_netsh(&[
                "interface",
                "ipv6",
                "set",
                "dnsservers",
                &format!("name={}", idx_str),
                "source=static",
                &format!("address={}", ipv6_servers[0]),
                "validate=no",
            ])?;

            // Add additional servers
            for server in ipv6_servers.iter().skip(1) {
                self.run_netsh(&[
                    "interface",
                    "ipv6",
                    "add",
                    "dnsservers",
                    &format!("name={}", idx_str),
                    &format!("address={}", server),
                    "validate=no",
                ])?;
            }
        }

        flush_dns_cache();
        Ok(())
    }

    async fn restore_dns(&self, iface: &str) -> Result<(), String> {
        info!("Restoring DNS configuration for {}", iface);

        let if_index = self
            .interface_index
            .lock()
            .unwrap()
            .or_else(|| self.get_interface_index(iface).ok());

        if let Some(idx) = if_index {
            let idx_str = idx.to_string();

            // Clear DNS servers (set to DHCP/automatic)
            let _ = self.run_netsh(&[
                "interface",
                "ipv4",
                "set",
                "dnsservers",
                &format!("name={}", idx_str),
                "source=dhcp",
            ]);

            let _ = self.run_netsh(&[
                "interface",
                "ipv6",
                "set",
                "dnsservers",
                &format!("name={}", idx_str),
                "source=dhcp",
            ]);
        }

        flush_dns_cache();
        Ok(())
    }

    async fn cleanup(&self, iface: &str) -> Result<(), String> {
        info!("Cleaning up interface {}", iface);

        // Restore DNS first
        let _ = self.restore_dns(iface).await;

        // Remove routes
        let _ = self.remove_routes(iface).await;

        // Remove endpoint route
        let _ = self.remove_endpoint_route().await;

        // Clear interface index
        *self.interface_index.lock().unwrap() = None;

        // Note: The TUN device will be destroyed when DeviceHandle is dropped
        // We don't need to explicitly remove the interface

        Ok(())
    }
}
