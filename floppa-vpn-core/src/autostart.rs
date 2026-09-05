//! What a start with nobody watching rebuilds from, and what a TUN is built from.
//!
//! Android starts the VPN service without any UI for always-on VPN, at boot, and to restore a
//! lockdown ("block connections without VPN") session. The intent it sends carries no
//! configuration, so the actor can only honour it from something written down earlier. That is
//! [`LastIntent`]: the order and split rules of the last connect that actually worked. The configs
//! themselves are not here — the actor owns the store, in the same process — which is what shrank
//! this file from a whole rebuilt tunnel to a single request.
//!
//! It is rewritten on every successful connect (winner first), removed when the configs are
//! forgotten, and *kept* on an explicit Disconnect: always-on means the OS restarts the service,
//! and whether that should happen is the system toggle's decision, not ours.
//!
//! Protection at rest matches `vpn-config.json`: a `0600` file in the app's private data
//! directory, written atomically (see [`private_file`](super::private_file)) — a file caught
//! half-written is one that does not parse, and the reader's only recourse is to stop, which under
//! lockdown is a device with no network until somebody opens the app. Only the calls that touch
//! the filesystem are Android-specific; the types and the derivations are plain data, so their
//! tests run on the host.

use super::actor::types::{SplitMode, TunnelParams};
use super::private_file::write_private;
use super::state::ProtocolConfig;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Bumped whenever the on-disk shape changes incompatibly. A file of another version is not
/// migrated, it is ignored: the next connect writes a fresh one, and a system start that finds
/// nothing usable stops rather than guessing.
pub const BUNDLE_VERSION: u32 = 2;

/// Also spelled out as `AUTOSTART_FILENAME` in `BootRetry.kt` on the Kotlin side: the Quick
/// Settings tile and the boot retry have to know whether there is anything to start *before*
/// starting anything, and the presence of this file is the only evidence of that available
/// without booting the actor.
pub const BUNDLE_FILENAME: &str = "autostart.json";

/// What `VpnService.Builder` needs, derived from the config and the split rules once, at connect
/// time — so nothing downstream ever parses a protocol config to build a TUN.
///
/// The field names are what the Kotlin side reads (camelCase); it is handed this as JSON when the
/// ladder asks for a descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunSpec {
    pub ipv4_addr: String,
    #[serde(default)]
    pub ipv6_addr: Option<String>,
    pub routes: Vec<String>,
    #[serde(default)]
    pub dns: Option<String>,
    pub mtu: u32,
    #[serde(default)]
    pub disallowed_apps: Vec<String>,
    #[serde(default)]
    pub allowed_apps: Vec<String>,
}

impl TunSpec {
    /// The one derivation of TUN parameters from a config and its split rules. Both the plugin
    /// start (over the intent) and the autonomous start (over the bundle) are built from this.
    pub fn derive(config: &ProtocolConfig, params: &TunnelParams) -> Self {
        // Resolvers only: `VpnService.Builder.addDnsServer` takes addresses, and a search domain
        // on the DNS line would just be logged as an invalid server on the Kotlin side.
        let dns_servers = config.dns_servers();
        let dns =
            (!dns_servers.is_empty()).then(|| floppa_tunnel_config::conf::comma_list(dns_servers));

        // Only the families this tunnel can actually carry. `ipv6_addr` is `None` here for every
        // protocol, so asking for `::/0` handed Android's whole IPv6 traffic to an interface with
        // no IPv6 address: packets left with a link-local source and were never answered. And
        // because Android prefers IPv6 wherever a route offers it, that was not a slow path but
        // no connectivity at all — a tunnel that reported Connected and carried nothing.
        // See `ProtocolConfig::has_ipv6_address`.
        let routes = floppa_tunnel_config::route::CATCH_ALL
            .iter()
            .filter(|net| net.is_ipv4() || config.has_ipv6_address())
            .map(ToString::to_string)
            .collect();

        let mut spec = Self {
            ipv4_addr: config.address(),
            ipv6_addr: None,
            routes,
            dns,
            mtu: config.get_mtu() as u32,
            disallowed_apps: Vec::new(),
            allowed_apps: Vec::new(),
        };
        if !params.apps.is_empty() {
            match params.split_mode {
                SplitMode::Exclude => spec.disallowed_apps = params.apps.clone(),
                SplitMode::Include => spec.allowed_apps = params.apps.clone(),
                SplitMode::All => {}
            }
        }
        spec
    }

    /// What the service is asked to establish, for one particular request.
    pub fn with_generation(self, generation: u64) -> TunPlan {
        TunPlan {
            generation,
            tun: self,
        }
    }
}

/// A [`TunSpec`] with the identity of the request it belongs to.
///
/// The generation travels *with* the spec because the answer comes back separately — as a
/// descriptor or as a reason — and has to name what it is answering. Serialized flat, so the
/// Kotlin side reads one object with the field names it already knows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunPlan {
    pub generation: u64,
    #[serde(flatten)]
    pub tun: TunSpec,
}

/// The last thing that was successfully connected: what to rebuild when the system asks for a
/// tunnel with nobody watching.
///
/// Only the *request* is persisted, not the tunnel. The actor has the configs — it owns the store
/// — so what it is missing when the system starts it cold is which protocols to try and what split
/// rules to build with, and that is all this is. The previous shape held a whole rebuilt tunnel
/// (a config, a TUN spec, a resolved endpoint) because the process reading it had no actor in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastIntent {
    pub version: u32,
    /// The order the last successful connect used, winner first.
    pub order: Vec<super::protocol::Protocol>,
    pub params: TunnelParams,
    /// Unix seconds; informational.
    pub saved_at: i64,
}

impl LastIntent {
    pub fn new(order: Vec<super::protocol::Protocol>, params: TunnelParams, saved_at: i64) -> Self {
        Self {
            version: BUNDLE_VERSION,
            order,
            params,
            saved_at,
        }
    }

    /// The request to raise when the system asks. An empty order is not one — the actor would
    /// refuse it, and stopping outright says so more clearly than a refusal in a log.
    pub fn request(self) -> Option<crate::actor::handle::IntentRequest> {
        (!self.order.is_empty()).then_some(crate::actor::handle::IntentRequest::Up {
            order: self.order,
            params: self.params,
        })
    }
}

fn bundle_path(dir: &Path) -> PathBuf {
    dir.join(BUNDLE_FILENAME)
}

/// Write the last-good intent, `0600`, replacing whatever was there.
///
/// The same protection as `vpn-config.json` even though this holds no key: it names which servers
/// this device connects to, and the directory it lives in is private anyway.
pub fn save(dir: &Path, intent: &LastIntent) -> Result<(), String> {
    let json = serde_json::to_string(intent).map_err(|e| format!("serialize: {e}"))?;
    let path = bundle_path(dir);
    write_private(&path, json.as_bytes()).map_err(|e| format!("write {}: {e}", path.display()))?;
    debug!(order = ?intent.order, "the last-good intent was written to {}", path.display());
    Ok(())
}

/// Read the last-good intent, if there is one this build can use.
///
/// Every reason for `None` is logged, because the caller's next step is to stop and the log line
/// is the only account of why.
pub fn load(dir: &Path) -> Option<LastIntent> {
    let path = bundle_path(dir);
    let json = match std::fs::read_to_string(&path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!("no last-good intent at {}", path.display());
            return None;
        }
        Err(e) => {
            warn!(
                "failed to read the last-good intent {}: {e}",
                path.display()
            );
            return None;
        }
    };
    let intent: LastIntent = match serde_json::from_str(&json) {
        Ok(intent) => intent,
        Err(e) => {
            warn!(
                "the last-good intent {} does not parse: {e}",
                path.display()
            );
            return None;
        }
    };
    if intent.version != BUNDLE_VERSION {
        warn!(
            found = intent.version,
            expected = BUNDLE_VERSION,
            "the last-good intent is of another version; ignoring it"
        );
        return None;
    }
    Some(intent)
}

/// The request a system-issued start should raise, if anything has ever connected.
pub fn last_intent() -> Option<crate::actor::handle::IntentRequest> {
    let dir = super::config::config_dir().ok()?;
    load(&dir)?.request()
}

/// Record what just connected, so the system can ask for it again with nobody watching.
///
/// Best-effort: an intent that could not be written costs the next always-on start, never this
/// connect.
pub fn remember(order: Vec<super::protocol::Protocol>, params: TunnelParams, saved_at: i64) {
    let Ok(dir) = super::config::config_dir() else {
        return;
    };
    if let Err(e) = save(&dir, &LastIntent::new(order, params, saved_at)) {
        warn!("failed to record the last-good intent: {e}");
    }
}

/// Delete it. A missing file is not an error.
pub fn remove(dir: &Path) {
    let path = bundle_path(dir);
    match std::fs::remove_file(&path) {
        Ok(()) => info!("the last-good intent was removed: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "failed to remove the last-good intent {}: {e}",
            path.display()
        ),
    }
}

/// The identity of one Android service start, minted by the UI process.
///
/// Deliberately **not** the cycle's `IntentEpoch`. An intent's epoch is shared by every protocol
/// and every pass of one cycle, so every "is this our generation?" check — the descriptor arriving,
/// the start failing, the instance being destroyed — could pass for a service instance we had
/// already moved on from. A generation is minted per *service start* instead: one value per
/// request for a TUN, never reused.
///
/// The base is random rather than a persisted counter: the value is only ever compared for
/// equality, so ordering would buy nothing, and a file read on the actor's task would buy a
/// blocking call per attempt. Zero is never minted, which is what makes it usable as "nothing is
/// being served".
#[derive(Debug)]
pub struct ServiceGenerations(u64);

impl ServiceGenerations {
    /// A fresh, process-unique sequence. `RandomState` is seeded per process by the standard
    /// library, which is where the entropy comes from — no dependency, no syscall.
    pub fn new() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let seed = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        // 2^20 generations of headroom per process: a cycle burns a handful.
        Self(u64::from(seed as u32) << 20)
    }

    /// The next generation. Monotonic within the process, and never zero.
    pub fn mint(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

impl Default for ServiceGenerations {
    fn default() -> Self {
        Self::new()
    }
}

/// Where `host` resolved to the last time anyone managed to resolve it.
///
/// The one thing a start under lockdown cannot do for itself. "Block connections without VPN"
/// means nothing reaches the network until the tunnel is up, so the resolver has nothing to answer
/// with — and the tunnel cannot come up without an address. A literal from the last successful
/// connect breaks that circle, and is the difference between a device that recovers on its own and
/// one that has no network until somebody opens the app.
const ENDPOINTS_FILENAME: &str = "last-endpoints.json";

fn endpoints_path(dir: &Path) -> PathBuf {
    dir.join(ENDPOINTS_FILENAME)
}

fn read_endpoints(dir: &Path) -> std::collections::BTreeMap<String, SocketAddr> {
    std::fs::read_to_string(endpoints_path(dir))
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Remember where a host resolved to. Best-effort in both directions: a failure to write costs a
/// future lockdown start, never this one.
pub fn remember_endpoint(host: &str, addr: SocketAddr) {
    let Ok(dir) = super::config::config_dir() else {
        return;
    };
    let mut known = read_endpoints(&dir);
    if known.get(host) == Some(&addr) {
        return;
    }
    known.insert(host.to_string(), addr);
    match serde_json::to_string(&known) {
        Ok(json) => {
            if let Err(e) = write_private(&endpoints_path(&dir), json.as_bytes()) {
                debug!("could not record where {host} resolves: {e}");
            }
        }
        Err(e) => debug!("could not encode the endpoint cache: {e}"),
    }
}

/// The last known address for `host`, for a start that cannot resolve one.
pub fn known_endpoint(host: &str) -> Option<SocketAddr> {
    let dir = super::config::config_dir().ok()?;
    read_endpoints(&dir).get(host).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Protocol;
    use crate::state::WgConfig;

    const WG_CONFIG: &str = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32
DNS = 1.1.1.1, 8.8.8.8

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = vpn.example.com:51820
AllowedIPs = 0.0.0.0/0
";

    fn config() -> ProtocolConfig {
        ProtocolConfig::WireGuard(WgConfig::from_config_str(WG_CONFIG).expect("fixture parses"))
    }

    /// The same fixture with an IPv6 twin on the `Address` line, as wg-quick allows.
    const WG_DUAL_STACK: &str = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32, fd00::2/128
DNS = 1.1.1.1

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = vpn.example.com:51820
AllowedIPs = 0.0.0.0/0
";

    #[test]
    fn a_tunnel_with_no_ipv6_address_asks_for_no_ipv6_routes() {
        // The defect this pins: `ipv6_addr` is None for every protocol, so claiming `::/0`
        // handed the device's IPv6 traffic to an interface that could not carry it. Clients
        // prefer IPv6 when a route offers it, so the result was not a slow path but a tunnel
        // that reported Connected and carried nothing.
        let spec = TunSpec::derive(&config(), &TunnelParams::new(SplitMode::All, vec![]));
        assert_eq!(spec.routes, vec!["0.0.0.0/0".to_string()]);
        assert!(spec.ipv6_addr.is_none(), "the premise of the filter");
    }

    #[test]
    fn a_dual_stack_config_keeps_its_ipv6_route() {
        // The gate is about what the tunnel can carry, not a blanket ban: a config whose
        // `Address` names an IPv6 twin really can route IPv6, and must still be asked to.
        let dual = ProtocolConfig::WireGuard(
            WgConfig::from_config_str(WG_DUAL_STACK).expect("fixture parses"),
        );
        let spec = TunSpec::derive(&dual, &TunnelParams::new(SplitMode::All, vec![]));
        assert_eq!(
            spec.routes,
            vec!["0.0.0.0/0".to_string(), "::/0".to_string()]
        );
    }

    fn intent(params: TunnelParams) -> LastIntent {
        LastIntent::new(
            vec![Protocol::AmneziaWg, Protocol::WireGuard],
            params,
            1_700_000_000,
        )
    }

    #[test]
    fn the_tun_spec_carries_the_split_rules_the_way_the_builder_wants_them() {
        let all = TunSpec::derive(&config(), &TunnelParams::new(SplitMode::All, vec![]));
        assert_eq!(all.ipv4_addr, "10.0.0.2/32");
        assert_eq!(all.dns.as_deref(), Some("1.1.1.1, 8.8.8.8"));
        assert!(all.disallowed_apps.is_empty() && all.allowed_apps.is_empty());
        assert!(all.routes.contains(&"0.0.0.0/0".to_string()));

        let exclude = TunSpec::derive(
            &config(),
            &TunnelParams::new(SplitMode::Exclude, vec!["b".into(), "a".into()]),
        );
        assert_eq!(exclude.disallowed_apps, vec!["a", "b"]);
        assert!(exclude.allowed_apps.is_empty());

        let include = TunSpec::derive(
            &config(),
            &TunnelParams::new(SplitMode::Include, vec!["x".into()]),
        );
        assert_eq!(include.allowed_apps, vec!["x"]);
        assert!(include.disallowed_apps.is_empty());

        // A mode with no apps is "everything through the tunnel", whatever the mode says.
        let empty = TunSpec::derive(&config(), &TunnelParams::new(SplitMode::Include, vec![]));
        assert!(empty.allowed_apps.is_empty());
    }

    #[test]
    fn the_tun_spec_uses_the_names_kotlin_reads() {
        let spec = TunSpec::derive(
            &config(),
            &TunnelParams::new(SplitMode::Exclude, vec!["a".into()]),
        );
        let json = serde_json::to_value(&spec).unwrap();
        assert!(
            json.get("ipv4Addr").is_some(),
            "camelCase, as the Kotlin side reads it"
        );
        assert!(json.get("disallowedApps").is_some());
        // The plugin payload is the same thing with a generation on it.
        let mut with_generation = serde_json::to_value(spec.with_generation(7)).unwrap();
        assert_eq!(with_generation["generation"], 7);
        with_generation
            .as_object_mut()
            .unwrap()
            .remove("generation");
        assert_eq!(json, with_generation);
    }

    #[test]
    fn the_last_good_intent_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let params = TunnelParams::new(SplitMode::Exclude, vec!["org.example".into()]);
        save(dir.path(), &intent(params.clone())).unwrap();

        let loaded = load(dir.path()).expect("it is readable");
        assert_eq!(loaded.version, BUNDLE_VERSION);
        assert_eq!(loaded.order, vec![Protocol::AmneziaWg, Protocol::WireGuard]);
        assert_eq!(loaded.params, params);
        assert_eq!(loaded.saved_at, 1_700_000_000);

        // And it becomes exactly the request a system-issued start raises.
        match loaded.request().expect("an order means a request") {
            crate::actor::handle::IntentRequest::Up {
                order,
                params: rules,
            } => {
                assert_eq!(order, vec![Protocol::AmneziaWg, Protocol::WireGuard]);
                assert_eq!(rules, params);
            }
            other => panic!("expected an Up, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_order_is_not_a_request() {
        // The actor would refuse it, and a service that stops says so more clearly than a
        // refusal buried in a log.
        assert!(
            LastIntent::new(vec![], TunnelParams::default(), 0)
                .request()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn what_is_written_is_private_to_the_app() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = bundle_path(dir.path());
        // Written twice: the second write must not widen a file that already exists either.
        save(dir.path(), &intent(TunnelParams::default())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(dir.path(), &intent(TunnelParams::default())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_missing_file_and_a_removed_one_both_load_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
        remove(dir.path()); // nothing there is not an error

        save(dir.path(), &intent(TunnelParams::default())).unwrap();
        assert!(load(dir.path()).is_some());
        remove(dir.path());
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn a_file_of_another_version_is_ignored_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let mut json = serde_json::to_value(intent(TunnelParams::default())).unwrap();
        json["version"] = serde_json::Value::from(BUNDLE_VERSION + 1);
        std::fs::write(bundle_path(dir.path()), json.to_string()).unwrap();
        assert!(load(dir.path()).is_none());

        std::fs::write(bundle_path(dir.path()), "{not json").unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn service_generations_are_unique_per_process_and_never_zero() {
        let mut generations = ServiceGenerations::new();
        let first = generations.mint();
        let second = generations.mint();
        assert!(second > first, "monotonic within the process");
        assert_ne!(first, 0, "zero means 'nothing is being served'");
        assert!(i64::try_from(second).is_ok(), "survives as a Kotlin Long");
        // The base is a multiple of 2^20 and the low bits are the per-process counter, so two
        // processes only collide when their random bases do.
        assert_eq!(first & 0xF_FFFF, 1);
        assert_eq!(second & 0xF_FFFF, 2);
    }

    #[test]
    fn the_endpoint_cache_answers_for_a_host_it_has_seen() {
        let dir = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "203.0.113.7:51820".parse().unwrap();
        let mut known = std::collections::BTreeMap::new();
        known.insert("vpn.example.com:51820".to_string(), addr);
        std::fs::write(
            endpoints_path(dir.path()),
            serde_json::to_string(&known).unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_endpoints(dir.path()).get("vpn.example.com:51820"),
            Some(&addr)
        );
        assert!(!read_endpoints(dir.path()).contains_key("other:51820"));
        // A cache that does not parse is simply not a cache; nothing depends on it existing.
        std::fs::write(endpoints_path(dir.path()), "{not json").unwrap();
        assert!(read_endpoints(dir.path()).is_empty());
    }
}
