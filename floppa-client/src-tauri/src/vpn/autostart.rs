//! The last-good bundle: everything the `:vpn` process needs to bring a tunnel up on its own.
//!
//! Android starts the VPN service without the UI process for always-on VPN, at boot, and to
//! restore a lockdown ("block connections without VPN") session. The intent it sends carries no
//! configuration, so the service can only honour it from something written down earlier. That is
//! this file: after every successful connect the UI process writes `autostart.json`, and a system
//! start reads it back and rebuilds exactly that tunnel — the same protocol, the same split rules,
//! the same resolved endpoint.
//!
//! The bundle is rewritten on every successful connect (the last used protocol wins), removed
//! when the configs are forgotten, and *kept* on an explicit Disconnect: always-on means the OS
//! restarts the service, and whether that should happen is the system toggle's decision, not ours.
//!
//! Protection at rest is the same as `vpn-config.json`, which already holds the same private key:
//! a `0600` file in the app's private data directory. Only the calls that read and write the file
//! are Android-specific; the types and the derivations are plain data, so their tests run on the
//! host.

use super::actor::types::{SplitMode, TunnelParams};
use super::state::ProtocolConfig;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Bumped whenever the on-disk shape changes incompatibly. A bundle of another version is not
/// migrated, it is ignored: the next connect writes a fresh one, and an autonomous start that
/// finds nothing usable stops rather than guessing.
pub const BUNDLE_VERSION: u32 = 1;

pub const BUNDLE_FILENAME: &str = "autostart.json";

/// The persisted counter behind [`next_autonomous_epoch`].
const EPOCH_FILENAME: &str = "autostart.epoch";

/// Where autonomous epochs live. See [`next_autonomous_epoch`].
pub const AUTONOMOUS_EPOCH_BASE: u64 = 1 << 62;

/// What `VpnService.Builder` needs, derived from the config and the split rules once, at connect
/// time — so the service never parses a protocol config to build a TUN.
///
/// This is [`tauri_plugin_vpn::VpnConfig`] without the epoch, which is per start rather than per
/// bundle. The field names match it (camelCase), and the Kotlin side reads the same names out of
/// the JSON that it reads out of the start intent's extras.
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

        let mut spec = Self {
            ipv4_addr: config.address(),
            ipv6_addr: None,
            routes: floppa_tunnel_config::route::CATCH_ALL
                .iter()
                .map(ToString::to_string)
                .collect(),
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

    /// The plugin's start payload for one particular generation.
    pub fn with_generation(self, generation: u64) -> tauri_plugin_vpn::VpnConfig {
        tauri_plugin_vpn::VpnConfig {
            ipv4_addr: self.ipv4_addr,
            ipv6_addr: self.ipv6_addr,
            routes: self.routes,
            dns: self.dns,
            mtu: self.mtu,
            disallowed_apps: self.disallowed_apps,
            allowed_apps: self.allowed_apps,
            generation,
        }
    }
}

/// What an autonomous start rebuilds.
///
/// `config` is the persisted [`ProtocolConfig`] shape — adjacently tagged, JSON — rather than the
/// bincode-only `WireConfig`: this file is JSON, and the tag that names the protocol is what makes
/// the file self-describing. `endpoint` is the literal the connect resolved to, so a start under
/// lockdown, where nothing can be resolved because nothing is allowed on the network yet, still has
/// an address to dial.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutostartBundle {
    pub version: u32,
    pub tun: TunSpec,
    pub config: ProtocolConfig,
    pub endpoint: SocketAddr,
    pub params: TunnelParams,
    /// Unix seconds; informational.
    pub saved_at: i64,
}

impl AutostartBundle {
    pub fn new(
        config: ProtocolConfig,
        endpoint: SocketAddr,
        params: TunnelParams,
        saved_at: i64,
    ) -> Self {
        Self {
            version: BUNDLE_VERSION,
            tun: TunSpec::derive(&config, &params),
            config,
            endpoint,
            params,
            saved_at,
        }
    }

    pub fn protocol(&self) -> super::protocol::Protocol {
        self.config.protocol()
    }
}

fn bundle_path(dir: &Path) -> PathBuf {
    dir.join(BUNDLE_FILENAME)
}

/// Write the bundle, `0600`, replacing whatever was there.
pub fn save(dir: &Path, bundle: &AutostartBundle) -> Result<(), String> {
    let json = serde_json::to_string(bundle).map_err(|e| format!("serialize: {e}"))?;
    let path = bundle_path(dir);
    write_private(&path, json.as_bytes())?;
    info!(
        protocol = %bundle.protocol(),
        endpoint = %bundle.endpoint,
        "autostart bundle written to {}",
        path.display()
    );
    Ok(())
}

/// Read the bundle, if there is one this build can use.
///
/// Every reason for `None` is logged, because the caller's next step is to stop and the log line
/// is the only account of why.
pub fn load(dir: &Path) -> Option<AutostartBundle> {
    let path = bundle_path(dir);
    let json = match std::fs::read_to_string(&path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!("no autostart bundle at {}", path.display());
            return None;
        }
        Err(e) => {
            warn!(
                "failed to read the autostart bundle {}: {e}",
                path.display()
            );
            return None;
        }
    };
    let bundle: AutostartBundle = match serde_json::from_str(&json) {
        Ok(bundle) => bundle,
        Err(e) => {
            warn!(
                "the autostart bundle {} does not parse: {e}",
                path.display()
            );
            return None;
        }
    };
    if bundle.version != BUNDLE_VERSION {
        warn!(
            found = bundle.version,
            expected = BUNDLE_VERSION,
            "the autostart bundle is of another version; ignoring it"
        );
        return None;
    }
    Some(bundle)
}

/// Delete the bundle. A missing file is not an error.
pub fn remove(dir: &Path) {
    let path = bundle_path(dir);
    match std::fs::remove_file(&path) {
        Ok(()) => info!("autostart bundle removed: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "failed to remove the autostart bundle {}: {e}",
            path.display()
        ),
    }
}

/// Mint the epoch for an autonomous start.
///
/// The UI process mints its epochs from a counter that starts at 1 in every process, so an epoch
/// the service invents for itself must come from a range no UI intent can ever reach: everything
/// at or above [`AUTONOMOUS_EPOCH_BASE`]. Within that range a counter persisted next to the bundle
/// keeps consecutive autonomous starts distinct, which is what `nativeStop`'s generation check
/// needs when the system restarts the service several times inside one `:vpn` process. A counter
/// that cannot be persisted still moves forward for the life of the process.
pub fn next_autonomous_epoch(dir: &Path) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static FLOOR: AtomicU64 = AtomicU64::new(0);

    let path = dir.join(EPOCH_FILENAME);
    let stored = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let counter = stored.max(FLOOR.load(Ordering::SeqCst)) + 1;
    FLOOR.store(counter, Ordering::SeqCst);
    if let Err(e) = write_private(&path, counter.to_string().as_bytes()) {
        warn!("failed to persist the autostart epoch counter: {e}");
    }
    AUTONOMOUS_EPOCH_BASE + counter
}

/// Whether an epoch was minted by [`next_autonomous_epoch`] rather than by a UI intent.
pub const fn is_autonomous_epoch(epoch: u64) -> bool {
    epoch >= AUTONOMOUS_EPOCH_BASE
}

/// The identity of one Android service start, minted by the UI process.
///
/// Deliberately **not** the cycle's `IntentEpoch`. An intent's epoch is shared by every protocol
/// and every pass of one cycle, and it restarts at 1 in each UI process while the `:vpn` process
/// outlives the UI — so every "is this our generation?" check (`wait_for_service`,
/// `start_tunnel`, `nativeStop`, `closeGeneration`) could pass for a service instance we had
/// already moved on from. A generation is minted per *service start* instead: one value per
/// `vpn().start()`, never reused, and never equal to one another process minted.
///
/// Uniqueness across processes comes from a random 32-bit base rather than a persisted counter:
/// the value only ever has to be compared for equality, so a counter's ordering would buy
/// nothing, and a file read on the actor's task would buy a blocking call per attempt. Every
/// value is at most `(2^32 - 1) << 20 + n`, comfortably below [`AUTONOMOUS_EPOCH_BASE`], so a
/// UI generation can never be mistaken for a start the service made on its own.
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

    /// The next generation. Monotonic within the process.
    pub fn mint(&mut self) -> u64 {
        self.0 += 1;
        debug_assert!(!is_autonomous_epoch(self.0));
        self.0
    }
}

impl Default for ServiceGenerations {
    fn default() -> Self {
        Self::new()
    }
}

/// The address to dial for an autonomous start.
///
/// The service is started before any TUN exists, so this is the one moment it can still resolve
/// a name — and under lockdown it cannot, because nothing is allowed on the network until the VPN
/// is up. The literal the last connect resolved to is the fallback, bounded so a start is never
/// held up by a resolver that is not going to answer.
pub async fn resolve_endpoint(bundle: &AutostartBundle, budget: std::time::Duration) -> SocketAddr {
    let host = bundle.config.endpoint_str();
    let lookup = tokio::time::timeout(budget, tokio::net::lookup_host(&host)).await;
    match lookup {
        Ok(Ok(mut addrs)) => match addrs.next() {
            Some(addr) => {
                if addr != bundle.endpoint {
                    info!(%host, was = %bundle.endpoint, now = %addr, "the endpoint moved since the bundle was written");
                }
                addr
            }
            None => bundle.endpoint,
        },
        Ok(Err(e)) => {
            info!(%host, fallback = %bundle.endpoint, "could not resolve the endpoint ({e}); using the stored literal");
            bundle.endpoint
        }
        Err(_) => {
            info!(%host, fallback = %bundle.endpoint, "resolving the endpoint timed out; using the stored literal");
            bundle.endpoint
        }
    }
}

/// `std::fs::write` followed by `chmod 0600`, creating the file private from the start on Unix.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        // The mode above only applies to a file that is being created; an existing one keeps
        // whatever it had.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::protocol::Protocol;
    use crate::vpn::state::WgConfig;

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

    fn endpoint() -> SocketAddr {
        "203.0.113.7:51820".parse().unwrap()
    }

    fn bundle(params: TunnelParams) -> AutostartBundle {
        AutostartBundle::new(config(), endpoint(), params, 1_700_000_000)
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
    fn the_plugin_payload_uses_the_same_field_names_as_the_bundle() {
        // Kotlin reads one set of names out of the intent extras and out of the bundle's JSON.
        let spec = bundle(TunnelParams::new(SplitMode::Exclude, vec!["a".into()])).tun;
        let from_bundle = serde_json::to_value(&spec).unwrap();
        let mut from_plugin = serde_json::to_value(spec.with_generation(7)).unwrap();
        assert_eq!(from_plugin["generation"], 7);
        from_plugin.as_object_mut().unwrap().remove("generation");
        assert_eq!(from_bundle, from_plugin);
        assert!(
            from_bundle.get("ipv4Addr").is_some(),
            "camelCase, as the Kotlin side reads it"
        );
        assert!(from_bundle.get("disallowedApps").is_some());
    }

    #[test]
    fn a_bundle_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let params = TunnelParams::new(SplitMode::Exclude, vec!["org.example".into()]);
        save(dir.path(), &bundle(params.clone())).unwrap();

        let loaded = load(dir.path()).expect("the bundle is readable");
        assert_eq!(loaded.version, BUNDLE_VERSION);
        assert_eq!(loaded.protocol(), Protocol::WireGuard);
        assert_eq!(loaded.endpoint, endpoint());
        assert_eq!(loaded.params, params);
        assert_eq!(loaded.tun.disallowed_apps, vec!["org.example"]);
        assert_eq!(loaded.config.endpoint_str(), "vpn.example.com:51820");
        assert_eq!(loaded.saved_at, 1_700_000_000);
    }

    #[cfg(unix)]
    #[test]
    fn the_bundle_is_private_to_the_app() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = bundle_path(dir.path());
        // Written twice: the second write must not widen a file that already exists either.
        save(dir.path(), &bundle(TunnelParams::default())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(dir.path(), &bundle(TunnelParams::default())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_missing_bundle_and_a_removed_one_both_load_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
        remove(dir.path()); // nothing there is not an error

        save(dir.path(), &bundle(TunnelParams::default())).unwrap();
        assert!(load(dir.path()).is_some());
        remove(dir.path());
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn a_bundle_of_another_version_is_ignored_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let mut json = serde_json::to_value(bundle(TunnelParams::default())).unwrap();
        json["version"] = serde_json::Value::from(BUNDLE_VERSION + 1);
        std::fs::write(bundle_path(dir.path()), json.to_string()).unwrap();
        assert!(load(dir.path()).is_none());

        std::fs::write(bundle_path(dir.path()), "{not json").unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn autonomous_epochs_are_out_of_the_ui_range_and_never_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let first = next_autonomous_epoch(dir.path());
        let second = next_autonomous_epoch(dir.path());
        assert!(is_autonomous_epoch(first));
        assert!(second > first, "the persisted counter moves forward");
        // Every epoch a UI process can mint starts at 1 and counts up; none of them is in the
        // reserved range.
        assert!(!is_autonomous_epoch(1));
        assert!(!is_autonomous_epoch(u32::MAX as u64));
        // The value survives as a Kotlin Long: bit 63 is never set.
        assert!(i64::try_from(second).is_ok());
    }

    #[test]
    fn service_generations_are_unique_per_process_and_out_of_the_autonomous_range() {
        let mut generations = ServiceGenerations::new();
        let first = generations.mint();
        let second = generations.mint();
        assert!(second > first, "monotonic within the process");
        assert!(
            !is_autonomous_epoch(second),
            "never mistakable for a bundle start"
        );
        assert!(i64::try_from(second).is_ok(), "survives as a Kotlin Long");
        // The base is a multiple of 2^20 and the low bits are the per-process counter, so two
        // processes only collide when their random bases do.
        assert_eq!(first & 0xF_FFFF, 1);
        assert_eq!(second & 0xF_FFFF, 2);
    }

    #[test]
    fn the_epoch_counter_is_persisted_across_processes() {
        // Simulated by a fresh directory whose counter file says a higher number than this
        // process has handed out.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(EPOCH_FILENAME), "41").unwrap();
        let epoch = next_autonomous_epoch(dir.path());
        assert!(epoch >= AUTONOMOUS_EPOCH_BASE + 42);
        let stored: u64 = std::fs::read_to_string(dir.path().join(EPOCH_FILENAME))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(AUTONOMOUS_EPOCH_BASE + stored, epoch);
    }

    #[tokio::test]
    async fn a_literal_endpoint_needs_no_resolver() {
        let literal = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = 198.51.100.9:51820
AllowedIPs = 0.0.0.0/0
";
        let config = ProtocolConfig::WireGuard(WgConfig::from_config_str(literal).unwrap());
        let bundle = AutostartBundle::new(config, endpoint(), TunnelParams::default(), 0);
        let resolved = resolve_endpoint(&bundle, std::time::Duration::from_secs(2)).await;
        assert_eq!(resolved, "198.51.100.9:51820".parse().unwrap());
    }

    #[tokio::test]
    async fn an_unresolvable_name_falls_back_to_the_stored_literal() {
        let unresolvable = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = floppa.invalid:51820
AllowedIPs = 0.0.0.0/0
";
        let config = ProtocolConfig::WireGuard(WgConfig::from_config_str(unresolvable).unwrap());
        let bundle = AutostartBundle::new(config, endpoint(), TunnelParams::default(), 0);
        let resolved = resolve_endpoint(&bundle, std::time::Duration::from_millis(1500)).await;
        assert_eq!(resolved, endpoint());
    }
}
