//! The wire between a process that holds the tunnel actor and one that only talks to it.
//!
//! On Android that is `:vpn` and the UI. It is `#[cfg(unix)]` rather than Android-only so the
//! round-trip tests run on the host, and so a later desktop split — a privileged helper and a UI —
//! uses this rather than a second copy of it.
//!
//! # Rule: every type on this wire round-trips through the wire's own codec
//!
//! The transport used to be bincode, which is not self-describing: any serde shape that needs the
//! format to describe itself — `deserialize_with` that reads a different shape than
//! `serialize_with` writes, `#[serde(untagged)]`, `#[serde(tag = …)]` (internally or adjacently
//! tagged enums), `#[serde(flatten)]`, `deserialize_any` — encoded fine and failed to *decode*
//! inside the framed transport, where it surfaced as "the connection to the server was already
//! shutdown" rather than as a decode error anyone could catch. That shipped once, as the
//! AmneziaWG `I` slots, and broke every AmneziaWG connect on device.
//!
//! The transport is now JSON (`tokio_serde::formats::Json`), which is self-describing, so that
//! whole class is gone. It had to go: this wire now carries the actor's own vocabulary —
//! `TunnelState`, and through it `CycleOutcome`, `AttemptError`, `BackendError`, `IntentError`,
//! every one of them an internally tagged enum — and mirroring all of that into bincode-safe
//! shapes would have been a tax on every future field.
//!
//! The rule the class of bug earned keeps its force, generalised: **every argument and return type
//! of every `VpnRpc` method, in every variant, round-trips through the codec that is actually on
//! the wire**, in `tests::wire_coverage` below. Adding a method or a field means adding it there.
//! JSON has one hazard of its own and the tests pin it: `serde_json` writes a non-finite `f64` as
//! `null` and then refuses to read it back.

use crate::actor::handle::IntentRequest;
use crate::actor::types::{CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelState};
use crate::protocol::Protocol;
use crate::store::ConfigError;

/// The version of everything else in this module.
///
/// Bump it whenever a peer built against the old shape would get something wrong: a method the
/// caller now needs and the old build does not have, a field whose meaning changed, an enum
/// variant an old build would fail to match.
///
/// Reordering methods is *not* such a change, and neither is adding one that nobody has to call.
/// The transport is JSON and tarpc's request enum is externally tagged, so what is on the wire is
/// the method's name: an old server meeting a new method answers
/// `unknown variant `SetSession`, expected one of …` and drops the connection, which the client
/// sees as "the connection to the server was already shutdown". Clear enough once you know, and
/// exactly why the version is stated up front instead — so the mismatch is named before a call
/// fails in a way that describes nothing.
///
/// One is the shape that shipped in 0.6.x, before the version was carried at all. Two added
/// [`VpnRpc::set_session`], which a desktop client does have to call. Three added
/// [`VpnRpc::resume`].
pub const PROTOCOL_VERSION: u32 = 3;

/// Why a session could not be handed over.
///
/// Typed rather than a sentence, because the two cases are answered differently: one means this
/// peer is not the kind that keeps sessions and never will be, the other means a write failed and
/// trying again may work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionError {
    /// The process holding the actor does not keep a session on anyone's behalf. That is the
    /// Android case: both processes are one uid and share the file directly, so there is nothing
    /// for this call to do and pretending otherwise would hide a caller doing the wrong thing.
    #[error("this peer does not keep a session for its clients")]
    NotKept,
    #[error("the session could not be stored: {detail}")]
    Failed { detail: String },
}

/// A published state, and which run of the actor published it.
///
/// The `boot` is what makes the sequence numbers mean anything across a restart. The actor stamps
/// every publish with a `seq` that only ever grows *within one run*, and the process holding it is
/// a service the system stops and starts — so a mirror holding seq 57 from a run that has died
/// would reject everything a fresh run publishes from seq 1, and freeze at a state that is no
/// longer true, exactly when the process came back. Comparing the run first turns that into an
/// adoption rather than a stall.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Published {
    pub boot: u64,
    pub state: TunnelState,
    /// What the answering build speaks. Carried on the answer every client asks for first, so no
    /// method has to exist for it — adding one would itself be a wire change, and an old peer
    /// would fail it in a way that says nothing about why.
    ///
    /// `#[serde(default)]` so a 0.6.x server, which sends no such field, reads back as 0 rather
    /// than as a decode failure. Zero is "older than versioning", which is a mismatch like any
    /// other, only more specific.
    #[serde(default)]
    pub protocol: u32,
}

/// How long the server holds a [`VpnRpc::state_since`] call open waiting for something to change.
///
/// A long poll rather than a subscription: tarpc is request/response, and this maps a `watch`
/// onto it exactly — ask for anything newer than what you have, and be told the moment there is.
/// Bounded so a client that has gone away is noticed, and so an idle connection still proves
/// itself alive from time to time.
pub const STATE_HOLD: std::time::Duration = std::time::Duration::from_secs(20);

/// The client's per-call deadline for a long poll: comfortably past the server's hold.
///
/// The two have to be ordered, and this is the trap the first cross-process call fell into once
/// already — tarpc's default deadline is 10 seconds, and a call held open longer than the caller
/// is willing to wait fails on a healthy connection, every time.
pub const STATE_POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The IPC socket name. Keep in sync with `FloppaVpnService.kt`.
///
/// On Android the two ends cannot disagree: they ship in the same APK, installing one replaces the
/// other, and installing force-stops every process of the package, so two builds are never live at
/// once. That is why this wire was allowed to change freely, in any way at all.
///
/// A desktop system service breaks that guarantee, and quietly. Upgrading the package replaces the
/// binaries on disk while the old one keeps running until something restarts it, so a new client
/// talking to a service nobody has restarted is the *ordinary* outcome of an update, not an
/// unlikely one. Hence [`PROTOCOL_VERSION`], which is stated on every answer to `state_since` —
/// the first call every client makes.
pub const SOCKET_NAME: &str = "vpn.sock";

/// Where the system service listens, when there is one.
///
/// Under `/run` rather than beside the state: it is a runtime object that must not survive a
/// reboot, and systemd creates the directory through `RuntimeDirectory=`. A client decides which
/// mode it is in by trying to connect here — by *connecting*, not by looking for the file, since
/// a socket left behind by a process that died refuses connections while still existing.
#[cfg(target_os = "linux")]
pub const SYSTEM_SOCKET_DIR: &str = "/run/floppa-vpn";

/// Where the system service keeps everything it persists: the config store, the rollback journal
/// and the server session.
///
/// Root-owned, `0700`. The service has no user session and therefore no keyring, and a VPN that
/// must come up before anyone logs in cannot depend on one — the same reason `wg-quick` keeps its
/// configs in `/etc/wireguard` rather than in anybody's.
#[cfg(target_os = "linux")]
pub const SYSTEM_STATE_DIR: &str = "/var/lib/floppa-vpn";

/// The actor's boundary, spelled for a socket.
///
/// One method per operation of [`TunnelControl`](crate::actor::handle::TunnelControl), plus
/// the state stream expressed as a long poll and the three log calls that are about the process
/// rather than the tunnel. Nothing here is about *tunnels* any more: the process on the other end
/// owns the tunnel outright, and what crosses is intent in one direction and published state in
/// the other.
///
/// A `Command` is not sent over the wire and cannot be — it carries the `oneshot` senders the
/// actor replies through. Typed methods are the honest spelling.
#[tarpc::service]
pub trait VpnRpc {
    /// The first published state newer than what the caller holds.
    ///
    /// `boot` and `seq` are what the caller last saw. A `boot` that is not this run's is answered
    /// on the spot, whatever the `seq`: everything the caller knows came from a run that is over.
    /// Otherwise this returns immediately when something newer already exists, and holds the call
    /// open until it does or [`STATE_HOLD`] elapses — at which point the current state comes back
    /// unchanged and the caller asks again. That is a `watch` over a request/response transport:
    /// the caller always knows what it last saw, so nothing is missed and nothing is replayed.
    async fn state_since(boot: u64, seq: u64) -> Published;

    async fn set_intent(intent: IntentRequest) -> Result<IntentAccepted, IntentError>;

    /// Wait for an epoch's cycle to finish.
    ///
    /// Held open for as long as the cycle takes — and a cycle is **not** bounded by the actor's
    /// budgets any more. One parked on a device with no network waits, spending nothing, for as
    /// long as the outage lasts. So the caller's deadline expiring says nothing about the actor:
    /// it comes back as [`IntentError::CycleStillRunning`], which is not a failure of anything.
    async fn await_cycle(epoch: IntentEpoch) -> Result<CycleOutcome, IntentError>;

    async fn import_config(raw: String) -> Result<Protocol, ConfigError>;

    async fn clear_configs() -> Result<(), IntentError>;

    async fn forget_preferred() -> Result<(), IntentError>;

    /// Resolves once the actor has nothing in flight.
    async fn await_quiescent();

    /// Resolves once every config write queued so far has landed.
    async fn flush_configs();

    /// Apply a new log configuration in the VPN process.
    async fn set_log_config(config: crate::logging::LogConfig);

    /// Start writing VPN process logs into a diagnostic capture.
    async fn start_log_capture(capture_id: String);

    /// Stop writing VPN process logs into a diagnostic capture.
    async fn stop_log_capture();

    /// Hand over — or take away — the credentials this device talks to the server with.
    ///
    /// `None` is a sign-out and removes what was stored. A signed-out device must not be able to
    /// make peers, and the process that would make them is this one.
    ///
    /// # Why the payload is opaque
    ///
    /// It is `floppa_provision::ServerSession` as JSON, and this crate deliberately does not know
    /// that. The session describes a *server relationship* — a base URL, a bearer token, which
    /// device we are to it — and `floppa-vpn-core` runs tunnels and knows nothing about servers;
    /// naming the type here would mean this crate depending on the one that talks to them, which
    /// is the layering `floppa-provision` exists to avoid. What the process holding the actor does
    /// with this is keep it safe at rest on behalf of a client that cannot: the client is an
    /// unprivileged program, the store is root-owned, and at boot there is no user logged in to
    /// ask. Custodian, not reader.
    ///
    /// Not needed on Android, where the UI and `:vpn` are one uid and share the file directly —
    /// there this answers [`SessionError::NotKept`].
    async fn set_session(session: Option<String>) -> Result<(), SessionError>;

    /// Raise the tunnel that last connected, if there is one recorded.
    ///
    /// `None` means nothing has ever connected here, which is not a failure — it is a machine that
    /// has been asked to bring back a tunnel it never had, and the honest answer is that there is
    /// nothing to bring back.
    ///
    /// What this exists for is a start with nobody watching. The caller cannot name the tunnel
    /// itself: the record of what last worked lives beside the configs, in the state directory,
    /// which only the process holding the actor can read. So the request is "whatever you had",
    /// and the process that knows answers it.
    async fn resume() -> Result<Option<IntentAccepted>, IntentError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::types::{
        AttemptError, AttemptFailure, AttemptProgress, ConfigSummary, ConfigsView, IntentView,
        Link, Phase, RetryProgress, SplitMode, SystemVpnMode, TrafficStats, TunnelParams,
    };
    use crate::logging::{LogConfig, LogProfile};
    use crate::state::SpeedTracker;

    /// Through the codec that is actually on the wire — `tokio_serde::formats::Json` is
    /// `serde_json` over the framed bytes — rather than through "serde works in general", which
    /// is what let a shape that only JSON forgives ship on a transport that did not.
    fn roundtrip<T>(value: &T) -> Result<T, String>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let bytes = serde_json::to_vec(value).map_err(|e| format!("encode: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("decode: {e}"))
    }

    /// Every argument and return type of every `VpnRpc` method, in every variant that can be
    /// constructed, through the wire's codec. Ordered by the trait: state_since, set_intent,
    /// await_cycle, import_config, clear_configs, forget_preferred, await_quiescent, flush_configs,
    /// set_log_config, start_log_capture, stop_log_capture.
    mod wire_coverage {
        use super::*;

        fn survives<T>(what: &str, value: &T) -> T
        where
            T: serde::Serialize + serde::de::DeserializeOwned,
        {
            roundtrip(value).unwrap_or_else(|e| panic!("{what} must survive the wire: {e}"))
        }

        fn params() -> Vec<TunnelParams> {
            vec![
                TunnelParams::default(),
                TunnelParams::new(SplitMode::All, vec![]),
                TunnelParams::new(SplitMode::Exclude, vec!["org.example.a".into()]),
                TunnelParams::new(
                    SplitMode::Include,
                    vec!["org.example.a".into(), "org.example.b".into()],
                ),
            ]
        }

        fn outcomes() -> Vec<CycleOutcome> {
            vec![
                CycleOutcome::Connected {
                    protocol: Protocol::AmneziaWg,
                    adopted: false,
                    failures: vec![],
                },
                CycleOutcome::Connected {
                    protocol: Protocol::WireGuard,
                    adopted: true,
                    failures: vec![AttemptFailure {
                        protocol: Protocol::AmneziaWg,
                        error: AttemptError::VerifyFailed,
                        pass: 0,
                    }],
                },
                CycleOutcome::Exhausted { failures: vec![] },
                CycleOutcome::Exhausted {
                    failures: vec![
                        AttemptFailure {
                            protocol: Protocol::AmneziaWg,
                            error: AttemptError::PermissionDenied,
                            pass: 0,
                        },
                        AttemptFailure {
                            protocol: Protocol::Vless,
                            error: AttemptError::ResolveFailed {
                                host: "vpn.example:443".into(),
                                detail: "no addresses".into(),
                            },
                            pass: 1,
                        },
                        AttemptFailure {
                            protocol: Protocol::WireGuard,
                            error: AttemptError::Backend {
                                error: crate::backend::BackendError::PermissionDenied {
                                    detail: "SO_MARK".into(),
                                },
                            },
                            pass: 2,
                        },
                        AttemptFailure {
                            protocol: Protocol::WireGuard,
                            error: AttemptError::Platform {
                                step: crate::rollback::StepKind::Dns,
                                detail: "resolvectl".into(),
                            },
                            pass: 2,
                        },
                    ],
                },
                CycleOutcome::LostGaveUp {
                    protocol: Protocol::Vless,
                    passes: 3,
                },
                CycleOutcome::UnwindFailed,
                CycleOutcome::Cancelled,
                CycleOutcome::Down,
            ]
        }

        /// The one big one: everything the UI renders travels inside this.
        fn states() -> Vec<TunnelState> {
            let mut connected = TunnelState::initial();
            connected.seq = 41;
            connected.phase = Phase::Connected;
            connected.busy = Phase::Connected.is_busy();
            connected.cancellable = Phase::Connected.is_cancellable();
            connected.intent = IntentView::Up;
            connected.epoch = IntentEpoch(7);
            connected.intent_order = vec![Protocol::AmneziaWg, Protocol::WireGuard];
            // Deliberately not the same rules as `params`: the two travel separately and a codec
            // that confused them would round-trip a state that says the tunnel already routes what
            // the settings ask for.
            connected.intent_params = Some(TunnelParams::new(
                SplitMode::Include,
                vec!["org.example.other".into()],
            ));
            connected.protocol = Some(Protocol::AmneziaWg);
            connected.params = Some(TunnelParams::new(
                SplitMode::Exclude,
                vec!["org.example".into()],
            ));
            connected.adopted = true;
            connected.server_endpoint = Some("203.0.113.7:51820".into());
            connected.assigned_ip = Some("10.0.0.2/32".into());
            connected.connected_at = Some(1_700_000_000);
            connected.last_packet_received = Some(3);
            connected.stats = TrafficStats {
                tx_bytes: 1,
                rx_bytes: 2,
                tx_bytes_per_sec: 1024.5,
                rx_bytes_per_sec: 0.0,
            };
            connected.last_outcome = Some(outcomes()[1].clone());
            connected.configs = ConfigsView {
                available: vec![Protocol::AmneziaWg, Protocol::Vless],
                preferred: Some(Protocol::AmneziaWg),
                summaries: vec![ConfigSummary {
                    protocol: Protocol::AmneziaWg,
                    address: "10.0.0.2/32".into(),
                    server_endpoint: "vpn.example:51820".into(),
                    dns: Some("1.1.1.1".into()),
                    allowed_ips: "0.0.0.0/0".into(),
                    mtu: 1420,
                }],
            };
            connected.backend_reachable = true;
            // Connected with the network gone: the tunnel is intact and the phone is in a lift.
            // One of the two combinations the link exists to express, and unrepresentable as a
            // phase — which is why it has to survive the wire as its own field.
            connected.link = Link::Offline;
            // The mode a person most needs told, on the state that most needs telling them.
            connected.vpn_mode = SystemVpnMode::Lockdown;

            let mut connecting = TunnelState::initial();
            connecting.phase = Phase::Connecting;
            connecting.attempt = Some(AttemptProgress {
                protocol: Protocol::Vless,
                index: 2,
                total: 3,
            });

            let mut retrying = TunnelState::initial();
            retrying.phase = Phase::Retrying;
            retrying.retry = Some(RetryProgress {
                pass: 2,
                max: 3,
                resume_in_ms: 4_000,
            });
            retrying.last_outcome = Some(outcomes()[3].clone());

            // The other one: a cycle parked because there is no network to spend a pass on.
            let mut parked = TunnelState::initial();
            parked.phase = Phase::Retrying;
            parked.link = Link::Offline;
            parked.retry = Some(RetryProgress {
                pass: 1,
                max: 3,
                resume_in_ms: 0,
            });

            let mut online = TunnelState::initial();
            online.link = Link::Online;
            online.vpn_mode = SystemVpnMode::AlwaysOn;

            // The fourth mode, so every variant crosses: `Off` is a definite answer and must not
            // be confused with `Unknown`, which `TunnelState::initial` already carries.
            let mut plain = TunnelState::initial();
            plain.vpn_mode = SystemVpnMode::Off;

            vec![
                TunnelState::initial(),
                connected,
                connecting,
                retrying,
                parked,
                online,
                plain,
            ]
        }

        #[test]
        fn the_published_state_every_shape() {
            for (i, state) in states().iter().enumerate() {
                let published = Published {
                    boot: u64::MAX,
                    state: state.clone(),
                    protocol: PROTOCOL_VERSION,
                };
                assert_eq!(
                    &survives(&format!("Published #{i}"), &published),
                    &published
                );
            }
            // state_since's arguments: the run the caller is following, and how far into it.
            assert_eq!(survives("boot", &u64::MAX), u64::MAX);
            assert_eq!(survives("seq", &u64::MAX), u64::MAX);
        }

        /// The version has to survive meeting a build that predates it, because that is the only
        /// situation it exists for. A 0.6.x server sends `{boot, state}` and nothing else, and a
        /// client that failed to decode that would report "no service" instead of "the wrong one".
        #[test]
        fn an_answer_from_before_versioning_reads_as_version_zero() {
            let json = serde_json::json!({
                "boot": 7u64,
                "state": TunnelState::initial(),
            });
            let published: Published =
                serde_json::from_value(json).expect("a pre-versioning answer still decodes");
            assert_eq!(published.boot, 7);
            assert_eq!(
                published.protocol, 0,
                "absent means older than versioning, which is a mismatch like any other"
            );
            assert_ne!(published.protocol, PROTOCOL_VERSION);
        }

        #[test]
        fn every_cycle_outcome() {
            for (i, outcome) in outcomes().iter().enumerate() {
                assert_eq!(&survives(&format!("CycleOutcome #{i}"), outcome), outcome);
            }
        }

        #[test]
        fn set_intent_every_argument_and_result() {
            let requests = [
                IntentRequest::Down,
                IntentRequest::Up {
                    order: vec![],
                    params: TunnelParams::default(),
                },
            ]
            .into_iter()
            .chain(params().into_iter().map(|params| IntentRequest::Up {
                order: vec![Protocol::AmneziaWg, Protocol::WireGuard, Protocol::Vless],
                params,
            }))
            .collect::<Vec<_>>();
            for (i, request) in requests.iter().enumerate() {
                assert_eq!(&survives(&format!("IntentRequest #{i}"), request), request);
            }

            let accepted: Result<IntentAccepted, IntentError> = Ok(IntentAccepted {
                epoch: IntentEpoch(9),
            });
            assert_eq!(survives("IntentAccepted", &accepted), accepted);
            for error in [
                IntentError::EmptyOrder,
                IntentError::NoUsableConfig,
                IntentError::ActorGone,
                IntentError::SettleTimeout,
                IntentError::CycleStillRunning,
            ] {
                let refused: Result<IntentAccepted, IntentError> = Err(error.clone());
                assert_eq!(survives("IntentError", &refused), refused);
            }
        }

        #[test]
        fn await_cycle_every_argument_and_result() {
            assert_eq!(survives("IntentEpoch", &IntentEpoch(3)), IntentEpoch(3));
            for outcome in outcomes() {
                let answer: Result<CycleOutcome, IntentError> = Ok(outcome);
                assert_eq!(survives("await_cycle Ok", &answer), answer);
            }
            let gone: Result<CycleOutcome, IntentError> = Err(IntentError::ActorGone);
            assert_eq!(survives("await_cycle Err", &gone), gone);
        }

        #[test]
        fn import_config_every_argument_and_result() {
            assert_eq!(
                survives("raw", &"[Interface]\n".to_string()),
                "[Interface]\n"
            );
            for protocol in [Protocol::WireGuard, Protocol::AmneziaWg, Protocol::Vless] {
                let imported: Result<Protocol, ConfigError> = Ok(protocol);
                assert_eq!(survives("import_config Ok", &imported), imported);
            }
            for error in [
                ConfigError::Empty,
                ConfigError::ActorGone,
                ConfigError::Unparseable {
                    detail: "line 3: expected `=`".into(),
                },
            ] {
                let refused: Result<Protocol, ConfigError> = Err(error.clone());
                assert_eq!(survives("ConfigError", &refused), refused);
            }
        }

        #[test]
        fn the_calls_that_answer_with_nothing_or_with_a_unit_result() {
            // clear_configs, forget_preferred
            let ok: Result<(), IntentError> = Ok(());
            assert_eq!(survives("Result<(), IntentError> Ok", &ok), ok);
            let err: Result<(), IntentError> = Err(IntentError::SettleTimeout);
            assert_eq!(survives("Result<(), IntentError> Err", &err), err);
            // await_quiescent, flush_configs, stop_log_capture
            survives("unit", &());
        }

        #[test]
        fn set_log_config_and_the_capture_id() {
            let shapes = [
                LogConfig::default(),
                LogConfig {
                    profile: LogProfile::Verbose,
                    custom_filter: None,
                    custom_filter_enabled: false,
                },
                LogConfig {
                    profile: LogProfile::Normal,
                    custom_filter: Some("floppa_client_lib=trace".into()),
                    custom_filter_enabled: true,
                },
            ];
            for (i, config) in shapes.iter().enumerate() {
                let back = survives(&format!("LogConfig #{i}"), config);
                assert_eq!(back.profile, config.profile);
                assert_eq!(back.custom_filter, config.custom_filter);
            }
            assert_eq!(
                survives("capture_id", &"2026-08-25T12-00-00Z".to_string()),
                "2026-08-25T12-00-00Z"
            );
        }

        /// `resume` answers with an option, and the `None` is load-bearing: "nothing has ever
        /// connected here" is an answer, not a failure, and a codec that lost the difference would
        /// turn a machine with no history into one reporting an error at every boot.
        #[test]
        fn resuming_when_there_is_nothing_to_resume() {
            let nothing: Result<Option<IntentAccepted>, IntentError> = Ok(None);
            assert_eq!(
                survives("Result<Option<IntentAccepted>, _> None", &nothing),
                nothing
            );

            let raised: Result<Option<IntentAccepted>, IntentError> = Ok(Some(IntentAccepted {
                epoch: IntentEpoch(7),
            }));
            assert_eq!(
                survives("Result<Option<IntentAccepted>, _> Some", &raised),
                raised
            );

            let refused: Result<Option<IntentAccepted>, IntentError> = Err(IntentError::ActorGone);
            assert_eq!(
                survives("Result<Option<IntentAccepted>, _> Err", &refused),
                refused
            );
        }

        #[test]
        fn the_session_handed_over_and_every_way_it_can_be_refused() {
            // The payload is opaque here on purpose, so what has to survive is a string, an
            // absent one — which is a sign-out and must not decode as an empty session — and the
            // two answers.
            let handed: Option<String> = Some(r#"{"version":1,"token":"a.b.c"}"#.into());
            assert_eq!(survives("Option<String> Some", &handed), handed);
            let cleared: Option<String> = None;
            assert_eq!(survives("Option<String> None", &cleared), cleared);

            let ok: Result<(), SessionError> = Ok(());
            assert_eq!(survives("Result<(), SessionError> Ok", &ok), ok);
            for refused in [
                SessionError::NotKept,
                SessionError::Failed {
                    detail: "no space left on device".into(),
                },
            ] {
                let err: Result<(), SessionError> = Err(refused.clone());
                assert_eq!(survives("Result<(), SessionError> Err", &err), err);
            }
        }
    }

    /// JSON's own hazard, and the only one this transport has: `serde_json` writes a non-finite
    /// `f64` as `null` and then refuses to read it back. The speed rates are the only floats that
    /// cross, and `SpeedTracker` never divides by an interval under 100 ms — so they are finite by
    /// construction, and this fails if that ever stops being true.
    #[test]
    fn the_only_floats_on_the_wire_are_finite() {
        let mut speed = SpeedTracker::new();
        // The first sample is the baseline; the second is computed over a near-zero interval,
        // which is exactly where a division would produce an infinity.
        speed.update(0, 0);
        let (tx, rx) = speed.update(u64::MAX, u64::MAX);
        assert!(tx.is_finite() && rx.is_finite(), "{tx} {rx}");

        let encoded = serde_json::to_string(&[f64::INFINITY]).unwrap();
        assert!(
            serde_json::from_str::<[f64; 1]>(&encoded).is_err(),
            "if this ever passes, JSON has learned about infinities and the guard above can go"
        );
    }
}
