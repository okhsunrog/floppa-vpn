//! Versioned provisioning with a cached, dual-stack platform network plan.
use foldfin::{client::ClientConfig, credential::Credential};
use ipnet::IpNet;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Clone, Serialize)]
#[serde(into = "Document")]
pub struct FoldfinVpnConfig(Document);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPolicy {
    #[default]
    Auto,
    H2Only,
    H3Only,
}

/// Cached before dialing, so Android can establish the intended TUN under lockdown.
/// Android's own lockdown setting determines whether untunneled traffic is blocked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPlan {
    pub revision: u64,
    pub addresses: Vec<IpNetwork>,
    pub peers: Vec<IpAddr>,
    pub routes: Vec<IpNet>,
    #[serde(default)]
    pub excluded_routes: Vec<IpNet>,
    #[serde(default)]
    pub dns: Vec<IpAddr>,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    endpoints: Vec<String>,
    server_name: String,
    token: String,
    #[serde(default)]
    transport: TransportPolicy,
    network: NetworkPlan,
    #[serde(default)]
    additional_ca_pem: Option<String>,
}
fn default_mtu() -> u16 {
    1280
}
impl<'de> Deserialize<'de> for FoldfinVpnConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(2) {
            return Err(serde::de::Error::custom(
                "Foldfin config v2 required; reissue the configuration",
            ));
        }
        let document =
            serde_json::from_value::<Document>(value).map_err(serde::de::Error::custom)?;
        Self::try_from(document).map_err(serde::de::Error::custom)
    }
}
impl std::fmt::Debug for FoldfinVpnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoldfinVpnConfig")
            .field("endpoints", &self.0.endpoints)
            .field("network", &self.0.network)
            .field("token", &"[REDACTED]")
            .finish()
    }
}
impl TryFrom<Document> for FoldfinVpnConfig {
    type Error = String;
    fn try_from(document: Document) -> Result<Self, String> {
        if document.version != 2 {
            return Err("Foldfin config v2 required; reissue the configuration".into());
        }
        if document.endpoints.is_empty() || document.endpoints.len() > 8 {
            return Err("Foldfin needs 1..8 endpoints".into());
        }
        for endpoint in &document.endpoints {
            endpoint
                .parse::<floppa_tunnel_config::Endpoint>()
                .map_err(|e| e.to_string())?;
        }
        if document.server_name.is_empty()
            || document.server_name.len() > 253
            || document.server_name.contains(['/', ' '])
            || (document.server_name.contains(':')
                && document.server_name.parse::<IpAddr>().is_err())
        {
            return Err("invalid Foldfin TLS verification name".into());
        }
        Credential::bearer(&document.token).map_err(|e| e.to_string())?;
        let plan = &document.network;
        if plan.mtu != 1280 {
            return Err("Foldfin network MTU must be 1280".into());
        }
        if plan.revision == 0
            || plan.routes.is_empty()
            || plan.excluded_routes.len() > 16
            || plan.routes.iter().chain(&plan.excluded_routes).any(|net| {
                net.addr() != net.network()
                    || !plan
                        .addresses
                        .iter()
                        .any(|a| a.ip().is_ipv4() == net.addr().is_ipv4())
            })
        {
            return Err("invalid Foldfin network revision or routed prefixes".into());
        }
        let config = Self(document);
        config.policy().validate().map_err(|e| e.to_string())?;
        let routes = config.routes();
        if routes.is_empty() || routes.len() > 4096 {
            return Err("Foldfin exclusions produce an invalid route set".into());
        }
        if config.dns().iter().any(|ip| {
            ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_loopback()
                || !routes.iter().any(|route| route.contains(*ip))
        }) {
            return Err("Foldfin DNS servers must be reachable through configured routes".into());
        }
        Ok(config)
    }
}
impl From<FoldfinVpnConfig> for Document {
    fn from(config: FoldfinVpnConfig) -> Self {
        config.0
    }
}
impl FoldfinVpnConfig {
    /// Public trust identity for the shared endpoint cache; credentials never enter its key.
    pub(crate) fn server_identity(&self) -> String {
        serde_json::to_string(&(&self.0.server_name, &self.0.additional_ca_pem))
            .expect("strings serialize")
    }

    pub fn endpoint(&self) -> &str {
        &self.0.endpoints[0]
    }
    pub fn endpoints(&self) -> &[String] {
        &self.0.endpoints
    }
    pub fn transport(&self) -> TransportPolicy {
        self.0.transport
    }
    pub fn network_plan(&self) -> &NetworkPlan {
        &self.0.network
    }
    pub fn addresses(&self) -> &[IpNetwork] {
        &self.0.network.addresses
    }
    pub fn address(&self) -> IpNetwork {
        *self
            .addresses()
            .iter()
            .find(|a| a.is_ipv4())
            .unwrap_or(&self.addresses()[0])
    }
    pub fn dns(&self) -> &[IpAddr] {
        &self.0.network.dns
    }
    pub fn routes(&self) -> Vec<IpNetwork> {
        let convert = |n: &IpNet| n.to_string().parse().expect("validated IP prefix");
        floppa_tunnel_config::route::exclude_networks(
            self.0
                .network
                .routes
                .iter()
                .map(convert)
                .collect::<Vec<_>>(),
            &self
                .0
                .network
                .excluded_routes
                .iter()
                .map(convert)
                .collect::<Vec<_>>(),
        )
    }
    pub fn mtu(&self) -> u16 {
        self.0.network.mtu
    }
    pub fn policy(&self) -> foldfin::vpn::Config {
        foldfin::vpn::Config {
            local: self.addresses().iter().map(IpNetwork::ip).collect(),
            peer: self.0.network.peers.clone(),
            local_routes: vec![],
            peer_routes: self.0.network.routes.clone(),
        }
    }
    pub fn client_config(&self) -> Result<ClientConfig, foldfin::error::Error> {
        Ok(ClientConfig {
            authority: self.endpoint().into(),
            server_name: self.0.server_name.clone(),
            credential: Credential::bearer(&self.0.token)?,
            additional_ca_pem: self
                .0
                .additional_ca_pem
                .as_ref()
                .map(|s| s.as_bytes().to_vec()),
        })
    }
}
