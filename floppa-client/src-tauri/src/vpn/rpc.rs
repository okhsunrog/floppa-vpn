//! The IPC wire format between the UI process and the `:vpn` process on Android.
//!
//! The service trait is Android-only, because tarpc is an Android-only dependency. The payload type
//! is not gated: it is plain data, and gating it would mean its tests compile on one target only.

use crate::vpn::actor::types::TunnelParams;
use crate::vpn::protocol::Protocol;
use serde::{Deserialize, Serialize};

/// The tunnel config as it crosses the process boundary.
///
/// Deliberately a separate type from [`ProtocolConfig`](crate::vpn::state::ProtocolConfig), which
/// is adjacently tagged (`#[serde(tag = "protocol", content = "config")]`) because its JSON form
/// is what sits in users' keyrings and must keep parsing.
///
/// Adjacent and internal tagging both need a self-describing format to *deserialize*: serde
/// buffers the content and re-reads it once it has seen the tag. bincode is not self-describing,
/// so the encode succeeds and the decode does not — and it fails inside the framed transport, so
/// it surfaces as "could not read from the transport" rather than as a decode error anyone can
/// catch. This enum is externally tagged, which bincode encodes as a plain variant index.
// Same shape as `ProtocolConfig`: the AmneziaWG variant is the largest by construction.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireConfig {
    WireGuard(crate::vpn::state::WgConfig),
    AmneziaWg(crate::vpn::state::AwgConfig),
    Vless(crate::vpn::state::VlessVpnConfig),
}

impl From<&crate::vpn::state::ProtocolConfig> for WireConfig {
    fn from(config: &crate::vpn::state::ProtocolConfig) -> Self {
        use crate::vpn::state::ProtocolConfig;
        match config {
            ProtocolConfig::WireGuard(wg) => Self::WireGuard(wg.clone()),
            ProtocolConfig::AmneziaWg(awg) => Self::AmneziaWg(awg.clone()),
            ProtocolConfig::Vless(vless) => Self::Vless(vless.clone()),
        }
    }
}

impl From<WireConfig> for crate::vpn::state::ProtocolConfig {
    fn from(config: WireConfig) -> Self {
        match config {
            WireConfig::WireGuard(wg) => Self::WireGuard(wg),
            WireConfig::AmneziaWg(awg) => Self::AmneziaWg(awg),
            WireConfig::Vless(vless) => Self::Vless(vless),
        }
    }
}

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
    /// indistinguishable from one describing the current tunnel.
    pub epoch: u64,
    /// The service is up and holding a descriptor, but no tunnel has been asked for yet.
    ///
    /// This is the state that only exists because the RPC server binds *before* the tunnel starts.
    /// Beforehand, "the service is starting" and "the service failed to start" both looked like an
    /// unreachable socket, and the only way to tell them apart was to wait and see.
    pub starting: bool,
    /// `VpnService.Builder.establish()` has handed the service its descriptor.
    ///
    /// The socket is bound *before* the TUN is established, so that an `establish()` that fails
    /// — consent revoked, another VPN holding lockdown, every selected app uninstalled — reaches
    /// the caller as `start_error` on its next poll instead of as a service that never answers.
    /// Until this is true a `start_tunnel` has no descriptor to run on and must not be sent.
    pub tun_ready: bool,
    /// Why the last start attempt failed, if it did. Returned rather than only logged, so the
    /// caller gets a reason instead of a timeout.
    pub start_error: Option<String>,
    pub last_packet_received: Option<i64>,
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
    /// `epoch` identifies the request; a call for a generation the service has moved past is
    /// rejected rather than obeyed.
    /// `endpoint` is the already-resolved `ip:port`. It is resolved by the caller because by the
    /// time this service exists, name resolution on the device points at a tunnel that is not up
    /// yet — so the service must never need DNS.
    /// `params` are the split rules the descriptor was built with; the service only echoes them,
    /// so that whoever finds this tunnel later learns what it routes from its owner.
    async fn start_tunnel(
        epoch: u64,
        config: WireConfig,
        endpoint: String,
        params: TunnelParams,
    ) -> Result<(), String>;

    /// Stop the tunnel and VPN service.
    async fn stop() -> Result<(), String>;

    /// Ping the VLESS server through the proxy chain.
    /// Updates last_packet_received on success.
    async fn ping() -> Result<(), String>;

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

    /// The wire uses bincode, so anything crossing it has to survive bincode specifically —
    /// not just "serde works".
    fn roundtrip<T>(value: &T) -> Result<T, String>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let bytes = bincode::serialize(value).map_err(|e| format!("encode: {e}"))?;
        bincode::deserialize(&bytes).map_err(|e| format!("decode: {e}"))
    }

    #[test]
    fn the_wire_config_survives_bincode() {
        let sent = WireConfig::WireGuard(wg());
        let received = roundtrip(&sent).expect("the wire type must round-trip");
        let restored: ProtocolConfig = received.into();
        assert_eq!(
            restored.protocol(),
            crate::vpn::protocol::Protocol::WireGuard
        );
        assert_eq!(restored.endpoint_str(), "vpn.example.com:51820");
    }

    #[test]
    fn the_persisted_config_does_not_survive_bincode() {
        // Not a quirk to work around — the reason WireConfig exists. ProtocolConfig is adjacently
        // tagged because its JSON form is in users' keyrings, and adjacent tagging needs a
        // self-describing format to deserialize. Sending it over this wire encoded fine and then
        // failed to decode *inside the framed transport*, which surfaced as "could not read from
        // the transport" — a connection error, with nothing pointing at the real cause.
        let sent = ProtocolConfig::WireGuard(wg());
        assert!(
            roundtrip(&sent).is_err(),
            "if this ever passes, bincode has become self-describing and WireConfig can go"
        );
    }

    #[test]
    fn the_tunnel_info_survives_bincode_with_its_params() {
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
            epoch: crate::vpn::autostart::AUTONOMOUS_EPOCH_BASE + 3,
            starting: false,
            tun_ready: true,
            start_error: None,
            last_packet_received: Some(1),
            tx_bytes: Some(10),
            rx_bytes: Some(20),
        };
        let received = roundtrip(&sent).expect("the reply type must round-trip");
        assert_eq!(received, sent);
    }

    #[test]
    fn converting_to_the_wire_and_back_preserves_the_protocol() {
        let original = ProtocolConfig::WireGuard(wg());
        let restored: ProtocolConfig = WireConfig::from(&original).into();
        assert_eq!(restored.protocol(), original.protocol());
    }
}
