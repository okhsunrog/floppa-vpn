//! Local destination policy. Rules are data, never privileged shell snippets.
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RouteTarget {
    Direct,
    Tunnel,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct RouteRule {
    #[serde(default)]
    pub name: String,
    pub destination: String,
    pub target: RouteTarget,
}

/// Canonical CIDRs, longest prefix first. Ambiguous duplicate destinations are errors.
pub fn validate(rules: &[RouteRule]) -> Result<Vec<RouteRule>, String> {
    if rules.len() > 64 {
        return Err("at most 64 destination rules are supported".into());
    }
    let mut parsed = Vec::new();
    for rule in rules {
        let raw = rule.destination.trim();
        let network: IpNetwork = raw
            .parse()
            .or_else(|_| raw.parse::<std::net::IpAddr>().map(IpNetwork::from))
            .map_err(|_| format!("invalid IP or CIDR: {raw}"))?;
        if !network.is_ipv4() {
            return Err("destination rules currently require IPv4".into());
        }
        let network =
            IpNetwork::new(network.network(), network.prefix()).map_err(|e| e.to_string())?;
        let mut rule = rule.clone();
        rule.destination = network.to_string();
        if parsed
            .iter()
            .any(|(_, r): &(u8, RouteRule)| r.destination == rule.destination)
        {
            return Err(format!("duplicate destination: {}", rule.destination));
        }
        parsed.push((network.prefix(), rule));
    }
    parsed.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.destination.cmp(&b.1.destination))
    });
    Ok(parsed.into_iter().map(|(_, r)| r).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rule(destination: &str) -> RouteRule {
        RouteRule {
            name: String::new(),
            destination: destination.into(),
            target: RouteTarget::Direct,
        }
    }
    #[test]
    fn canonical_and_specific_first() {
        let rules = validate(&[
            rule("0.0.0.0/0"),
            rule("194.186.63.4/24"),
            rule("160.79.104.1"),
        ])
        .unwrap();
        assert_eq!(
            rules
                .iter()
                .map(|r| r.destination.as_str())
                .collect::<Vec<_>>(),
            ["160.79.104.1/32", "194.186.63.0/24", "0.0.0.0/0"]
        );
    }
    #[test]
    fn rejects_ambiguous_and_unsafe_input() {
        assert!(validate(&[rule("10.1.0.1/24"), rule("10.1.0.2/24")]).is_err());
        assert!(validate(&[rule("1.1.1.1; reboot")]).is_err());
        assert!(validate(&[rule("::/0")]).is_err());
    }
}

/// Exact kernel objects retained in the rollback journal; no gateway lookup on undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedRule {
    pub destination: String,
    pub table: u32,
    pub device: Option<String>,
    pub gateway: Option<String>,
    pub priority: u32,
}
