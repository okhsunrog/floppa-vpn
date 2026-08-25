use serde::{Deserialize, Serialize};
use std::fmt;

use crate::auth::DeviceIdentity;

/// Tunnel protocol as the server names it. The clap names are the wire strings, so a typo is a
/// usage error instead of a request the server would coerce into some default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[value(name = "wireguard")]
    WireGuard,
    #[value(name = "amneziawg")]
    AmneziaWg,
    #[value(name = "vless")]
    Vless,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::WireGuard => "wireguard",
            Protocol::AmneziaWg => "amneziawg",
            Protocol::Vless => "vless",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Peer lifecycle as the daemon drives it; `pending_add` is a peer that exists and is about
/// to work, not a reason to create another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerSyncStatus {
    PendingAdd,
    Active,
    PendingRemove,
    Removed,
}

impl PeerSyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerSyncStatus::PendingAdd => "pending_add",
            PeerSyncStatus::Active => "active",
            PeerSyncStatus::PendingRemove => "pending_remove",
            PeerSyncStatus::Removed => "removed",
        }
    }

    /// The peer is (or is about to be) usable.
    pub fn is_live(self) -> bool {
        matches!(self, PeerSyncStatus::PendingAdd | PeerSyncStatus::Active)
    }
}

impl fmt::Display for PeerSyncStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MeResponse {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub is_admin: bool,
    pub subscription: Option<SubscriptionInfo>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SubscriptionInfo {
    pub plan_name: String,
    pub plan_display_name: String,
    pub speed_limit_mbps: Option<i32>,
    pub max_peers: i32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MyPeer {
    pub id: i64,
    pub assigned_ip: String,
    pub sync_status: PeerSyncStatus,
    #[serde(default = "default_protocol")]
    pub protocol: Protocol,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
}

fn default_protocol() -> Protocol {
    Protocol::WireGuard
}

/// `GET /me/peers` returns an object wrapping the peer list (not a bare array).
#[derive(Debug, Deserialize)]
struct MyPeersResponse {
    peers: Vec<MyPeer>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePeerResponse {
    pub id: i64,
    pub assigned_ip: String,
    pub config: String,
}

#[derive(Debug, Serialize)]
struct CreatePeerRequest {
    device_name: Option<String>,
    device_id: Option<String>,
    protocol: Protocol,
}

#[derive(Debug, Deserialize)]
pub struct VlessConfigResponse {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: AuthUserInfo,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AuthUserInfo {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub is_admin: bool,
}

#[derive(Debug, Serialize)]
struct ExchangeCodeRequest {
    code: String,
}

/// Error body the server sends with every non-2xx status.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: String,
}

/// Failures talking to the server, by what the caller can do about them. The message is the
/// server's own where it sent one.
#[derive(Debug, thiserror::Error)]
pub enum ApiClientError {
    #[error("Authentication failed. Token may be expired. Run `floppa-cli login` again.")]
    Unauthorized,
    /// 402: no active subscription.
    #[error("{0}")]
    PaymentRequired(String),
    /// 403: plan limit reached.
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    /// 409: e.g. a peer already exists for this device and protocol.
    #[error("{0}")]
    Conflict(String),
    #[error("{what} failed: {status} ({message})")]
    Status {
        what: &'static str,
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("Failed to reach API: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Failed to parse {what} response: {source}")]
    Decode {
        what: &'static str,
        source: reqwest::Error,
    },
}

/// Map a non-success status to the typed error, with the server's message when there is one.
async fn check(
    resp: reqwest::Response,
    what: &'static str,
) -> Result<reqwest::Response, ApiClientError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let message = match serde_json::from_str::<ErrorBody>(&body) {
        Ok(body) => body.message,
        Err(_) if body.trim().is_empty() => status.canonical_reason().unwrap_or("").to_string(),
        Err(_) => body.trim().to_string(),
    };
    Err(match status.as_u16() {
        401 => ApiClientError::Unauthorized,
        402 => ApiClientError::PaymentRequired(message),
        403 => ApiClientError::Forbidden(message),
        404 => ApiClientError::NotFound(message),
        409 => ApiClientError::Conflict(message),
        _ => ApiClientError::Status {
            what,
            status,
            message,
        },
    })
}

async fn json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    what: &'static str,
) -> Result<T, ApiClientError> {
    resp.json()
        .await
        .map_err(|source| ApiClientError::Decode { what, source })
}

impl ApiClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn get(
        &self,
        path: &str,
        what: &'static str,
    ) -> Result<reqwest::Response, ApiClientError> {
        let resp = self
            .client
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        check(resp, what).await
    }

    pub async fn get_me(&self) -> Result<MeResponse, ApiClientError> {
        const WHAT: &str = "GET /me";
        json(self.get("/me", WHAT).await?, WHAT).await
    }

    pub async fn list_peers(&self) -> Result<Vec<MyPeer>, ApiClientError> {
        const WHAT: &str = "GET /me/peers";
        let body: MyPeersResponse = json(self.get("/me/peers", WHAT).await?, WHAT).await?;
        Ok(body.peers)
    }

    pub async fn create_peer(
        &self,
        device: &DeviceIdentity,
        protocol: Protocol,
    ) -> Result<CreatePeerResponse, ApiClientError> {
        const WHAT: &str = "POST /me/peers";
        let resp = self
            .client
            .post(self.url("/me/peers"))
            .bearer_auth(&self.token)
            .json(&CreatePeerRequest {
                device_name: Some(device.name.clone()),
                device_id: Some(device.id.clone()),
                protocol,
            })
            .send()
            .await?;
        json(check(resp, WHAT).await?, WHAT).await
    }

    pub async fn get_peer_config(&self, peer_id: i64) -> Result<String, ApiClientError> {
        const WHAT: &str = "GET /me/peers/{id}/config";
        self.get(&format!("/me/peers/{peer_id}/config"), WHAT)
            .await?
            .text()
            .await
            .map_err(|source| ApiClientError::Decode { what: WHAT, source })
    }

    /// The current user's peer for this device and `protocol`, if any (`None` on 404).
    pub async fn get_peer_by_device(
        &self,
        device_id: &str,
        protocol: Protocol,
    ) -> Result<Option<MyPeer>, ApiClientError> {
        const WHAT: &str = "GET /me/peers/by-device/{id}";
        let path = format!("/me/peers/by-device/{device_id}?protocol={protocol}");
        match self.get(&path, WHAT).await {
            Ok(resp) => json(resp, WHAT).await.map(Some),
            Err(ApiClientError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Config for `protocol`: the VLESS URI, or this device's peer config (created on first use).
    pub async fn config_for(
        &self,
        protocol: Protocol,
        device: &DeviceIdentity,
    ) -> Result<String, ApiClientError> {
        match protocol {
            Protocol::Vless => self.get_vless_config().await,
            Protocol::WireGuard | Protocol::AmneziaWg => {
                self.find_or_create_peer(protocol, device).await
            }
        }
    }

    /// This device's peer for `protocol` (WireGuard or AmneziaWG), created if it has none.
    /// Only a peer registered under this device id is reused; a `pending_add` one counts as
    /// existing, since the daemon is about to activate it.
    async fn find_or_create_peer(
        &self,
        protocol: Protocol,
        device: &DeviceIdentity,
    ) -> Result<String, ApiClientError> {
        if let Some(peer) = self.get_peer_by_device(&device.id, protocol).await?
            && peer.sync_status.is_live()
        {
            eprintln!(
                "Using existing {protocol} peer: {} ({}, {})",
                peer.assigned_ip, peer.id, peer.sync_status
            );
            return self.get_peer_config(peer.id).await;
        }

        eprintln!("Creating new {protocol} peer (device: {})...", device.name);
        let created = self.create_peer(device, protocol).await?;
        eprintln!("Peer created: {} ({})", created.assigned_ip, created.id);
        Ok(created.config)
    }

    /// Fetch VLESS config for the current user.
    pub async fn get_vless_config(&self) -> Result<String, ApiClientError> {
        const WHAT: &str = "GET /me/vless-config";
        let vless: VlessConfigResponse =
            json(self.get("/me/vless-config", WHAT).await?, WHAT).await?;
        Ok(vless.uri)
    }

    /// Exchange a one-time login code for a JWT token (no auth required).
    pub async fn exchange_code(base_url: &str, code: &str) -> Result<AuthResponse, ApiClientError> {
        const WHAT: &str = "POST /auth/telegram/exchange-code";
        let url = format!(
            "{}/auth/telegram/exchange-code",
            base_url.trim_end_matches('/')
        );
        let resp = reqwest::Client::new()
            .post(&url)
            .json(&ExchangeCodeRequest {
                code: code.to_string(),
            })
            .send()
            .await?;
        json(check(resp, WHAT).await?, WHAT).await
    }
}
