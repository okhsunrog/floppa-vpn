//! What the user wants. Only commands change it.
//!
//! An intent is not a request to do something once: [`Intent::Up`] outlives a failure, and that
//! is the whole of auto-reconnect. It is the first axis of the decision table in
//! [`reconcile`](super::reconcile).

use super::status::UpStatus;
use crate::protocol::Protocol;
use serde::{Deserialize, Serialize};
use specta::Type;

/// Monotonic, bumped on every accepted intent change — including Down.
///
/// Carried into the Android service and echoed back by it, so an observation from a previous
/// service instance, or a stop for a superseded generation, is rejectable by value rather than by
/// guesswork.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize, Type,
)]
pub struct IntentEpoch(pub u64);

impl std::fmt::Display for IntentEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SplitMode {
    #[default]
    All,
    Include,
    Exclude,
}

/// Everything a *self-initiated* reconnect needs, because at reconnect time there is no caller to
/// supply it. `apps` is sorted and deduped on construction, so `PartialEq` means "the same tunnel"
/// rather than "the same list written the same way". `allow_lan` and `private_routes` default
/// off so intents written by older clients remain valid.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct TunnelParams {
    pub split_mode: SplitMode,
    pub apps: Vec<String>,
    #[serde(default)]
    pub allow_lan: bool,
    /// IPv4 CIDRs of the private networks the user's plan grants (`GET /me/private-networks`),
    /// routed into the tunnel on top of everything else — including when `allow_lan` has taken
    /// the private ranges out of it. Canonical, sorted and deduplicated by
    /// [`Self::with_private_routes`].
    #[serde(default)]
    pub private_routes: Vec<String>,
}

impl TunnelParams {
    pub fn new(split_mode: SplitMode, mut apps: Vec<String>) -> Self {
        apps.sort_unstable();
        apps.dedup();
        Self {
            split_mode,
            apps,
            allow_lan: false,
            private_routes: Vec::new(),
        }
    }

    pub fn with_allow_lan(mut self, allow_lan: bool) -> Self {
        self.allow_lan = allow_lan;
        self
    }

    /// Set the private routes, keeping only IPv4 networks and writing each the one way
    /// (`10.66.66.7/24` becomes `10.66.66.0/24`), so equal grants make equal params. Sorted as
    /// strings, which is what the app's `Array.prototype.sort` does to the same list.
    pub fn with_private_routes(mut self, routes: impl IntoIterator<Item = String>) -> Self {
        let mut canonical: Vec<String> = routes
            .into_iter()
            .filter_map(|r| r.trim().parse::<ipnetwork::Ipv4Network>().ok())
            .filter_map(|n| ipnetwork::Ipv4Network::new(n.network(), n.prefix()).ok())
            .map(|n| n.to_string())
            .collect();
        canonical.sort_unstable();
        canonical.dedup();
        self.private_routes = canonical;
        self
    }

    /// The private routes as networks. Entries that do not parse — only possible in an intent
    /// written by hand — are skipped rather than failing the tunnel.
    pub fn private_route_networks(&self) -> Vec<ipnetwork::IpNetwork> {
        self.private_routes
            .iter()
            .filter_map(|r| r.parse::<ipnetwork::Ipv4Network>().ok())
            .map(ipnetwork::IpNetwork::V4)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpIntent {
    pub epoch: IntentEpoch,
    /// Probe order, most preferred first. Non-empty by construction.
    pub order: Vec<Protocol>,
    /// `None` means "any tunnel of a protocol in `order` satisfies me" — used only by the
    /// bootstrap adoption intent. Every caller-issued Up carries `Some`.
    pub params: Option<TunnelParams>,
}

impl UpIntent {
    pub fn accepts(&self, p: Protocol) -> bool {
        self.order.contains(&p)
    }

    /// Is an already-established tunnel good enough for this intent?
    ///
    /// This is what makes "press Connect while connected" a no-op and "change the split rules,
    /// then reconnect" a real teardown — with no branch in the frontend.
    pub fn satisfied_by(&self, up: &UpStatus) -> bool {
        self.accepts(up.protocol)
            && match (&self.params, &up.params) {
                (None, _) => true,
                (Some(want), Some(have)) => want == have,
                // An adopted tunnel's split rules are unknown, so it cannot be proven to match.
                (Some(_), None) => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Down { epoch: IntentEpoch },
    Up(UpIntent),
}

impl Intent {
    pub fn epoch(&self) -> IntentEpoch {
        match self {
            Self::Down { epoch, .. } => *epoch,
            Self::Up(u) => u.epoch,
        }
    }

    pub fn params(&self) -> Option<&TunnelParams> {
        match self {
            Self::Up(u) => u.params.as_ref(),
            Self::Down { .. } => None,
        }
    }

    pub fn is_up(&self) -> bool {
        matches!(self, Self::Up(_))
    }
}

impl Default for Intent {
    fn default() -> Self {
        Self::Down {
            epoch: IntentEpoch(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(order: &[Protocol], params: Option<TunnelParams>) -> UpIntent {
        UpIntent {
            epoch: IntentEpoch(1),
            order: order.to_vec(),
            params,
        }
    }

    fn up_status(protocol: Protocol, params: Option<TunnelParams>) -> UpStatus {
        UpStatus {
            epoch: IntentEpoch(1),
            protocol,
            params,
            adopted: false,
            server_endpoint: "example:51820".into(),
            assigned_ip: "10.0.0.2/32".into(),
            connected_at: 0,
            dark_since: None,
            probing_since: None,
            resolved: true,
        }
    }

    #[test]
    fn tunnel_params_compare_by_content_not_by_spelling() {
        let a = TunnelParams::new(SplitMode::Include, vec!["b".into(), "a".into(), "b".into()]);
        let b = TunnelParams::new(SplitMode::Include, vec!["a".into(), "b".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn private_routes_are_canonical_ipv4_and_compare_by_content() {
        let a = TunnelParams::new(SplitMode::All, vec![]).with_private_routes([
            "10.66.66.7/24".to_string(),
            "10.65.65.0/24".into(),
            "fd00::/8".into(),
            "garbage".into(),
            "10.66.66.0/24".into(),
        ]);
        assert_eq!(a.private_routes, ["10.65.65.0/24", "10.66.66.0/24"]);
        let b = TunnelParams::new(SplitMode::All, vec![])
            .with_private_routes(["10.66.66.0/24".to_string(), "10.65.65.0/24".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn an_intent_written_before_private_routes_still_reads() {
        let old: TunnelParams =
            serde_json::from_str(r#"{"split_mode":"all","apps":[],"allow_lan":true}"#).unwrap();
        assert!(old.private_routes.is_empty());
        assert!(old.allow_lan);
    }

    #[test]
    fn a_running_tunnel_satisfies_an_intent_that_wants_the_same_thing() {
        let params = TunnelParams::new(SplitMode::All, vec![]);
        let intent = up(&[Protocol::AmneziaWg], Some(params.clone()));
        assert!(intent.satisfied_by(&up_status(Protocol::AmneziaWg, Some(params))));
    }

    #[test]
    fn different_split_rules_are_not_satisfied_by_the_running_tunnel() {
        let intent = up(
            &[Protocol::AmneziaWg],
            Some(TunnelParams::new(SplitMode::Include, vec!["x".into()])),
        );
        let running = up_status(
            Protocol::AmneziaWg,
            Some(TunnelParams::new(SplitMode::All, vec![])),
        );
        assert!(!intent.satisfied_by(&running));
    }

    #[test]
    fn an_adopted_tunnel_cannot_satisfy_an_intent_that_specifies_params() {
        let intent = up(
            &[Protocol::AmneziaWg],
            Some(TunnelParams::new(SplitMode::All, vec![])),
        );
        assert!(!intent.satisfied_by(&up_status(Protocol::AmneziaWg, None)));
    }

    #[test]
    fn the_bootstrap_adoption_intent_accepts_any_params() {
        let intent = up(&[Protocol::AmneziaWg], None);
        assert!(intent.satisfied_by(&up_status(Protocol::AmneziaWg, None)));
        assert!(intent.satisfied_by(&up_status(
            Protocol::AmneziaWg,
            Some(TunnelParams::default())
        )));
    }

    #[test]
    fn a_protocol_outside_the_order_never_satisfies_the_intent() {
        let intent = up(&[Protocol::AmneziaWg], None);
        assert!(!intent.satisfied_by(&up_status(Protocol::Vless, None)));
    }
}
