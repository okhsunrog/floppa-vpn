use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::fmt;
use std::str::FromStr;

/// VPN tunnel protocol. WireGuard and AmneziaWG share the peers table (keypair + IP);
/// AmneziaWG adds interface-wide obfuscation and runs on its own server interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT")]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[sqlx(rename = "wireguard")]
    WireGuard,
    /// Default protocol for new peers (DPI-resistant).
    #[default]
    #[sqlx(rename = "amneziawg")]
    AmneziaWg,
}

impl Protocol {
    /// Database/config string form ("wireguard" | "amneziawg").
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Protocol::WireGuard => "wireguard",
            Protocol::AmneziaWg => "amneziawg",
        }
    }
}

/// The database/config form: "wireguard" | "amneziawg".
impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// A string that names no known [`Protocol`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown protocol {0:?} (expected \"wireguard\" or \"amneziawg\")")]
pub struct ProtocolParseError(pub String);

/// Accepts exactly the database/config form (see [`Protocol::as_db_str`]).
impl FromStr for Protocol {
    type Err = ProtocolParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "wireguard" => Ok(Protocol::WireGuard),
            "amneziawg" => Ok(Protocol::AmneziaWg),
            other => Err(ProtocolParseError(other.to_owned())),
        }
    }
}

impl TryFrom<&str> for Protocol {
    type Error = ProtocolParseError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Peer synchronization status with WireGuard interface.
///
/// Stored in `peers.sync_status` (TEXT, CHECK-constrained by migration 0014); bind it in
/// `query!` macros as `$n` with `PeerSyncStatus::Active as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
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

/// How a subscription came to exist. Stored in `subscriptions.source` (TEXT, CHECK-constrained
/// by migration 0014); bind it in `query!` macros as `$n` with `SubscriptionSource::Trial as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionSource {
    /// The one-time real trial (the "basic" plan's `trial_minutes`), claims `users.trial_used_at`.
    Trial,
    /// The short credential-signup taster; does not consume the real trial.
    Taster,
    /// Paid with Telegram Stars (including credit-funded plan switches).
    Purchase,
    /// Granted by an admin from the panel.
    AdminGrant,
}

/// Telegram user
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: i64,
    pub telegram_id: Option<i64>,
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
    /// Tunnel protocol (wireguard or amneziawg)
    #[serde(default)]
    pub protocol: Protocol,
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
    /// Whether this plan is visible to users (false = admin-only like "friends")
    pub is_public: bool,
    /// If set, this is a trial plan; the subscription lasts this many minutes (auto-expires)
    pub trial_minutes: Option<i32>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_parses_its_own_display_form() {
        for p in [Protocol::WireGuard, Protocol::AmneziaWg] {
            assert_eq!(p.to_string().parse::<Protocol>(), Ok(p));
            assert_eq!(Protocol::try_from(p.as_db_str()), Ok(p));
        }
        assert_eq!(
            "WireGuard".parse::<Protocol>(),
            Err(ProtocolParseError("WireGuard".into()))
        );
        assert!("vless".parse::<Protocol>().is_err());
    }

    #[test]
    fn enums_serialize_to_their_db_form() {
        assert_eq!(
            serde_json::to_string(&Protocol::AmneziaWg).unwrap(),
            "\"amneziawg\""
        );
        assert_eq!(
            serde_json::to_string(&PeerSyncStatus::PendingRemove).unwrap(),
            "\"pending_remove\""
        );
        assert_eq!(
            serde_json::to_string(&SubscriptionSource::AdminGrant).unwrap(),
            "\"admin_grant\""
        );
    }
}
