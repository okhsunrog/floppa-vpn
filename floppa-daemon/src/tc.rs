//! Traffic Control (tc) module for per-peer bandwidth limiting using HFSC.
//!
//! Uses Linux tc with HFSC (Hierarchical Fair Service Curve) qdisc for
//! precise bandwidth control. Handles both egress (outbound) and ingress
//! (inbound via IFB device) traffic shaping.

use anyhow::{Context, Result, anyhow};
use std::process::Command;
use tracing::info;

/// Linux IFNAMSIZ is 16 including the NUL terminator.
const MAX_IFNAME_LEN: usize = 15;

/// HFSC minor id of the default class (unlimited peers). `0xffff` is never a valid
/// client class id — it would be the `.255.255` broadcast address of a /16.
const DEFAULT_CLASS: &str = "ffff";

#[derive(Debug, thiserror::Error)]
pub enum TcError {
    #[error("IFB device name '{0}' exceeds {MAX_IFNAME_LEN} characters")]
    IfbNameTooLong(String),
    #[error("invalid peer IPv4 address '{0}'")]
    InvalidPeerIp(String),
    #[error("peer IP '{0}' maps to a reserved tc class id")]
    ReservedClassId(String),
}

/// IFB device name for ingress traffic shaping
fn ifb_device(interface: &str) -> std::result::Result<String, TcError> {
    let name = format!("ifb-{}", interface.trim_start_matches("wg-"));
    if name.len() > MAX_IFNAME_LEN {
        return Err(TcError::IfbNameTooLong(name));
    }
    Ok(name)
}

/// Render a peer's tc class id. tc parses the minor as hex, so the number and the
/// text must agree: `0x105` is "1:105", not "1:261".
fn class_id_str(class_id: u16) -> String {
    format!("1:{class_id:x}")
}

/// Setup traffic control infrastructure on the WireGuard interface.
/// Must be called once on daemon startup before adding any peer limits.
///
/// Creates:
/// - HFSC root qdisc on the WG interface (egress)
/// - IFB device for ingress shaping
/// - Ingress qdisc to redirect traffic to IFB
/// - HFSC root qdisc on IFB
pub fn setup_tc(interface: &str, total_bandwidth_mbit: u32) -> Result<()> {
    let ifb = ifb_device(interface)?;

    // Clean up any existing qdiscs (ignore errors if none exist)
    let _ = tc(&["qdisc", "del", "dev", interface, "root"]);
    let _ = tc(&["qdisc", "del", "dev", interface, "ingress"]);
    let _ = Command::new("ip").args(["link", "del", &ifb]).status();

    // === EGRESS (outbound) setup ===
    // Create HFSC root qdisc with default class 1:ffff (unlimited peers go here)
    tc(&[
        "qdisc",
        "add",
        "dev",
        interface,
        "root",
        "handle",
        "1:",
        "hfsc",
        "default",
        DEFAULT_CLASS,
    ])?;

    // Root class with total available bandwidth
    let rate = format!("{}mbit", total_bandwidth_mbit);
    tc(&[
        "class", "add", "dev", interface, "parent", "1:", "classid", "1:1", "hfsc", "sc", "rate",
        &rate, "ul", "rate", &rate,
    ])?;

    // Default class for unlimited peers (gets full bandwidth, no hard cap)
    let default_classid = format!("1:{DEFAULT_CLASS}");
    tc(&[
        "class",
        "add",
        "dev",
        interface,
        "parent",
        "1:1",
        "classid",
        &default_classid,
        "hfsc",
        "ls",
        "rate",
        &rate,
    ])?;

    // === INGRESS (inbound) setup via IFB ===
    // Load IFB kernel module (required for IFB device creation)
    let _ = Command::new("modprobe").arg("ifb").status();

    // Create IFB device
    let ifb_output = Command::new("ip")
        .args(["link", "add", "name", &ifb, "type", "ifb"])
        .output()
        .context("Failed to create IFB device")?;
    if !ifb_output.status.success() {
        let stderr = String::from_utf8_lossy(&ifb_output.stderr);
        return Err(anyhow!(
            "Failed to create IFB device {}: {}",
            ifb,
            stderr.trim()
        ));
    }

    Command::new("ip")
        .args(["link", "set", &ifb, "up"])
        .status()
        .context("Failed to bring up IFB device")?;

    // Create ingress qdisc to redirect incoming traffic
    tc(&[
        "qdisc", "add", "dev", interface, "handle", "ffff:", "ingress",
    ])?;

    // Redirect all ingress traffic to IFB device
    tc(&[
        "filter", "add", "dev", interface, "parent", "ffff:", "matchall", "action", "mirred",
        "egress", "redirect", "dev", &ifb,
    ])?;

    // Create HFSC qdisc on IFB for ingress shaping
    tc(&[
        "qdisc",
        "add",
        "dev",
        &ifb,
        "root",
        "handle",
        "1:",
        "hfsc",
        "default",
        DEFAULT_CLASS,
    ])?;

    // Root class on IFB
    tc(&[
        "class", "add", "dev", &ifb, "parent", "1:", "classid", "1:1", "hfsc", "sc", "rate", &rate,
        "ul", "rate", &rate,
    ])?;

    // Default class on IFB for unlimited peers
    tc(&[
        "class",
        "add",
        "dev",
        &ifb,
        "parent",
        "1:1",
        "classid",
        &default_classid,
        "hfsc",
        "ls",
        "rate",
        &rate,
    ])?;

    info!(
        interface,
        ifb = %ifb,
        total_bandwidth_mbit,
        "Traffic control initialized"
    );

    Ok(())
}

/// Add rate limit for a specific peer.
/// Creates HFSC class and filter for both egress and ingress.
///
/// Idempotent: the class is `replace`d and the filter is deleted before it is
/// re-added, so a retry after a partial failure (class created, filter not)
/// converges instead of erroring out or stacking duplicate filters.
///
/// # Arguments
/// * `interface` - WireGuard interface name
/// * `peer_ip` - Peer's assigned IP (e.g., "10.100.0.5")
/// * `rate_mbit` - Bandwidth limit in Mbps
pub fn add_peer_limit(interface: &str, peer_ip: &str, rate_mbit: u32) -> Result<()> {
    let ifb = ifb_device(interface)?;
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = class_id_str(class_id);
    let prio = class_id.to_string();
    let rate = format!("{}mbit", rate_mbit);
    let ip_mask = format!("{}/32", peer_ip);

    // Egress: traffic TO the peer (match dst). Ingress: traffic FROM the peer, via IFB (match src).
    for (dev, direction) in [(interface, "dst"), (ifb.as_str(), "src")] {
        tc(&[
            "class",
            "replace",
            "dev",
            dev,
            "parent",
            "1:1",
            "classid",
            &classid_str,
            "hfsc",
            "ls",
            "rate",
            &rate,
            "ul",
            "rate",
            &rate,
        ])?;

        // Clear a filter left behind by a previous partial attempt (ignore "not found").
        let _ = tc(&[
            "filter", "del", "dev", dev, "parent", "1:", "protocol", "ip", "prio", &prio,
        ]);
        tc(&[
            "filter",
            "add",
            "dev",
            dev,
            "parent",
            "1:",
            "protocol",
            "ip",
            "prio",
            &prio,
            "u32",
            "match",
            "ip",
            direction,
            &ip_mask,
            "classid",
            &classid_str,
        ])?;
    }

    info!(peer_ip, rate_mbit, class_id, "Added rate limit for peer");

    Ok(())
}

/// Remove rate limit for a specific peer.
/// Removes the HFSC class and filter for both egress and ingress.
pub fn remove_peer_limit(interface: &str, peer_ip: &str) -> Result<()> {
    let ifb = ifb_device(interface)?;
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = class_id_str(class_id);
    let prio = class_id.to_string();

    for dev in [interface, ifb.as_str()] {
        // Each peer's filter lives at its own prio: deleting by prio without a
        // handle removes every filter at that prio, which is exactly this peer's.
        let _ = tc(&[
            "filter", "del", "dev", dev, "parent", "1:", "protocol", "ip", "prio", &prio,
        ]);
        let _ = tc(&[
            "class",
            "del",
            "dev",
            dev,
            "parent",
            "1:1",
            "classid",
            &classid_str,
        ]);
    }

    info!(peer_ip, class_id, "Removed rate limit for peer");

    Ok(())
}

/// Update rate limit for an existing peer.
/// Uses tc class change to modify the existing class.
fn update_peer_limit(interface: &str, peer_ip: &str, rate_mbit: u32) -> Result<()> {
    let ifb = ifb_device(interface)?;
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = class_id_str(class_id);
    let rate = format!("{}mbit", rate_mbit);

    // Update egress class
    tc(&[
        "class",
        "change",
        "dev",
        interface,
        "parent",
        "1:1",
        "classid",
        &classid_str,
        "hfsc",
        "ls",
        "rate",
        &rate,
        "ul",
        "rate",
        &rate,
    ])?;

    // Update ingress class on IFB
    tc(&[
        "class",
        "change",
        "dev",
        &ifb,
        "parent",
        "1:1",
        "classid",
        &classid_str,
        "hfsc",
        "ls",
        "rate",
        &rate,
        "ul",
        "rate",
        &rate,
    ])?;

    info!(peer_ip, rate_mbit, class_id, "Updated rate limit for peer");

    Ok(())
}

/// Set the rate limit for a peer, whether or not it already has one.
///
/// Tries the cheap in-place class change first (the usual case for a
/// subscription change) and falls back to creating class + filters when the
/// class does not exist yet (fresh daemon start, tc rules are ephemeral).
pub fn set_peer_limit(interface: &str, peer_ip: &str, rate_mbit: u32) -> Result<()> {
    update_peer_limit(interface, peer_ip, rate_mbit)
        .or_else(|_| add_peer_limit(interface, peer_ip, rate_mbit))
}

/// Cleanup all traffic control rules. Called on daemon shutdown.
pub fn cleanup_tc(interface: &str) -> Result<()> {
    let ifb = ifb_device(interface)?;

    let _ = tc(&["qdisc", "del", "dev", interface, "root"]);
    let _ = tc(&["qdisc", "del", "dev", interface, "ingress"]);
    let _ = Command::new("ip").args(["link", "del", &ifb]).status();

    info!(interface, "Traffic control cleaned up");

    Ok(())
}

/// Convert a peer IP to its tc class id: the host offset within a /16 (last two
/// octets), rendered in hex by [`class_id_str`]. `1:1` is the root class and
/// `1:ffff` the default class; neither is a valid client address in a /16.
fn ip_to_class_id(ip: &str) -> std::result::Result<u16, TcError> {
    let addr: std::net::Ipv4Addr = ip
        .parse()
        .map_err(|_| TcError::InvalidPeerIp(ip.to_string()))?;
    let [_, _, third, fourth] = addr.octets();
    let class_id = u16::from_be_bytes([third, fourth]);
    if class_id <= 1 || class_id == u16::MAX {
        return Err(TcError::ReservedClassId(ip.to_string()));
    }
    Ok(class_id)
}

/// Execute a tc command
fn tc(args: &[&str]) -> Result<()> {
    let output = Command::new("tc")
        .args(args)
        .output()
        .context("Failed to execute tc command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("tc {} failed: {}", args.join(" "), stderr.trim()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_id_is_host_offset_in_slash16() {
        assert_eq!(ip_to_class_id("10.100.0.5").unwrap(), 5);
        assert_eq!(ip_to_class_id("10.100.0.99").unwrap(), 99);
        assert_eq!(ip_to_class_id("10.100.0.255").unwrap(), 255);
        assert_eq!(ip_to_class_id("10.100.1.5").unwrap(), 261);
        assert_eq!(ip_to_class_id("10.100.255.254").unwrap(), 65534);
        // 1:1 is the root class, 1:ffff the default class
        assert!(matches!(
            ip_to_class_id("10.100.0.0"),
            Err(TcError::ReservedClassId(_))
        ));
        assert!(matches!(
            ip_to_class_id("10.100.0.1"),
            Err(TcError::ReservedClassId(_))
        ));
        assert!(matches!(
            ip_to_class_id("10.100.255.255"),
            Err(TcError::ReservedClassId(_))
        ));
        assert!(matches!(
            ip_to_class_id("invalid"),
            Err(TcError::InvalidPeerIp(_))
        ));
    }

    #[test]
    fn class_id_renders_as_hex_minor() {
        // tc parses the minor as hex, so the text must be the hex form of the number
        assert_eq!(class_id_str(5), "1:5");
        assert_eq!(class_id_str(99), "1:63");
        assert_eq!(class_id_str(261), "1:105");
        assert_eq!(class_id_str(65534), "1:fffe");
        assert_ne!(class_id_str(65534), format!("1:{DEFAULT_CLASS}"));
    }

    #[test]
    fn ifb_device_name() {
        assert_eq!(ifb_device("wg-floppa").unwrap(), "ifb-floppa");
        assert_eq!(ifb_device("wg0").unwrap(), "ifb-wg0");
        assert_eq!(ifb_device("wg-elevenchars").unwrap(), "ifb-elevenchars");
        assert!(matches!(
            ifb_device("wg-twelvecharss"),
            Err(TcError::IfbNameTooLong(_))
        ));
    }
}
