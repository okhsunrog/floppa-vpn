//! In-process VPN backend.
//!
//! The tunnel runs directly in the current process using gotatun (WireGuard)
//! or shoes (VLESS+REALITY). Used on desktop platforms (Linux, Windows, macOS).

use super::{VpnBackend, VpnFullInfo};
use crate::vpn::state::ProtocolConfig;
use crate::vpn::tunnel::TunnelManager;
use async_trait::async_trait;

pub struct InProcessBackend {
    tunnel_manager: TunnelManager,
}

impl InProcessBackend {
    pub fn new() -> Self {
        Self {
            tunnel_manager: TunnelManager::default(),
        }
    }
}

#[async_trait]
impl VpnBackend for InProcessBackend {
    async fn start(
        &self,
        config: &ProtocolConfig,
        interface_name: &str,
        fwmark: Option<u32>,
        endpoint: std::net::SocketAddr,
    ) -> Result<(), String> {
        match config {
            ProtocolConfig::WireGuard(wg) => {
                #[cfg(not(target_os = "android"))]
                {
                    self.tunnel_manager
                        .start_wireguard(wg, interface_name, fwmark, endpoint)
                        .await
                }
                #[cfg(target_os = "android")]
                {
                    let _ = (wg, interface_name, fwmark, endpoint);
                    Err("On Android, use start_with_fd".into())
                }
            }
            ProtocolConfig::Vless(vless) => {
                #[cfg(not(target_os = "android"))]
                {
                    let shoes_config = vless.to_shoes_config();
                    self.tunnel_manager
                        .start_vless(&shoes_config, interface_name)
                        .await
                }
                #[cfg(target_os = "android")]
                {
                    let _ = (vless, interface_name);
                    Err("On Android, use start_with_fd".into())
                }
            }
        }
    }

    async fn start_with_fd(&self, config: &ProtocolConfig, tun_fd: i32) -> Result<(), String> {
        match config {
            ProtocolConfig::WireGuard(wg) => {
                #[cfg(target_os = "android")]
                {
                    use std::os::fd::RawFd;
                    self.tunnel_manager
                        .start_wireguard_with_fd(wg, tun_fd as RawFd)
                        .await
                }
                #[cfg(not(target_os = "android"))]
                {
                    let _ = (wg, tun_fd);
                    Err("start_with_fd is only supported on Android".into())
                }
            }
            ProtocolConfig::Vless(vless) => {
                let shoes_config = vless.to_shoes_config();
                self.tunnel_manager
                    .start_vless_with_fd(&shoes_config, tun_fd)
                    .await
            }
        }
    }

    async fn stop(&self) -> Result<(), String> {
        self.tunnel_manager.stop().await
    }

    async fn get_all_info(&self) -> Option<VpnFullInfo> {
        Some(VpnFullInfo {
            is_running: self.tunnel_manager.is_running().await,
            stats: self.tunnel_manager.get_stats().await,
            last_handshake: self.tunnel_manager.get_last_handshake().await,
            connected_secs: None,
        })
    }
}
