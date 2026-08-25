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

use crate::vpn::actor::handle::IntentRequest;
use crate::vpn::actor::types::{
    CycleOutcome, IntentAccepted, IntentEpoch, IntentError, TunnelState,
};
use crate::vpn::protocol::Protocol;
use crate::vpn::store::ConfigError;

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
/// This never needs versioning, and the wire format never needs to stay backward compatible.
/// Both ends ship in the same APK and are always the same build: installing one replaces the
/// other, and installing force-stops every process of the package, so two builds cannot be live
/// at once. Change the format freely — including the method set, which shifts tarpc's dispatch
/// indices.
pub const SOCKET_NAME: &str = "vpn.sock";

/// The actor's boundary, spelled for a socket.
///
/// One method per operation of [`TunnelControl`](crate::vpn::actor::handle::TunnelControl), plus
/// the state stream expressed as a long poll and the three log calls that are about the process
/// rather than the tunnel. Nothing here is about *tunnels* any more: the process on the other end
/// owns the tunnel outright, and what crosses is intent in one direction and published state in
/// the other.
///
/// A `Command` is not sent over the wire and cannot be — it carries the `oneshot` senders the
/// actor replies through. Typed methods are the honest spelling.
#[cfg(unix)]
#[tarpc::service]
pub trait VpnRpc {
    /// The state as of now. Used once, to seed the mirror before the long polls take over.
    async fn state() -> TunnelState;

    /// The first published state newer than `seq`.
    ///
    /// Returns immediately when one already exists, and otherwise holds the call open until one
    /// does or [`STATE_HOLD`] elapses — at which point the current state is returned unchanged and
    /// the client asks again. That is a `watch` over a request/response transport: the client
    /// always knows what it last saw, so nothing can be missed and nothing is replayed.
    async fn state_since(seq: u64) -> TunnelState;

    async fn set_intent(intent: IntentRequest) -> Result<IntentAccepted, IntentError>;

    /// Wait for an epoch's cycle to finish. Held open for as long as the cycle takes, which is
    /// bounded by the actor's own budgets rather than by anything here.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{LogConfig, LogProfile};
    use crate::vpn::actor::types::{
        AttemptError, AttemptFailure, AttemptProgress, ConfigSummary, ConfigsView, IntentView,
        Phase, RetryProgress, SplitMode, TrafficStats, TunnelParams,
    };
    use crate::vpn::state::SpeedTracker;

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
    /// constructed, through the wire's codec. Ordered by the trait: state, state_since, set_intent,
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
                                error: crate::vpn::backend::BackendError::PermissionDenied {
                                    detail: "SO_MARK".into(),
                                },
                            },
                            pass: 2,
                        },
                        AttemptFailure {
                            protocol: Protocol::WireGuard,
                            error: AttemptError::Platform {
                                step: crate::vpn::rollback::StepKind::Dns,
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

            vec![TunnelState::initial(), connected, connecting, retrying]
        }

        #[test]
        fn the_published_state_every_shape() {
            for (i, state) in states().iter().enumerate() {
                assert_eq!(&survives(&format!("TunnelState #{i}"), state), state);
            }
            // state_since's argument, and the seed call's absence of one.
            assert_eq!(survives("seq", &u64::MAX), u64::MAX);
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
                IntentRequest::Forget,
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
