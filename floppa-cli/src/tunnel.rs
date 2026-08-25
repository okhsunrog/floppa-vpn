//! The WireGuard / AmneziaWG tunnel: gotatun over a TUN device this process creates. The config
//! itself is parsed and turned into device settings by `floppa-tunnel-config`; what is left here
//! is the endpoint lookup and the OS side of the interface.

use anyhow::{Result, anyhow};
use floppa_tunnel_config::{TunnelConfig, device, route};
use gotatun::device::{Device, DeviceBuilder};
use gotatun::tun::tun_async_device::TunDevice;
use gotatun::udp::socket::UdpSocketFactory;
use ipnetwork::IpNetwork;
use std::net::SocketAddr;

use crate::net;

pub const DEFAULT_INTERFACE_NAME: &str = "floppa0";

pub type FloppaDevice = Device<(UdpSocketFactory, TunDevice, TunDevice)>;

/// Resolve the peer endpoint once; the same address feeds the tunnel and the host route.
pub async fn resolve_endpoint(config: &TunnelConfig) -> Result<SocketAddr> {
    let endpoint = &config.peer.endpoint;
    let addrs = tokio::net::lookup_host(endpoint.to_string())
        .await
        .map_err(|e| anyhow!("Failed to resolve endpoint '{endpoint}': {e}"))?;
    route::pick_endpoint(addrs)
        .ok_or_else(|| anyhow!("Endpoint '{endpoint}' resolved to no addresses"))
}

/// Assign the tunnel address and MTU to the TUN interface and bring it up.
pub fn bring_up_interface(config: &TunnelConfig, interface: &str) -> Result<IpNetwork> {
    let addr = config.interface.address;
    net::run_ip(&["addr", "add", &addr.to_string(), "dev", interface])?;
    net::run_ip(&["link", "set", interface, "mtu", &config.mtu().to_string()])?;
    net::run_ip(&["link", "set", interface, "up"])?;
    Ok(addr)
}

pub async fn create_tunnel(
    config: &TunnelConfig,
    endpoint: SocketAddr,
    interface: &str,
) -> Result<FloppaDevice> {
    let mut tun_config = tun::Configuration::default();
    tun_config.tun_name(interface).mtu(config.mtu());
    let tun_device = tun::create_as_async(&tun_config)?;
    let gota_tun = TunDevice::from_tun_device(tun_device)?;

    let builder = DeviceBuilder::new().with_default_udp().with_ip(gota_tun);
    // AmneziaWG obfuscation is applied here when the config carries it; absent → plain WireGuard.
    let builder = device::configure(builder, config, endpoint)?;
    Ok(builder.build().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `floppa_core::services::generate_tunnel_config` produces for `[wireguard]`, with
    /// real keys.
    const SERVER_WG_CONF: &str = "\
[Interface]
PrivateKey = gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=
Address = 10.200.0.5/32
DNS = 8.8.8.8

[Peer]
PublicKey = HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=
Endpoint = vpn.test.com:51820
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
";

    /// The shape `floppa_core::services::generate_tunnel_config` produces for `[amneziawg]` with
    /// the default preset.
    const SERVER_AWG_CONF: &str = "\
[Interface]
PrivateKey = gI6EdUSYvn8ugXOt8QQD6Yc+JyiZxIhp3GInSWRfWGE=
Address = 10.101.0.5/32
DNS = 1.1.1.1
MTU = 1280
Jc = 6
Jmin = 55
Jmax = 205
S1 = 72
S2 = 56
S3 = 32
S4 = 16
H1 = 234567-345678
H2 = 3456789-4567890
H3 = 56789012-67890123
H4 = 456789012-567890123
I1 = <b 0xc30000000108><r 8><b 0x08><r 8><b 0x0045dc><t><r 16>

[Peer]
PublicKey = HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=
Endpoint = vpn.test.com:51821
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
";

    #[test]
    fn a_server_generated_wireguard_conf_round_trips() {
        let config = TunnelConfig::parse(SERVER_WG_CONF).unwrap();
        assert!(!config.is_amneziawg());
        assert_eq!(config.interface.address.to_string(), "10.200.0.5/32");
        assert_eq!(config.mtu(), floppa_tunnel_config::conf::WIREGUARD_MTU);
        assert_eq!(
            config.dns_servers(),
            vec!["8.8.8.8".parse::<std::net::IpAddr>().unwrap()]
        );
        assert_eq!(config.peer.endpoint.to_string(), "vpn.test.com:51820");
        assert_eq!(config.peer.allowed_ips, route::CATCH_ALL);
        assert_eq!(config.keepalive(), 25);
    }

    #[test]
    fn a_server_generated_amneziawg_conf_round_trips_into_gotatun_settings() {
        let config = TunnelConfig::parse(SERVER_AWG_CONF).unwrap();
        assert_eq!(config.mtu(), 1280);
        assert_eq!(
            config.obfuscation,
            Some(floppa_tunnel_config::AwgObfuscation::default()),
            "the server renders its default preset; the client must read it back exactly"
        );
        let awg = device::awg_config(config.obfuscation.as_ref().unwrap()).unwrap();
        assert_eq!((awg.jc, awg.jmin, awg.jmax), (6, 55, 205));
        assert!(awg.i_packets[0].is_some());
    }
}
