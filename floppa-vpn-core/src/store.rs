//! The config store, owned outright by the actor task.
//!
//! No lock: the actor is the only thing that touches it. That is not just tidiness — the previous
//! `RwLock<SavedVpnConfigs>` was written from Tauri commands *and* read mid-connect, so importing a
//! config could change which protocol an in-flight attempt was about to use.
//!
//! One deliberate split runs through this module: **storing a config and choosing a protocol are
//! different operations.** Importing writes a config and nothing else; `preferred` is written only
//! when a protocol has actually connected. Previously a single `active_protocol` string meant both
//! "which config the next connect picks" and "which protocol worked last", which forced the probe
//! loop to overwrite it before every attempt — and so a failed cycle left the *last failed*
//! protocol recorded as the preferred one.

use super::protocol::{Preference, Protocol};
use super::state::{ProtocolConfig, SavedVpnConfigs};
use crate::actor::types::{ConfigSummary, ConfigsView};
use crate::config as vpn_config;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigError {
    #[error("the config is empty")]
    Empty,
    #[error("could not parse the config: {detail}")]
    Unparseable { detail: String },
    /// A transport failure, not a config problem: nothing looked at the config at all.
    #[error("the tunnel actor is not running")]
    ActorGone,
}

/// What the persistence task is asked to write. Only the newest matters: a later save supersedes
/// an earlier one, and a delete supersedes any save before it.
#[derive(Debug)]
pub(crate) enum Write {
    Save(Box<SavedVpnConfigs>),
    Delete,
}

/// One request to the persistence task: what to write, if anything, and whom to tell once it —
/// and everything queued before it — has been written. A request with nothing to write is a
/// flush: it is answered when the queue ahead of it has drained.
#[derive(Debug)]
struct PersistOp {
    write: Option<Write>,
    done: Option<oneshot::Sender<()>>,
}

/// A write on its way to the keyring or the file. Await it to know it has arrived.
///
/// The actor answers "the configs are forgotten" through this rather than as soon as its in-memory
/// copy is empty: the previous, synchronous `clear()` meant "deleted" when it returned, and the
/// asynchronous one silently stopped meaning that — an app quit right after Forget left the keys
/// on disk.
#[derive(Debug)]
pub struct Persisted(Option<oneshot::Receiver<()>>);

impl Persisted {
    /// Resolves once the write is done — or once nothing more will happen to it, which is the
    /// same thing to a waiter: a persistence task that is gone has already been reported.
    pub async fn wait(self) {
        if let Some(rx) = self.0 {
            let _ = rx.await;
        }
    }
}

/// Writes configs to the OS keyring or the fallback file from a task of their own.
///
/// The keyring is synchronous and can block for as long as it takes the user to answer an unlock
/// dialog. Doing that on the actor task stalled every command and observation behind it — and a
/// stall longer than the staleness window read as the tunnel going dark. The actor keeps the
/// in-memory copy as the source of truth and only *sends* here; each write runs on a blocking
/// thread, and a burst of writes collapses to the last one. A sender that needs to know when its
/// write has landed gets a [`Persisted`] to wait on.
#[derive(Debug, Clone)]
pub struct Persister {
    /// `None` keeps everything in memory, for tests.
    tx: Option<mpsc::UnboundedSender<PersistOp>>,
}

impl Persister {
    /// Start the persistence task. Must be called from within a Tokio runtime.
    pub fn spawn() -> Self {
        Self::spawn_with(|write| match write {
            Write::Save(configs) => vpn_config::save_configs(&configs),
            Write::Delete => vpn_config::delete_configs(),
        })
    }

    /// Start the persistence task with `writer` doing the actual writes, on a blocking thread.
    pub(crate) fn spawn_with(writer: impl Fn(Write) + Send + Sync + 'static) -> Self {
        let writer = Arc::new(writer);
        let (tx, mut rx) = mpsc::unbounded_channel::<PersistOp>();
        tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut write = first.write;
                let mut waiting: Vec<oneshot::Sender<()>> = first.done.into_iter().collect();
                // Coalesce whatever queued up behind a slow write: only the newest state is
                // worth writing, and everyone who asked is answered once it is — the state they
                // asked about has been superseded by what is written, never lost.
                while let Ok(next) = rx.try_recv() {
                    if next.write.is_some() {
                        write = next.write;
                    }
                    waiting.extend(next.done);
                }
                if let Some(write) = write {
                    let writer = writer.clone();
                    let written = tokio::task::spawn_blocking(move || writer(write)).await;
                    if let Err(e) = written {
                        warn!("config persistence task failed: {e}");
                    }
                }
                for done in waiting {
                    let _ = done.send(());
                }
            }
        });
        Self { tx: Some(tx) }
    }

    /// Queue a write and forget about it.
    fn send(&self, write: Write) {
        self.enqueue(PersistOp {
            write: Some(write),
            done: None,
        });
    }

    /// Queue a write and get a [`Persisted`] that resolves once it has been written.
    fn send_acknowledged(&self, write: Write) -> Persisted {
        self.request(Some(write))
    }

    /// Resolves once everything queued so far has been written. With no persistence task (the
    /// in-memory store used by tests) it resolves at once.
    pub fn flush(&self) -> Persisted {
        self.request(None)
    }

    fn request(&self, write: Option<Write>) -> Persisted {
        let (done, rx) = oneshot::channel();
        let sent = self.enqueue(PersistOp {
            write,
            done: Some(done),
        });
        Persisted(sent.then_some(rx))
    }

    /// Whether the request reached the persistence task.
    fn enqueue(&self, op: PersistOp) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        if tx.send(op).is_err() {
            warn!("config persistence task is gone; the change stays in memory only");
            return false;
        }
        true
    }
}

#[derive(Debug)]
pub struct ConfigStore {
    configs: SavedVpnConfigs,
    persister: Persister,
}

impl ConfigStore {
    /// Load whatever is persisted, from a blocking thread. A missing or unreadable store is an
    /// empty one, never an error: the app must still start so the user can import a config.
    pub async fn load() -> Self {
        let configs = match tokio::task::spawn_blocking(vpn_config::load_configs).await {
            Ok(loaded) => loaded.unwrap_or_default(),
            Err(e) => {
                warn!("loading configs failed: {e}");
                SavedVpnConfigs::default()
            }
        };
        Self {
            configs,
            persister: Persister::spawn(),
        }
    }

    /// A store that never touches the keyring or the filesystem.
    #[cfg(test)]
    pub(crate) fn in_memory(configs: SavedVpnConfigs) -> Self {
        Self::with_persister(configs, Persister { tx: None })
    }

    /// A store writing through the given persister, for tests that fake the writes.
    #[cfg(test)]
    pub(crate) fn with_persister(configs: SavedVpnConfigs, persister: Persister) -> Self {
        Self { configs, persister }
    }

    /// Parse a config string and store it under its own protocol key.
    ///
    /// Deliberately does **not** change `preferred`: importing a config is not a statement that it
    /// works, and the previous behaviour of switching to whatever was imported last is what made
    /// server sync silently reorder the user's protocol.
    pub fn import(&mut self, raw: &str) -> Result<Protocol, ConfigError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::Empty);
        }

        let config = ProtocolConfig::parse(trimmed).map_err(|e| ConfigError::Unparseable {
            detail: e.to_string(),
        })?;
        let protocol = config.protocol();
        match config {
            ProtocolConfig::WireGuard(wg) => self.configs.wireguard = Some(wg),
            ProtocolConfig::AmneziaWg(awg) => self.configs.amneziawg = Some(awg),
            ProtocolConfig::Vless(vless) => self.configs.vless = Some(vless),
        }

        self.save();
        info!(%protocol, "stored config");
        Ok(protocol)
    }

    pub fn get(&self, protocol: Protocol) -> Option<ProtocolConfig> {
        self.configs.get(protocol)
    }

    /// The set of protocols with a stored config. Order is deterministic but carries no preference.
    pub fn available(&self) -> Vec<Protocol> {
        self.configs.available_protocols()
    }

    pub fn preferred(&self) -> Option<Protocol> {
        self.configs.preferred_protocol.0
    }

    /// Record that a protocol actually worked. The only caller is the success path.
    pub fn set_preferred(&mut self, protocol: Option<Protocol>) {
        if self.configs.preferred_protocol.0 == protocol {
            return;
        }
        self.configs.preferred_protocol = Preference(protocol);
        self.save();
    }

    /// The probe order actually usable right now: the caller's order narrowed to protocols we hold
    /// a config for, with the last known-good one moved to the front so a reconnect goes straight
    /// to what worked.
    ///
    /// The rest keeps the caller's relative order. A swap would have put whatever was first into
    /// the preferred protocol's old slot, demoting the caller's first choice behind everything
    /// that happened to sit between them.
    pub fn resolve_order(&self, requested: &[Protocol]) -> Vec<Protocol> {
        let mut order: Vec<Protocol> = Vec::with_capacity(requested.len());
        for p in requested {
            if self.get(*p).is_some() && !order.contains(p) {
                order.push(*p);
            }
        }

        if let Some(preferred) = self.preferred()
            && let Some(pos) = order.iter().position(|p| *p == preferred)
        {
            let preferred = order.remove(pos);
            order.insert(0, preferred);
        }
        order
    }

    pub fn view(&self) -> ConfigsView {
        ConfigsView {
            available: self.available(),
            preferred: self.preferred(),
            summaries: self
                .available()
                .into_iter()
                .filter_map(|p| self.get(p).map(|c| summarize(p, &c)))
                .collect(),
        }
    }

    /// Forget everything. The in-memory copy is empty on return; the returned [`Persisted`]
    /// resolves once the keyring or file is, too.
    #[must_use = "the wipe is only on disk once this resolves"]
    pub fn clear(&mut self) -> Persisted {
        self.configs = SavedVpnConfigs::default();
        self.persister.send_acknowledged(Write::Delete)
    }

    /// Resolves once every write queued so far has landed. Used on exit, so a Forget or an import
    /// issued just before quitting is not lost with the process.
    pub fn flush(&self) -> Persisted {
        self.persister.flush()
    }

    fn save(&self) {
        self.persister
            .send(Write::Save(Box::new(self.configs.clone())));
    }
}

fn summarize(protocol: Protocol, config: &ProtocolConfig) -> ConfigSummary {
    ConfigSummary {
        protocol,
        address: config.address(),
        server_endpoint: config.endpoint_str(),
        dns: config.dns_line(),
        allowed_ips: config.allowed_ips_line(),
        mtu: config.get_mtu(),
    }
}

/// A fake persistence backend: every write blocks on a gate the test opens, and is recorded.
#[cfg(test)]
pub(crate) mod testing {
    use super::{Persister, Write};
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Default)]
    pub(crate) struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        pub(crate) fn closed() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub(crate) fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }

        /// Block the calling (blocking-pool) thread until the gate is open.
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
        }
    }

    /// A persister whose writes wait at `gate` and are then appended to `writes`.
    pub(crate) fn gated_persister(
        gate: Arc<Gate>,
        writes: Arc<Mutex<Vec<&'static str>>>,
    ) -> Persister {
        Persister::spawn_with(move |write| {
            gate.wait();
            writes.lock().unwrap().push(match write {
                Write::Save(_) => "save",
                Write::Delete => "delete",
            });
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Gate, gated_persister};
    use super::*;
    use crate::state::{AwgConfig, VlessVpnConfig, WgConfig};
    use std::sync::Mutex;
    use std::time::Duration;

    const WG_CONFIG: &str = "\
[Interface]
PrivateKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Address = 10.0.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=
Endpoint = vpn.example.com:51820
AllowedIPs = 0.0.0.0/0
";

    const VLESS_URI: &str = "vless://0f7f6d3c-0a1c-4f1e-9d3a-1b2c3d4e5f60@vpn.example.com:443?security=reality&sni=example.com&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=0123abcd&flow=xtls-rprx-vision#floppa";

    fn store_with(configs: SavedVpnConfigs) -> ConfigStore {
        ConfigStore::in_memory(configs)
    }

    fn wg() -> WgConfig {
        WgConfig::from_config_str(WG_CONFIG).expect("fixture must parse")
    }

    #[test]
    fn an_empty_config_is_rejected_rather_than_stored() {
        let mut store = store_with(SavedVpnConfigs::default());
        assert_eq!(store.import("   "), Err(ConfigError::Empty));
        assert!(store.view().available.is_empty());
    }

    #[test]
    fn resolve_order_drops_protocols_we_have_no_config_for() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard, Protocol::Vless]),
            vec![Protocol::WireGuard]
        );
    }

    #[test]
    fn resolve_order_puts_the_last_working_protocol_first() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            preferred_protocol: Preference(Some(Protocol::WireGuard)),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard]),
            vec![Protocol::WireGuard, Protocol::AmneziaWg],
            "a reconnect should go straight to what worked"
        );
    }

    #[test]
    fn resolve_order_keeps_the_requested_order_when_nothing_is_preferred() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard]),
            vec![Protocol::AmneziaWg, Protocol::WireGuard]
        );
    }

    #[test]
    fn moving_the_preferred_protocol_first_keeps_the_rest_in_the_requested_order() {
        // A swap put the caller's first choice into the preferred protocol's old slot — behind
        // everything between them — so the fallback after the known-good protocol was wrong.
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            vless: Some(VlessVpnConfig::from_uri(VLESS_URI).expect("fixture must parse")),
            preferred_protocol: Preference(Some(Protocol::Vless)),
        });
        assert_eq!(
            store.resolve_order(&[Protocol::AmneziaWg, Protocol::WireGuard, Protocol::Vless]),
            vec![Protocol::Vless, Protocol::AmneziaWg, Protocol::WireGuard],
        );
    }

    #[test]
    fn a_protocol_requested_twice_is_probed_once() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            amneziawg: Some(AwgConfig {
                wg: wg(),
                obfuscation: Default::default(),
            }),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[
                Protocol::WireGuard,
                Protocol::AmneziaWg,
                Protocol::WireGuard
            ]),
            vec![Protocol::WireGuard, Protocol::AmneziaWg],
        );
    }

    #[test]
    fn a_preferred_protocol_we_no_longer_hold_does_not_resurface() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            preferred_protocol: Preference(Some(Protocol::Vless)),
            ..Default::default()
        });
        assert_eq!(
            store.resolve_order(&[Protocol::WireGuard]),
            vec![Protocol::WireGuard]
        );
    }

    #[test]
    fn the_view_reports_available_as_a_set_and_preferred_separately() {
        let store = store_with(SavedVpnConfigs {
            wireguard: Some(wg()),
            preferred_protocol: Preference(Some(Protocol::WireGuard)),
            ..Default::default()
        });
        let view = store.view();
        assert_eq!(view.available, vec![Protocol::WireGuard]);
        assert_eq!(view.preferred, Some(Protocol::WireGuard));
        assert_eq!(view.summaries.len(), 1);
        assert_eq!(view.summaries[0].server_endpoint, "vpn.example.com:51820");
    }

    #[tokio::test]
    async fn a_clear_is_acknowledged_only_once_the_delete_has_run() {
        let gate = Gate::closed();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut store = ConfigStore::with_persister(
            SavedVpnConfigs {
                wireguard: Some(wg()),
                ..Default::default()
            },
            gated_persister(gate.clone(), writes.clone()),
        );

        let mut cleared = std::pin::pin!(store.clear().wait());
        assert!(
            store.view().available.is_empty(),
            "gone from memory at once"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut cleared)
                .await
                .is_err(),
            "not acknowledged while the delete is still blocked"
        );
        assert!(writes.lock().unwrap().is_empty());

        gate.open();
        tokio::time::timeout(Duration::from_secs(5), cleared)
            .await
            .expect("acknowledged once the delete ran");
        assert_eq!(*writes.lock().unwrap(), vec!["delete"]);
    }

    #[tokio::test]
    async fn a_flush_resolves_once_everything_queued_before_it_is_written() {
        let gate = Gate::closed();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut store = ConfigStore::with_persister(
            SavedVpnConfigs::default(),
            gated_persister(gate.clone(), writes.clone()),
        );

        store.import(WG_CONFIG).expect("fixture must parse");
        store.set_preferred(Some(Protocol::WireGuard));
        let _ = store.clear();
        let mut flushed = std::pin::pin!(store.flush().wait());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut flushed)
                .await
                .is_err(),
            "nothing has been written yet"
        );

        gate.open();
        tokio::time::timeout(Duration::from_secs(5), flushed)
            .await
            .expect("the queue drained");
        // A burst collapses to its newest state, and the delete is what was newest.
        let writes = writes.lock().unwrap();
        assert_eq!(writes.last(), Some(&"delete"));
        assert!(writes.len() <= 3);
    }

    #[tokio::test]
    async fn the_in_memory_store_acknowledges_at_once() {
        let mut store = store_with(SavedVpnConfigs::default());
        tokio::time::timeout(Duration::from_secs(1), store.clear().wait())
            .await
            .expect("nothing to wait for");
        tokio::time::timeout(Duration::from_secs(1), store.flush().wait())
            .await
            .expect("nothing to wait for");
    }
}
