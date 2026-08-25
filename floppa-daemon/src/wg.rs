use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use floppa_core::AmneziaWgConfig;
use floppa_core::config::AwgObfuscation;
use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use tracing::{debug, info, warn};

/// Userspace tool driving an interface: `wg`, or `awg` (a drop-in superset of `wg`
/// that also speaks the AmneziaWG obfuscation params).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgTool {
    Wg,
    Awg,
}

impl WgTool {
    /// Name of the CLI binary.
    pub fn binary(self) -> &'static str {
        match self {
            WgTool::Wg => "wg",
            WgTool::Awg => "awg",
        }
    }
}

impl fmt::Display for WgTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.binary())
    }
}

/// One peer's counters from `wg show <iface> dump`. Byte directions are the
/// server's: `rx` is what the peer sent us, `tx` what we sent the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStat {
    pub public_key: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub last_handshake: Option<DateTime<Utc>>,
}

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

/// Desired obfuscation params as `(key, value)` pairs in `awg set` / lowercase `showconf` form.
/// Empty I-packet slots are omitted: they are initiator-only and left as-is on the interface.
fn awg_obfuscation_params(o: &AwgObfuscation) -> Vec<(&'static str, String)> {
    let mut params = vec![
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
    ];
    for (k, val) in [
        ("i1", &o.i1),
        ("i2", &o.i2),
        ("i3", &o.i3),
        ("i4", &o.i4),
        ("i5", &o.i5),
    ] {
        if !val.is_empty() {
            params.push((k, val.clone()));
        }
    }
    params
}

/// Parse the `[Interface]` section of `wg/awg showconf` into lowercase `key → value`.
/// Peer sections are ignored. The map contains `privatekey`; never log it.
fn parse_showconf_interface(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut in_interface = false;
    for line in output.lines().map(str::trim) {
        if line.starts_with('[') {
            in_interface = line.eq_ignore_ascii_case("[Interface]");
            continue;
        }
        if !in_interface {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    map
}

/// Read the live `[Interface]` config of an interface.
fn read_interface_conf(tool: WgTool, interface: &str) -> Result<HashMap<String, String>> {
    let output = Command::new(tool.binary())
        .args(["showconf", interface])
        .output()
        .with_context(|| format!("Failed to run {tool} showconf"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{tool} showconf {interface} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_showconf_interface(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Canonical form of a header value: `awg` prints a degenerate range as `N-N` in some
/// versions and `N` in others, and config may spell it either way.
fn normalize_range(value: &str) -> String {
    match value.split_once('-') {
        Some((lo, hi)) if lo.trim() == hi.trim() => lo.trim().to_string(),
        _ => value.trim().to_string(),
    }
}

/// Whether every desired param already has that value on the interface.
fn awg_params_in_sync(current: &HashMap<String, String>, desired: &[(&str, String)]) -> bool {
    desired.iter().all(|(k, v)| {
        current
            .get(*k)
            .is_some_and(|cur| normalize_range(cur) == normalize_range(v))
    })
}

/// Re-apply AmneziaWG obfuscation params to an existing interface via device-level `awg set`,
/// but only when they differ from what the interface already has.
///
/// Every `awg set` on a live interface re-runs `jp_spec_setup` in the kernel module, and on
/// module builds before ac946a9 (2026-03-25) that races the handshake-send path when an
/// I-packet is configured — a use-after-free that has taken the whole VPS down. A daemon
/// restart must therefore not touch a correctly configured interface at all; the diff makes
/// the reconcile a pure read in the steady state.
fn reconcile_awg_obfuscation(interface: &str, o: &AwgObfuscation) -> Result<()> {
    let desired = awg_obfuscation_params(o);

    match read_interface_conf(WgTool::Awg, interface) {
        Ok(current) if awg_params_in_sync(&current, &desired) => {
            debug!(
                interface,
                "AmneziaWG obfuscation params already match config; skipping awg set"
            );
            return Ok(());
        }
        Ok(_) => info!(
            interface,
            "AmneziaWG obfuscation params differ from config; applying"
        ),
        Err(e) => warn!(
            interface,
            error = %e,
            "Could not read AmneziaWG interface config; applying obfuscation params blindly"
        ),
    }

    let mut args: Vec<String> = vec!["set".into(), interface.into()];
    for (k, v) in desired {
        args.push(k.into());
        args.push(v);
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

/// Add a peer to a WireGuard/AmneziaWG interface.
pub fn add_peer(tool: WgTool, interface: &str, public_key: &str, allowed_ip: &str) -> Result<()> {
    let status = Command::new(tool.binary())
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

/// Remove a peer from a WireGuard/AmneziaWG interface.
pub fn remove_peer(tool: WgTool, interface: &str, public_key: &str) -> Result<()> {
    let status = Command::new(tool.binary())
        .args(["set", interface, "peer", public_key, "remove"])
        .status()?;

    if !status.success() {
        return Err(anyhow!("{tool} set remove failed with status: {}", status));
    }

    Ok(())
}

/// Get traffic stats for all peers on an interface.
pub fn get_peer_stats(tool: WgTool, interface: &str) -> Result<Vec<PeerStat>> {
    let output = Command::new(tool.binary())
        .args(["show", interface, "dump"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("{tool} show dump failed"));
    }

    Ok(parse_wg_dump(&String::from_utf8_lossy(&output.stdout))?)
}

#[derive(Debug, thiserror::Error)]
pub enum DumpParseError {
    #[error("peer line {line} has {found} columns, expected at least {expected}")]
    MissingColumns {
        line: usize,
        found: usize,
        expected: usize,
    },
    #[error("peer line {line}: invalid {field} '{value}'")]
    InvalidNumber {
        line: usize,
        field: &'static str,
        value: String,
    },
}

/// Parse `wg show <iface> dump` / `awg show <iface> dump` output.
///
/// The first line describes the interface (its column count differs between `wg`
/// and `awg`) and is skipped. Every following line is one peer:
/// `public-key psk endpoint allowed-ips latest-handshake transfer-rx transfer-tx keepalive`.
pub fn parse_wg_dump(dump: &str) -> Result<Vec<PeerStat>, DumpParseError> {
    const COLUMNS: usize = 8;
    let mut stats = Vec::new();

    for (idx, line) in dump.lines().enumerate().skip(1) {
        let line_no = idx + 1;
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < COLUMNS {
            return Err(DumpParseError::MissingColumns {
                line: line_no,
                found: parts.len(),
                expected: COLUMNS,
            });
        }
        let number = |field: &'static str, value: &str| {
            value
                .parse::<u64>()
                .map_err(|_| DumpParseError::InvalidNumber {
                    line: line_no,
                    field,
                    value: value.to_string(),
                })
        };

        let handshake_secs = number("latest-handshake", parts[4])?;
        let last_handshake = (handshake_secs > 0)
            .then(|| DateTime::from_timestamp(handshake_secs as i64, 0))
            .flatten();

        stats.push(PeerStat {
            public_key: parts[0].to_string(),
            rx_bytes: number("transfer-rx", parts[5])?,
            tx_bytes: number("transfer-tx", parts[6])?,
            last_handshake,
        });
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOWCONF: &str = "[Interface]\nListenPort = 51821\nPrivateKey = cHJpdmF0ZQ==\nJc = 6\nJmin = 55\nJmax = 205\nS1 = 72\nS2 = 56\nS3 = 32\nS4 = 16\nH1 = 234567-345678\nH2 = 3456789-4567890\nH3 = 56789012-67890123\nH4 = 456789012-567890123\n\n[Peer]\nPublicKey = cGVlcg==\nAllowedIPs = 10.101.0.2/32\n";

    #[test]
    fn parses_interface_section_only() {
        let conf = parse_showconf_interface(SHOWCONF);
        assert_eq!(conf["listenport"], "51821");
        assert_eq!(conf["privatekey"], "cHJpdmF0ZQ==");
        assert_eq!(conf["h1"], "234567-345678");
        assert!(!conf.contains_key("publickey"));
        assert!(!conf.contains_key("allowedips"));
    }

    #[test]
    fn default_obfuscation_matches_its_own_showconf() {
        let conf = parse_showconf_interface(SHOWCONF);
        let mut o = AwgObfuscation::default();
        o.i1.clear();
        assert!(awg_params_in_sync(&conf, &awg_obfuscation_params(&o)));

        // A configured I1 that the interface lacks is a diff.
        o.i1 = "<b 0x01>".to_string();
        assert!(!awg_params_in_sync(&conf, &awg_obfuscation_params(&o)));

        // And so is any changed scalar.
        o.i1.clear();
        o.jc = 7;
        assert!(!awg_params_in_sync(&conf, &awg_obfuscation_params(&o)));
    }

    /// Captured from `wg show wg-floppa dump` (keys shortened).
    const DUMP: &str = "cHJpdmF0ZQ==\tc2VydmVy\t51820\toff\n\
        cGVlcjE=\t(none)\t203.0.113.7:41641\t10.100.0.2/32\t1700000000\t12345\t67890\toff\n\
        cGVlcjI=\t(none)\t(none)\t10.100.0.3/32\t0\t0\t0\toff\n";

    #[test]
    fn parses_dump_peer_lines() {
        let stats = parse_wg_dump(DUMP).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(
            stats[0],
            PeerStat {
                public_key: "cGVlcjE=".to_string(),
                rx_bytes: 12345,
                tx_bytes: 67890,
                last_handshake: DateTime::from_timestamp(1_700_000_000, 0),
            }
        );
        // No handshake yet → None, not the epoch
        assert_eq!(stats[1].public_key, "cGVlcjI=");
        assert_eq!(stats[1].last_handshake, None);
        assert_eq!((stats[1].rx_bytes, stats[1].tx_bytes), (0, 0));
    }

    #[test]
    fn dump_with_only_interface_line_has_no_peers() {
        assert_eq!(parse_wg_dump("k\tk\t51820\toff\n").unwrap(), vec![]);
        assert_eq!(parse_wg_dump("").unwrap(), vec![]);
    }

    #[test]
    fn malformed_dump_lines_are_errors() {
        assert!(matches!(
            parse_wg_dump("iface\npk\t(none)\t(none)\n"),
            Err(DumpParseError::MissingColumns {
                line: 2,
                found: 3,
                ..
            })
        ));
        assert!(matches!(
            parse_wg_dump("iface\npk\t(none)\t(none)\t(none)\t0\tlots\t0\toff\n"),
            Err(DumpParseError::InvalidNumber {
                line: 2,
                field: "transfer-rx",
                ..
            })
        ));
    }

    #[test]
    fn degenerate_ranges_compare_equal() {
        assert_eq!(normalize_range("5-5"), "5");
        assert_eq!(normalize_range("5"), "5");
        assert_eq!(normalize_range("5-9"), "5-9");
    }
}
