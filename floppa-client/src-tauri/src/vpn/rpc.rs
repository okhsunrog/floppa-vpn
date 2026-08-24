//! The IPC wire format between the UI process and the `:vpn` process on Android.
//!
//! The service trait itself is Android-only, because tarpc is an Android-only dependency. The
//! payload type and the rule for when to believe it are not gated: they encode a safety property,
//! and a property whose tests only compile on one target is a property nobody checks.

/// All tunnel info returned in a single RPC call.
///
/// `protocol`, `endpoint` and `address` come from the process that actually owns the tunnel. They
/// exist so the UI process never has to infer what is running from its own settings — a guess that
/// is wrong precisely when it matters, after a failed probe cycle rewrote them.
///
/// # Wire compatibility
///
/// The format is bincode, which is not self-describing: fields are positional, so adding one is not
/// backward compatible and `#[serde(default)]` cannot help — there is no field name that could be
/// missing. Measured against the previous shape, three of four realistic payloads fail to decode
/// outright; the fourth decodes *silently* into `is_running: true` with a plausible protocol and an
/// empty endpoint.
///
/// That last shape is the dangerous one, so [`TunnelInfo::running_identity`] refuses it rather than
/// relying on the two builds never meeting. (They should not: installing an APK force-stops every
/// process of the package, so an older `:vpn` does not survive an update. A stale socket *file*
/// does survive, but with nothing listening it is a refused connection, which is already handled.)
///
/// When the method set itself changes the request-enum indices shift too, and `stop()` stops
/// working across versions. That is the point at which the socket path must be versioned.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelInfo {
    pub is_running: bool,
    pub protocol: Option<crate::vpn::protocol::Protocol>,
    pub endpoint: Option<String>,
    pub address: Option<String>,
    pub last_packet_received: Option<i64>,
    pub connected_secs: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
}

/// The IPC socket name.
///
/// Version this (and bump the Kotlin side with it) when the *method set* changes: tarpc dispatches
/// by variant index, so adding or reordering methods breaks every call including `stop()`, and an
/// unreachable old peer is better than one we can neither read nor stop.
pub const SOCKET_NAME: &str = "vpn.sock";

impl TunnelInfo {
    /// The identity of the running tunnel, if it reported one we can actually trust.
    ///
    /// A tunnel is only believed when it says it is running *and* names both its protocol and its
    /// endpoint. A real tunnel always has both; the shapes that lack them come from a peer whose
    /// wire format we do not share, and adopting one would mean claiming to be connected through a
    /// tunnel we cannot describe, stop, or roll back.
    pub fn running_identity(&self) -> Option<(crate::vpn::protocol::Protocol, String, String)> {
        if !self.is_running {
            return None;
        }
        let protocol = self.protocol?;
        let endpoint = self.endpoint.clone().filter(|e| !e.is_empty())?;
        let address = self.address.clone().unwrap_or_default();
        Some((protocol, endpoint, address))
    }
}

#[cfg(target_os = "android")]
#[tarpc::service]
pub trait VpnRpc {
    /// Get all tunnel info in a single call.
    async fn get_full_info() -> TunnelInfo;

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
    use crate::vpn::protocol::Protocol;

    fn info(is_running: bool, protocol: Option<Protocol>, endpoint: Option<&str>) -> TunnelInfo {
        TunnelInfo {
            is_running,
            protocol,
            endpoint: endpoint.map(str::to_owned),
            address: Some("10.0.0.2/32".into()),
            last_packet_received: None,
            connected_secs: None,
            tx_bytes: None,
            rx_bytes: None,
        }
    }

    #[test]
    fn a_fully_identified_running_tunnel_is_believed() {
        let identity = info(true, Some(Protocol::AmneziaWg), Some("vpn.example:51820"))
            .running_identity()
            .expect("a real tunnel identifies itself");
        assert_eq!(identity.0, Protocol::AmneziaWg);
        assert_eq!(identity.1, "vpn.example:51820");
    }

    #[test]
    fn a_stopped_tunnel_has_no_identity_however_it_is_labelled() {
        assert!(
            info(false, Some(Protocol::WireGuard), Some("vpn.example:51820"))
                .running_identity()
                .is_none()
        );
    }

    #[test]
    fn the_shape_a_version_skew_decodes_into_is_refused() {
        // Measured: of four realistic payloads from the previous struct layout, three fail to
        // decode and the fourth silently becomes exactly this — running, plausible protocol, no
        // endpoint. Believing it would mean adopting a tunnel we cannot describe or stop.
        assert!(
            info(true, Some(Protocol::WireGuard), None)
                .running_identity()
                .is_none()
        );
        assert!(
            info(true, Some(Protocol::WireGuard), Some(""))
                .running_identity()
                .is_none()
        );
    }

    #[test]
    fn a_running_tunnel_without_a_protocol_is_refused() {
        assert!(
            info(true, None, Some("vpn.example:51820"))
                .running_identity()
                .is_none()
        );
    }
}
