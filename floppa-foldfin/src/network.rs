//! Render deployment-owned network files from the same public address plan.
//! No commands are run here; applying host firewall/routing remains explicit.
use crate::node::ProcessConfig;
use anyhow::{Result, ensure};
use std::fmt::Write;

pub fn render(config: &ProcessConfig, user: &str, uplink: &str) -> Result<String> {
    config.validate()?;
    ensure!(
        !user.is_empty()
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid service user"
    );
    ensure!(
        !uplink.is_empty()
            && uplink.len() < 16
            && uplink
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
        "invalid uplink interface"
    );
    let name = &config.interface;
    let v4 = config.public.ipv4_pool;
    let mut text = format!(
        "# /etc/systemd/network/80-{name}.netdev\n[NetDev]\nName={name}\nKind=tun\n\n[Tun]\nUser={user}\nPacketInfo=no\nMultiQueue=no\n\n# /etc/systemd/network/80-{name}.network\n[Match]\nName={name}\n\n[Link]\nRequiredForOnline=no\nMTUBytes=1280\n\n[Network]\nConfigureWithoutCarrier=yes\nLinkLocalAddressing=no\nIPv6AcceptRA=no\nAddress={}/{}\n",
        config.public.addresses(1)[0],
        v4.prefix()
    );
    if let Some(v6) = config.public.ipv6_pool {
        writeln!(
            text,
            "Address={}/{}",
            config.public.addresses(1)[1],
            v6.prefix()
        )?;
    }
    writeln!(
        text,
        "\n# Dedicated nftables table. Load with nft -f after reviewing existing firewall.\n# Enable IPv4/IPv6 forwarding explicitly in deployment, never from the node.\n# NAT below is for internet exits; replace with upstream routes for routed sites.\ntable inet floppa_foldfin {{\n chain input {{ type filter hook input priority -10; policy accept; iifname \"{name}\" drop; }}\n chain forward {{ type filter hook forward priority -10; policy accept;\n  iifname \"{name}\" oifname \"{name}\" drop\n  iifname \"{name}\" oifname != \"{uplink}\" drop\n  oifname \"{name}\" iifname != \"{uplink}\" drop\n }}\n chain postrouting {{ type nat hook postrouting priority srcnat; policy accept;\n  ip saddr {v4} oifname \"{uplink}\" masquerade"
    )?;
    if let Some(v6) = config.public.ipv6_pool {
        writeln!(text, "  ip6 saddr {v6} oifname \"{uplink}\" masquerade")?;
    }
    text.push_str(" }\n}\n# Existing firewall must allow TCP+UDP listen port, and forwarding on this TUN.\n# Private destinations on another interface need an explicit deployment firewall rule.\n");
    Ok(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_shell_and_nft_injection() {
        let config: ProcessConfig =
            toml::from_str(include_str!("../deploy/config.example.toml")).unwrap();
        assert!(render(&config, "floppa-foldfin", "eth0; flush ruleset").is_err());
        let files = render(&config, "floppa-foldfin", "eth0").unwrap();
        assert!(files.contains("Address=10.233.0.1/24"));
        assert!(files.contains("Address=fd33::1/120"));
        assert!(files.contains("oifname != \"eth0\" drop"));
    }
}
