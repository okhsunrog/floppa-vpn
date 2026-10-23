//! Linux platform implementation for VPN operations
//!
//! Uses a privileged Rust helper (`floppa-network-helper`) run via pkexec.
//! A polkit policy (`dev.okhsunrog.floppa-vpn.policy`) allows the helper to
//! run without a password prompt for active desktop sessions.

use super::{
    DnsSnapshot, Gateway, IpFamily, Platform, PlatformError, PolicyRoutingSnapshot, TunParams,
};
use crate::protocol::InterfaceName;
use async_trait::async_trait;
use floppa_tunnel_config::route;
use ipnetwork::IpNetwork;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;
use tracing::{debug, info};

/// How long an *undo* may wait on the helper.
///
/// Apply calls are unbounded — pkexec may legitimately be waiting for the user — but an undo runs
/// during unwind and on exit, where a pkexec that never returns (a dismissed agent, a hung
/// session) used to hang the whole teardown. On timeout the child is killed and the step is
/// reported as residual rather than waited for forever.
const UNDO_TIMEOUT: Duration = Duration::from_secs(30);

/// Path to the installed network helper
const HELPER_PATH: &str = "/usr/lib/floppa-vpn/floppa-network-helper-v2";
const POLICY_PATH: &str = "/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.network-v2.policy";

// Build-time packaging contract. Never accept privileged executable paths from the
// runtime environment. Native packages can keep the portable installation path.
const PACKAGED_HELPER: Option<&str> = option_env!("FLOPPA_PACKAGED_HELPER");
const PACKAGED_POLICY: Option<&str> = option_env!("FLOPPA_PACKAGED_POLICY");

fn check_packaged_resources(helper: &str, policy: &str) -> Result<(), PlatformError> {
    for (path, executable) in [(helper, true), (policy, false)] {
        let metadata = std::fs::metadata(path).map_err(|e| {
            PlatformError::Unavailable(format!("packaged network resource {path} unavailable: {e}"))
        })?;
        if !std::path::Path::new(path).is_absolute()
            || !metadata.is_file()
            || (executable && metadata.mode() & 0o111 == 0)
        {
            return Err(PlatformError::Unavailable(format!(
                "invalid packaged network resource: {path}"
            )));
        }
    }
    Ok(())
}

/// Linux platform implementation.
///
/// Deliberately holds no undo state: what was applied lives in the caller's rollback stack. The
/// previous version kept `original_resolv_conf`, `saved_gateway`, `saved_endpoint_ip` and
/// `saved_routes` here, which was a second rollback stack competing with the real one.
pub struct LinuxPlatform;

impl LinuxPlatform {
    pub async fn prepare_ownership(&self) -> Result<(), PlatformError> {
        self.ensure_helper().await?;
        self.run_helper(&["prepare-ownership"], None).await
    }

    async fn ensure_helper(&self) -> Result<(), PlatformError> {
        tokio::task::spawn_blocking(Self::check_helper_installed)
            .await
            .map_err(|e| PlatformError::Unavailable(format!("helper check task panicked: {e}")))?
    }

    pub fn new() -> Self {
        Self
    }

    /// Whether this process is already root.
    ///
    /// It decides whether polkit is involved at all. A desktop app runs as the user and asks
    /// polkit to raise a helper; `sudo floppa` is *already* the user polkit would have
    /// escalated to, and asking again means either a password prompt for a privilege we hold or —
    /// in a container, which is where the integration suite runs — a `pkexec` that is not
    /// installed and a connect that fails for no reason a user could act on.
    fn running_as_root() -> bool {
        // SAFETY: `geteuid` reads the calling process's effective uid. It takes no arguments,
        // touches no memory and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    fn helper_path() -> std::path::PathBuf {
        if let Some(helper) = PACKAGED_HELPER {
            return helper.into();
        }
        // Source builds and portable root CLI bundles may ship the binary next to floppa.
        // An unprivileged client always uses the administrator-installed polkit target.
        if Self::running_as_root()
            && let Ok(executable) = std::env::current_exe()
            && let Some(parent) = executable.parent()
        {
            let sibling = parent.join("floppa-network-helper");
            if sibling.is_file() {
                return sibling;
            }
        }
        HELPER_PATH.into()
    }

    fn check_helper_installed() -> Result<(), PlatformError> {
        match (PACKAGED_HELPER, PACKAGED_POLICY) {
            (Some(helper), Some(policy)) => check_packaged_resources(helper, policy)?,
            (None, None) => {
                if !Self::running_as_root() {
                    check_packaged_resources(HELPER_PATH, POLICY_PATH)?;
                    for path in [HELPER_PATH, POLICY_PATH] {
                        let metadata = std::fs::metadata(path).map_err(|e| {
                            PlatformError::Unavailable(format!("network resource {path}: {e}"))
                        })?;
                        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                            return Err(PlatformError::Unavailable(format!(
                                "network resource {path} must be root-owned and not writable by other users"
                            )));
                        }
                    }
                }
            }
            _ => {
                return Err(PlatformError::Unavailable(
                    "package must configure both network helper and polkit policy".into(),
                ));
            }
        }
        let output = std::process::Command::new(Self::helper_path())
            .arg("protocol-version")
            .output()
            .map_err(|e| PlatformError::Unavailable(format!(
                "Rust network helper is unavailable: {e}; install the matching floppa-network-helper package or run its install command as root"
            )))?;
        if !output.status.success() || output.stdout != b"2\n" {
            return Err(PlatformError::Unavailable(
                "network helper protocol mismatch; install the matching Rust floppa-network-helper"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Run the network helper, through polkit or directly.
    ///
    /// With the polkit policy installed, the escalated path runs without a password prompt for
    /// active desktop sessions. Runs on tokio's process reactor rather than blocking a worker
    /// thread, so a slow pkexec stalls only this call; `timeout` bounds the wait for undo paths.
    ///
    /// Packaged processes use the build-time path. Portable root bundles use a sibling
    /// binary; desktop clients use the administrator-installed polkit target.
    async fn run_helper(
        &self,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<(), PlatformError> {
        debug!("Running helper: {:?}", args);

        let root = Self::running_as_root();
        let helper = Self::helper_path();
        let mut command = if root {
            tokio::process::Command::new(&helper)
        } else {
            let mut command = tokio::process::Command::new("pkexec");
            command.arg(&helper);
            command
        };
        command.args(args).kill_on_drop(true);
        let spawned = command.output();
        let output = match timeout {
            Some(limit) => tokio::time::timeout(limit, spawned).await.map_err(|_| {
                PlatformError::Failed(format!(
                    "network helper {} timed out after {}s",
                    args.first().copied().unwrap_or_default(),
                    limit.as_secs()
                ))
            })?,
            None => spawned.await,
        }
        .map_err(|e| PlatformError::Unavailable(format!("failed to run network helper: {e}")))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // Only meaningful for the polkit path: pkexec exits 126 when authorisation is declined or
        // dismissed, and 127 when the helper could not be launched at all. Both are pointless to
        // retry with another protocol. Run directly, those codes are the helper's own.
        if root {
            return Err(PlatformError::Failed(format!(
                "network helper failed: {stderr}"
            )));
        }
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

    /// Get the default gateway for `family` from the routing table (no privileges needed).
    ///
    /// `ip route show default` lists routes lowest metric first, so the first `via` is the one
    /// the kernel would use.
    async fn get_default_gateway(family: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        let flag = match family {
            IpFamily::V4 => "-4",
            IpFamily::V6 => "-6",
        };
        let output = tokio::process::Command::new("ip")
            .args([flag, "route", "show", "default"])
            .output()
            .await
            .map_err(|e| PlatformError::Failed(format!("failed to read default route: {e}")))?;
        let route_output = String::from_utf8_lossy(&output.stdout);
        parse_default_gateway(&route_output)
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
    fn has_shared_recovery(&self) -> bool {
        true
    }

    async fn recover_network(&self) -> Result<(), PlatformError> {
        self.ensure_helper().await?;
        self.run_helper(&["recover-network"], Some(UNDO_TIMEOUT))
            .await
    }

    fn uses_policy_routing(&self) -> bool {
        true
    }

    async fn capture_policy_routing(
        &self,
        routes: &[IpNetwork],
    ) -> Result<Option<PolicyRoutingSnapshot>, PlatformError> {
        if routes.is_empty() {
            return Ok(None);
        }
        let ipv4 = routes.iter().any(IpNetwork::is_ipv4);
        let ipv6 = routes.iter().any(IpNetwork::is_ipv6);
        let src_valid_mark = None;
        Ok(Some(PolicyRoutingSnapshot {
            ipv4,
            ipv6,
            src_valid_mark,
        }))
    }

    async fn apply_policy_routing(
        &self,
        snapshot: &PolicyRoutingSnapshot,
    ) -> Result<(), PlatformError> {
        self.run_helper(
            &[
                "add-policy",
                if snapshot.ipv4 { "1" } else { "0" },
                if snapshot.ipv6 { "1" } else { "0" },
            ],
            None,
        )
        .await
    }

    async fn restore_policy_routing(
        &self,
        snapshot: &PolicyRoutingSnapshot,
    ) -> Result<(), PlatformError> {
        let _ = snapshot;
        self.run_helper(&["recover-policy"], Some(UNDO_TIMEOUT))
            .await
    }

    fn tun_params(&self) -> TunParams {
        TunParams {
            manage_device: false,
            wintun_file: None,
        }
    }

    /// Verify the packaged helper and protocol before any privileged mutation.
    async fn preflight(&self) -> Result<(), PlatformError> {
        crate::socket_mark::preflight()
            .map_err(|e| PlatformError::PermissionDenied(e.to_string()))?;
        self.ensure_helper().await?;
        // Retry any leftover undo before taking new snapshots, including after a failed attempt.
        self.run_helper(&["recover-network"], Some(UNDO_TIMEOUT))
            .await?;
        self.run_helper(&["check-policy"], None)
            .await
            .map_err(|error| match error {
                PlatformError::Failed(detail) => PlatformError::Unavailable(detail),
                other => other,
            })
    }

    async fn prepare_link(&self, iface: &InterfaceName) -> Result<(), PlatformError> {
        // Create a persistent TUN owned by the current user, so gotatun can open it unprivileged.
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| PlatformError::Failed(format!("failed to read process metadata: {e}")))?
            .uid();
        self.run_helper(&["ensure-tun", iface.as_str(), &uid.to_string()], None)
            .await
    }

    async fn release_link(&self, _iface: &InterfaceName) -> Result<(), PlatformError> {
        // Remove only the link recorded by the helper; a subsequent owner may have another UID.
        self.run_helper(&["recover-link"], Some(UNDO_TIMEOUT)).await
    }

    async fn configure_mtu(&self, iface: &InterfaceName, mtu: u16) -> Result<(), PlatformError> {
        self.run_helper(&["set-mtu", iface.as_str(), &mtu.to_string()], None)
            .await
    }

    async fn configure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        info!("Configuring address {addr} on interface {iface}");
        self.run_helper(&["configure", iface.as_str(), &addr.to_string()], None)
            .await
    }

    async fn deconfigure_address(
        &self,
        iface: &InterfaceName,
        addr: IpNetwork,
    ) -> Result<(), PlatformError> {
        self.run_helper(
            &["flush-addr", iface.as_str(), &addr.to_string()],
            Some(UNDO_TIMEOUT),
        )
        .await
    }

    async fn default_gateway(&self, family: IpFamily) -> Result<Option<Gateway>, PlatformError> {
        Self::get_default_gateway(family).await
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
        let _ = (endpoint, gateway);
        Err(PlatformError::Unavailable(
            "Linux uses marked transport sockets, not endpoint routes".into(),
        ))
    }

    async fn remove_endpoint_route(
        &self,
        endpoint: IpAddr,
        gateway: Option<&Gateway>,
    ) -> Result<(), PlatformError> {
        let _ = (endpoint, gateway);
        Err(PlatformError::Unavailable(
            "Linux has no endpoint route to restore".into(),
        ))
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
        self.run_helper(&args, None).await
    }

    async fn remove_routes(
        &self,
        iface: &InterfaceName,
        routes: &[IpNetwork],
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        let _ = (iface, routes);
        self.run_helper(&["recover-routes"], Some(UNDO_TIMEOUT))
            .await
    }

    async fn capture_dns(
        &self,
        _iface: &InterfaceName,
        _if_index: Option<u32>,
    ) -> Result<DnsSnapshot, PlatformError> {
        // The exact file/link snapshot is captured by the privileged helper before mutation.
        Ok(DnsSnapshot::ResolvConf {
            content: None,
            symlink_target: None,
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

        // Choose and journal the backend together in the privileged helper. Recovery uses
        // that journal even if the system's DNS owner changes before disconnect.
        let strs: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
        let mut args = vec!["set-dns-auto", iface.as_str()];
        args.extend(strs.iter().map(String::as_str));
        self.run_helper(&args, None).await
    }

    async fn restore_dns(
        &self,
        iface: &InterfaceName,
        snapshot: &DnsSnapshot,
        _if_index: Option<u32>,
    ) -> Result<(), PlatformError> {
        let _ = (iface, snapshot);
        self.run_helper(&["recover-dns"], Some(UNDO_TIMEOUT)).await
    }

    async fn ipv6_enabled(&self) -> bool {
        let enabled = Self::is_ipv6_enabled();
        if !enabled {
            info!("IPv6 is disabled on host, skipping IPv6 VPN routes");
        }
        enabled
    }
}

/// Parse the first `via` next hop out of `ip route show default` output.
///
/// A next hop that is not an address is an error, not a gateway: the string used to be passed on
/// to the helper as-is and only rejected there.
fn parse_default_gateway(route_output: &str) -> Result<Option<Gateway>, PlatformError> {
    route::parse_default_route(route_output)
        .map(|gateway| gateway.map(|gateway| Gateway(gateway.via)))
        .map_err(|e| PlatformError::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_resources_must_exist_and_helper_must_be_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        let policy = dir.path().join("policy");
        let validate =
            || check_packaged_resources(helper.to_str().unwrap(), policy.to_str().unwrap());
        assert!(validate().is_err());
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&policy, "packaged policy").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validate().is_err());
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate().is_ok());
        std::fs::remove_file(&policy).unwrap();
        assert!(validate().is_err());
    }

    #[test]
    fn gateway_is_the_first_via() {
        let out = "default via 192.168.1.1 dev wlan0 proto dhcp metric 600\n\
                   default via 10.0.0.1 dev eth0 proto static metric 700\n";
        assert_eq!(
            parse_default_gateway(out).unwrap(),
            Some("192.168.1.1".parse().unwrap())
        );
    }

    #[test]
    fn ipv6_gateway_parses() {
        let out = "default via fe80::1 dev wlan0 proto ra metric 600 pref medium\n";
        assert_eq!(
            parse_default_gateway(out).unwrap(),
            Some("fe80::1".parse().unwrap())
        );
    }

    #[test]
    fn no_default_route_is_none() {
        assert_eq!(parse_default_gateway("").unwrap(), None);
    }

    #[test]
    fn a_non_address_next_hop_is_an_error() {
        assert!(parse_default_gateway("default via garbage dev eth0").is_err());
    }
}
