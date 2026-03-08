use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Peer synchronization status with WireGuard interface
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum PeerSyncStatus {
    /// Peer added to DB, waiting for daemon to add to WireGuard
    PendingAdd,
    /// Peer is active in WireGuard
    Active,
    /// Peer marked for removal, waiting for daemon to remove from WireGuard
    PendingRemove,
    /// Peer removed from WireGuard (kept in DB for history)
    Removed,
}

/// Telegram user
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: i64,
    pub telegram_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub photo_url: Option<String>,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    pub trial_used_at: Option<DateTime<Utc>>,
}

/// WireGuard VPN peer
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Peer {
    pub id: i64,
    pub user_id: i64,
    pub public_key: String,
    /// Encrypted WireGuard private key
    pub private_key_encrypted: Option<String>,
    /// Assigned IP within VPN subnet, e.g. "10.100.0.5"
    pub assigned_ip: String,
    pub sync_status: PeerSyncStatus,
    pub created_at: DateTime<Utc>,
    /// Last WireGuard handshake time (updated by daemon)
    pub last_handshake: Option<DateTime<Utc>>,
    /// FK to app_installations (NULL for bot/web-created peers)
    pub installation_id: Option<i64>,
}

/// App installation (device) tracked independently of VPN peers
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AppInstallation {
    pub id: i64,
    pub user_id: i64,
    pub device_id: String,
    pub device_name: Option<String>,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Subscription plan definition
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Plan {
    pub id: i32,
    pub name: String,
    pub display_name: String,
    /// Bandwidth limit in Mbps (None = unlimited)
    pub default_speed_limit_mbps: Option<i32>,
    /// Maximum number of WireGuard peers allowed
    pub max_peers: i32,
    /// Price in rubles (0 = free)
    pub price_rub: i32,
    /// Whether this plan is visible to users (false = admin-only like "friends")
    pub is_public: bool,
    /// If set, this is a trial plan with auto-expiration
    pub trial_days: Option<i32>,
    /// Price in Telegram Stars (None = not purchasable with Stars)
    pub price_stars: Option<i32>,
    /// Subscription period in days (None = admin-only permanent plan)
    pub period_days: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// User subscription period.
/// Limits (speed, traffic, max_peers) come from the associated plan.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Subscription {
    pub id: i64,
    pub user_id: i64,
    pub plan_id: i32,
    pub starts_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub payment_id: Option<String>,
    pub source: String,
    pub created_at: DateTime<Utc>,
}
