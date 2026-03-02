//! In-process VPN backend.
//!
//! The tunnel runs directly in the current process using gotatun.
//! Used on desktop platforms (Linux, Windows, macOS) and currently on Android
//! (until the two-process architecture is implemented).

use super::VpnBackend;
use crate::vpn::state::{TrafficStats, WgConfig};
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
        config: &WgConfig,
        interface_name: &str,
        fwmark: Option<u32>,
    ) -> Result<(), String> {
        self.tunnel_manager
            .start(config, interface_name, fwmark)
            .await
    }

    async fn start_with_fd(&self, config: &WgConfig, tun_fd: i32) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            use std::os::fd::RawFd;
            self.tunnel_manager
                .start_with_fd(config, tun_fd as RawFd)
                .await
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (config, tun_fd);
            Err("start_with_fd is only supported on Android".into())
        }
    }

    async fn stop(&self) -> Result<(), String> {
        self.tunnel_manager.stop().await
    }

    async fn is_running(&self) -> bool {
        self.tunnel_manager.is_running().await
    }

    async fn get_stats(&self) -> Option<TrafficStats> {
        self.tunnel_manager.get_stats().await
    }

    async fn get_last_handshake(&self) -> Option<i64> {
        self.tunnel_manager.get_last_handshake().await
    }

    async fn get_interface_name(&self) -> Option<String> {
        self.tunnel_manager.get_interface_name().await
    }
}
