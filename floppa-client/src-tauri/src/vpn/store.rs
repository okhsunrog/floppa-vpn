//! The config store, owned outright by the actor task.
//!
//! No lock: the actor is the only thing that touches it. That is not just tidiness — the previous
//! `RwLock<SavedVpnConfigs>` was written from Tauri commands *and* read mid-connect, so importing a
//! config could change which protocol an in-flight attempt was about to use.
//!
//! One deliberate split runs through this module: **storing a config and choosing a protocol are
//! different operations.** Importing writes a config and nothing else; `preferred` is written only
//! when a protocol has actually connected. Previously a single `active_protocol` string meant both
//! "which config the next connect picks" and "which protocol worked last", which forced the probe
//! loop to overwrite it before every attempt — and so a failed cycle left the *last failed*
//! protocol recorded as the preferred one.

use super::protocol::{Preference, Protocol};
use super::state::{
    AwgConfig, ProtocolConfig, SavedVpnConfigs, VlessVpnConfig, WgConfig, config_str_is_amneziawg,
};
use crate::vpn::actor::types::{ConfigSummary, ConfigsView};
use crate::vpn::config as vpn_config;
use serde::{Deserialize, Serialize};
use specta::Type;
use tracing::info;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigError {
    #[error("the config is empty")]
    Empty,
    #[error("could not parse the config: {detail}")]
    Unparseable { detail: String },
}

#[derive(Debug, Default)]
pub struct ConfigStore {
    configs: SavedVpnConfigs,
}

impl ConfigStore {
    /// Load whatever is persisted. A missing or unreadable store is an empty one, never an error:
    /// the app must still start so the user can import a config.
    pub fn load() -> Self {
        let configs = vpn_config::load_configs().unwrap_or_default();
        Self { configs }
    }

    /// Parse a config string and store it under its own protocol key.
    ///
    /// Deliberately does **not** change `preferred`: importing a config is not a statement that it
    /// works, and the previous behaviour of switching to whatever was imported last is what made
    /// server sync silently reorder the user's protocol.
    pub fn import(&mut self, raw: &str) -> Result<Protocol, ConfigError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::Empty);
        }

        let protocol = if trimmed.starts_with("vless://") {
            let vless =
                VlessVpnConfig::from_uri(trimmed).map_err(|e| ConfigError::Unparseable {
                    detail: e.to_string(),
                })?;
            self.configs.vless = Some(vless);
            Protocol::Vless
        } else if config_str_is_amneziawg(raw) {
            let awg = AwgConfig::from_config_str(raw).map_err(|e| ConfigError::Unparseable {
                detail: e.to_string(),
            })?;
            self.configs.amneziawg = Some(awg);
            Protocol::AmneziaWg
        } else {
            let wg = WgConfig::from_config_str(raw).map_err(|e| ConfigError::Unparseable {
                detail: e.to_string(),
            })?;
            self.configs.wireguard = Some(wg);
            Protocol::WireGuard
        };

        self.save();
        info!(%protocol, "stored config");
        Ok(protocol)
    }

    pub fn get(&self, protocol: Protocol) -> Option<ProtocolConfig> {
        self.configs.get(protocol)
    }

    /// The set of protocols with a stored config. Order is deterministic but carries no preference.
    pub fn available(&self) -> Vec<Protocol> {
        self.configs.available_protocols()
    }

    pub fn preferred(&self) -> Option<Protocol> {
        self.configs.preferred_protocol.0
    }

    /// Record that a protocol actually worked. The only caller is the success path.
    pub fn set_preferred(&mut self, protocol: Option<Protocol>) {
        if self.configs.preferred_protocol.0 == protocol {
            return;
        }
        self.configs.preferred_protocol = Preference(protocol);
        self.save();
    }

    pub fn has_any(&self) -> bool {
        self.configs.has_any()
    }

    /// The probe order actually usable right now: the caller's order narrowed to protocols we hold
    /// a config for, with the last known-good one moved to the front so a reconnect goes straight
    /// to what worked.
    pub fn resolve_order(&self, requested: &[Protocol]) -> Vec<Protocol> {
        let mut order: Vec<Protocol> = requested
            .iter()
            .copied()
            .filter(|p| self.get(*p).is_some())
            .collect();
        order.dedup();

        if let Some(preferred) = self.preferred()
            && let Some(pos) = order.iter().position(|p| *p == preferred)
        {
            order.swap(0, pos);
        }
        order
    }

    pub fn view(&self) -> ConfigsView {
        ConfigsView {
            available: self.available(),
            preferred: self.preferred(),
            summaries: self
                .available()
                .into_iter()
                .filter_map(|p| self.get(p).map(|c| summarize(p, &c)))
                .collect(),
        }
    }

    pub fn clear(&mut self) {
        self.configs = SavedVpnConfigs::default();
        vpn_config::delete_configs();
    }

    fn save(&self) {
        vpn_config::save_configs(&self.configs);
    }
}

fn summarize(protocol: Protocol, config: &ProtocolConfig) -> ConfigSummary {
    ConfigSummary {
        protocol,
        address: config.address().to_string(),
        server_endpoint: config.endpoint_str().to_string(),
        dns: match config {
            ProtocolConfig::WireGuard(wg) => wg.dns.clone(),
            ProtocolConfig::AmneziaWg(awg) => awg.wg.dns.clone(),
            ProtocolConfig::Vless(vless) => vless.dns.clone(),
        },
        allowed_ips: match config {
            ProtocolConfig::WireGuard(wg) => wg.allowed_ips.clone(),
            ProtocolConfig::AmneziaWg(awg) => awg.wg.allowed_ips.clone(),
            ProtocolConfig::Vless(vless) => vless.allowed_ips.clone(),
        },
        mtu: config.get_mtu(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WG_CONFIG: &str = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = vpn.example.com:51820
AllowedIPs = 0.0.0.0/0
";

    /// A store that never touches the keyring or the filesystem.
    fn store_with(configs: SavedVpnConfigs) -> ConfigStore {
        ConfigStore { configs }
    }

    fn wg() -> WgConfig {
        WgConfig::from_config_str(WG_CONFIG).expect("fixture must parse")
    }

    #[test]
    fn an_empty_config_is_rejected_rather_than_stored() {
        let mut store = store_with(SavedVpnConfigs::default());
        assert_eq!(store.import("   "), Err(ConfigError::Empty));
        assert!(!store.has_any());
    }

    #[test]
    fn resolve_order_drops_protocols_we_have_no_config_for() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard, Protocol::Vless]),
            vec![Protocol::WireGuard]
        );
    }

    #[test]
    fn resolve_order_puts_the_last_working_protocol_first() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            preferred_protocol: Preference(Some(Protocol::WireGuard)),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard]),
            vec![Protocol::WireGuard, Protocol::AmneziaWg],
            "a reconnect should go straight to what worked"
        );
    }

    #[test]
    fn resolve_order_keeps_the_requested_order_when_nothing_is_preferred() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard]),
            vec![Protocol::AmneziaWg, Protocol::WireGuard]
        );
    }

    #[test]
    fn a_preferred_protocol_we_no_longer_hold_does_not_resurface() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            preferred_protocol: Preference(Some(Protocol::Vless)),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::WireGuard]),
            vec![Protocol::WireGuard]
        );
    }

    #[test]
    fn the_view_reports_available_as_a_set_and_preferred_separately() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            preferred_protocol: Preference(Some(Protocol::WireGuard)),
            ..Default::default()
        });
        let view = store.view();
        assert_eq!(view.available, vec![Protocol::WireGuard]);
        assert_eq!(view.preferred, Some(Protocol::WireGuard));
        assert_eq!(view.summaries.len(), 1);
        assert_eq!(view.summaries[0].server_endpoint, "vpn.example.com:51820");
    }
}
