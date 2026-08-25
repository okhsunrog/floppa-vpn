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

/// What a bot notification was about; `notification_log.kind` (TEXT, CHECK-constrained by
/// migration 0016). The `(subscription_id, kind)` unique index makes each kind fire once per
/// subscription. Bind as `NotificationKind::ExpiryNow as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT")]
pub enum NotificationKind {
    /// Sent about a day before the subscription ends.
    #[sqlx(rename = "expiry_1d_before")]
    #[serde(rename = "expiry_1d_before")]
    ExpiryOneDayBefore,
    /// Sent once the subscription has ended.
    #[sqlx(rename = "expiry_now")]
    #[serde(rename = "expiry_now")]
    ExpiryNow,
}

/// How a Telegram link code was consumed; `telegram_link_codes.kind` (TEXT, NULL until
/// consumed, CHECK-constrained by migration 0016). Bind as `LinkCodeKind::Simple as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum LinkCodeKind {
    /// The Telegram was free and simply attached to the account that minted the code.
    Simple,
    /// The Telegram's established account was merged into the account that minted the code.
    Merge,
}

/// Lifecycle of a `payments` row (TEXT column `status`). Bind as `PaymentStatus::Completed as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    /// The charge produced a subscription.
    Completed,
    /// Telegram reported a successful charge, but turning it into a subscription failed; the
    /// row keeps the charge id and the reason so the payment is not lost in a log line.
    Failed,
}

/// A user's interface language, stored in `users.language` (TEXT; NULL = no preference yet).
///
/// Only languages the bot has translations for are representable, so the column can never
/// hold a value no reader understands. Bind it in `query!` macros as `Lang::Ru as _`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    /// Database form ("en" | "ru").
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    /// Map an IETF language tag the way Telegram sends it in `language_code` (`ru`, `en-GB`,
    /// `pt-br`) to a supported language. `None` for tags we have no translation for, so the
    /// caller can leave the stored preference untouched instead of recording a useless value.
    pub fn from_language_tag(tag: &str) -> Option<Self> {
        let primary = tag.split(['-', '_']).next()?;
        match primary.to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "ru" => Some(Lang::Ru),
            _ => None,
        }
    }
}

/// The database form: "en" | "ru".
impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// A string that names no supported [`Lang`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported language {0:?} (expected \"en\" or \"ru\")")]
pub struct LangParseError(pub String);

/// Accepts exactly the database form (see [`Lang::as_db_str`]); use
/// [`Lang::from_language_tag`] for Telegram's looser `language_code`.
impl FromStr for Lang {
    type Err = LangParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "en" => Ok(Lang::En),
            "ru" => Ok(Lang::Ru),
            other => Err(LangParseError(other.to_owned())),
        }
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
    fn lang_from_telegram_language_tag() {
        assert_eq!(Lang::from_language_tag("ru"), Some(Lang::Ru));
        assert_eq!(Lang::from_language_tag("en-GB"), Some(Lang::En));
        assert_eq!(Lang::from_language_tag("RU_ru"), Some(Lang::Ru));
        assert_eq!(Lang::from_language_tag("de"), None);
        assert_eq!(Lang::from_language_tag(""), None);
    }

    #[test]
    fn lang_db_form_round_trips() {
        for lang in [Lang::En, Lang::Ru] {
            assert_eq!(lang.to_string().parse::<Lang>(), Ok(lang));
        }
        assert!("en-GB".parse::<Lang>().is_err());
    }

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
        assert_eq!(
            serde_json::to_string(&NotificationKind::ExpiryOneDayBefore).unwrap(),
            "\"expiry_1d_before\""
        );
        assert_eq!(
            serde_json::to_string(&LinkCodeKind::Merge).unwrap(),
            "\"merge\""
        );
    }
}
