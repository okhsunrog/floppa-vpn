//! Linux platform implementation for VPN operations
//!
//! Uses a privileged helper script (`floppa-network-helper`) run via pkexec.
//! A polkit policy (`dev.okhsunrog.floppa-vpn.policy`) allows the helper to
//! run without a password prompt for active desktop sessions.

use super::Platform;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use tracing::{debug, info, warn};

/// Path to the installed network helper script
const HELPER_PATH: &str = "/usr/lib/floppa-vpn/floppa-network-helper";
const POLICY_PATH: &str = "/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy";

const HELPER_CONTENT: &str = include_str!("../../../resources/linux/floppa-network-helper");
const POLICY_CONTENT: &str =
    include_str!("../../../resources/linux/dev.okhsunrog.floppa-vpn.policy");

/// Linux platform implementation
pub struct LinuxPlatform {
    /// Original resolv.conf content (for restoration)
    original_resolv_conf: std::sync::Mutex<Option<String>>,
    /// Whether systemd-resolved is available
    has_resolvectl: bool,
    /// Saved default gateway for endpoint route cleanup
    saved_gateway: std::sync::Mutex<Option<String>>,
    /// Saved endpoint IP for route cleanup
    saved_endpoint_ip: std::sync::Mutex<Option<IpAddr>>,
    /// Saved routes for cleanup (the actual routes added, after /0 splitting)
    saved_routes: std::sync::Mutex<Vec<String>>,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        let has_resolvectl = Command::new("resolvectl")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if has_resolvectl {
            debug!("systemd-resolved available, will use resolvectl for DNS");
        } else {
            debug!("resolvectl not available, will modify /etc/resolv.conf directly");
        }

        // Install polkit policy + helper if missing or outdated
        if let Err(e) = Self::ensure_polkit_installed() {
            warn!("Failed to install polkit policy: {}", e);
        }

        Self {
            original_resolv_conf: std::sync::Mutex::new(None),
            has_resolvectl,
            saved_gateway: std::sync::Mutex::new(None),
            saved_endpoint_ip: std::sync::Mutex::new(None),
            saved_routes: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Check if the polkit policy and helper are installed and up-to-date.
    /// If not, write them to temp files and use pkexec to install (one password prompt).
    fn ensure_polkit_installed() -> Result<(), String> {
        let helper_ok = std::fs::read_to_string(HELPER_PATH).is_ok_and(|c| c == HELPER_CONTENT);
        let policy_ok = std::fs::read_to_string(POLICY_PATH).is_ok_and(|c| c == POLICY_CONTENT);

        if helper_ok && policy_ok {
            debug!("Polkit policy and helper already installed");
            return Ok(());
        }

        info!("Installing polkit policy and network helper...");

        // Write embedded files to temp files with random names (prevents TOCTOU attacks)
        let tmp_helper = tempfile::Builder::new()
            .prefix("floppa-helper-")
            .tempfile()
            .map_err(|e| format!("Failed to create temp helper: {}", e))?;
        let tmp_policy = tempfile::Builder::new()
            .prefix("floppa-policy-")
            .tempfile()
            .map_err(|e| format!("Failed to create temp policy: {}", e))?;

        std::fs::write(tmp_helper.path(), HELPER_CONTENT)
            .map_err(|e| format!("Failed to write temp helper: {}", e))?;
        std::fs::write(tmp_policy.path(), POLICY_CONTENT)
            .map_err(|e| format!("Failed to write temp policy: {}", e))?;

        // Single pkexec call to install both files
        let script = format!(
            "mkdir -p /usr/lib/floppa-vpn && \
             install -m 755 {} {} && \
             install -m 644 {} {}",
            tmp_helper.path().display(),
            HELPER_PATH,
            tmp_policy.path().display(),
            POLICY_PATH,
        );

        let output = Command::new("pkexec")
            .args(["sh", "-c", &script])
            .output()
            .map_err(|e| format!("Failed to run pkexec: {}", e))?;

        // tmp_helper and tmp_policy are automatically cleaned up on drop

        if output.status.success() {
            info!("Polkit policy and helper installed successfully");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("pkexec install failed: {}", stderr))
        }
    }

    /// Run the network helper via pkexec.
    ///
    /// With the polkit policy installed, this runs without a password prompt
    /// for active desktop sessions.
    fn run_helper(&self, args: &[&str]) -> Result<(), String> {
        debug!("Running helper: {:?}", args);

        let output = Command::new("pkexec")
            .arg(HELPER_PATH)
            .args(args)
            .output()
            .map_err(|e| format!("Failed to run network helper: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("Network helper failed: {}", stderr))
        }
    }

    /// Run the network helper, ignoring failures (for cleanup operations).
    fn run_helper_ignore_errors(&self, args: &[&str]) {
        debug!("Running helper (ignore errors): {:?}", args);

        match Command::new("pkexec").arg(HELPER_PATH).args(args).output() {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Helper command failed (ignored): {}", stderr);
            }
            Err(e) => warn!("Failed to run helper (ignored): {}", e),
            _ => {}
        }
    }

    /// Get the default gateway IP from the routing table (no privileges needed).
    fn get_default_gateway() -> Result<Option<String>, String> {
        let output = Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .map_err(|e| format!("Failed to get default route: {}", e))?;
        let route_output = String::from_utf8_lossy(&output.stdout);
        // Parse "default via 192.168.1.1 dev eth0"
        Ok(route_output
            .split_whitespace()
            .skip_while(|&w| w != "via")
            .nth(1)
            .map(|s| s.to_string()))
    }

    /// Check if IPv6 is enabled in the kernel.
    ///
    /// If the procfs knob is unavailable, assume enabled to avoid silently
    /// dropping IPv6 routes on non-standard systems.
    fn is_ipv6_enabled() -> bool {
        match std::fs::read_to_string("/proc/sys/net/ipv6/conf/all/disable_ipv6") {
            Ok(v) => v.trim() != "1",
            Err(_) => true,
        }
    }
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Platform for LinuxPlatform {
    async fn prepare_tun(&self, iface: &str) -> Result<(), String> {
        // Create persistent TUN owned by the current user via pkexec helper.
        // This allows unprivileged opening of the device from gotatun.
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| format!("Failed to read process metadata: {e}"))?
            .uid();
        self.run_helper(&["ensure-tun", iface, &uid.to_string()])
    }

    async fn configure_address(&self, iface: &str, addr: IpNetwork) -> Result<(), String> {
        info!("Configuring address {} on interface {}", addr, iface);

        self.run_helper(&["configure", iface, &addr.to_string()])
    }

    async fn add_endpoint_route(&self, endpoint_ip: IpAddr) -> Result<(), String> {
        let gateway =
            Self::get_default_gateway()?.ok_or_else(|| "No default gateway found".to_string())?;

        let endpoint_route = format!("{}/32", endpoint_ip);
        info!("Adding endpoint route: {} via {}", endpoint_route, gateway);

        self.run_helper(&["add-route", &endpoint_route, "via", &gateway])?;

        *self.saved_gateway.lock().unwrap() = Some(gateway);
        *self.saved_endpoint_ip.lock().unwrap() = Some(endpoint_ip);
        Ok(())
    }

    async fn remove_endpoint_route(&self) -> Result<(), String> {
        if let Some(endpoint_ip) = self.saved_endpoint_ip.lock().unwrap().take() {
            let endpoint_route = format!("{}/32", endpoint_ip);
            info!("Removing endpoint route: {}", endpoint_route);
            self.run_helper_ignore_errors(&["del-route", &endpoint_route]);
        }
        self.saved_gateway.lock().unwrap().take();
        Ok(())
    }

    async fn add_routes(&self, iface: &str, allowed_ips: &[IpNetwork]) -> Result<(), String> {
        let ipv6_enabled = Self::is_ipv6_enabled();
        if !ipv6_enabled {
            info!("IPv6 is disabled on host, skipping IPv6 VPN routes");
        }

        let mut routes = Vec::new();
        for network in allowed_ips {
            if network.is_ipv6() && !ipv6_enabled {
                debug!("Skipping IPv6 route because IPv6 is disabled: {}", network);
                continue;
            }

            if network.prefix() == 0 {
                if network.is_ipv4() {
                    routes.push("0.0.0.0/1".to_string());
                    routes.push("128.0.0.0/1".to_string());
                } else {
                    routes.push("::/1".to_string());
                    routes.push("8000::/1".to_string());
                }
            } else {
                routes.push(network.to_string());
            }
        }

        info!("Adding {} routes via interface {}", routes.len(), iface);

        if !routes.is_empty() {
            let mut args: Vec<&str> = vec!["add-routes", iface];
            let route_refs: Vec<&str> = routes.iter().map(|s| s.as_str()).collect();
            args.extend(route_refs);
            self.run_helper(&args)?;
            *self.saved_routes.lock().unwrap() = routes;
        }

        Ok(())
    }

    async fn remove_routes(&self, iface: &str) -> Result<(), String> {
        let routes = self
            .saved_routes
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        if routes.is_empty() {
            info!("No saved routes to remove");
            return Ok(());
        }

        info!("Removing {} routes via interface {}", routes.len(), iface);
        let mut args: Vec<&str> = vec!["del-routes", iface];
        let route_refs: Vec<&str> = routes.iter().map(|s| s.as_str()).collect();
        args.extend(route_refs);
        self.run_helper_ignore_errors(&args);

        Ok(())
    }

    async fn configure_dns(&self, iface: &str, servers: &[IpAddr]) -> Result<(), String> {
        if servers.is_empty() {
            info!("No DNS servers to configure");
            return Ok(());
        }

        info!("Configuring DNS servers: {:?}", servers);

        if self.has_resolvectl {
            let server_strs: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
            let mut args: Vec<&str> = vec!["set-dns", iface];
            let refs: Vec<&str> = server_strs.iter().map(|s| s.as_str()).collect();
            args.extend(refs);
            self.run_helper(&args)?;
        } else {
            // Fallback: modify /etc/resolv.conf directly
            let original = std::fs::read_to_string("/etc/resolv.conf").ok();
            *self.original_resolv_conf.lock().unwrap() = original;

            let mut content = String::from("# Generated by floppa-vpn\n");
            for server in servers {
                content.push_str(&format!("nameserver {}\n", server));
            }

            let tmp = tempfile::Builder::new()
                .prefix("floppa-resolv-")
                .tempfile()
                .map_err(|e| format!("Failed to create temp resolv.conf: {}", e))?;
            std::fs::write(tmp.path(), &content)
                .map_err(|e| format!("Failed to write temp resolv.conf: {}", e))?;

            self.run_helper(&["set-resolv-conf", &tmp.path().to_string_lossy()])?;
            // tmp is automatically cleaned up on drop
        }

        Ok(())
    }

    async fn restore_dns(&self, iface: &str) -> Result<(), String> {
        info!("Restoring DNS configuration");

        if self.has_resolvectl {
            self.run_helper_ignore_errors(&["revert-dns", iface]);
        } else if let Some(original) = self.original_resolv_conf.lock().unwrap().take() {
            let tmp = tempfile::Builder::new()
                .prefix("floppa-resolv-restore-")
                .tempfile()
                .map_err(|e| format!("Failed to create temp resolv.conf: {}", e))?;
            std::fs::write(tmp.path(), &original)
                .map_err(|e| format!("Failed to write temp resolv.conf: {}", e))?;

            self.run_helper(&["set-resolv-conf", &tmp.path().to_string_lossy()])?;
        } else {
            warn!("No original resolv.conf to restore");
        }

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

        // Bring interface down and flush addresses
        self.run_helper_ignore_errors(&["deconfigure", iface]);

        Ok(())
    }
}
