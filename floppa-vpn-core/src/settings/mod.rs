//! User preferences, independent of the process that owns the tunnel.
//!
//! This is the new settings format, not a reader for legacy GUI or autostart data.
//! The owner supplies a path and platform capabilities. Secrets, server grants,
//! operation timers and the last successful protocol do not belong in this file.
mod storage;
pub use storage::{SettingsDocument, SettingsError, SettingsStore};

use crate::actor::intent::SplitMode;
use crate::protocol::Protocol;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ProtocolSettings {
    pub mode: SelectionMode,
    /// The ordered allowlist. Never append newly supported protocols on load.
    pub auto_order: Vec<Protocol>,
    pub manual: Protocol,
}

impl Default for ProtocolSettings {
    fn default() -> Self {
        Self {
            mode: SelectionMode::Auto,
            auto_order: Protocol::ALL.to_vec(),
            manual: Protocol::AmneziaWg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(deny_unknown_fields)]
pub struct AppRouting {
    pub mode: SplitMode,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(deny_unknown_fields)]
pub struct RoutingSettings {
    pub allow_lan: bool,
    pub excluded_routes: Vec<String>,
    pub apps: AppRouting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoverySettings {
    #[default]
    Persistent,
    Limited {
        rounds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(deny_unknown_fields)]
pub struct StartupSettings {
    pub connect_on_boot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub protocols: ProtocolSettings,
    pub routing: RoutingSettings,
    pub recovery: RecoverySettings,
    pub startup: StartupSettings,
}

/// Supplied by the deployment, not inferred from the build target. For example,
/// a standalone desktop process cannot promise system boot startup by itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsCapabilities {
    pub destination_exclusions: bool,
    pub app_routing: bool,
    pub connect_on_boot: bool,
}

impl Settings {
    pub fn validate(&self, capabilities: SettingsCapabilities) -> Result<(), SettingsError> {
        if self.protocols.auto_order.is_empty() {
            return Err(SettingsError::Invalid(
                "auto_order must not be empty".into(),
            ));
        }
        for (i, protocol) in self.protocols.auto_order.iter().enumerate() {
            if self.protocols.auto_order[..i].contains(protocol) {
                return Err(SettingsError::Invalid(
                    "auto_order contains duplicates".into(),
                ));
            }
        }
        if matches!(self.recovery, RecoverySettings::Limited { rounds: 0 }) {
            return Err(SettingsError::Invalid(
                "recovery rounds must be positive".into(),
            ));
        }
        let routes = crate::routing::validate_exclusions(&self.routing.excluded_routes)
            .map_err(SettingsError::Invalid)?;
        if routes != self.routing.excluded_routes {
            return Err(SettingsError::Invalid(
                "excluded_routes must contain sorted, unique canonical IPv4 networks".into(),
            ));
        }
        if !capabilities.destination_exclusions && !routes.is_empty() {
            return Err(SettingsError::Unsupported("destination exclusions"));
        }
        if !capabilities.app_routing
            && (self.routing.apps.mode != SplitMode::All || !self.routing.apps.items.is_empty())
        {
            return Err(SettingsError::Unsupported("application routing"));
        }
        if self
            .routing
            .apps
            .items
            .iter()
            .any(|item| item.trim().is_empty())
        {
            return Err(SettingsError::Invalid(
                "application identifiers must not be empty".into(),
            ));
        }
        if self.startup.connect_on_boot && !capabilities.connect_on_boot {
            return Err(SettingsError::Unsupported("connect on boot"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectReason {
    User,
    Recovery { last_successful: Option<Protocol> },
}

impl ProtocolSettings {
    /// Derive a fresh attempt plan without rewriting user priority or expanding
    /// the allowlist. An empty result is explicit, not permission to try others.
    pub fn plan(
        &self,
        available: &[Protocol],
        reason: ConnectReason,
    ) -> Result<Vec<Protocol>, SettingsError> {
        let requested = match self.mode {
            SelectionMode::Auto => self.auto_order.as_slice(),
            SelectionMode::Manual => std::slice::from_ref(&self.manual),
        };
        let mut order: Vec<_> = requested
            .iter()
            .copied()
            .filter(|p| available.contains(p))
            .collect();
        if order.is_empty() {
            return Err(SettingsError::NoAvailableProtocol);
        }
        if let ConnectReason::Recovery {
            last_successful: Some(protocol),
        } = reason
            && let Some(index) = order.iter().position(|p| *p == protocol)
        {
            order.remove(index);
            order.insert(0, protocol);
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests;
