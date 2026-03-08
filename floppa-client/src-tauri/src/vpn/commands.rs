use super::backend::VpnBackend;
use super::config as vpn_config;
use super::platform::{Platform, PlatformImpl};
use super::state::{
    ConnectionInfo, ConnectionStatus, ProtocolConfig, SavedVpnConfigs, VlessVpnConfig, VpnState,
    WgConfig,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, State};
#[allow(unused_imports)]
use tracing::{error, info, warn};

/// Get the persistent device UUID (created on first call)
#[tauri::command]
#[specta::specta]
pub fn get_device_id() -> Result<String, String> {
    vpn_config::get_or_create_device_id()
}

/// Get the device name (Android: manufacturer+model, desktop: hostname)
#[tauri::command]
#[specta::specta]
pub fn get_device_name(#[allow(unused_variables)] app: AppHandle) -> String {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        match app.vpn().get_device_name() {
            Ok(name) => return name,
            Err(e) => {
                warn!("Failed to get Android device name: {e}");
            }
        }
    }
    vpn_config::get_device_name()
}

/// Parse a config string (WireGuard or VLESS URI), store under the right protocol key, and persist.
#[tauri::command]
#[specta::specta]
pub async fn set_active_config(
    config_str: String,
    state: State<'_, Arc<VpnState>>,
) -> Result<(), String> {
    info!("Setting active config");
    let trimmed = config_str.trim();
    let mut configs = state.configs.write().await;
    if trimmed.starts_with("vless://") {
        let vless = VlessVpnConfig::from_uri(trimmed)?;
        configs.vless = Some(vless);
        configs.active_protocol = "vless".to_string();
    } else {
        let wg = WgConfig::from_config_str(&config_str)?;
        configs.wireguard = Some(wg);
        configs.active_protocol = "wireguard".to_string();
    };
    vpn_config::save_configs(&configs);
    Ok(())
}

/// Clear all configs from memory and delete persisted config. Disconnects first if connected.
#[tauri::command]
#[specta::specta]
pub async fn clear_config(
    app: AppHandle,
    state: State<'_, Arc<VpnState>>,
    backend: State<'_, Arc<dyn VpnBackend>>,
    platform: State<'_, Arc<PlatformImpl>>,
) -> Result<(), String> {
    let status = state.connection.read().await.status;
    if status != ConnectionStatus::Disconnected {
        disconnect(app, state.clone(), backend, platform).await?;
    }
    *state.configs.write().await = SavedVpnConfigs::default();
    vpn_config::delete_configs();
    Ok(())
}

/// Load persisted VPN configs into memory (called on startup).
#[tauri::command]
#[specta::specta]
pub async fn load_saved_config(state: State<'_, Arc<VpnState>>) -> Result<bool, String> {
    if state.configs.read().await.has_any() {
        return Ok(true);
    }
    if let Some(configs) = vpn_config::load_configs() {
        *state.configs.write().await = configs;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get active protocol's config (without private key for security)
#[tauri::command]
#[specta::specta]
pub async fn get_config(state: State<'_, Arc<VpnState>>) -> Result<Option<ConfigSafe>, String> {
    let configs = state.configs.read().await;
    let config = configs.active_config();
    Ok(config.as_ref().map(|c| ConfigSafe {
        protocol: c.protocol_name().to_string(),
        address: c.address().to_string(),
        dns: match c {
            ProtocolConfig::WireGuard(wg) => wg.dns.clone(),
            ProtocolConfig::Vless(vless) => vless.dns.clone(),
        },
        server_endpoint: c.endpoint_str().to_string(),
        allowed_ips: match c {
            ProtocolConfig::WireGuard(wg) => wg.allowed_ips.clone(),
            ProtocolConfig::Vless(vless) => vless.allowed_ips.clone(),
        },
        mtu: Some(c.get_mtu()),
    }))
}

/// Switch the active protocol (must disconnect first)
#[tauri::command]
#[specta::specta]
pub async fn set_active_protocol(
    protocol: String,
    state: State<'_, Arc<VpnState>>,
) -> Result<(), String> {
    let mut configs = state.configs.write().await;
    match protocol.as_str() {
        "wireguard" if configs.wireguard.is_some() => {
            configs.active_protocol = "wireguard".to_string();
        }
        "vless" if configs.vless.is_some() => {
            configs.active_protocol = "vless".to_string();
        }
        _ => return Err(format!("No cached config for protocol '{protocol}'")),
    }
    vpn_config::save_configs(&configs);
    Ok(())
}

/// Get list of protocols that have cached configs
#[tauri::command]
#[specta::specta]
pub async fn get_available_protocols(
    state: State<'_, Arc<VpnState>>,
) -> Result<Vec<String>, String> {
    let configs = state.configs.read().await;
    Ok(configs.available_protocols())
}

/// Safe config info (no private keys or secrets)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Type)]
pub struct ConfigSafe {
    pub protocol: String,
    pub address: String,
    pub dns: Option<String>,
    pub server_endpoint: String,
    pub allowed_ips: String,
    pub mtu: Option<u16>,
}

/// Split tunneling mode
#[derive(Debug, Clone, Serialize, Deserialize, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum SplitMode {
    #[default]
    All,
    Include,
    Exclude,
}

/// Information about an installed app (for split tunneling UI)
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AppInfo {
    pub package_name: String,
    pub label: String,
    pub is_system: bool,
    pub icon: Option<String>,
}

/// Safe area insets (status bar, nav bar) in dp
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SafeAreaInsets {
    pub top: f64,
    pub bottom: f64,
}

const INTERFACE_NAME: &str = "floppa0";

/// fwmark used on Linux for policy routing
#[cfg(target_os = "linux")]
const FWMARK: u32 = 0x666c6f70; // "flop" in hex

/// Check whether the current process has effective CAP_NET_ADMIN.
#[cfg(target_os = "linux")]
fn has_cap_net_admin() -> bool {
    const CAP_NET_ADMIN_BIT: u32 = 12;

    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to read /proc/self/status: {e}");
            return false;
        }
    };

    let cap_eff_hex = match status
        .lines()
        .find(|line| line.starts_with("CapEff:"))
        .and_then(|line| line.split_whitespace().nth(1))
    {
        Some(v) => v,
        None => {
            warn!("CapEff not found in /proc/self/status");
            return false;
        }
    };

    match u128::from_str_radix(cap_eff_hex, 16) {
        Ok(bits) => (bits & (1u128 << CAP_NET_ADMIN_BIT)) != 0,
        Err(e) => {
            warn!("Failed to parse CapEff value '{cap_eff_hex}': {e}");
            false
        }
    }
}

/// Connect to VPN
#[tauri::command]
#[specta::specta]
pub async fn connect(
    #[allow(unused_variables)] app: AppHandle,
    state: State<'_, Arc<VpnState>>,
    backend: State<'_, Arc<dyn VpnBackend>>,
    #[allow(unused_variables)] platform: State<'_, Arc<PlatformImpl>>,
    split_mode: Option<SplitMode>,
    selected_apps: Option<Vec<String>>,
) -> Result<(), String> {
    info!("Connecting to VPN");

    // Guard: only allow connect from Disconnected
    {
        let conn = state.connection.read().await;
        match conn.status {
            ConnectionStatus::Disconnected => {}
            ConnectionStatus::Connecting | ConnectionStatus::VerifyingConnection => {
                return Err("Already connecting".to_string());
            }
            ConnectionStatus::Connected => return Err("Already connected".to_string()),
            ConnectionStatus::Disconnecting => return Err("Disconnecting in progress".to_string()),
        }
    }

    let proto_config = state
        .configs
        .read()
        .await
        .active_config()
        .ok_or("No active config")?;

    {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Connecting;
    }

    #[cfg(target_os = "android")]
    let result = connect_android(
        &app,
        &state,
        &backend,
        proto_config,
        split_mode,
        selected_apps,
    )
    .await;

    #[cfg(not(target_os = "android"))]
    let result = connect_desktop(
        &state,
        &backend,
        &platform,
        proto_config,
        split_mode,
        selected_apps,
    )
    .await;

    result
}

#[cfg(target_os = "android")]
async fn connect_android(
    app: &AppHandle,
    state: &Arc<VpnState>,
    backend: &Arc<dyn VpnBackend>,
    config: ProtocolConfig,
    split_mode: Option<SplitMode>,
    selected_apps: Option<Vec<String>>,
) -> Result<(), String> {
    use tauri_plugin_vpn::VpnExt;

    let granted = app
        .vpn()
        .prepare()
        .map_err(|e| format!("VPN prepare failed: {e}"))?;
    if !granted {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err("VPN permission denied".to_string());
    }

    // Serialize config for the Android VPN service (WG config text or vless:// URI)
    let protocol_config_str = match &config {
        ProtocolConfig::WireGuard(wg) => wg.to_config_str(),
        ProtocolConfig::Vless(vless) => vless.uri.clone(),
    };

    let dns = match &config {
        ProtocolConfig::WireGuard(wg) => wg.dns.clone(),
        ProtocolConfig::Vless(vless) => vless.dns.clone(),
    };

    let mut vpn_config = tauri_plugin_vpn::VpnConfig {
        ipv4_addr: config.address().to_string(),
        ipv6_addr: None,
        routes: vec!["0.0.0.0/0".into(), "::/0".into()],
        dns,
        mtu: config.get_mtu() as u32,
        disallowed_apps: vec![],
        allowed_apps: vec![],
        protocol_config: Some(protocol_config_str),
    };

    let mode = split_mode.unwrap_or_default();
    let apps = selected_apps.unwrap_or_default();
    match mode {
        SplitMode::Exclude if !apps.is_empty() => vpn_config.disallowed_apps = apps,
        SplitMode::Include if !apps.is_empty() => vpn_config.allowed_apps = apps,
        _ => {}
    }

    if let Err(e) = app.vpn().start(vpn_config) {
        error!("VPN start failed: {e}");
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err(format!("VPN start failed: {e}"));
    }

    // Poll until connected or timeout
    let timeout = std::time::Duration::from_secs(10);
    let poll_interval = std::time::Duration::from_millis(500);
    let start = std::time::Instant::now();
    let mut poll_count = 0u32;
    loop {
        tokio::time::sleep(poll_interval).await;
        poll_count += 1;
        if backend.get_all_info().await.is_some_and(|i| i.is_running) {
            info!(
                "Tunnel ready after {poll_count} polls ({:.1}s)",
                start.elapsed().as_secs_f64()
            );
            break;
        }
        if start.elapsed() > timeout {
            error!(
                "Tunnel not ready after {poll_count} polls ({:.1}s)",
                start.elapsed().as_secs_f64()
            );
            // IPC is likely down (that's why we timed out), so use Kotlin-side stop
            if let Err(e) = app.vpn().stop() {
                error!("Failed to stop VPN service after timeout: {e}");
            }
            let mut conn = state.connection.write().await;
            *conn = ConnectionInfo::default();
            return Err("Connection timed out".to_string());
        }
    }

    {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::VerifyingConnection;
    }

    match &config {
        ProtocolConfig::WireGuard(_) => {
            info!("Tunnel up on Android, verifying WireGuard handshake...");
            if wait_for_handshake(backend, std::time::Duration::from_secs(5))
                .await
                .is_err()
            {
                info!("No handshake after 5s — peer likely invalid, stopping tunnel");
                if let Err(e) = backend.stop().await {
                    error!("Failed to stop tunnel after verification failure: {e}");
                }
                let mut conn = state.connection.write().await;
                *conn = ConnectionInfo::default();
                return Err("Connection verification failed — config may be invalid".to_string());
            }
        }
        ProtocolConfig::Vless(_) => {
            info!("Tunnel up on Android, verifying VLESS connectivity...");
            if let Err(e) = verify_vless_connectivity(std::time::Duration::from_secs(10)).await {
                info!("VLESS connectivity check failed: {e}");
                if let Err(e) = backend.stop().await {
                    error!("Failed to stop tunnel after verification failure: {e}");
                }
                let mut conn = state.connection.write().await;
                *conn = ConnectionInfo::default();
                return Err(format!("Connection verification failed: {e}"));
            }
        }
    }

    state.speed_tracker.write().await.reset();
    let mut conn = state.connection.write().await;
    conn.status = ConnectionStatus::Connected;
    conn.protocol = Some(config.protocol_name().to_string());
    conn.connected_at = Some(chrono::Utc::now().timestamp());
    conn.server_endpoint = Some(config.endpoint_str().to_string());
    conn.assigned_ip = Some(config.address().to_string());
    info!("Connected successfully on Android");
    Ok(())
}

#[cfg(not(target_os = "android"))]
async fn connect_desktop(
    state: &Arc<VpnState>,
    backend: &Arc<dyn VpnBackend>,
    platform: &Arc<PlatformImpl>,
    config: ProtocolConfig,
    _split_mode: Option<SplitMode>,
    _selected_apps: Option<Vec<String>>,
) -> Result<(), String> {
    let endpoint = tokio::net::lookup_host(config.endpoint_str())
        .await
        .map_err(|e| {
            format!(
                "Failed to resolve endpoint '{}': {e}",
                config.endpoint_str()
            )
        })?
        .next()
        .ok_or_else(|| {
            format!(
                "Endpoint '{}' resolved to no addresses",
                config.endpoint_str()
            )
        })?;
    let endpoint_ip = endpoint.ip();

    #[cfg(target_os = "linux")]
    if let Err(e) = platform.prepare_tun(INTERFACE_NAME).await {
        error!("Failed to prepare TUN interface: {e}");
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err(format!("Failed to prepare TUN interface: {e}"));
    }

    #[cfg(target_os = "linux")]
    let fwmark = if has_cap_net_admin() {
        Some(FWMARK)
    } else {
        info!("CAP_NET_ADMIN not present, running without fwmark");
        None
    };

    #[cfg(target_os = "linux")]
    let start_result = match backend
        .start(&config, INTERFACE_NAME, fwmark, endpoint)
        .await
    {
        Err(e)
            if fwmark.is_some()
                && (e.contains("Operation not permitted") || e.contains("Permission denied")) =>
        {
            warn!("Tunnel start with fwmark failed due permissions, retrying without fwmark");
            backend.start(&config, INTERFACE_NAME, None, endpoint).await
        }
        result => result,
    };

    #[cfg(not(target_os = "linux"))]
    let start_result = backend.start(&config, INTERFACE_NAME, None, endpoint).await;

    match start_result {
        Ok(()) => {
            let addr = config.address_network()?;
            if let Err(e) = platform.configure_address(INTERFACE_NAME, addr).await {
                error!("Failed to configure address: {e}");
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(e);
            }

            if let Err(e) = platform.add_endpoint_route(endpoint_ip).await {
                error!("Failed to add endpoint route: {e}");
                let _ = platform.cleanup(INTERFACE_NAME).await;
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(e);
            }

            let allowed_ips = config.allowed_ips_networks();
            if let Err(e) = platform.add_routes(INTERFACE_NAME, &allowed_ips).await {
                error!("Failed to add routes: {e}");
                let _ = platform.cleanup(INTERFACE_NAME).await;
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(e);
            }

            let dns_servers = config.dns_servers();
            if !dns_servers.is_empty()
                && let Err(e) = platform.configure_dns(INTERFACE_NAME, &dns_servers).await
            {
                error!("Failed to configure DNS: {e}");
            }

            // Protocol-specific verification
            {
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::VerifyingConnection;
            }

            match &config {
                ProtocolConfig::WireGuard(_) => {
                    info!("Tunnel up, verifying WireGuard handshake...");
                    if wait_for_handshake(backend, std::time::Duration::from_secs(5))
                        .await
                        .is_err()
                    {
                        info!("No handshake after 5s — peer likely invalid, disconnecting");
                        let _ = platform.cleanup(INTERFACE_NAME).await;
                        let _ = backend.stop().await;
                        let mut conn = state.connection.write().await;
                        conn.status = ConnectionStatus::Disconnected;
                        return Err(
                            "Connection verification failed — config may be invalid".to_string()
                        );
                    }
                }
                ProtocolConfig::Vless(_) => {
                    info!("Tunnel up, verifying VLESS connectivity...");
                    if let Err(e) =
                        verify_vless_connectivity(std::time::Duration::from_secs(10)).await
                    {
                        info!("VLESS connectivity check failed: {e}");
                        let _ = platform.cleanup(INTERFACE_NAME).await;
                        let _ = backend.stop().await;
                        let mut conn = state.connection.write().await;
                        conn.status = ConnectionStatus::Disconnected;
                        return Err(format!("Connection verification failed: {e}"));
                    }
                }
            }

            state.speed_tracker.write().await.reset();
            let mut conn = state.connection.write().await;
            conn.status = ConnectionStatus::Connected;
            conn.protocol = Some(config.protocol_name().to_string());
            conn.connected_at = Some(chrono::Utc::now().timestamp());
            conn.server_endpoint = Some(config.endpoint_str().to_string());
            conn.assigned_ip = Some(config.address().to_string());
            info!("Connected successfully");
            Ok(())
        }
        Err(e) => {
            let mut conn = state.connection.write().await;
            conn.status = ConnectionStatus::Disconnected;
            error!("Connection failed: {e}");
            Err(e)
        }
    }
}

/// After tunnel is up, wait for the first WireGuard handshake to confirm
/// the peer actually exists on the server. Returns Ok if handshake observed,
/// Err if timed out (peer likely deleted/invalid).
async fn wait_for_handshake(
    backend: &Arc<dyn VpnBackend>,
    timeout: std::time::Duration,
) -> Result<(), ()> {
    let poll_interval = std::time::Duration::from_millis(500);
    let start = std::time::Instant::now();
    loop {
        if let Some(info) = backend.get_all_info().await
            && let Some(secs) = info.last_packet_received
            && secs < 10
        {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(());
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Verify VLESS tunnel connectivity by making a TCP connection through the tunnel.
/// This goes: TUN → smoltcp → VLESS proxy → server → target, proving end-to-end connectivity.
async fn verify_vless_connectivity(timeout: std::time::Duration) -> Result<(), String> {
    tokio::time::timeout(timeout, async {
        tokio::net::TcpStream::connect("1.1.1.1:443")
            .await
            .map_err(|e| format!("connectivity check failed: {e}"))
    })
    .await
    .map_err(|_| "connectivity check timed out".to_string())?
    .map(|_stream| ())
}

/// Disconnect from VPN
#[tauri::command]
#[specta::specta]
pub async fn disconnect(
    #[allow(unused_variables)] app: AppHandle,
    state: State<'_, Arc<VpnState>>,
    backend: State<'_, Arc<dyn VpnBackend>>,
    platform: State<'_, Arc<PlatformImpl>>,
) -> Result<(), String> {
    info!("Disconnecting from VPN");

    // Guard: reject if not connected or already disconnecting
    {
        let conn = state.connection.read().await;
        match conn.status {
            ConnectionStatus::Disconnecting => return Err("Already disconnecting".to_string()),
            ConnectionStatus::Disconnected => return Err("Not connected".to_string()),
            _ => {}
        }
    }

    {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnecting;
    }

    let _ = platform.cleanup(INTERFACE_NAME).await;

    if let Err(e) = backend.stop().await {
        error!("Backend stop failed: {e}");
        // IPC failed — fall back to Kotlin-side stop via ACTION_STOP intent
        #[cfg(target_os = "android")]
        {
            use tauri_plugin_vpn::VpnExt;
            info!("Falling back to Kotlin-side stop");
            if let Err(e2) = app.vpn().stop() {
                error!("Kotlin stop also failed: {e2}");
            }
        }
    }

    let mut conn = state.connection.write().await;
    *conn = ConnectionInfo::default();
    state.speed_tracker.write().await.reset();
    info!("Disconnected");
    Ok(())
}

/// Get current connection info with live traffic stats
#[tauri::command]
#[specta::specta]
pub async fn get_connection_info(
    state: State<'_, Arc<VpnState>>,
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<ConnectionInfo, String> {
    let mut conn = state.connection.write().await;

    let info = backend.get_all_info().await;
    let is_running = info.as_ref().is_some_and(|i| i.is_running);

    match conn.status {
        // Auto-detect: on Android the :vpn process can outlive the app.
        // If the tunnel is running, show Connected so the user can disconnect.
        ConnectionStatus::Disconnected if is_running => {
            let configs = state.configs.read().await;
            let config = configs.active_config();
            conn.status = ConnectionStatus::Connected;
            let connected_at = info
                .as_ref()
                .and_then(|i| i.connected_secs)
                .map(|secs| chrono::Utc::now().timestamp() - secs as i64)
                .unwrap_or_else(|| chrono::Utc::now().timestamp());
            conn.connected_at = Some(connected_at);
            conn.last_packet_received = info.as_ref().and_then(|i| i.last_packet_received);
            if let Some(ref cfg) = config {
                conn.protocol = Some(cfg.protocol_name().to_string());
                conn.server_endpoint = Some(cfg.endpoint_str().to_string());
                conn.assigned_ip = Some(cfg.address().to_string());
            }
            state.speed_tracker.write().await.reset();
            info!("Detected running tunnel, updated status to Connected");
        }
        // Tunnel died during connection verification
        ConnectionStatus::VerifyingConnection if !is_running => {
            *conn = ConnectionInfo::default();
            info!("Tunnel stopped during connection verification, reset to Disconnected");
        }
        // Tunnel dropped while connected
        ConnectionStatus::Connected if !is_running => {
            *conn = ConnectionInfo::default();
        }
        // Normal connected state — update stats
        ConnectionStatus::Connected if is_running => {
            if let Some(ref info) = info {
                if let Some(ref raw_stats) = info.stats {
                    let mut tracker = state.speed_tracker.write().await;
                    let (tx_speed, rx_speed) =
                        tracker.update(raw_stats.tx_bytes, raw_stats.rx_bytes);
                    conn.stats = super::state::TrafficStats {
                        tx_bytes: raw_stats.tx_bytes,
                        rx_bytes: raw_stats.rx_bytes,
                        tx_bytes_per_sec: tx_speed,
                        rx_bytes_per_sec: rx_speed,
                    };
                }
                conn.last_packet_received = info.last_packet_received;
            }
        }
        _ => {}
    }

    Ok(conn.clone())
}

/// Get list of installed apps for split tunneling (Android only)
#[tauri::command]
#[specta::specta]
pub async fn get_installed_apps(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<Vec<AppInfo>, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        let plugin_apps = app
            .vpn()
            .get_installed_apps()
            .map_err(|e| format!("Failed to get installed apps: {e}"))?;
        Ok(plugin_apps
            .into_iter()
            .map(|a| AppInfo {
                package_name: a.package_name,
                label: a.label,
                is_system: a.is_system,
                icon: a.icon,
            })
            .collect())
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(vec![])
    }
}

/// Check if battery optimization is disabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn is_battery_optimization_disabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .is_battery_optimization_disabled()
            .map_err(|e| format!("Failed to check battery optimization: {e}"));
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true) // Not applicable on desktop
    }
}

/// Request the user to disable battery optimization (Android only)
/// Returns whether battery optimization is now disabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn request_disable_battery_optimization(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .request_disable_battery_optimization()
            .map_err(|e| format!("Failed to request battery optimization: {e}"));
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Check if notifications are enabled (Android only)
#[tauri::command]
#[specta::specta]
pub async fn are_notifications_enabled(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .are_notifications_enabled()
            .map_err(|e| format!("Failed to check notifications: {e}"));
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Request notification permission (Android only)
/// Returns whether notifications are now enabled after the user responds.
#[tauri::command]
#[specta::specta]
pub async fn open_notification_settings(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .open_notification_settings()
            .map_err(|e| format!("Failed to request notification permission: {e}"));
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Set status bar icon style to match app theme (Android only)
#[tauri::command]
#[specta::specta]
pub async fn set_status_bar_style(
    #[allow(unused_variables)] app: AppHandle,
    #[allow(unused_variables)] is_dark: bool,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .set_status_bar_style(is_dark)
            .map_err(|e| format!("Failed to set status bar style: {e}"));
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

/// Get safe area insets (status bar, nav bar heights) in dp
#[tauri::command]
#[specta::specta]
pub async fn get_safe_area_insets(
    #[allow(unused_variables)] app: AppHandle,
) -> Result<SafeAreaInsets, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        let insets = app
            .vpn()
            .get_safe_area_insets()
            .map_err(|e| format!("Failed to get safe area insets: {e}"))?;
        Ok(SafeAreaInsets {
            top: insets.top,
            bottom: insets.bottom,
        })
    }

    #[cfg(not(target_os = "android"))]
    {
        Ok(SafeAreaInsets {
            top: 0.0,
            bottom: 0.0,
        })
    }
}
