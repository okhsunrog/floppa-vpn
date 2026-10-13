//! Local route exclusions. No interfaces, gateways or policy-table allocation.
use ipnetwork::Ipv4Network;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    pub excluded_routes: Vec<String>,
}

pub fn validate_exclusions(input: &[String]) -> Result<Vec<String>, String> {
    if input.len() > 64 {
        return Err("at most 64 exclusions are supported".into());
    }
    let mut result = Vec::new();
    for raw in input {
        let raw = raw.trim();
        let net: Ipv4Network = raw
            .parse()
            .or_else(|_| raw.parse::<std::net::Ipv4Addr>().map(Ipv4Network::from))
            .map_err(|_| format!("invalid IPv4 address or network: {raw}"))?;
        result.push(
            Ipv4Network::new(net.network(), net.prefix())
                .unwrap()
                .to_string(),
        );
    }
    result.sort();
    result.dedup();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonicalizes_and_deduplicates() {
        assert_eq!(
            validate_exclusions(&[
                "194.186.63.1/24".into(),
                "194.186.63.0/24".into(),
                "1.1.1.1".into()
            ])
            .unwrap(),
            ["1.1.1.1/32", "194.186.63.0/24"]
        );
    }
    #[test]
    fn exclusions_survive_saved_connection_reload() {
        use crate::actor::types::TunnelParams;
        use crate::autostart::{LastIntent, load, save};
        let dir = tempfile::tempdir().unwrap();
        let params = TunnelParams {
            excluded_routes: vec!["194.186.63.0/24".into()],
            ..TunnelParams::default()
        };
        let intent = LastIntent::new(
            vec![crate::protocol::Protocol::WireGuard],
            params.clone(),
            0,
        );
        save(dir.path(), &intent).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.params, params);
        assert!(
            matches!(loaded.request(), Some(crate::actor::handle::IntentRequest::Up { params: restored, .. }) if restored == params)
        );
        let old: TunnelParams = serde_json::from_str(r#"{"split_mode":"all","apps":[]}"#).unwrap();
        assert!(old.excluded_routes.is_empty());
    }

    #[test]
    fn rejects_invalid_and_legacy_configs() {
        for value in ["::/0", "a.example", "1.2.3.4; reboot", "1.2.3.4/33"] {
            assert!(validate_exclusions(&[value.into()]).is_err());
        }
        assert!(
            serde_json::from_str::<RoutingConfig>(
                r#"[{"destination":"1.1.1.1","target":"direct"}]"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<RoutingConfig>(r#"{"excluded_routes":[],"target":"direct"}"#)
                .is_err()
        );
    }
}
