//! Traffic Control (tc) module for per-peer bandwidth limiting using HFSC.
//!
//! Uses Linux tc with HFSC (Hierarchical Fair Service Curve) qdisc for
//! precise bandwidth control. Handles both egress (outbound) and ingress
//! (inbound via IFB device) traffic shaping.

use anyhow::{Context, Result, anyhow};
use std::process::Command;
use tracing::info;

/// IFB device name for ingress traffic shaping
fn ifb_device(interface: &str) -> String {
    format!("ifb-{}", interface.trim_start_matches("wg-"))
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
    let ifb = ifb_device(interface);

    // Clean up any existing qdiscs (ignore errors if none exist)
    let _ = tc(&["qdisc", "del", "dev", interface, "root"]);
    let _ = tc(&["qdisc", "del", "dev", interface, "ingress"]);
    let _ = Command::new("ip").args(["link", "del", &ifb]).status();

    // === EGRESS (outbound) setup ===
    // Create HFSC root qdisc with default class 1:99 (unlimited peers go here)
    tc(&[
        "qdisc", "add", "dev", interface, "root", "handle", "1:", "hfsc", "default", "99",
    ])?;

    // Root class with total available bandwidth
    let rate = format!("{}mbit", total_bandwidth_mbit);
    tc(&[
        "class", "add", "dev", interface, "parent", "1:", "classid", "1:1", "hfsc", "sc", "rate",
        &rate, "ul", "rate", &rate,
    ])?;

    // Default class for unlimited peers (gets full bandwidth, no hard cap)
    tc(&[
        "class", "add", "dev", interface, "parent", "1:1", "classid", "1:99", "hfsc", "ls", "rate",
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
        "qdisc", "add", "dev", &ifb, "root", "handle", "1:", "hfsc", "default", "99",
    ])?;

    // Root class on IFB
    tc(&[
        "class", "add", "dev", &ifb, "parent", "1:", "classid", "1:1", "hfsc", "sc", "rate", &rate,
        "ul", "rate", &rate,
    ])?;

    // Default class on IFB for unlimited peers
    tc(&[
        "class", "add", "dev", &ifb, "parent", "1:1", "classid", "1:99", "hfsc", "ls", "rate",
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
/// # Arguments
/// * `interface` - WireGuard interface name
/// * `peer_ip` - Peer's assigned IP (e.g., "10.100.0.5")
/// * `rate_mbit` - Bandwidth limit in Mbps
pub fn add_peer_limit(interface: &str, peer_ip: &str, rate_mbit: u32) -> Result<()> {
    let ifb = ifb_device(interface);
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = format!("1:{}", class_id);
    let rate = format!("{}mbit", rate_mbit);
    let dst = format!("{}/32", peer_ip);
    let src = format!("{}/32", peer_ip);

    // === EGRESS (traffic TO peer) ===
    // Create class with rate limit
    tc(&[
        "class",
        "add",
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

    // Filter to match destination IP
    tc(&[
        "filter",
        "add",
        "dev",
        interface,
        "parent",
        "1:",
        "protocol",
        "ip",
        "prio",
        "1",
        "u32",
        "match",
        "ip",
        "dst",
        &dst,
        "classid",
        &classid_str,
    ])?;

    // === INGRESS (traffic FROM peer, via IFB) ===
    // Create class on IFB
    tc(&[
        "class",
        "add",
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

    // Filter to match source IP on IFB
    tc(&[
        "filter",
        "add",
        "dev",
        &ifb,
        "parent",
        "1:",
        "protocol",
        "ip",
        "prio",
        "1",
        "u32",
        "match",
        "ip",
        "src",
        &src,
        "classid",
        &classid_str,
    ])?;

    info!(peer_ip, rate_mbit, class_id, "Added rate limit for peer");

    Ok(())
}

/// Remove rate limit for a specific peer.
/// Removes the HFSC class and filter for both egress and ingress.
pub fn remove_peer_limit(interface: &str, peer_ip: &str) -> Result<()> {
    let ifb = ifb_device(interface);
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = format!("1:{}", class_id);
    let dst = format!("{}/32", peer_ip);
    let src = format!("{}/32", peer_ip);

    // Remove egress filter and class
    let _ = tc(&[
        "filter", "del", "dev", interface, "parent", "1:", "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "dst", &dst,
    ]);
    let _ = tc(&[
        "class",
        "del",
        "dev",
        interface,
        "parent",
        "1:1",
        "classid",
        &classid_str,
    ]);

    // Remove ingress filter and class from IFB
    let _ = tc(&[
        "filter", "del", "dev", &ifb, "parent", "1:", "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "src", &src,
    ]);
    let _ = tc(&[
        "class",
        "del",
        "dev",
        &ifb,
        "parent",
        "1:1",
        "classid",
        &classid_str,
    ]);

    info!(peer_ip, class_id, "Removed rate limit for peer");

    Ok(())
}

/// Update rate limit for an existing peer.
/// Uses tc class change to modify the existing class.
pub fn update_peer_limit(interface: &str, peer_ip: &str, rate_mbit: u32) -> Result<()> {
    let ifb = ifb_device(interface);
    let class_id = ip_to_class_id(peer_ip)?;
    let classid_str = format!("1:{}", class_id);
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

/// Cleanup all traffic control rules. Called on daemon shutdown.
pub fn cleanup_tc(interface: &str) -> Result<()> {
    let ifb = ifb_device(interface);

    let _ = tc(&["qdisc", "del", "dev", interface, "root"]);
    let _ = tc(&["qdisc", "del", "dev", interface, "ingress"]);
    let _ = Command::new("ip").args(["link", "del", &ifb]).status();

    info!(interface, "Traffic control cleaned up");

    Ok(())
}

/// Convert peer IP to a tc class ID.
/// Uses the last two octets to create a unique ID (supports /16 subnets).
/// Example: 10.100.5.42 -> 542
fn ip_to_class_id(ip: &str) -> Result<u32> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return Err(anyhow!("Invalid IP address: {}", ip));
    }

    let third: u32 = parts[2].parse().context("Invalid IP octet")?;
    let fourth: u32 = parts[3].parse().context("Invalid IP octet")?;

    // Class ID: third_octet * 256 + fourth_octet
    // This gives us unique IDs for a /16 subnet
    // Avoid 0 and 99 (default class)
    let class_id = third * 256 + fourth;
    if class_id == 0 || class_id == 1 || class_id == 99 {
        return Err(anyhow!("Reserved class ID for IP: {}", ip));
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
    fn test_ip_to_class_id() {
        assert_eq!(ip_to_class_id("10.100.0.5").unwrap(), 5);
        assert_eq!(ip_to_class_id("10.100.1.5").unwrap(), 261); // 1*256 + 5
        assert_eq!(ip_to_class_id("10.100.0.100").unwrap(), 100);
        assert!(ip_to_class_id("10.100.0.1").is_err()); // class_id 1 collides with root HFSC class
        assert!(ip_to_class_id("invalid").is_err());
    }

    #[test]
    fn test_ifb_device_name() {
        assert_eq!(ifb_device("wg-floppa"), "ifb-floppa");
        assert_eq!(ifb_device("wg0"), "ifb-wg0");
    }
}
