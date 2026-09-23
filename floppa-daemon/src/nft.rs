//! Client isolation: the daemon's own nftables table.
//!
//! A client reaches the public internet and nothing else, unless its plan grants it a private
//! network (`private_networks` / `plan_private_networks`). The table is replaced whole in one
//! `nft -f` transaction, so there is never a moment with half a ruleset, and it is left in place
//! when the daemon stops: the peers stay configured on the interfaces after the daemon exits, and
//! so must the rules that confine them.
//!
//! The chains run at `filter - 10`, ahead of an iptables-nft `filter` table on the same host.
//! That order does not let this table widen anything, since a packet must be accepted by every
//! base chain on a hook, but it does mean a mark set here is visible to the host's own firewall.

use anyhow::{Context, Result, bail};
use floppa_core::Config;
use ipnetwork::Ipv4Network;
use std::io::Write;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};

/// The table's name, in the `inet` family.
pub const TABLE: &str = "floppa";

/// Destinations no client reaches unless the operator allows them: RFC 1918, shared address
/// space (CGNAT, RFC 6598), link-local, and loopback.
const PRIVATE_V4: [&str; 6] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "169.254.0.0/16",
    "127.0.0.0/8",
];
/// Unique-local and link-local.
const PRIVATE_V6: [&str; 2] = ["fc00::/7", "fe80::/10"];

/// One client address allowed into one private network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivateAccess {
    pub client: Ipv4Addr,
    pub network: Ipv4Network,
}

/// Everything the ruleset is rendered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isolation {
    /// The client-facing interfaces, one per protocol.
    pub interfaces: Vec<String>,
    /// Private destinations every client may reach anyway (`[isolation] allow_destinations`).
    pub allow_destinations: Vec<Ipv4Network>,
    /// Mark set on packets let through by `private_access`.
    pub private_access_mark: Option<u32>,
    /// Which client may reach which private network, from the database. Sorted and deduplicated
    /// by [`Self::with_private_access`], so equal grants render equal tables.
    pub private_access: Vec<PrivateAccess>,
}

fn set_literal<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let items: Vec<String> = items.into_iter().map(|s| s.as_ref().to_owned()).collect();
    format!("{{ {} }}", items.join(", "))
}

impl Isolation {
    /// The isolation `config` asks for, with no private access granted yet; `None` when it is
    /// turned off.
    pub fn from_config(config: &Config) -> Option<Self> {
        config.isolation.enabled.then(|| Self {
            interfaces: std::iter::once(config.wireguard.interface.clone())
                .chain(config.amneziawg.as_ref().map(|awg| awg.interface.clone()))
                .collect(),
            allow_destinations: config.isolation.allow_destinations.clone(),
            private_access_mark: config.isolation.private_access_mark,
            private_access: Vec::new(),
        })
    }

    /// The same isolation with `grants` as its private access.
    pub fn with_private_access(&self, mut grants: Vec<PrivateAccess>) -> Self {
        grants.sort_unstable();
        grants.dedup();
        Self {
            private_access: grants,
            ..self.clone()
        }
    }

    /// The `nft -f` script that replaces the table. `table` followed by `delete table` makes the
    /// delete succeed whether or not the table existed, in the same transaction as the rebuild.
    pub fn render(&self) -> String {
        let ifaces = set_literal(self.interfaces.iter().map(|i| format!("\"{i}\"")));
        let private_v4 = set_literal(PRIVATE_V4);
        let private_v6 = set_literal(PRIVATE_V6);
        let allowed = (!self.allow_destinations.is_empty())
            .then(|| set_literal(self.allow_destinations.iter().map(ToString::to_string)));
        let mark = self
            .private_access_mark
            .map(|m| format!("meta mark set {m:#x} "))
            .unwrap_or_default();
        let grant_rule = format!("ip saddr . ip daddr @private_access {mark}accept");

        let mut out = String::new();
        out.push_str(&format!(
            "table inet {TABLE}\ndelete table inet {TABLE}\n\n"
        ));
        out.push_str(&format!("table inet {TABLE} {{\n"));

        out.push_str("    set private_access {\n");
        out.push_str("        type ipv4_addr . ipv4_addr\n");
        out.push_str("        flags interval\n");
        if !self.private_access.is_empty() {
            let elements = set_literal(
                self.private_access
                    .iter()
                    .map(|a| format!("{} . {}", a.client, a.network)),
            );
            out.push_str(&format!("        elements = {elements}\n"));
        }
        out.push_str("    }\n\n");

        // Traffic a client forwards through the host.
        out.push_str("    chain forward {\n");
        out.push_str("        type filter hook forward priority filter - 10; policy accept;\n");
        out.push_str(&format!("        iifname != {ifaces} accept\n"));
        out.push_str("        ct state established,related accept\n");
        out.push_str(&format!("        {grant_rule}\n"));
        if let Some(allowed) = &allowed {
            out.push_str(&format!("        ip daddr {allowed} accept\n"));
        }
        out.push_str(&format!("        oifname {ifaces} drop\n"));
        out.push_str(&format!("        ip daddr {private_v4} drop\n"));
        out.push_str(&format!("        ip6 daddr {private_v6} drop\n"));
        out.push_str("    }\n\n");

        // Traffic a client sends to the host itself. Its public addresses stay reachable, since
        // a connected client still talks to the service's API on them; its private ones do not,
        // save for ICMP so a client can still ping its gateway, and for a granted network that
        // happens to contain one of them.
        out.push_str("    chain input {\n");
        out.push_str("        type filter hook input priority filter - 10; policy accept;\n");
        out.push_str(&format!("        iifname != {ifaces} accept\n"));
        out.push_str("        ct state established,related accept\n");
        out.push_str(&format!("        {grant_rule}\n"));
        if let Some(allowed) = &allowed {
            out.push_str(&format!("        ip daddr {allowed} accept\n"));
        }
        out.push_str("        meta l4proto { icmp, ipv6-icmp } accept\n");
        out.push_str(&format!("        ip daddr {private_v4} drop\n"));
        out.push_str(&format!("        ip6 daddr {private_v6} drop\n"));
        out.push_str("    }\n");

        out.push_str("}\n");
        out
    }

    /// Replace the table on the host.
    pub fn apply(&self) -> Result<()> {
        run_nft(&self.render())
    }
}

/// Feed a script to `nft -f -`.
fn run_nft(script: &str) -> Result<()> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run nft (is the nftables package installed?)")?;
    child
        .stdin
        .take()
        .context("nft stdin unavailable")?
        .write_all(script.as_bytes())
        .context("failed to write the ruleset to nft")?;
    let output = child.wait_with_output().context("failed to wait for nft")?;
    if !output.status.success() {
        bail!(
            "nft rejected the ruleset: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Remove the table, for a host where isolation has been turned off.
pub fn remove() -> Result<()> {
    run_nft(&format!("table inet {TABLE}\ndelete table inet {TABLE}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolation(allow: &[&str]) -> Isolation {
        Isolation {
            interfaces: vec!["wg-floppa".into(), "awg-floppa".into()],
            allow_destinations: allow.iter().map(|a| a.parse().unwrap()).collect(),
            private_access_mark: None,
            private_access: Vec::new(),
        }
    }

    fn grant(client: &str, network: &str) -> PrivateAccess {
        PrivateAccess {
            client: client.parse().unwrap(),
            network: network.parse().unwrap(),
        }
    }

    /// The lines of one block (`chain forward`, `set private_access`), trimmed, in order.
    fn block<'a>(rendered: &'a str, header: &str) -> Vec<&'a str> {
        let header = format!("{header} {{");
        rendered
            .lines()
            .map(str::trim)
            .skip_while(|l| *l != header)
            .skip(1)
            .take_while(|l| *l != "}")
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn chain<'a>(rendered: &'a str, name: &str) -> Vec<&'a str> {
        block(rendered, &format!("chain {name}"))
    }

    #[test]
    fn replaces_the_table_in_one_transaction() {
        let rendered = isolation(&[]).render();
        assert!(rendered.starts_with("table inet floppa\ndelete table inet floppa\n"));
        assert_eq!(rendered.matches("table inet floppa {").count(), 1);
    }

    #[test]
    fn clients_reach_neither_each_other_nor_private_ranges() {
        let rendered = isolation(&[]).render();
        let forward = chain(&rendered, "forward");
        assert_eq!(
            forward[1], r#"iifname != { "wg-floppa", "awg-floppa" } accept"#,
            "only client traffic is judged here"
        );
        assert!(forward.contains(&r#"oifname { "wg-floppa", "awg-floppa" } drop"#));
        let private = forward
            .iter()
            .position(|l| l.starts_with("ip daddr {"))
            .unwrap();
        for range in PRIVATE_V4 {
            assert!(forward[private].contains(range), "{range} missing");
        }
        assert!(forward.iter().any(|l| l.starts_with("ip6 daddr {")));
        assert!(
            !rendered.contains("policy drop"),
            "the public internet stays open"
        );
    }

    #[test]
    fn exceptions_and_grants_come_before_every_drop() {
        let rendered = isolation(&["10.8.0.53/32"])
            .with_private_access(vec![grant("10.101.0.10", "10.66.66.0/24")])
            .render();
        for name in ["forward", "input"] {
            let lines = chain(&rendered, name);
            let first_drop = lines.iter().position(|l| l.ends_with(" drop")).unwrap();
            for accepted in [
                "ip daddr { 10.8.0.53/32 } accept",
                "ip saddr . ip daddr @private_access accept",
            ] {
                let at = lines
                    .iter()
                    .position(|l| *l == accepted)
                    .unwrap_or_else(|| panic!("{name}: no `{accepted}` in {lines:#?}"));
                assert!(at < first_drop, "{name}: {lines:#?}");
            }
        }
    }

    #[test]
    fn nothing_granted_renders_no_empty_literal() {
        // `elements = { }` and `ip daddr { }` are syntax errors in nft.
        let rendered = isolation(&[]).render();
        assert!(!rendered.contains("{  }"));
        assert!(!rendered.contains("elements"));
        // The set and the rule stay, so the table's shape does not depend on the data.
        assert!(rendered.contains("set private_access {"));
    }

    #[test]
    fn grants_are_client_and_network_pairs() {
        let rendered = isolation(&[])
            .with_private_access(vec![
                grant("10.101.0.10", "10.66.66.0/24"),
                grant("10.100.0.16", "10.65.65.0/24"),
                grant("10.101.0.10", "10.66.66.0/24"),
            ])
            .render();
        let set = block(&rendered, "set private_access");
        assert_eq!(
            set,
            [
                "type ipv4_addr . ipv4_addr",
                "flags interval",
                "elements = { 10.100.0.16 . 10.65.65.0/24, 10.101.0.10 . 10.66.66.0/24 }",
            ]
        );
    }

    #[test]
    fn equal_grants_in_any_order_render_the_same_table() {
        let base = isolation(&[]);
        let a = base.with_private_access(vec![
            grant("10.101.0.10", "10.66.66.0/24"),
            grant("10.100.0.16", "10.65.65.0/24"),
        ]);
        let b = base.with_private_access(vec![
            grant("10.100.0.16", "10.65.65.0/24"),
            grant("10.101.0.10", "10.66.66.0/24"),
        ]);
        assert_eq!(a, b);
        assert_eq!(a.render(), b.render());
    }

    #[test]
    fn a_configured_mark_is_set_on_granted_traffic() {
        let mut marked = isolation(&[]);
        marked.private_access_mark = Some(0x464c_0001);
        let rendered = marked.render();
        for name in ["forward", "input"] {
            assert!(
                chain(&rendered, name).contains(
                    &"ip saddr . ip daddr @private_access meta mark set 0x464c0001 accept"
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn the_host_answers_pings_but_keeps_its_private_addresses_closed() {
        let rendered = isolation(&[]).render();
        let input = chain(&rendered, "input");
        let icmp = input.iter().position(|l| l.contains("icmp")).unwrap();
        let private = input
            .iter()
            .position(|l| l.starts_with("ip daddr {"))
            .unwrap();
        assert!(icmp < private);
    }
}
