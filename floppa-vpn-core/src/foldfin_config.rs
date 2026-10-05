//! Versioned Foldfin provisioning document. Validated at import and persistence boundaries.
use foldfin::{
    client::{ClientConfig, TlsProfile},
    credential::Credential,
};
use ipnet::IpNet;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "Document", into = "Document")]
pub struct FoldfinVpnConfig(Document);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    endpoint: String,
    server_name: String,
    token: String,
    address: IpNetwork,
    peer: IpAddr,
    #[serde(default)]
    dns: Vec<IpAddr>,
    routes: Vec<IpNet>,
    #[serde(default = "default_mtu")]
    mtu: u16,
    #[serde(default)]
    chrome_tls: bool,
    #[serde(default)]
    additional_ca_pem: Option<String>,
}
fn default_mtu() -> u16 {
    1280
}

impl std::fmt::Debug for FoldfinVpnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoldfinVpnConfig")
            .field("endpoint", &self.0.endpoint)
            .field("address", &self.0.address)
            .field("token", &"[REDACTED]")
            .finish()
    }
}
impl TryFrom<Document> for FoldfinVpnConfig {
    type Error = String;
    fn try_from(document: Document) -> Result<Self, String> {
        if document.version != 1 {
            return Err("unsupported Foldfin config version".into());
        }
        document
            .endpoint
            .parse::<floppa_tunnel_config::Endpoint>()
            .map_err(|e| e.to_string())?;
        if document.server_name.is_empty()
            || document.server_name.len() > 253
            || document.server_name.contains(['/', ' '])
            || (document.server_name.contains(':')
                && document.server_name.parse::<IpAddr>().is_err())
        {
            return Err("invalid Foldfin TLS verification name".into());
        }
        Credential::bearer(&document.token).map_err(|e| e.to_string())?;
        if !(1280..=1500).contains(&document.mtu) {
            return Err("Foldfin MTU must be 1280..=1500".into());
        }
        if document.routes.is_empty()
            || document
                .routes
                .iter()
                .any(|net| net.addr().is_ipv4() != document.address.ip().is_ipv4())
        {
            return Err("Foldfin routes must match the configured address family".into());
        }
        let config = Self(document);
        config.policy().validate().map_err(|e| e.to_string())?;
        Ok(config)
    }
}
impl From<FoldfinVpnConfig> for Document {
    fn from(config: FoldfinVpnConfig) -> Self {
        config.0
    }
}
impl FoldfinVpnConfig {
    pub fn endpoint(&self) -> &str {
        &self.0.endpoint
    }
    pub fn address(&self) -> IpNetwork {
        self.0.address
    }
    pub fn dns(&self) -> &[IpAddr] {
        &self.0.dns
    }
    pub fn routes(&self) -> Vec<IpNetwork> {
        self.0
            .routes
            .iter()
            .map(|n| n.to_string().parse().expect("validated IP prefix"))
            .collect()
    }
    pub fn mtu(&self) -> u16 {
        self.0.mtu
    }
    pub fn policy(&self) -> foldfin::vpn::Config {
        foldfin::vpn::Config {
            local: vec![self.0.address.ip()],
            peer: vec![self.0.peer],
            local_routes: vec![],
            peer_routes: self.0.routes.clone(),
        }
    }
    pub fn client_config(&self) -> Result<ClientConfig, foldfin::error::Error> {
        Ok(ClientConfig {
            authority: self.0.endpoint.clone(),
            server_name: self.0.server_name.clone(),
            credential: Credential::bearer(&self.0.token)?,
            additional_ca_pem: self
                .0
                .additional_ca_pem
                .as_ref()
                .map(|s| s.as_bytes().to_vec()),
            tls_profile: if self.0.chrome_tls {
                TlsProfile::Chrome154Linux
            } else {
                TlsProfile::Rustls
            },
        })
    }
}
