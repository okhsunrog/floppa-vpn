use anyhow::{Context, Result, bail};
use floppa_core::config::RegionRoutingConfig;
use std::collections::HashMap;
use std::process::Command;

fn rule_exists(current: &str, assigned_ip: &str, config: &RegionRoutingConfig) -> bool {
    current.lines().any(|line| {
        line.starts_with(&format!("{}:", config.rule_priority))
            && line.contains(&format!("from {assigned_ip} "))
            && (line.contains(&format!("lookup {}", config.routing_table))
                || line.contains(&format!("table {}", config.routing_table)))
    })
}

/// Make the per-peer source rule match its effective region. Europe has no
/// dedicated rule and falls through to the deployment's existing VPN-subnet rule.
pub fn reconcile_peer(
    assigned_ip: &str,
    region_id: &str,
    regions: &HashMap<String, RegionRoutingConfig>,
) -> Result<()> {
    if region_id != "europe" && !regions.contains_key(region_id) {
        bail!("region {region_id:?} has no routing configuration");
    }

    let output = Command::new("ip")
        .args(["-4", "rule", "show"])
        .output()
        .context("failed to execute ip rule show")?;
    if !output.status.success() {
        bail!(
            "ip rule show failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let current = String::from_utf8_lossy(&output.stdout);
    let source = format!("{assigned_ip}/32");

    for (configured_region, config) in regions {
        // iproute2 renders a host prefix without `/32`, even when it was
        // supplied on the command line.
        let exists = rule_exists(&current, assigned_ip, config);
        let wanted = configured_region == region_id;
        if exists == wanted {
            continue;
        }

        let action = if wanted { "add" } else { "del" };
        let status = Command::new("ip")
            .args([
                "-4",
                "rule",
                action,
                "priority",
                &config.rule_priority.to_string(),
                "from",
                &source,
                "table",
                &config.routing_table,
            ])
            .status()
            .with_context(|| format!("failed to execute ip rule {action}"))?;
        if !status.success() {
            bail!(
                "ip rule {action} failed for {source} via table {}",
                config.routing_table
            );
        }
    }

    Ok(())
}

pub fn remove_peer(
    assigned_ip: &str,
    regions: &HashMap<String, RegionRoutingConfig>,
) -> Result<()> {
    reconcile_peer(assigned_ip, "europe", regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn singapore() -> RegionRoutingConfig {
        RegionRoutingConfig {
            routing_table: "singapore".into(),
            rule_priority: 90,
        }
    }

    #[test]
    fn finds_the_host_rule_as_iproute2_renders_it() {
        let rules = "0: from all lookup local\n90: from 10.100.0.2 lookup singapore\n";
        assert!(rule_exists(rules, "10.100.0.2", &singapore()));
    }

    #[test]
    fn does_not_confuse_one_host_with_a_longer_address() {
        let rules = "90: from 10.100.0.20 lookup singapore\n";
        assert!(!rule_exists(rules, "10.100.0.2", &singapore()));
    }
}
