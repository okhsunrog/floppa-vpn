//! The IPC wire format between the UI process and the `:vpn` process on Android.
//!
//! The service trait is Android-only, because tarpc is an Android-only dependency. The payload type
//! is not gated: it is plain data, and gating it would mean its tests compile on one target only.
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

use crate::vpn::actor::types::TunnelParams;
use crate::vpn::protocol::Protocol;
use serde::{Deserialize, Serialize};

/// The identity of a running tunnel, as reported by the process that owns it.
///
/// The UI process never infers this from its own settings. That guess is wrong precisely when it
/// matters — after a failed probe cycle has rewritten them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInfo {
    pub protocol: Protocol,
    pub endpoint: String,
    pub address: String,
    pub connected_secs: Option<u64>,
    /// The split rules the tunnel was built with. The service is told them at every start, so a
    /// UI process that finds this tunnel later can adopt it knowing what it routes — and hand a
    /// Connect with the same rules over to it instead of rebuilding.
    pub params: TunnelParams,
    /// Started by the service itself from the autostart bundle, with no UI process involved.
    pub autonomous: bool,
}

/// All tunnel info returned in a single RPC call.
///
/// `running` is one field rather than a `bool` beside a set of `Option`s, so "a tunnel is up but we
/// do not know which" cannot be written down. The owning process reads both halves out of the same
/// `Option`, so they could never disagree — encoding that in the type means nothing downstream has
/// to check for a combination that cannot occur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub running: Option<RunningInfo>,
    /// Which generation of the service is answering.
    ///
    /// Carried so a reply from a service instance that has since been superseded is rejectable by
    /// value rather than by timing. Without it, an observation from the previous instance is
    /// indistinguishable from one describing the current tunnel. Minted per service start, never
    /// per intent: an intent epoch is shared by every pass of a cycle and restarts at 1 in each
    /// UI process, so comparing by it matched instances it was meant to reject.
    pub generation: u64,
    /// The service is coming up: bound, perhaps established, with no tunnel asked for yet.
    ///
    /// This is the state that only exists because the RPC server binds *before* the tunnel starts.
    /// Beforehand, "the service is starting" and "the service failed to start" both looked like an
    /// unreachable socket, and the only way to tell them apart was to wait and see. Read off the
    /// generation's own phase, so a generation that has been *stopped* — which also has no tunnel
    /// and no error — does not claim to still be on its way up.
    pub starting: bool,
    /// `VpnService.Builder.establish()` has handed the service its descriptor.
    ///
    /// The socket is bound *before* the TUN is established, so that an `establish()` that fails
    /// — consent revoked, another VPN holding lockdown, every selected app uninstalled — reaches
    /// the caller as `start_error` on its next poll instead of as a service that never answers.
    /// True only while the descriptor is *available*: once it has been handed to a tunnel this
    /// goes back to false, because a start request against it can only fail from then on.
    pub tun_ready: bool,
    /// Why the last start attempt failed, if it did. Returned rather than only logged, so the
    /// caller gets a reason instead of a timeout.
    pub start_error: Option<String>,
    pub last_packet_received: Option<i64>,
    /// Seconds since the far side last gave evidence of being there — a completed handshake for
    /// the WireGuard family, an inbound packet for VLESS. The owning process computes it because
    /// only it knows which of the two applies.
    pub silent_secs: Option<i64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
}

/// The IPC socket name. Keep in sync with `FloppaVpnService.kt`.
///
/// This never needs versioning, and the wire format never needs to stay backward compatible.
/// Both ends ship in the same APK and are always the same build: installing one replaces the
/// other, and installing force-stops every process of the package, so two builds cannot be live
/// at once. Change the format freely — including the method set, which shifts tarpc's dispatch
/// indices.
pub const SOCKET_NAME: &str = "vpn.sock";

#[cfg(target_os = "android")]
#[tarpc::service]
pub trait VpnRpc {
    /// Get all tunnel info in a single call.
    async fn get_full_info() -> TunnelInfo;

    /// Start a tunnel on the descriptor the service is already holding.
    ///
    /// The config travels typed, over this call, rather than as a string in the Intent that
    /// started the service. That removes the guessing at the other end — the service used to
    /// re-derive the protocol by sniffing the config text, three ways, having been told nothing —
    /// and it means a failure to start comes back here as a reason instead of being logged and
    /// then inferred from a timeout.
    ///
    /// It travels in its *persisted* shape. That used to be impossible: adjacent tagging needs a
    /// self-describing format to deserialize, so a mirror type (`WireConfig`) existed purely to
    /// spell the same three variants in a way bincode could decode. With a JSON transport the
    /// mirror is dead weight, and one shape means one place for a config to be wrong.
    ///
    /// `generation` identifies the request; a call for a generation the service has moved past is
    /// rejected rather than obeyed.
    /// `endpoint` is the already-resolved `ip:port`. It is resolved by the caller because by the
    /// time this service exists, name resolution on the device points at a tunnel that is not up
    /// yet — so the service must never need DNS.
    /// `params` are the split rules the descriptor was built with; the service only echoes them,
    /// so that whoever finds this tunnel later learns what it routes from its owner.
    async fn start_tunnel(
        generation: u64,
        config: crate::vpn::state::ProtocolConfig,
        endpoint: String,
        params: TunnelParams,
    ) -> Result<(), String>;

    /// Stop the tunnel and VPN service.
    async fn stop() -> Result<(), String>;

    /// Ping the VLESS server through the proxy chain.
    /// Updates last_packet_received on success.
    async fn ping() -> Result<(), String>;

    /// Make the far side prove it is there: a forced rehandshake for the WireGuard family, a ping
    /// for VLESS. The answer is the verdict a silent tunnel is judged by — silence alone never is.
    async fn probe() -> Result<(), String>;

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
    use crate::vpn::state::{ProtocolConfig, WgConfig};

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

    fn wg() -> WgConfig {
        WgConfig::from_config_str(WG_CONFIG).expect("fixture must parse")
    }

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

    /// The persisted shape *is* the wire shape now. It could not be while the transport was
    /// bincode — adjacent tagging needs a self-describing format to deserialize — which is the
    /// whole reason a mirror type existed.
    #[test]
    fn the_persisted_config_survives_the_wire() {
        let sent = ProtocolConfig::WireGuard(wg());
        let received = roundtrip(&sent).expect("the config must round-trip");
        assert_eq!(
            received.protocol(),
            crate::vpn::protocol::Protocol::WireGuard
        );
        assert_eq!(received.endpoint_str(), "vpn.example.com:51820");
    }

    /// The AmneziaWG variant is the one that actually shipped broken: its `I` slots used to
    /// deserialize as `Option` while serializing as `String` — a shape JSON forgives and the
    /// bincode transport of the time did not, so every AmneziaWG `start_tunnel` failed to decode
    /// at the service. The slots are symmetric now and stay covered whatever the codec is.
    #[test]
    fn the_awg_config_survives_the_wire() {
        let obfuscation = floppa_tunnel_config::AwgObfuscation {
            i2: String::new(),
            i3: "<b 0xdeadbeef>".into(),
            ..Default::default()
        };
        let sent = ProtocolConfig::AmneziaWg(crate::vpn::state::AwgConfig {
            wg: wg(),
            obfuscation: obfuscation.clone(),
        });
        let received = roundtrip(&sent).expect("the AmneziaWG config must round-trip");
        match received {
            ProtocolConfig::AmneziaWg(awg) => assert_eq!(awg.obfuscation, obfuscation),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// Every argument and return type of every `VpnRpc` method, in every variant that can be
    /// constructed, through the wire's codec. Ordered by the trait: get_full_info, start_tunnel,
    /// stop, ping, probe, set_log_config, start_log_capture, stop_log_capture.
    mod wire_coverage {
        use super::*;
        use crate::logging::{LogConfig, LogProfile};
        use crate::vpn::actor::types::SplitMode;
        use crate::vpn::state::{AwgConfig, VlessVpnConfig};
        use floppa_tunnel_config::AwgObfuscation;

        fn survives<T>(what: &str, value: &T) -> T
        where
            T: serde::Serialize + serde::de::DeserializeOwned,
        {
            roundtrip(value).unwrap_or_else(|e| panic!("{what} must survive the wire: {e}"))
        }

        /// Types without `PartialEq` are compared through their JSON form.
        fn same_json<T: serde::Serialize>(a: &T, b: &T) -> bool {
            serde_json::to_value(a).unwrap() == serde_json::to_value(b).unwrap()
        }

        fn params() -> [TunnelParams; 3] {
            [
                TunnelParams::default(),
                TunnelParams::new(SplitMode::Exclude, vec!["org.example".into(), "a.b".into()]),
                TunnelParams::new(SplitMode::Include, vec!["org.only".into()]),
            ]
        }

        fn running(params: TunnelParams, autonomous: bool) -> RunningInfo {
            RunningInfo {
                protocol: Protocol::AmneziaWg,
                endpoint: "203.0.113.7:51820".into(),
                address: "10.0.0.2/32".into(),
                connected_secs: Some(12),
                params,
                autonomous,
            }
        }

        #[test]
        fn get_full_info_every_shape() {
            let shapes = [
                // Bound, TUN not established yet (the state bind-before-establish introduced).
                TunnelInfo {
                    running: None,
                    generation: 3,
                    starting: true,
                    tun_ready: false,
                    start_error: None,
                    last_packet_received: None,
                    silent_secs: None,
                    tx_bytes: None,
                    rx_bytes: None,
                },
                // Established, idle.
                TunnelInfo {
                    running: None,
                    generation: 3,
                    starting: true,
                    tun_ready: true,
                    start_error: None,
                    last_packet_received: None,
                    silent_secs: None,
                    tx_bytes: None,
                    rx_bytes: None,
                },
                // Failed to start.
                TunnelInfo {
                    running: None,
                    generation: 3,
                    starting: false,
                    tun_ready: false,
                    start_error: Some("VpnService.Builder.establish() returned null".into()),
                    last_packet_received: None,
                    silent_secs: None,
                    tx_bytes: None,
                    rx_bytes: None,
                },
                // Running, requested by the UI. The descriptor has been taken, so it is no
                // longer available for a start.
                TunnelInfo {
                    running: Some(running(params()[1].clone(), false)),
                    generation: 3,
                    starting: false,
                    tun_ready: false,
                    start_error: None,
                    last_packet_received: Some(1_700_000_000),
                    silent_secs: Some(0),
                    tx_bytes: Some(1),
                    rx_bytes: Some(2),
                },
                // Running, started autonomously (always-on).
                TunnelInfo {
                    running: Some(running(params()[2].clone(), true)),
                    generation: 1 << 62,
                    starting: false,
                    tun_ready: false,
                    start_error: None,
                    last_packet_received: Some(0),
                    // Silent long enough that the actor would want it probed.
                    silent_secs: Some(240),
                    tx_bytes: Some(0),
                    rx_bytes: Some(0),
                },
            ];
            for (i, info) in shapes.iter().enumerate() {
                assert_eq!(&survives(&format!("TunnelInfo #{i}"), info), info);
            }
        }

        #[test]
        fn start_tunnel_every_argument_and_result() {
            // generation
            assert_eq!(survives("generation", &(1u64 << 62)), 1u64 << 62);
            // config: every protocol; AmneziaWG with the default preset, with custom slots, and
            // with every slot unset (the shape that used to break).
            let configs = [
                ProtocolConfig::WireGuard(wg()),
                ProtocolConfig::AmneziaWg(AwgConfig {
                    wg: wg(),
                    obfuscation: AwgObfuscation::default(),
                }),
                ProtocolConfig::AmneziaWg(AwgConfig {
                    wg: wg(),
                    obfuscation: AwgObfuscation {
                        i1: String::new(),
                        i3: "<b 0xdeadbeef><r 8>".into(),
                        ..AwgObfuscation::default()
                    },
                }),
                ProtocolConfig::AmneziaWg(AwgConfig {
                    wg: wg(),
                    obfuscation: AwgObfuscation {
                        i1: String::new(),
                        ..AwgObfuscation::default()
                    },
                }),
                ProtocolConfig::Vless(VlessVpnConfig {
                    uri: "vless://uuid@203.0.113.7:443?security=reality".into(),
                    uuid: "0b6f9e9a-1c2d-4e5f-8a9b-0c1d2e3f4a5b".into(),
                    server_addr: "203.0.113.7:443".into(),
                    server_name: "www.example.com".into(),
                    reality_public_key: "pubkey".into(),
                    reality_short_id: "1a3805da21c80ea1".into(),
                    flow: Some("xtls-rprx-vision".into()),
                    address: "10.0.0.2/32".into(),
                    dns: Some("1.1.1.1".into()),
                    mtu: Some(1500),
                    allowed_ips: "0.0.0.0/0, ::/0".into(),
                }),
                ProtocolConfig::Vless(VlessVpnConfig {
                    uri: String::new(),
                    uuid: String::new(),
                    server_addr: "h:1".into(),
                    server_name: String::new(),
                    reality_public_key: String::new(),
                    reality_short_id: String::new(),
                    flow: None,
                    address: "10.0.0.2/32".into(),
                    dns: None,
                    mtu: None,
                    allowed_ips: String::new(),
                }),
            ];
            for (i, config) in configs.iter().enumerate() {
                let back = survives(&format!("ProtocolConfig #{i}"), config);
                assert!(
                    same_json(&back, config),
                    "ProtocolConfig #{i} changed in transit"
                );
            }
            // endpoint
            assert_eq!(
                survives("endpoint", &"[2001:db8::1]:51820".to_string()),
                "[2001:db8::1]:51820"
            );
            // params
            for p in params() {
                assert_eq!(survives("TunnelParams", &p), p);
            }
            // result
            let ok: Result<(), String> = Ok(());
            let err: Result<(), String> = Err("the tunnel descriptor has already been used".into());
            assert_eq!(survives("start_tunnel Ok", &ok), ok);
            assert_eq!(survives("start_tunnel Err", &err), err);
        }

        /// `stop`, `ping` and `probe` all take nothing and answer the same way.
        #[test]
        fn stop_ping_and_probe_results() {
            for r in [Ok(()), Err("engine: stopped twice".to_string())] {
                assert_eq!(survives("Result<(), String>", &r), r);
            }
        }

        #[test]
        fn set_log_config_every_shape() {
            let shapes = [
                LogConfig::default(),
                LogConfig {
                    profile: LogProfile::Verbose,
                    custom_filter: None,
                    custom_filter_enabled: false,
                },
                LogConfig {
                    profile: LogProfile::Normal,
                    custom_filter: Some("floppa=trace,gotatun=debug".into()),
                    custom_filter_enabled: true,
                },
            ];
            for (i, c) in shapes.iter().enumerate() {
                let back = survives(&format!("LogConfig #{i}"), c);
                assert!(same_json(&back, c), "LogConfig #{i} changed in transit");
            }
        }

        #[test]
        fn log_capture_arguments() {
            assert_eq!(
                survives("capture_id", &"2026-08-25T12-00-00Z".to_string()),
                "2026-08-25T12-00-00Z"
            );
            survives("unit", &());
        }
    }

    #[test]
    fn the_tunnel_info_survives_the_wire_with_its_params() {
        use crate::vpn::actor::types::SplitMode;
        let sent = TunnelInfo {
            running: Some(RunningInfo {
                protocol: crate::vpn::protocol::Protocol::AmneziaWg,
                endpoint: "203.0.113.7:51820".into(),
                address: "10.0.0.2/32".into(),
                connected_secs: Some(12),
                params: TunnelParams::new(SplitMode::Exclude, vec!["org.example".into()]),
                autonomous: true,
            }),
            generation: crate::vpn::autostart::AUTONOMOUS_EPOCH_BASE + 3,
            starting: false,
            tun_ready: true,
            start_error: None,
            last_packet_received: Some(1),
            silent_secs: Some(3),
            tx_bytes: Some(10),
            rx_bytes: Some(20),
        };
        let received = roundtrip(&sent).expect("the reply type must round-trip");
        assert_eq!(received, sent);
    }

    /// JSON's own hazard, and the only one this transport has: `serde_json` writes a non-finite
    /// `f64` as `null` and then refuses to read it back. The rates are the only floats that cross,
    /// and `SpeedTracker` never divides by an interval under 100 ms — so they are finite by
    /// construction, and this is here to fail if that ever stops being true.
    #[test]
    fn the_only_floats_on_the_wire_are_finite() {
        use crate::vpn::state::SpeedTracker;

        let mut speed = SpeedTracker::new();
        // First sample is the baseline; the second is computed over a near-zero interval, which is
        // exactly where a division would produce an infinity.
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
