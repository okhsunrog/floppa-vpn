use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

fn make_client() -> reqwest::Client {
    let builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
    // https_only disabled in test builds so tests can use local HTTP mock servers.
    #[cfg(not(test))]
    let builder = builder.https_only(true);
    builder.build().expect("reqwest client build failed")
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
    pub sync_status: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
}

fn default_protocol() -> String {
    "wireguard".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub device_name: String,
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
    protocol: Option<String>,
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

#[derive(Debug, Serialize)]
struct AccountLoginRequest {
    login: String,
    password: String,
}

impl ApiClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            client: make_client(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn get_peer_by_device(
        &self,
        device_id: &str,
        protocol: &str,
    ) -> Result<Option<MyPeer>> {
        let resp = self
            .client
            .get(format!(
                "{}?protocol={}",
                self.url(&format!("/me/peers/by-device/{device_id}")),
                protocol
            ))
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
            bail!(
                "GET /me/peers/by-device/{device_id} failed: {}",
                resp.status()
            );
        }

        resp.json()
            .await
            .context("Failed to parse peer-by-device response")
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
        device_name: Option<String>,
        device_id: Option<String>,
        protocol: Option<&str>,
    ) -> Result<CreatePeerResponse> {
        let resp = self
            .client
            .post(self.url("/me/peers"))
            .bearer_auth(&self.token)
            .json(&CreatePeerRequest {
                device_name,
                device_id,
                protocol: protocol.map(|p| p.to_string()),
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

    pub async fn delete_peer(&self, peer_id: i64) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/me/peers/{peer_id}")))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 401 {
            bail!("Authentication failed. Run `floppa-cli login` again.");
        }
        if resp.status() == 404 {
            bail!("Peer {peer_id} not found.");
        }
        if !resp.status().is_success() {
            bail!("DELETE /me/peers/{peer_id} failed: {}", resp.status());
        }
        Ok(())
    }

    pub async fn regenerate_vless_config(&self) -> Result<String> {
        let resp = self
            .client
            .post(self.url("/me/vless-config/regenerate"))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 401 {
            bail!("Authentication failed. Run `floppa-cli login` again.");
        }
        if !resp.status().is_success() {
            bail!("POST /me/vless-config/regenerate failed: {}", resp.status());
        }

        let vless: VlessConfigResponse = resp
            .json()
            .await
            .context("Failed to parse regenerated VLESS config response")?;
        Ok(vless.uri)
    }

    /// Find an existing active peer for `protocol` ("wireguard" | "amneziawg"), or create one.
    pub async fn find_or_create_peer(&self, protocol: &str) -> Result<String> {
        let identity = get_or_create_device_identity()?;

        if let Some(peer) = self
            .get_peer_by_device(&identity.device_id, protocol)
            .await?
        {
            eprintln!(
                "Using existing {protocol} peer for device {}: {} ({})",
                identity.device_id, peer.assigned_ip, peer.id
            );
            return self.get_peer_config(peer.id).await;
        }

        let peers = self.list_peers().await?;
        let peer_id = if let Some(peer) = peers.iter().find(|p| {
            p.device_id.as_deref() == Some(identity.device_id.as_str()) && p.protocol == protocol
        }) {
            eprintln!(
                "Using existing {protocol} peer for device {}: {} ({})",
                identity.device_id, peer.assigned_ip, peer.id
            );
            peer.id
        } else {
            eprintln!(
                "Creating new {protocol} peer (device_id: {}, device: {})...",
                identity.device_id, identity.device_name
            );
            let created = self
                .create_peer(
                    Some(identity.device_name),
                    Some(identity.device_id),
                    Some(protocol),
                )
                .await?;
            eprintln!("Peer created: {} ({})", created.assigned_ip, created.id);
            return Ok(created.config);
        };

        self.get_peer_config(peer_id).await
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

    /// Authenticate with a login + password and return a JWT token (no auth required).
    pub async fn login_account(
        base_url: &str,
        login: &str,
        password: &str,
    ) -> Result<AuthResponse> {
        let client = make_client();
        let url = format!("{}/auth/account/login", base_url.trim_end_matches('/'));

        let resp = client
            .post(&url)
            .json(&AccountLoginRequest {
                login: login.to_string(),
                password: password.to_string(),
            })
            .send()
            .await
            .context("Failed to log in with account credentials")?;

        if resp.status() == 401 {
            bail!("Invalid login or password. Try again or use Telegram login.");
        }
        if !resp.status().is_success() {
            bail!("Account login failed: {}", resp.status());
        }

        resp.json().await.context("Failed to parse auth response")
    }

    /// Exchange a one-time login code for a JWT token (no auth required).
    pub async fn exchange_code(base_url: &str, code: &str) -> Result<AuthResponse> {
        let client = make_client();
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

fn config_dir() -> Result<PathBuf> {
    crate::paths::floppa_config_dir()
}

pub fn get_or_create_device_identity() -> Result<DeviceIdentity> {
    let path = config_dir()?.join("device.json");
    if let Ok(raw) = fs::read_to_string(&path) {
        let identity: DeviceIdentity = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse device identity file: {}", path.display()))?;
        if !identity.device_id.is_empty() && !identity.device_name.is_empty() {
            return Ok(identity);
        }
    }

    let identity = DeviceIdentity {
        device_id: random_device_id(),
        device_name: hostname(),
    };
    let raw = serde_json::to_string_pretty(&identity)?;
    write_device_json(&path, &raw)?;
    eprintln!("Created device identity: {}", identity.device_id);
    Ok(identity)
}

pub fn reset_device_identity() -> Result<DeviceIdentity> {
    let path = config_dir()?.join("device.json");
    let identity = DeviceIdentity {
        device_id: random_device_id(),
        device_name: hostname(),
    };
    let raw = serde_json::to_string_pretty(&identity)?;
    write_device_json(&path, &raw)?;
    eprintln!("Reset device identity: {}", identity.device_id);
    Ok(identity)
}

fn write_device_json(path: &std::path::Path, raw: &str) -> Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        f.write_all(raw.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(())
}

fn random_device_id() -> String {
    rand::random::<u128>()
        .to_be_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "floppa-cli".to_string())
}
