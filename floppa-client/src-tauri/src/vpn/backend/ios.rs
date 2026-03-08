//! iOS VPN backend (future).
//!
//! Communicates with a Network Extension (`NEPacketTunnelProvider`) via Apple's
//! `NETunnelProviderManager` API. The extension runs in a separate process managed
//! by iOS, surviving app closure.
//!
//! ```text
//! UI Process (Tauri/WKWebView)     Network Extension (separate process)
//! ┌───────────────────┐           ┌──────────────────────────────┐
//! │ IosBackend        │──Apple──→ │ NEPacketTunnelProvider       │
//! │ (this file)       │   IPC     │    └─ GotatunTunnel (Rust)   │
//! └───────────────────┘           └──────────────────────────────┘
//! ```
//!
//! ## Communication
//!
//! - **UI → Extension**: `NETunnelProviderManager.sendProviderMessage(Data)`
//! - **Extension → UI**: `completionHandler(Data)` for responses
//! - **Shared data**: App Groups (UserDefaults / files) for config persistence
//! - **Serialization**: `bincode(VpnCommand)` → `Data` → `bincode(VpnResponse)`
//!
//! The Network Extension binary links against a shared `floppa-tunnel` Rust static library
//! that contains `GotatunTunnel` and the protocol types (`VpnCommand`/`VpnResponse`).
//!
//! ## Implementation plan
//!
//! 1. Create Xcode Network Extension target with `NEPacketTunnelProvider` subclass
//! 2. Add `com.apple.developer.networking.networkextension` entitlement (requires Apple Developer Program)
//! 3. Create Swift `PacketTunnelProvider` that loads Rust `.a` via C FFI:
//!    - `floppa_tunnel_handle_message(data, len) -> (data, len)` — process VpnCommand, return VpnResponse
//!    - `startTunnel()`: read config from App Group, call Rust to start gotatun with `packetFlow` fd
//!    - `stopTunnel()`: call Rust to stop gotatun
//!    - `handleAppMessage()`: deserialize VpnCommand, call Rust handler, return VpnResponse
//! 4. In this file (`IosBackend`): call `NETunnelProviderManager` via `objc2` crate
//!    or via thin Swift helpers exposed through C FFI
//! 5. App Groups for shared config between app and extension

#![allow(dead_code)]

use super::{VpnBackend, VpnFullInfo};
use crate::vpn::state::ProtocolConfig;
use async_trait::async_trait;

pub struct IosBackend {
    // TODO: NETunnelProviderManager handle
    //
    // Options for calling Apple frameworks from Rust:
    // a) `objc2` crate — direct Objective-C runtime calls
    // b) Swift helper functions exposed via C FFI — simpler, recommended
    //
    // The manager is used to:
    // - Load/save VPN configuration: loadAllFromPreferences()
    // - Start/stop the tunnel: connection.startVPNTunnel()
    // - Send messages to extension: sendProviderMessage()
    // - Observe status: NEVPNConnection.status (via NotificationCenter)
}

impl IosBackend {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl VpnBackend for IosBackend {
    async fn start(
        &self,
        _config: &ProtocolConfig,
        _interface_name: &str,
        _tun_params: &crate::vpn::platform::TunParams,
        _endpoint: std::net::SocketAddr,
    ) -> Result<(), String> {
        // TODO: iOS connect flow:
        //
        // 1. Save WgConfig to App Group shared storage (so extension can read it)
        //    let config_data = bincode::serialize(config)?;
        //    write to App Group container: <app_group>/wg_config.bin
        //
        // 2. Load or create NETunnelProviderManager
        //    let managers = NETunnelProviderManager::loadAllFromPreferences().await;
        //    let manager = managers.first() or create new
        //
        // 3. Configure the manager's protocol (NETunnelProviderProtocol)
        //    protocol.serverAddress = config.peer_endpoint
        //    protocol.providerBundleIdentifier = "dev.okhsunrog.floppa-vpn.tunnel"
        //
        // 4. Save preferences and start tunnel
        //    manager.saveToPreferences().await;
        //    manager.connection.startVPNTunnel();
        //
        // 5. The extension's startTunnel(options:completionHandler:) fires:
        //    - Reads config from App Group
        //    - Creates packetFlow (TUN interface)
        //    - Calls floppa_tunnel_start(config_ptr, config_len, fd) via C FFI
        //    - GotatunTunnel starts with the packetFlow fd
        //
        // 6. Monitor NEVPNConnection.status for .connected confirmation
        //
        // interface_name and tun_params are ignored on iOS.
        Err("IosBackend not yet implemented".into())
    }

    async fn start_with_fd(&self, _config: &ProtocolConfig, _tun_fd: i32) -> Result<(), String> {
        // Not used on iOS — the Network Extension creates and owns the TUN device.
        // NEPacketTunnelProvider.packetFlow provides the virtual interface internally.
        Err("start_with_fd is not used on iOS; the Network Extension manages the TUN device".into())
    }

    async fn stop(&self) -> Result<(), String> {
        // TODO: manager.connection.stopVPNTunnel()
        // The extension's stopTunnel(with:completionHandler:) fires,
        // which calls floppa_tunnel_stop() via C FFI.
        Err("IosBackend not yet implemented".into())
    }

    async fn ping(&self) -> Result<(), String> {
        Err("IosBackend not yet implemented".into())
    }

    async fn get_all_info(&self) -> Option<VpnFullInfo> {
        // TODO: Check NEVPNConnection.status and query stats via sendProviderMessage()
        Some(VpnFullInfo {
            is_running: false,
            ..Default::default()
        })
    }
}
