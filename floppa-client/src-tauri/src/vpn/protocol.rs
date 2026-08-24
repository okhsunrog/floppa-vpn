//! The single protocol representation for the crate, plus the validated interface name.
//!
//! Everything that used to pass a protocol around as a `String` goes through [`Protocol`].
//! The only place raw strings still appear is the persistence boundary, where the serde
//! renames below are load-bearing.

use serde::{Deserialize, Serialize};
use specta::Type;

/// The ONLY protocol representation in the crate.
///
/// The serde renames are load-bearing: these exact strings are already on users' disks and in
/// their OS keyrings via the old `SavedVpnConfigs.active_protocol: String`, and in all three
/// arms of the migration chain in `config.rs::parse_stored_configs`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type,
)]
pub enum Protocol {
    #[serde(rename = "wireguard")]
    WireGuard,
    #[serde(rename = "amneziawg")]
    AmneziaWg,
    #[serde(rename = "vless")]
    Vless,
}

impl Protocol {
    /// Preference order used when nothing else is known. AmneziaWG first: it is the project
    /// default because plain WireGuard is DPI-blocked on the networks this client targets.
    pub const ALL: [Self; 3] = [Self::AmneziaWg, Self::WireGuard, Self::Vless];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WireGuard => "wireguard",
            Self::AmneziaWg => "amneziawg",
            Self::Vless => "vless",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown protocol `{0}`")]
pub struct UnknownProtocol(pub String);

impl std::str::FromStr for Protocol {
    type Err = UnknownProtocol;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "wireguard" => Ok(Self::WireGuard),
            "amneziawg" => Ok(Self::AmneziaWg),
            "vless" => Ok(Self::Vless),
            other => Err(UnknownProtocol(other.to_owned())),
        }
    }
}

/// Persisted "the protocol that last actually worked".
///
/// An unparseable or empty legacy value deserializes to `None` — an explicit migration decision,
/// never the silent WireGuard fallback that `SavedVpnConfigs::active_config`'s `_ =>` arm used
/// to perform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preference(pub Option<Protocol>);

impl Serialize for Preference {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.map_or("", Protocol::as_str))
    }
}

impl<'de> Deserialize<'de> for Preference {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Option::<String>::deserialize(d)?.unwrap_or_default();
        if !raw.is_empty() && raw.parse::<Protocol>().is_err() {
            tracing::warn!(value = %raw, "migrating unknown persisted protocol to None");
        }
        Ok(Self(raw.parse().ok()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid interface name `{0}` (expected floppa<N>)")]
pub struct InvalidInterfaceName(pub String);

/// Validated against the privileged helper's own `^floppa[0-9]+$` check
/// (`resources/linux/floppa-network-helper:11`). Replaces the `const INTERFACE_NAME` in
/// `commands.rs` and the duplicated bare `"floppa0"` literal in `lib.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InterfaceName(String);

impl InterfaceName {
    pub const DEFAULT: &'static str = "floppa0";

    pub fn new(s: impl Into<String>) -> Result<Self, InvalidInterfaceName> {
        let s = s.into();
        let ok = s
            .strip_prefix("floppa")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
        if ok {
            Ok(Self(s))
        } else {
            Err(InvalidInterfaceName(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for InterfaceName {
    fn default() -> Self {
        Self(Self::DEFAULT.to_owned())
    }
}

impl std::fmt::Display for InterfaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrips_through_its_wire_string() {
        for p in Protocol::ALL {
            assert_eq!(p.as_str().parse::<Protocol>(), Ok(p));
        }
    }

    #[test]
    fn protocol_wire_strings_match_what_is_already_persisted() {
        assert_eq!(Protocol::WireGuard.as_str(), "wireguard");
        assert_eq!(Protocol::AmneziaWg.as_str(), "amneziawg");
        assert_eq!(Protocol::Vless.as_str(), "vless");
    }

    #[test]
    fn unknown_protocol_string_is_rejected_not_defaulted() {
        assert!("openvpn".parse::<Protocol>().is_err());
        assert!("".parse::<Protocol>().is_err());
    }

    #[test]
    fn preference_migrates_unknown_and_empty_to_none() {
        let unknown: Preference = serde_json::from_str(r#""openvpn""#).unwrap();
        assert_eq!(unknown, Preference(None));
        let empty: Preference = serde_json::from_str(r#""""#).unwrap();
        assert_eq!(empty, Preference(None));
        let missing: Preference = serde_json::from_str("null").unwrap();
        assert_eq!(missing, Preference(None));
    }

    #[test]
    fn preference_roundtrips_a_known_protocol() {
        let p = Preference(Some(Protocol::AmneziaWg));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#""amneziawg""#);
        assert_eq!(serde_json::from_str::<Preference>(&json).unwrap(), p);
    }

    #[test]
    fn interface_name_matches_the_helper_allowlist() {
        assert!(InterfaceName::new("floppa0").is_ok());
        assert!(InterfaceName::new("floppa42").is_ok());
        assert!(InterfaceName::new("floppa").is_err());
        assert!(InterfaceName::new("floppa0x").is_err());
        assert!(InterfaceName::new("wg0").is_err());
        assert!(InterfaceName::new("../floppa0").is_err());
    }

    #[test]
    fn interface_name_default_is_allowed() {
        assert_eq!(InterfaceName::default().as_str(), InterfaceName::DEFAULT);
        assert!(InterfaceName::new(InterfaceName::DEFAULT).is_ok());
    }
}
