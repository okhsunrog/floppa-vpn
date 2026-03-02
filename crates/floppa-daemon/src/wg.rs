use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::process::Command;

/// Peer statistics: (public_key, tx_bytes, rx_bytes, last_handshake)
pub type PeerStats = Vec<(String, u64, u64, Option<DateTime<Utc>>)>;

/// Check if WireGuard interface exists
fn interface_exists(interface: &str) -> bool {
    Command::new("ip")
        .args(["link", "show", interface])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure WireGuard interface exists and is configured.
/// Creates the interface if it doesn't exist.
pub fn ensure_interface(
    interface: &str,
    private_key: &str,
    listen_port: u16,
    server_ip: &str,
    subnet: &str,
) -> Result<()> {
    if interface_exists(interface) {
        tracing::debug!(interface, "WireGuard interface already exists");
        return Ok(());
    }

    tracing::info!(interface, "Creating WireGuard interface");

    // Create interface
    let status = Command::new("ip")
        .args(["link", "add", "dev", interface, "type", "wireguard"])
        .status()
        .context("Failed to create WireGuard interface")?;

    if !status.success() {
        return Err(anyhow!("ip link add failed"));
    }

    // Set private key using process substitution workaround
    // We write the key to wg via stdin
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("wg")
        .args([
            "set",
            interface,
            "private-key",
            "/dev/stdin",
            "listen-port",
            &listen_port.to_string(),
        ])
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to spawn wg set")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(private_key.trim().as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("wg set private-key failed"));
    }

    // Calculate address with prefix from subnet
    let prefix = subnet.split('/').nth(1).unwrap_or("24");
    let address = format!("{}/{}", server_ip, prefix);

    // Assign IP address
    let status = Command::new("ip")
        .args(["address", "add", &address, "dev", interface])
        .status()
        .context("Failed to assign IP address")?;

    if !status.success() {
        return Err(anyhow!("ip address add failed"));
    }

    // Bring interface up
    let status = Command::new("ip")
        .args(["link", "set", interface, "up"])
        .status()
        .context("Failed to bring interface up")?;

    if !status.success() {
        return Err(anyhow!("ip link set up failed"));
    }

    tracing::info!(
        interface,
        address,
        listen_port,
        "WireGuard interface created"
    );
    Ok(())
}

/// Add a peer to WireGuard interface
pub fn add_peer(interface: &str, public_key: &str, allowed_ip: &str) -> Result<()> {
    // Use wireguard-uapi for netlink-based management
    // For now, fall back to wg command
    let status = std::process::Command::new("wg")
        .args([
            "set",
            interface,
            "peer",
            public_key,
            "allowed-ips",
            &format!("{}/32", allowed_ip),
        ])
        .status()?;

    if !status.success() {
        return Err(anyhow!("wg set failed with status: {}", status));
    }

    Ok(())
}

/// Remove a peer from WireGuard interface
pub fn remove_peer(interface: &str, public_key: &str) -> Result<()> {
    let status = std::process::Command::new("wg")
        .args(["set", interface, "peer", public_key, "remove"])
        .status()?;

    if !status.success() {
        return Err(anyhow!("wg set remove failed with status: {}", status));
    }

    Ok(())
}

/// Get traffic stats for all peers
pub fn get_peer_stats(interface: &str) -> Result<PeerStats> {
    let output = std::process::Command::new("wg")
        .args(["show", interface, "dump"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("wg show dump failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut stats = Vec::new();

    // Skip first line (interface info), parse peer lines
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 7 {
            let public_key = parts[0].to_string();
            let last_handshake = parts[4]
                .parse::<i64>()
                .ok()
                .filter(|&t| t > 0)
                .and_then(|t| DateTime::from_timestamp(t, 0));
            let rx_bytes = parts[5].parse().unwrap_or(0);
            let tx_bytes = parts[6].parse().unwrap_or(0);

            stats.push((public_key, tx_bytes, rx_bytes, last_handshake));
        }
    }

    Ok(stats)
}
