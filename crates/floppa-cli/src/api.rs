use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct MeResponse {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub is_admin: bool,
    pub subscription: Option<SubscriptionInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionInfo {
    pub plan_name: String,
    pub plan_display_name: String,
    pub speed_limit_mbps: Option<i32>,
    pub traffic_limit_bytes: Option<i64>,
    pub max_peers: i32,
}

#[derive(Debug, Deserialize)]
pub struct MyPeer {
    pub id: i64,
    pub assigned_ip: String,
    pub sync_status: String,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
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
}

#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: AuthUserInfo,
}

#[derive(Debug, Deserialize)]
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

        resp.json().await.context("Failed to parse peers response")
    }

    pub async fn create_peer(&self, device_name: Option<String>) -> Result<CreatePeerResponse> {
        let resp = self
            .client
            .post(self.url("/me/peers"))
            .bearer_auth(&self.token)
            .json(&CreatePeerRequest {
                device_name,
                device_id: None,
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

        resp.text()
            .await
            .context("Failed to read config response")
    }

    /// Find an existing active peer or create a new one. Returns the WG config string.
    pub async fn find_or_create_peer(&self) -> Result<String> {
        let peers = self.list_peers().await?;

        // Look for an active peer
        let active = peers.iter().find(|p| p.sync_status == "active");

        let peer_id = if let Some(peer) = active {
            eprintln!("Using existing peer: {} ({})", peer.assigned_ip, peer.id);
            peer.id
        } else {
            let hostname = hostname();
            eprintln!("Creating new peer (device: {hostname})...");
            let created = self.create_peer(Some(hostname)).await?;
            eprintln!("Peer created: {} ({})", created.assigned_ip, created.id);
            return Ok(created.config);
        };

        self.get_peer_config(peer_id).await
    }

    /// Exchange a one-time login code for a JWT token (no auth required).
    pub async fn exchange_code(base_url: &str, code: &str) -> Result<AuthResponse> {
        let client = reqwest::Client::new();
        let url = format!("{}/auth/telegram/exchange-code", base_url.trim_end_matches('/'));

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

        resp.json()
            .await
            .context("Failed to parse auth response")
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| {
            std::fs::read_to_string("/etc/hostname")
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|_| "floppa-cli".to_string())
}
