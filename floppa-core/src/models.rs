use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::fmt;
use std::str::FromStr;

/// VPN tunnel protocol. WireGuard and AmneziaWG share the peers table (keypair + IP);
/// AmneziaWG adds interface-wide obfuscation and runs on its own server interface.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
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
