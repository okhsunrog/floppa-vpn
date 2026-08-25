//! Linux platform implementation for VPN operations
//!
//! Uses a privileged helper script (`floppa-network-helper`) run via pkexec.
//! A polkit policy (`dev.okhsunrog.floppa-vpn.policy`) allows the helper to
//! run without a password prompt for active desktop sessions.

use super::{DnsSnapshot, Gateway, Platform, PlatformError, TunParams};
use crate::vpn::protocol::InterfaceName;
use async_trait::async_trait;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use tracing::{debug, info, warn};

/// Firewall mark for policy routing — "flop" in hex.
const FWMARK: u32 = 0x666c6f70;

/// Path to the installed network helper script
const HELPER_PATH: &str = "/usr/lib/floppa-vpn/floppa-network-helper";
const POLICY_PATH: &str = "/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy";

const HELPER_CONTENT: &str = include_str!("../../../resources/linux/floppa-network-helper");
const POLICY_CONTENT: &str =
    include_str!("../../../resources/linux/dev.okhsunrog.floppa-vpn.policy");

/// Linux platform implementation.
///
/// Deliberately holds no undo state: what was applied lives in the caller's rollback stack. The
/// previous version kept `original_resolv_conf`, `saved_gateway`, `saved_endpoint_ip` and
/// `saved_routes` here, which was a second rollback stack competing with the real one.
pub struct LinuxPlatform;

impl LinuxPlatform {
    pub fn new() -> Self {
        Self
    }

    /// Is systemd-resolved the thing that owns name resolution right now?
    ///
    /// Asked at the moment DNS is about to be touched, not at startup, and asked of the system
    /// rather than of `$PATH`: on Arch, Debian and Ubuntu Server the `resolvectl` binary is
    /// installed whether or not the service runs. Deciding by the binary meant `resolvectl dns`
    /// failed on those hosts, the default `Tolerate` policy shrugged, and the tunnel came up with
    /// the system's DNS untouched — while the `/etc/resolv.conf` path that would have worked was
    /// unreachable.
    fn systemd_resolved_active() -> bool {
        // The documented sign: /etc/resolv.conf is (a possibly relative) symlink into resolved's
        // runtime directory.
        if let Ok(real) = std::fs::canonicalize(RESOLV_CONF)
            && real.starts_with("/run/systemd/resolve")
        {
            debug!("systemd-resolved owns {RESOLV_CONF}");
            return true;
        }
        // Otherwise ask the service itself; `status` fails when it is not running.
        let running = Command::new("resolvectl")
            .arg("status")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if running {
            debug!("systemd-resolved is running; using resolvectl for DNS");
        } else {
            debug!("systemd-resolved not active; will replace {RESOLV_CONF} directly");
        }
        running
    }

    /// Check if the polkit policy and helper are installed and up-to-date.
    /// If not, write them to temp files and use pkexec to install (one password prompt).
    fn ensure_polkit_installed() -> Result<(), PlatformError> {
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
            .map_err(|e| PlatformError::Failed(format!("failed to create temp helper: {e}")))?;
        let tmp_policy = tempfile::Builder::new()
            .prefix("floppa-policy-")
            .tempfile()
            .map_err(|e| PlatformError::Failed(format!("failed to create temp policy: {e}")))?;

        std::fs::write(tmp_helper.path(), HELPER_CONTENT)
            .map_err(|e| PlatformError::Failed(format!("failed to write temp helper: {e}")))?;
        std::fs::write(tmp_policy.path(), POLICY_CONTENT)
            .map_err(|e| PlatformError::Failed(format!("failed to write temp policy: {e}")))?;

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
            .map_err(|e| PlatformError::Unavailable(format!("failed to run pkexec: {e}")))?;

        // tmp_helper and tmp_policy are automatically cleaned up on drop

        if output.status.success() {
            info!("Polkit policy and helper installed successfully");
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // Same codes as `run_helper`: 126 is a declined or dismissed dialog, 127 is pkexec itself
        // being unusable. Reported apart because "you said no" is not "your system is broken".
        Err(match output.status.code() {
            Some(126) => PlatformError::PermissionDenied(if stderr.is_empty() {
                "authorisation declined".to_string()
            } else {
                stderr
            }),
            _ => PlatformError::Unavailable(format!("pkexec install failed: {stderr}")),
        })
    }

    /// Run the network helper via pkexec.
    ///
    /// With the polkit policy installed, this runs without a password prompt
    /// for active desktop sessions.
    fn run_helper(&self, args: &[&str]) -> Result<(), PlatformError> {
        debug!("Running helper: {:?}", args);

        let output = Command::new("pkexec")
            .arg(HELPER_PATH)
            .args(args)
            .output()
            .map_err(|e| {
                PlatformError::Unavailable(format!("failed to run network helper: {e}"))
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // pkexec exits 126 when authorisation is declined or dismissed, and 127 when the helper
        // could not be launched at all. Both are pointless to retry with another protocol.
        Err(match output.status.code() {
            Some(126) => PlatformError::PermissionDenied(if stderr.is_empty() {
                "authorisation declined".to_string()
            } else {
                stderr
            }),
            Some(127) => PlatformError::Unavailable(format!("helper not executable: {stderr}")),
            _ => PlatformError::Failed(format!("network helper failed: {stderr}")),
        })
    }

    /// Get the default gateway IP from the routing table (no privileges needed).
    fn get_default_gateway() -> Result<Option<String>, PlatformError> {
        let output = Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .map_err(|e| PlatformError::Failed(format!("failed to read default route: {e}")))?;
        let route_output = String::from_utf8_lossy(&output.stdout);
        // Parse "default via 192.168.1.1 dev eth0"
        Ok(route_output
            .split_whitespace()
            .skip_while(|&w| w != "via")
            .nth(1)
            .map(|s| s.to_string()))
    }

    /// Check whether the current process has effective CAP_NET_ADMIN.
    fn has_cap_net_admin() -> bool {
        const CAP_NET_ADMIN_BIT: u32 = 12;

        let status = match std::fs::read_to_string("/proc/self/status") {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to read /proc/self/status: {e}");
                return false;
            }
        };

        let cap_eff_hex = match status
            .lines()
            .find(|line| line.starts_with("CapEff:"))
            .and_then(|line| line.split_whitespace().nth(1))
        {
            Some(v) => v,
            None => {
                warn!("CapEff not found in /proc/self/status");
                return false;
            }
        };

        match u128::from_str_radix(cap_eff_hex, 16) {
            Ok(bits) => (bits & (1u128 << CAP_NET_ADMIN_BIT)) != 0,
            Err(e) => {
                warn!("Failed to parse CapEff value '{cap_eff_hex}': {e}");
                false
            }
        }
    }

    /// Write `/etc/resolv.conf` through the privileged helper.
    ///
    /// The helper replaces the path rather than `cp`-ing onto it, so a symlink to the resolver's
    /// stub file is never written through.
    ///
    /// The temp file goes to `/tmp` explicitly, not `$TMPDIR`: the helper accepts only
    /// `/tmp/floppa-resolv*` as a source, so on a host with `TMPDIR` set elsewhere every DNS
    /// configuration used to be refused.
    fn write_resolv_conf(&self, content: &str) -> Result<(), PlatformError> {
        let tmp = tempfile::Builder::new()
            .prefix("floppa-resolv-")
            .tempfile_in(RESOLV_TEMP_DIR)
            .map_err(|e| {
                PlatformError::Failed(format!(
                    "failed to create temp resolv.conf in {RESOLV_TEMP_DIR}: {e}"
                ))
            })?;
        let path = tmp.path().to_string_lossy().to_string();
        std::fs::write(tmp.path(), content).map_err(|e| {
            PlatformError::Failed(format!("failed to write temp resolv.conf {path}: {e}"))
        })?;
        self.run_helper(&["set-resolv-conf", &path])
            .map_err(|e| PlatformError::Failed(format!("set-resolv-conf {path}: {e}")))
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
    fn tun_params(&self) -> TunParams {
        let fwmark = if Self::has_cap_net_admin() {
            Some(FWMARK)
        } else {
            info!("CAP_NET_ADMIN not present, running without fwmark");
            None
        };

        TunParams {
            manage_device: false,
            fwmark,
            wintun_file: None,
        }
    }

    /// Install the privileged helper if it is missing or outdated, and report whether it is usable.
    ///
    /// Deliberately here and not in the constructor. Installing runs `pkexec`, which raises an
    /// authentication dialog — and the constructor runs during app startup, on the thread that has
    /// not built the window yet, so a machine needing the helper sat on a password prompt with
    /// nothing on screen to explain it. This is the first step of an attempt, which is exactly when
    /// the user has asked for something that needs the privilege.
    ///
    /// Re-checked per attempt rather than remembered: when nothing needs installing this is two
    /// file reads, and when the user dismisses the dialog, pressing Connect again asks again
    /// instead of failing from a decision cached at startup.
    async fn preflight(&self) -> Result<(), PlatformError> {
        // Blocking: it spawns pkexec and waits for a human.
        tokio::task::spawn_blocking(Self::ensure_polkit_installed)
            .await
            .map_err(|e| PlatformError::Unavailable(format!("helper install task panicked: {e}")))?
    }

    async fn prepare_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        // Create a persistent TUN owned by the current user, so gotatun can open it unprivileged.
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| PlatformError::Failed(format!("failed to read process metadata: {e}")))?
            .uid();
        self.run_helper(&["ensure-tun", iface.as_str(), &uid.to_string()])
    }

    async fn release_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        // `deconfigure` is down + addr flush, both `|| true`-guarded in the helper, so this is
        // safe when nothing was ever prepared.
        self.run_helper(&["deconfigure", iface.as_str()])
    }

    async fn configure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        info!("Configuring address {addr} on interface {iface}");
        self.run_helper(&["configure", iface.as_str(), &addr.to_string()])
    }

    async fn deconfigure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        self.run_helper(&["flush-addr", iface.as_str(), &addr.to_string()])
    }

    async fn default_gateway(&self) -> Result<Option<Gateway>, PlatformError> {
        Ok(Self::get_default_gateway()?.map(Gateway))
    }

    async fn interface_index(&self, _iface: &InterfaceName) -> Option<u32> {
        // Linux scopes routes by device name, so the index is never needed.
        None
    }

    async fn add_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        let gateway = gateway.ok_or_else(|| {
            PlatformError::Failed("no default gateway; cannot pin the endpoint route".to_string())
        })?;
        let route = endpoint_route(endpoint);
        info!("Adding endpoint route: {route} via {}", gateway.0);
        self.run_helper(&["add-route", &route, "via", &gateway.0])
    }

    async fn remove_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        let route = endpoint_route(endpoint);
        info!("Removing endpoint route: {route}");
        // Matching on the gateway too: without it this deletes any route to the endpoint, which
        // after a roaming event is the wrong one.
        match gateway {
            Some(gw) => self.run_helper(&["del-route", &route, "via", &gw.0]),
            None => self.run_helper(&["del-route", &route]),
        }
    }

    async fn add_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if routes.is_empty() {
            return Ok(());
        }
        info!("Adding {} routes via interface {iface}", routes.len());
        let strs: Vec<String> = routes.iter().map(|r| r.to_string()).collect();
        let mut args: Vec<&str> = vec!["add-routes", iface.as_str()];
        args.extend(strs.iter().map(|s| s.as_str()));
        self.run_helper(&args)
    }

    async fn remove_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if routes.is_empty() {
            return Ok(());
        }
        info!("Removing {} routes via interface {iface}", routes.len());
        let strs: Vec<String> = routes.iter().map(|r| r.to_string()).collect();
        let mut args: Vec<&str> = vec!["del-routes", iface.as_str()];
        args.extend(strs.iter().map(|s| s.as_str()));
        self.run_helper(&args)
    }

    async fn capture_dns(
        &self,
        _iface: &InterfaceName,
        _if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        if Self::systemd_resolved_active() {
            return Ok(DnsSnapshot::Resolvectl);
        }
        // Record whether the path is a symlink BEFORE writing, so the restore can put the link
        // back rather than writing through it into the resolver's own stub file.
        let path = std::path::Path::new(RESOLV_CONF);
        let symlink_target = std::fs::symlink_metadata(path)
            .ok()
            .filter(|m| m.file_type().is_symlink())
            .and_then(|_| std::fs::read_link(path).ok());
        Ok(DnsSnapshot::ResolvConf {
            content: std::fs::read_to_string(path).ok(),
            symlink_target,
        })
    }

    async fn configure_dns(
        &self,
        iface: &InterfaceName,
        servers: &[IpAddr],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        if servers.is_empty() {
            info!("No DNS servers to configure");
            return Ok(());
        }
        info!("Configuring DNS servers: {servers:?}");

        if Self::systemd_resolved_active() {
            let strs: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
            let mut args: Vec<&str> = vec!["set-dns", iface.as_str()];
            args.extend(strs.iter().map(|s| s.as_str()));
            return self.run_helper(&args);
        }

        let mut content = String::from("# Generated by floppa-vpn\n");
        for server in servers {
            content.push_str(&format!("nameserver {server}\n"));
        }
        self.write_resolv_conf(&content)
    }

    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        snapshot: &DnsSnapshot,
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        info!("Restoring DNS configuration");
        match snapshot {
            DnsSnapshot::Resolvectl => self.run_helper(&["revert-dns", iface.as_str()]),
            DnsSnapshot::ResolvConf {
                symlink_target: Some(target),
                ..
            } => self.run_helper(&["restore-resolv-conf-link", &target.to_string_lossy()]),
            DnsSnapshot::ResolvConf {
                content: Some(content),
                symlink_target: None,
            } => self.write_resolv_conf(content),
            DnsSnapshot::ResolvConf {
                content: None,
                symlink_target: None,
            } => {
                // There was no resolv.conf before us. Leaving ours in place is strictly better
                // than deleting the file and taking name resolution down with it.
                warn!("no original resolv.conf was captured; leaving the generated one in place");
                Ok(())
            }
            other => {
                warn!("ignoring non-Linux DNS snapshot: {other:?}");
                Ok(())
            }
        }
    }

    async fn ipv6_enabled(&self) -> bool {
        let enabled = Self::is_ipv6_enabled();
        if !enabled {
            info!("IPv6 is disabled on host, skipping IPv6 VPN routes");
        }
        enabled
    }
}

const RESOLV_CONF: &str = "/etc/resolv.conf";
/// Must match the prefix `set-resolv-conf` accepts in the helper.
const RESOLV_TEMP_DIR: &str = "/tmp";

/// The single-host route covering `endpoint`.
fn endpoint_route(endpoint: IpAddr) -> String {
    let prefix = if endpoint.is_ipv4() { 32 } else { 128 };
    format!("{endpoint}/{prefix}")
}
