//! From a [`TunnelConfig`] to gotatun's device settings. Behind the `gotatun` feature.
//!
//! The clients differ in how they get a TUN device and a UDP socket (Android protects its
//! sockets through `VpnService`, Linux may set an fwmark); the key, the peer and the AmneziaWG
//! settings are the same everywhere, and this is where they are applied.

use crate::conf::TunnelConfig;
use gotatun::device::{DeviceBuilder, Peer};
use gotatun::noise::awg::{AwgConfig, AwgConfigError, MagicHeader, ObfChain};
use gotatun::x25519;
use std::net::SocketAddr;

use crate::awg::AwgObfuscation;

/// An obfuscation parameter gotatun rejects. The parser accepts the header and signature specs
/// as text; whether they are valid specs is only known here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceConfigError {
    /// One of `H1`–`H4`.
    #[error("AWG header '{spec}': {source}")]
    Header {
        /// The spec as written.
        spec: String,
        /// gotatun's objection.
        source: AwgConfigError,
    },
    /// One of `I1`–`I5`.
    #[error("AWG signature packet '{spec}': {source}")]
    Signature {
        /// The spec as written.
        spec: String,
        /// gotatun's objection.
        source: AwgConfigError,
    },
}

/// gotatun's AmneziaWG settings for `obfuscation`.
///
/// Starts from gotatun's default (the standard-WireGuard baseline) and overrides the fields this
/// crate models, so a field gotatun adds later keeps its default.
#[allow(clippy::field_reassign_with_default)]
pub fn awg_config(obfuscation: &AwgObfuscation) -> Result<AwgConfig, DeviceConfigError> {
    let header = |spec: &str| {
        MagicHeader::parse(spec).map_err(|source| DeviceConfigError::Header {
            spec: spec.to_string(),
            source,
        })
    };
    let signature = |spec: Option<&str>| {
        spec.map(|spec| {
            ObfChain::parse(spec).map_err(|source| DeviceConfigError::Signature {
                spec: spec.to_string(),
                source,
            })
        })
        .transpose()
    };

    let mut awg = AwgConfig::default();
    awg.jc = obfuscation.jc as usize;
    awg.jmin = obfuscation.jmin as usize;
    awg.jmax = obfuscation.jmax as usize;
    awg.s1 = obfuscation.s1 as usize;
    awg.s2 = obfuscation.s2 as usize;
    awg.s3 = obfuscation.s3 as usize;
    awg.s4 = obfuscation.s4 as usize;
    awg.h1 = header(&obfuscation.h1)?;
    awg.h2 = header(&obfuscation.h2)?;
    awg.h3 = header(&obfuscation.h3)?;
    awg.h4 = header(&obfuscation.h4)?;
    let [i1, i2, i3, i4, i5] = obfuscation.signature_packets();
    awg.i_packets = [
        signature(i1)?,
        signature(i2)?,
        signature(i3)?,
        signature(i4)?,
        signature(i5)?,
    ];
    Ok(awg)
}

/// The config's peer, with `endpoint` as its address. The endpoint is resolved by the caller:
/// the same address feeds the tunnel and the host route that keeps it reachable.
pub fn peer(config: &TunnelConfig, endpoint: SocketAddr) -> Peer {
    let mut peer = Peer::new(x25519::PublicKey::from(config.peer.public_key.to_bytes()))
        .with_endpoint(endpoint)
        .with_allowed_ips(config.peer.allowed_ips.iter().copied());
    peer.keepalive = Some(config.keepalive());
    if let Some(psk) = &config.peer.preshared_key {
        peer = peer.with_preshared_key(psk.to_bytes());
    }
    peer
}

/// Apply `config` to a builder that already has its transport: the private key, the peer, and
/// the AmneziaWG settings when the config is an AmneziaWG one.
pub fn configure<Udp, TunTx, TunRx>(
    builder: DeviceBuilder<Udp, TunTx, TunRx>,
    config: &TunnelConfig,
    endpoint: SocketAddr,
) -> Result<DeviceBuilder<Udp, TunTx, TunRx>, DeviceConfigError> {
    let builder = builder
        .with_private_key(x25519::StaticSecret::from(
            config.interface.private_key.to_bytes(),
        ))
        .with_peer(peer(config, endpoint));
    match &config.obfuscation {
        Some(obfuscation) => Ok(builder.with_awg(awg_config(obfuscation)?)),
        None => Ok(builder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=";

    fn config(interface_extra: &str, peer_extra: &str) -> TunnelConfig {
        TunnelConfig::parse(&format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n{interface_extra}\n\
             [Peer]\nPublicKey = {KEY}\nEndpoint = 1.2.3.4:51820\n{peer_extra}\n"
        ))
        .unwrap()
    }

    #[test]
    fn the_peer_carries_endpoint_routes_keepalive_and_psk() {
        let endpoint: SocketAddr = "1.2.3.4:51820".parse().unwrap();
        let plain = peer(&config("", ""), endpoint);
        assert_eq!(plain.endpoint, Some(endpoint));
        assert_eq!(plain.allowed_ips, crate::route::CATCH_ALL.to_vec());
        assert_eq!(plain.keepalive, Some(crate::conf::DEFAULT_KEEPALIVE));
        assert_eq!(plain.preshared_key, None);
        assert_eq!(
            *plain.public_key.as_bytes(),
            KEY.parse::<crate::conf::PublicKey>().unwrap().to_bytes()
        );

        let with = peer(
            &config(
                "",
                &format!("PresharedKey = {KEY}\nPersistentKeepalive = 15"),
            ),
            endpoint,
        );
        assert_eq!(with.keepalive, Some(15));
        assert!(with.preshared_key.is_some());
    }

    #[test]
    fn the_preset_converts_and_a_bad_spec_names_itself() {
        let awg = awg_config(&AwgObfuscation::default()).unwrap();
        assert_eq!(awg.jc, 6);
        assert_eq!(awg.s3, 32);
        assert!(awg.i_packets[0].is_some());
        assert!(awg.i_packets[1].is_none());

        let baseline = awg_config(&AwgObfuscation::wireguard()).unwrap();
        assert_eq!(baseline.jc, 0);
        assert_eq!(baseline.i_packets.iter().filter(|p| p.is_some()).count(), 0);

        let mut bad = AwgObfuscation::wireguard();
        bad.h1 = "not-a-header".into();
        assert!(matches!(
            awg_config(&bad),
            Err(DeviceConfigError::Header { spec, .. }) if spec == "not-a-header"
        ));
        let mut bad = AwgObfuscation::wireguard();
        bad.i2 = "<nonsense".into();
        assert!(matches!(
            awg_config(&bad),
            Err(DeviceConfigError::Signature { spec, .. }) if spec == "<nonsense"
        ));
    }
}
