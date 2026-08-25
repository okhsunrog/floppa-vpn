use anyhow::{Context, Result, bail};
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

    pub async fn get_me(&self) -> Result<MeResponse> {
        let resp = self
            .client
            .get(self.url("/me"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach API")?;

        if resp.status() == 401 {
            bail!("Authentication failed. Token may be expired. Run `floppa-cli login` again.");
        }
        if !resp.status().is_success() {
            bail!("GET /me failed: {}", resp.status());
        }

        resp.json().await.context("Failed to parse /me response")
    }

    pub async fn list_peers(&self) -> Result<Vec<MyPeer>> {
        let resp = self
            .client
            .get(self.url("/me/peers"))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 401 {
            bail!("Authentication failed. Run `floppa-cli login` again.");
        }
        if !resp.status().is_success() {
            bail!("GET /me/peers failed: {}", resp.status());
        }

        let body: MyPeersResponse = resp
            .json()
            .await
            .context("Failed to parse peers response")?;
        Ok(body.peers)
    }

    pub async fn create_peer(
        &self,
        device: &DeviceIdentity,
        protocol: Protocol,
    ) -> Result<CreatePeerResponse> {
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

        if resp.status() == 402 {
            bail!("No active subscription. Cannot create peer.");
        }
        if resp.status() == 403 {
            bail!("Peer limit reached for your plan.");
        }
        if !resp.status().is_success() {
            bail!("POST /me/peers failed: {}", resp.status());
        }

        resp.json()
            .await
            .context("Failed to parse create peer response")
    }

    pub async fn get_peer_config(&self, peer_id: i64) -> Result<String> {
        let resp = self
            .client
            .get(self.url(&format!("/me/peers/{peer_id}/config")))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if !resp.status().is_success() {
            bail!("GET /me/peers/{}/config failed: {}", peer_id, resp.status());
        }

        resp.text().await.context("Failed to read config response")
    }

    /// The current user's peer for this device and `protocol`, if any (`None` on 404).
    pub async fn get_peer_by_device(
        &self,
        device_id: &str,
        protocol: Protocol,
    ) -> Result<Option<MyPeer>> {
        let resp = self
            .client
            .get(self.url(&format!(
                "/me/peers/by-device/{device_id}?protocol={protocol}"
            )))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 401 {
            bail!("Authentication failed. Run `floppa-cli login` again.");
        }
        if resp.status() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("GET /me/peers/by-device failed: {}", resp.status());
        }

        resp.json()
            .await
            .map(Some)
            .context("Failed to parse peer response")
    }

    /// Config for `protocol`: the VLESS URI, or this device's peer config (created on first use).
    pub async fn config_for(&self, protocol: Protocol, device: &DeviceIdentity) -> Result<String> {
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
    ) -> Result<String> {
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
    pub async fn get_vless_config(&self) -> Result<String> {
        let resp = self
            .client
            .get(self.url("/me/vless-config"))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 401 {
            bail!("Authentication failed. Run `floppa-cli login` again.");
        }
        if resp.status() == 404 {
            bail!("VLESS not available on this server.");
        }
        if !resp.status().is_success() {
            bail!("GET /me/vless-config failed: {}", resp.status());
        }

        let vless: VlessConfigResponse = resp
            .json()
            .await
            .context("Failed to parse VLESS config response")?;
        Ok(vless.uri)
    }

    /// Exchange a one-time login code for a JWT token (no auth required).
    pub async fn exchange_code(base_url: &str, code: &str) -> Result<AuthResponse> {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/auth/telegram/exchange-code",
            base_url.trim_end_matches('/')
        );

        let resp = client
            .post(&url)
            .json(&ExchangeCodeRequest {
                code: code.to_string(),
            })
            .send()
            .await
            .context("Failed to exchange login code")?;

        if resp.status() == 401 {
            bail!("Login code expired or invalid. Try again.");
        }
        if !resp.status().is_success() {
            bail!("Code exchange failed: {}", resp.status());
        }

        resp.json().await.context("Failed to parse auth response")
    }
}
