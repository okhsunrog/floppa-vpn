use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use floppa_core::config::{AwgObfuscation, TunnelInterfaceConfig};
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

/// One peer's line from `wg show <iface> dump`. Byte directions are the
/// server's: `rx` is what the peer sent us, `tx` what we sent the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStat {
    pub public_key: String,
    /// The peer's allowed-ips as printed (`10.100.0.2/32`), in interface order; empty for
    /// `(none)`.
    pub allowed_ips: Vec<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub last_handshake: Option<DateTime<Utc>>,
}

impl WgTool {
    /// `ip link add ... type <link_type>` for this tool's kernel module.
    fn link_type(self) -> &'static str {
        match self {
            WgTool::Wg => "wireguard",
            WgTool::Awg => "amneziawg",
        }
    }
}

/// Check if WireGuard interface exists
fn interface_exists(interface: &str) -> bool {
    Command::new("ip")
        .args(["link", "show", interface])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run an `ip` subcommand, failing on a non-zero exit.
fn ip(args: &[&str]) -> Result<()> {
    let output = Command::new("ip")
        .args(args)
        .output()
        .with_context(|| format!("Failed to run ip {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "ip {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Ensure the interface exists and matches `iface` (its `[wireguard]`/`[amneziawg]` section,
/// with `private_key` from the secrets), creating and/or reconfiguring it.
///
/// Idempotent and convergent: the live `[Interface]` (`wg/awg showconf`) is diffed
/// against the spec and only the differing params go into a single `set`, then the
/// address and link state are (re)applied unconditionally. That covers a config
/// change across a restart, an interface left half-configured by an earlier crash,
/// and — for AmneziaWG — a steady state where no `awg set` is issued at all.
///
/// Not touching a correctly configured AmneziaWG interface matters: every `awg set`
/// re-runs `jp_spec_setup` in the kernel module, and on builds before ac946a9
/// (2026-03-25) that races the handshake-send path whenever an I-packet is
/// configured — a use-after-free that has taken the whole VPS down.
pub fn ensure_interface(
    tool: WgTool,
    iface: &TunnelInterfaceConfig,
    private_key: &str,
) -> Result<()> {
    let interface = iface.interface.as_str();

    let created = if interface_exists(interface) {
        debug!(interface, %tool, "Interface already exists; reconciling");
        false
    } else {
        info!(interface, %tool, "Creating interface");
        ip(&["link", "add", "dev", interface, "type", tool.link_type()]).with_context(|| {
            format!(
                "Failed to create {} interface (is the {} kernel module loaded?)",
                tool,
                tool.link_type()
            )
        })?;
        true
    };

    let current = if created {
        HashMap::new()
    } else {
        read_interface_conf(tool, interface).unwrap_or_else(|e| {
            warn!(interface, error = %e, "Could not read interface config; applying spec blindly");
            HashMap::new()
        })
    };

    // Everything that differs goes into one `set`; the private key is fed over stdin.
    let params = params_to_set(&current, iface, private_key);
    if params.is_empty() {
        debug!(interface, %tool, "Interface config already matches spec");
    } else {
        info!(
            interface,
            %tool,
            params = ?params.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            "Applying interface config"
        );
        let needs_key = params.iter().any(|(_, v)| v == PRIVATE_KEY_STDIN);
        let mut args: Vec<String> = vec!["set".into(), interface.into()];
        args.extend(params.into_iter().flat_map(|(k, v)| [k, v]));
        run_set(tool, &args, needs_key.then(|| private_key.trim()))?;
    }

    // Only the client subnet's prefix length is used for the server address.
    let address = format!("{}/{}", iface.get_server_ip(), iface.client_subnet.prefix());
    ip(&["address", "replace", &address, "dev", interface])?;
    ip(&["link", "set", interface, "up"])?;

    info!(
        interface,
        %tool,
        address,
        listen_port = iface.get_listen_port(),
        created,
        "Interface ready"
    );
    Ok(())
}

/// Run `<tool> set ...`, feeding `private_key` over stdin when the args reference `/dev/stdin`.
fn run_set(tool: WgTool, args: &[String], private_key: Option<&str>) -> Result<()> {
    let mut child = Command::new(tool.binary())
        .args(args)
        .stdin(if private_key.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
        .with_context(|| format!("Failed to spawn {tool} set"))?;
    if let (Some(key), Some(mut stdin)) = (private_key, child.stdin.take()) {
        stdin.write_all(key.as_bytes())?;
    }
    if !child.wait()?.success() {
        return Err(anyhow!("{tool} set failed"));
    }
    Ok(())
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

/// Whether the interface already has `value` for lowercase showconf key `key`.
fn param_matches(current: &HashMap<String, String>, key: &str, value: &str) -> bool {
    current
        .get(key)
        .is_some_and(|cur| normalize_range(cur) == normalize_range(value))
}

/// Marker value for the private key in [`params_to_set`]; the key itself goes over stdin.
const PRIVATE_KEY_STDIN: &str = "/dev/stdin";

/// `<tool> set` key/value pairs needed to bring `current` (parsed showconf) in line with
/// `iface` + `private_key`. Empty when the interface already matches. The private key, if it
/// differs, is reported as `("private-key", "/dev/stdin")` and never as its value.
fn params_to_set(
    current: &HashMap<String, String>,
    iface: &TunnelInterfaceConfig,
    private_key: &str,
) -> Vec<(String, String)> {
    let mut params = Vec::new();
    if current.get("privatekey").map(String::as_str) != Some(private_key.trim()) {
        params.push(("private-key".to_string(), PRIVATE_KEY_STDIN.to_string()));
    }
    let port = iface.get_listen_port().to_string();
    if !param_matches(current, "listenport", &port) {
        params.push(("listen-port".to_string(), port));
    }
    if let Some(o) = &iface.obfuscation {
        for (k, v) in awg_obfuscation_params(o) {
            if !param_matches(current, k, &v) {
                params.push((k.to_string(), v));
            }
        }
    }
    params
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

        let allowed_ips = match parts[3] {
            "(none)" => Vec::new(),
            list => list.split(',').map(|ip| ip.trim().to_string()).collect(),
        };

        stats.push(PeerStat {
            public_key: parts[0].to_string(),
            allowed_ips,
            rx_bytes: number("transfer-rx", parts[5])?,
            tx_bytes: number("transfer-tx", parts[6])?,
            last_handshake,
        });
    }

    Ok(stats)
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// An `[amneziawg]` section as `Config::parse` settles it, with `o` as its obfuscation.
    fn awg_iface(o: &AwgObfuscation) -> TunnelInterfaceConfig {
        TunnelInterfaceConfig {
            interface: "awg-floppa".into(),
            endpoint: "vpn.example.com:51821".into(),
            listen_port: Some(51821),
            client_subnet: "10.101.0.0/24".parse().unwrap(),
            server_ip: None,
            dns: vec![],
            allowed_ips: "0.0.0.0/0".into(),
            rate_limit: None,
            mtu: Some(1280),
            obfuscation: Some(o.clone()),
        }
    }

    #[test]
    fn matching_interface_needs_no_set() {
        let conf = parse_showconf_interface(SHOWCONF);
        let mut o = AwgObfuscation::default();
        o.i1.clear();
        assert!(params_to_set(&conf, &awg_iface(&o), "cHJpdmF0ZQ==\n").is_empty());
    }

    #[test]
    fn only_differing_params_are_set() {
        let conf = parse_showconf_interface(SHOWCONF);
        let mut o = AwgObfuscation::default();
        o.i1.clear();

        // A configured I1 the interface lacks, and a changed scalar
        o.i1 = "<b 0x01>".to_string();
        o.jc = 7;
        let params = params_to_set(&conf, &awg_iface(&o), "cHJpdmF0ZQ==");
        assert_eq!(
            params,
            vec![
                ("jc".to_string(), "7".to_string()),
                ("i1".to_string(), "<b 0x01>".to_string()),
            ]
        );

        // Key and port differ → set over stdin, key never appears in the args
        o = AwgObfuscation::default();
        o.i1.clear();
        let mut iface = awg_iface(&o);
        iface.listen_port = Some(51899);
        let params = params_to_set(&conf, &iface, "b3RoZXI=");
        assert_eq!(
            params,
            vec![
                ("private-key".to_string(), PRIVATE_KEY_STDIN.to_string()),
                ("listen-port".to_string(), "51899".to_string()),
            ]
        );
        assert!(params.iter().all(|(_, v)| !v.contains("b3RoZXI=")));
    }

    #[test]
    fn fresh_interface_gets_everything() {
        let o = AwgObfuscation::default();
        let params = params_to_set(&HashMap::new(), &awg_iface(&o), "cHJpdmF0ZQ==");
        let keys: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "private-key",
                "listen-port",
                "jc",
                "jmin",
                "jmax",
                "s1",
                "s2",
                "s3",
                "s4",
                "h1",
                "h2",
                "h3",
                "h4",
                "i1"
            ]
        );
    }

    /// Captured from `wg show wg-floppa dump` (keys shortened).
    pub(crate) const DUMP: &str = "cHJpdmF0ZQ==\tc2VydmVy\t51820\toff\n\
        cGVlcjE=\t(none)\t203.0.113.7:41641\t10.100.0.2/32\t1700000000\t12345\t67890\toff\n\
        cGVlcjI=\t(none)\t(none)\t10.100.0.3/32\t0\t0\t0\toff\n\
        cGVlcjM=\t(none)\t(none)\t(none)\t0\t0\t0\toff\n";

    #[test]
    fn parses_dump_peer_lines() {
        let stats = parse_wg_dump(DUMP).unwrap();
        assert_eq!(stats.len(), 3);
        assert_eq!(
            stats[0],
            PeerStat {
                public_key: "cGVlcjE=".to_string(),
                allowed_ips: vec!["10.100.0.2/32".to_string()],
                rx_bytes: 12345,
                tx_bytes: 67890,
                last_handshake: DateTime::from_timestamp(1_700_000_000, 0),
            }
        );
        // No handshake yet → None, not the epoch
        assert_eq!(stats[1].public_key, "cGVlcjI=");
        assert_eq!(stats[1].last_handshake, None);
        assert_eq!((stats[1].rx_bytes, stats[1].tx_bytes), (0, 0));
        // A peer with no allowed-ips at all
        assert_eq!(stats[2].allowed_ips, Vec::<String>::new());
    }

    #[test]
    fn dump_allowed_ips_list_is_split() {
        let dump =
            "k\tk\t51820\toff\npk\t(none)\t(none)\t10.100.0.2/32,fd00::2/128\t0\t0\t0\toff\n";
        let stats = parse_wg_dump(dump).unwrap();
        assert_eq!(stats[0].allowed_ips, ["10.100.0.2/32", "fd00::2/128"]);
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
