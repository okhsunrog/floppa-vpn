use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use floppa_core::AmneziaWgConfig;
use std::io::Write;
use std::process::{Command, Stdio};
use tracing::{debug, info};

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
        debug!(interface, "WireGuard interface already exists");
        return Ok(());
    }

    info!(interface, "Creating WireGuard interface");

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

    info!(
        interface,
        address, listen_port, "WireGuard interface created"
    );
    Ok(())
}

/// Ensure the AmneziaWG interface exists and is configured.
///
/// AmneziaWG is WireGuard plus interface-wide obfuscation. The kernel `amneziawg` module
/// provides the `amneziawg` link type, and `awg` is a drop-in superset of `wg`. We bring the
/// interface up with `awg setconf` (the same path `awg-quick` uses), feeding it an
/// `[Interface]`-only config (PrivateKey + ListenPort + obfuscation params; Address/DNS/MTU
/// are kernel-level and applied via `ip`, not `awg`).
pub fn ensure_awg_interface(awg: &AmneziaWgConfig, private_key: &str) -> Result<()> {
    let interface = &awg.interface;
    if interface_exists(interface) {
        // Interface persists across daemon restarts. Reconcile the obfuscation params from config
        // (via device-level `awg set`, which leaves peers + key + listen-port intact) so changes
        // to the params take effect on restart without a manual interface recreation.
        debug!(
            interface,
            "AmneziaWG interface exists; reconciling obfuscation params"
        );
        reconcile_awg_obfuscation(interface, &awg.obfuscation)?;
        return Ok(());
    }

    info!(interface, "Creating AmneziaWG interface");

    let status = Command::new("ip")
        .args(["link", "add", "dev", interface, "type", "amneziawg"])
        .status()
        .context("Failed to create AmneziaWG interface (is the amneziawg kernel module loaded?)")?;
    if !status.success() {
        return Err(anyhow!("ip link add type amneziawg failed"));
    }

    // [Interface] config for `awg setconf` (peerless at creation; peers are added incrementally).
    let conf = build_awg_setconf(awg, private_key);
    let mut child = Command::new("awg")
        .args(["setconf", interface, "/dev/stdin"])
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to spawn awg setconf")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(conf.as_bytes())?;
    }
    if !child.wait()?.success() {
        return Err(anyhow!("awg setconf failed"));
    }

    let address = format!("{}/{}", awg.get_server_ip(), awg.client_subnet.prefix());
    let status = Command::new("ip")
        .args(["address", "add", &address, "dev", interface])
        .status()
        .context("Failed to assign AmneziaWG IP address")?;
    if !status.success() {
        return Err(anyhow!("ip address add failed"));
    }

    let status = Command::new("ip")
        .args(["link", "set", interface, "up"])
        .status()
        .context("Failed to bring AmneziaWG interface up")?;
    if !status.success() {
        return Err(anyhow!("ip link set up failed"));
    }

    info!(
        interface,
        address,
        port = awg.get_listen_port(),
        "AmneziaWG interface created"
    );
    Ok(())
}

/// Build the `awg setconf` `[Interface]` block (no Address/DNS/MTU — those are not `awg` keys).
fn build_awg_setconf(awg: &AmneziaWgConfig, private_key: &str) -> String {
    let o = &awg.obfuscation;
    let mut s = format!(
        "[Interface]\nPrivateKey = {}\nListenPort = {}\n",
        private_key.trim(),
        awg.get_listen_port(),
    );
    s.push_str(&format!(
        "Jc = {}\nJmin = {}\nJmax = {}\n",
        o.jc, o.jmin, o.jmax
    ));
    s.push_str(&format!(
        "S1 = {}\nS2 = {}\nS3 = {}\nS4 = {}\n",
        o.s1, o.s2, o.s3, o.s4
    ));
    s.push_str(&format!(
        "H1 = {}\nH2 = {}\nH3 = {}\nH4 = {}\n",
        o.h1, o.h2, o.h3, o.h4
    ));
    for (n, val) in [(1, &o.i1), (2, &o.i2), (3, &o.i3), (4, &o.i4), (5, &o.i5)] {
        if !val.is_empty() {
            s.push_str(&format!("I{n} = {val}\n"));
        }
    }
    s
}

/// Re-apply AmneziaWG obfuscation params to an existing interface via device-level `awg set`.
/// Unlike `awg setconf`, this leaves peers, private key, and listen port untouched, so it can
/// reconcile config changes on a live interface without dropping connections.
fn reconcile_awg_obfuscation(
    interface: &str,
    o: &floppa_core::config::AwgObfuscation,
) -> Result<()> {
    let mut args: Vec<String> = vec!["set".into(), interface.into()];
    for (k, v) in [
        ("jc", o.jc.to_string()),
        ("jmin", o.jmin.to_string()),
        ("jmax", o.jmax.to_string()),
        ("s1", o.s1.to_string()),
        ("s2", o.s2.to_string()),
        ("s3", o.s3.to_string()),
        ("s4", o.s4.to_string()),
        ("h1", o.h1.clone()),
        ("h2", o.h2.clone()),
        ("h3", o.h3.clone()),
        ("h4", o.h4.clone()),
    ] {
        args.push(k.into());
        args.push(v);
    }
    // I-packets are initiator-only; the responder (server) doesn't send them, but set any that
    // are configured for completeness. (Empty slots are left as-is.)
    for (n, val) in [(1, &o.i1), (2, &o.i2), (3, &o.i3), (4, &o.i4), (5, &o.i5)] {
        if !val.is_empty() {
            args.push(format!("i{n}"));
            args.push(val.clone());
        }
    }

    let status = Command::new("awg")
        .args(&args)
        .status()
        .context("Failed to spawn awg set for obfuscation reconcile")?;
    if !status.success() {
        return Err(anyhow!("awg set (obfuscation reconcile) failed"));
    }
    Ok(())
}

/// Add a peer to a WireGuard/AmneziaWG interface (`tool` is "wg" or "awg").
pub fn add_peer(tool: &str, interface: &str, public_key: &str, allowed_ip: &str) -> Result<()> {
    let status = Command::new(tool)
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
        return Err(anyhow!("{tool} set failed with status: {}", status));
    }

    Ok(())
}

/// Remove a peer from a WireGuard/AmneziaWG interface (`tool` is "wg" or "awg").
pub fn remove_peer(tool: &str, interface: &str, public_key: &str) -> Result<()> {
    let status = Command::new(tool)
        .args(["set", interface, "peer", public_key, "remove"])
        .status()?;

    if !status.success() {
        return Err(anyhow!("{tool} set remove failed with status: {}", status));
    }

    Ok(())
}

/// Get traffic stats for all peers on an interface (`tool` is "wg" or "awg").
pub fn get_peer_stats(tool: &str, interface: &str) -> Result<PeerStats> {
    let output = Command::new(tool)
        .args(["show", interface, "dump"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("{tool} show dump failed"));
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
