use super::backend::VpnBackend;
use super::config as vpn_config;
use super::platform::{Platform, PlatformImpl};
use super::state::{
    AwgConfig, ConnectError, ConnectionInfo, ConnectionStatus, ProtocolConfig, SavedVpnConfigs,
    VlessVpnConfig, VpnState, WgConfig, config_str_is_amneziawg,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, State};
#[allow(unused_imports)]
use tracing::{error, info, warn};

static LOG_CAPTURE_STATE: OnceLock<Mutex<LogCaptureState>> = OnceLock::new();

#[derive(Default)]
struct LogCaptureState {
    active: Option<ActiveLogCapture>,
    latest_capture_id: Option<String>,
}

struct ActiveLogCapture {
    id: String,
    previous_config: crate::logging::LogConfig,
    capture_config: crate::logging::LogConfig,
    started_at: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct LogCaptureStatus {
    pub active: bool,
    pub capture_id: Option<String>,
}

/// Get the persistent device ID.
/// Android: ANDROID_ID (stable across reinstalls, per signing key).
/// Desktop: random UUID persisted in config dir.
#[tauri::command]
#[specta::specta]
pub fn get_device_id(#[allow(unused_variables)] app: AppHandle) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vpn::VpnExt;
        return app
            .vpn()
            .get_device_id()
            .map_err(|e| format!("Failed to get ANDROID_ID: {e}"));
    }

    #[cfg(not(target_os = "android"))]
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
    } else if config_str_is_amneziawg(&config_str) {
        let awg = AwgConfig::from_config_str(&config_str)?;
        configs.amneziawg = Some(awg);
        configs.active_protocol = "amneziawg".to_string();
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
            ProtocolConfig::AmneziaWg(awg) => awg.wg.dns.clone(),
            ProtocolConfig::Vless(vless) => vless.dns.clone(),
        },
        server_endpoint: c.endpoint_str().to_string(),
        allowed_ips: match c {
            ProtocolConfig::WireGuard(wg) => wg.allowed_ips.clone(),
            ProtocolConfig::AmneziaWg(awg) => awg.wg.allowed_ips.clone(),
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
        "amneziawg" if configs.amneziawg.is_some() => {
            configs.active_protocol = "amneziawg".to_string();
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
) -> Result<(), ConnectError> {
    let connect_start = std::time::Instant::now();
    info!("Connecting to VPN");

    // Guard: only allow connect from Disconnected
    {
        let conn = state.connection.read().await;
        match conn.status {
            ConnectionStatus::Disconnected => {}
            ConnectionStatus::Connecting | ConnectionStatus::VerifyingConnection => {
                return Err(ConnectError::busy("Already connecting"));
            }
            ConnectionStatus::Connected => return Err(ConnectError::busy("Already connected")),
            ConnectionStatus::Disconnecting => {
                return Err(ConnectError::busy("Disconnecting in progress"));
            }
        }
    }

    let proto_config = state
        .configs
        .read()
        .await
        .active_config()
        .ok_or_else(|| ConnectError::tunnel("No active config"))?;

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

    if result.is_ok() {
        info!(
            phase = "total",
            duration_ms = connect_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            "Total connect time"
        );
    }

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
) -> Result<(), ConnectError> {
    use tauri_plugin_vpn::VpnExt;

    let phase_start = std::time::Instant::now();
    let granted = app
        .vpn()
        .prepare()
        .map_err(|e| ConnectError::tunnel(format!("VPN prepare failed: {e}")))?;
    if !granted {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err(ConnectError::permission("VPN permission denied"));
    }

    // Serialize config for the Android VPN service (WG/AWG config text or vless:// URI)
    let protocol_config_str = match &config {
        ProtocolConfig::WireGuard(wg) => wg.to_config_str(),
        ProtocolConfig::AmneziaWg(awg) => awg.to_config_str(),
        ProtocolConfig::Vless(vless) => vless.uri.clone(),
    };

    let dns = match &config {
        ProtocolConfig::WireGuard(wg) => wg.dns.clone(),
        ProtocolConfig::AmneziaWg(awg) => awg.wg.dns.clone(),
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

    info!(
        phase = "vpn_prepare",
        duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "Android VPN prepared"
    );

    let phase_start = std::time::Instant::now();
    if let Err(e) = app.vpn().start(vpn_config) {
        error!("VPN start failed: {e}");
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err(ConnectError::tunnel(format!("VPN start failed: {e}")));
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
            return Err(ConnectError::tunnel("Connection timed out"));
        }
    }

    info!(
        phase = "tunnel_start",
        duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "Android tunnel started"
    );

    {
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::VerifyingConnection;
    }

    let phase_start = std::time::Instant::now();
    match &config {
        ProtocolConfig::WireGuard(_) | ProtocolConfig::AmneziaWg(_) => {
            info!("Tunnel up on Android, verifying handshake...");
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
                return Err(ConnectError::verify(
                    "Connection verification failed — config may be invalid",
                ));
            }
        }
        ProtocolConfig::Vless(vless) => {
            info!("Tunnel up on Android, verifying VLESS connectivity...");
            if let Err(e) =
                verify_vless_connectivity(vless, std::time::Duration::from_secs(10)).await
            {
                info!("VLESS connectivity check failed: {e}");
                if let Err(e) = backend.stop().await {
                    error!("Failed to stop tunnel after verification failure: {e}");
                }
                let mut conn = state.connection.write().await;
                *conn = ConnectionInfo::default();
                return Err(ConnectError::verify(format!(
                    "Connection verification failed: {e}"
                )));
            }
        }
    }

    info!(
        phase = "verify",
        duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "Connection verified"
    );

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
) -> Result<(), ConnectError> {
    use super::platform::Platform;

    let phase_start = std::time::Instant::now();
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
    info!(
        phase = "dns_resolve",
        duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "DNS resolution complete"
    );

    let phase_start = std::time::Instant::now();
    if let Err(e) = platform.prepare_tun(INTERFACE_NAME).await {
        error!("Failed to prepare TUN interface: {e}");
        let mut conn = state.connection.write().await;
        conn.status = ConnectionStatus::Disconnected;
        return Err(ConnectError::tunnel(format!(
            "Failed to prepare TUN interface: {e}"
        )));
    }

    info!(
        phase = "tun_prepare",
        duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        "TUN interface prepared"
    );

    let tun_params = platform.tun_params();

    // Try starting with fwmark; if it fails due to permissions, retry without.
    let phase_start = std::time::Instant::now();
    let start_result = match backend
        .start(&config, INTERFACE_NAME, &tun_params, endpoint)
        .await
    {
        Err(e)
            if tun_params.fwmark.is_some()
                && (e.contains("Operation not permitted") || e.contains("Permission denied")) =>
        {
            warn!("Tunnel start with fwmark failed due to permissions, retrying without fwmark");
            let mut retry_params = tun_params;
            retry_params.fwmark = None;
            backend
                .start(&config, INTERFACE_NAME, &retry_params, endpoint)
                .await
        }
        result => result,
    };

    match start_result {
        Ok(()) => {
            info!(
                phase = "tunnel_start",
                duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                "Tunnel started"
            );
            let addr = config.address_network()?;
            if let Err(e) = platform.configure_address(INTERFACE_NAME, addr).await {
                error!("Failed to configure address: {e}");
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(ConnectError::tunnel(e));
            }

            if let Err(e) = platform.add_endpoint_route(endpoint_ip).await {
                error!("Failed to add endpoint route: {e}");
                let _ = platform.cleanup(INTERFACE_NAME).await;
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(ConnectError::tunnel(e));
            }

            let allowed_ips = config.allowed_ips_networks();
            if let Err(e) = platform.add_routes(INTERFACE_NAME, &allowed_ips).await {
                error!("Failed to add routes: {e}");
                let _ = platform.cleanup(INTERFACE_NAME).await;
                let _ = backend.stop().await;
                let mut conn = state.connection.write().await;
                conn.status = ConnectionStatus::Disconnected;
                return Err(ConnectError::tunnel(e));
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

            let phase_start = std::time::Instant::now();
            match &config {
                ProtocolConfig::WireGuard(_) | ProtocolConfig::AmneziaWg(_) => {
                    info!("Tunnel up, verifying handshake...");
                    if wait_for_handshake(backend, std::time::Duration::from_secs(5))
                        .await
                        .is_err()
                    {
                        info!("No handshake after 5s — peer likely invalid, disconnecting");
                        let _ = platform.cleanup(INTERFACE_NAME).await;
                        let _ = backend.stop().await;
                        let mut conn = state.connection.write().await;
                        conn.status = ConnectionStatus::Disconnected;
                        return Err(ConnectError::verify(
                            "Connection verification failed — config may be invalid",
                        ));
                    }
                }
                ProtocolConfig::Vless(vless) => {
                    info!("Tunnel up, verifying VLESS connectivity...");
                    if let Err(e) =
                        verify_vless_connectivity(vless, std::time::Duration::from_secs(10)).await
                    {
                        info!("VLESS connectivity check failed: {e}");
                        let _ = platform.cleanup(INTERFACE_NAME).await;
                        let _ = backend.stop().await;
                        let mut conn = state.connection.write().await;
                        conn.status = ConnectionStatus::Disconnected;
                        return Err(ConnectError::verify(format!(
                            "Connection verification failed: {e}"
                        )));
                    }
                }
            }

            info!(
                phase = "verify",
                duration_ms = phase_start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                "Connection verified"
            );

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
            Err(ConnectError::tunnel(e))
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

/// Verify VLESS connectivity by making a test TCP connection through the proxy chain directly.
/// Bypasses TUN — proves: server reachable → REALITY handshake → UUID accepted → proxy works.
async fn verify_vless_connectivity(
    vless_config: &VlessVpnConfig,
    timeout: std::time::Duration,
) -> Result<(), String> {
    vless_config
        .to_shoes_config()
        .check_connectivity(timeout)
        .await
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
            conn.status = ConnectionStatus::Connected;
            info!("Detected running tunnel, updated status to Connected");

            // For VLESS, ping the tunnel in the background to update health dot.
            // VLESS has no keepalives, so last_packet_received may be stale.
            if config
                .as_ref()
                .is_some_and(|c| matches!(c, ProtocolConfig::Vless(_)))
            {
                let backend = backend.inner().clone();
                tokio::spawn(async move {
                    if let Err(e) = backend.ping().await {
                        warn!("Background VLESS ping failed: {e}");
                    }
                });
            }
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

/// Get the log directory path
#[tauri::command]
#[specta::specta]
pub fn get_log_dir() -> Result<String, String> {
    crate::get_log_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Log directory not initialized".to_string())
}

/// Get current diagnostic capture status.
#[tauri::command]
#[specta::specta]
pub fn get_log_capture_status() -> LogCaptureStatus {
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));
    state
        .lock()
        .map(|guard| LogCaptureStatus {
            active: guard.active.is_some(),
            capture_id: guard
                .active
                .as_ref()
                .map(|capture| capture.id.clone())
                .or_else(|| guard.latest_capture_id.clone())
                .or_else(|| {
                    crate::get_log_dir()
                        .and_then(|log_dir| latest_capture_dir(log_dir.as_path()))
                        .and_then(|path| {
                            path.file_name()
                                .map(|name| name.to_string_lossy().to_string())
                        })
                }),
        })
        .unwrap_or(LogCaptureStatus {
            active: false,
            capture_id: None,
        })
}

/// Start a diagnostic capture. This enables verbose runtime logs and starts
/// writing capture files without changing the user's saved profile permanently.
#[tauri::command]
#[specta::specta]
pub async fn start_log_capture(
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<LogCaptureStatus, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));

    {
        let guard = state.lock().map_err(|_| "Capture state poisoned")?;
        if let Some(active) = &guard.active {
            return Ok(LogCaptureStatus {
                active: true,
                capture_id: Some(active.id.clone()),
            });
        }
    }

    let capture_id = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let previous_config = crate::logging::get_log_config();
    let mut capture_config = previous_config.clone();
    capture_config.profile = crate::logging::LogProfile::Verbose;

    crate::logging::apply_log_config(&capture_config);
    backend.set_log_config(&capture_config).await;
    if let Err(e) = crate::logging::write_active_capture_id(log_dir, &capture_id) {
        crate::logging::apply_log_config(&previous_config);
        backend.set_log_config(&previous_config).await;
        return Err(e);
    }
    if let Err(e) = crate::logging::start_file_capture(log_dir, "ui", &capture_id) {
        crate::logging::clear_active_capture_id(log_dir);
        crate::logging::apply_log_config(&previous_config);
        backend.set_log_config(&previous_config).await;
        return Err(e);
    }
    backend.start_log_capture(&capture_id).await;

    info!(capture_id, "Diagnostic log capture started");

    {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.active = Some(ActiveLogCapture {
            id: capture_id.clone(),
            previous_config,
            capture_config,
            started_at: chrono::Local::now().to_rfc3339(),
        });
        guard.latest_capture_id = Some(capture_id.clone());
    }

    Ok(LogCaptureStatus {
        active: true,
        capture_id: Some(capture_id),
    })
}

/// Stop the active diagnostic capture and restore the previous runtime profile.
#[tauri::command]
#[specta::specta]
pub async fn stop_log_capture(
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<LogCaptureStatus, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let state = LOG_CAPTURE_STATE.get_or_init(|| Mutex::new(LogCaptureState::default()));
    let active = {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.active.take()
    };

    let Some(active) = active else {
        return Ok(get_log_capture_status());
    };

    info!(capture_id = active.id, "Diagnostic log capture stopping");
    backend.stop_log_capture().await;
    let _ = crate::logging::stop_file_capture();
    crate::logging::clear_active_capture_id(log_dir);

    crate::logging::apply_log_config(&active.previous_config);
    backend.set_log_config(&active.previous_config).await;

    write_capture_manifest(log_dir, &active)?;
    cleanup_old_captures(log_dir);

    {
        let mut guard = state.lock().map_err(|_| "Capture state poisoned")?;
        guard.latest_capture_id = Some(active.id.clone());
    }

    Ok(LogCaptureStatus {
        active: false,
        capture_id: Some(active.id),
    })
}

/// Export latest diagnostic capture as a tar.gz archive via native save dialog.
/// Returns `true` if saved successfully, `false` if the user cancelled.
#[tauri::command]
#[specta::specta]
pub async fn export_logs(app: AppHandle) -> Result<bool, String> {
    let log_dir = crate::get_log_dir().ok_or("Log directory not initialized")?;
    let capture_dir = latest_capture_dir(log_dir).ok_or("No diagnostic captures found")?;
    let capture_id = capture_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or("Invalid capture directory")?;
    let archive_buf = build_log_archive(&capture_dir)?;

    let filename = format!("floppa-logs-{capture_id}.tar.gz");

    #[cfg(not(target_os = "android"))]
    {
        use tauri_plugin_dialog::DialogExt;

        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .file()
            .set_file_name(&filename)
            .add_filter("Archive", &["tar.gz", "gz"])
            .save_file(move |path| {
                let _ = tx.send(path);
            });

        let file_path = rx.await.map_err(|_| "Dialog closed unexpectedly")?;
        let Some(file_path) = file_path else {
            return Ok(false);
        };

        let path = file_path
            .into_path()
            .map_err(|e| format!("Invalid save path: {e}"))?;

        std::fs::write(&path, &archive_buf).map_err(|e| format!("Failed to write archive: {e}"))?;
    }

    #[cfg(target_os = "android")]
    {
        use tauri_plugin_android_fs::AndroidFsExt;

        let api = app.android_fs_async();
        let uri = api
            .file_picker()
            .save_file(None, &filename, Some("application/gzip"), false)
            .await
            .map_err(|e| format!("Save dialog failed: {e}"))?;

        let Some(uri) = uri else {
            return Ok(false);
        };

        api.write(&uri, &archive_buf)
            .await
            .map_err(|e| format!("Failed to write archive: {e}"))?;
    }

    Ok(true)
}

fn latest_capture_dir(log_dir: &Path) -> Option<PathBuf> {
    let captures_dir = log_dir.join("captures");
    let mut dirs = std::fs::read_dir(captures_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.pop()
}

fn write_capture_manifest(log_dir: &Path, capture: &ActiveLogCapture) -> Result<(), String> {
    let capture_dir = log_dir.join("captures").join(&capture.id);
    let stopped_at = chrono::Local::now().to_rfc3339();

    let log_config_json = serde_json::to_vec_pretty(&capture.capture_config)
        .map_err(|e| format!("Failed to serialize capture log config: {e}"))?;
    std::fs::write(capture_dir.join("log-config.json"), log_config_json)
        .map_err(|e| format!("Failed to write capture log config: {e}"))?;

    let manifest = serde_json::json!({
        "schema_version": 1,
        "capture_id": capture.id,
        "started_at": capture.started_at,
        "stopped_at": stopped_at,
        "app_version": env!("CARGO_PKG_VERSION"),
        "profile_during_capture": capture.capture_config.profile.clone(),
        "custom_filter_enabled": capture.capture_config.custom_filter_enabled,
        "custom_filter": capture.capture_config.custom_filter.clone(),
        "files": capture_file_entries(&capture_dir),
    });

    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize capture manifest: {e}"))?;
    std::fs::write(capture_dir.join("manifest.json"), manifest_json)
        .map_err(|e| format!("Failed to write capture manifest: {e}"))?;
    Ok(())
}

fn capture_file_entries(capture_dir: &Path) -> Vec<serde_json::Value> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(capture_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Ok(metadata) = path.metadata()
                && let Some(name) = path.file_name()
            {
                files.push(serde_json::json!({
                    "name": name.to_string_lossy(),
                    "bytes": metadata.len(),
                }));
            }
        }
    }
    files.sort_by_key(|entry| {
        entry
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or_default()
            .to_string()
    });
    files
}

fn cleanup_old_captures(log_dir: &Path) {
    let captures_dir = log_dir.join("captures");
    let Ok(entries) = std::fs::read_dir(&captures_dir) else {
        return;
    };

    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let mut dirs = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();

    let keep_from = dirs.len().saturating_sub(3);
    for (idx, path) in dirs.iter().enumerate() {
        let old_by_count = idx < keep_from;
        let old_by_age = path
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if old_by_count || old_by_age {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn build_log_archive(capture_dir: &Path) -> Result<Vec<u8>, String> {
    let mut archive_buf = Vec::new();
    {
        let gz_encoder =
            flate2::write::GzEncoder::new(&mut archive_buf, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(gz_encoder);

        let entries =
            std::fs::read_dir(capture_dir).map_err(|e| format!("Failed to read capture: {e}"))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name()
            {
                tar_builder
                    .append_path_with_name(&path, name)
                    .map_err(|e| format!("Failed to add file to archive: {e}"))?;
            }
        }

        tar_builder
            .finish()
            .map_err(|e| format!("Failed to finalize archive: {e}"))?;
    }
    Ok(archive_buf)
}

/// Get the current log configuration.
#[tauri::command]
#[specta::specta]
pub fn get_log_config() -> crate::logging::LogConfig {
    crate::logging::get_log_config()
}

/// Apply a new log configuration. Persists to disk and propagates to VPN process.
#[tauri::command]
#[specta::specta]
pub async fn set_log_config(
    config: crate::logging::LogConfig,
    backend: State<'_, Arc<dyn VpnBackend>>,
) -> Result<(), String> {
    crate::logging::apply_log_config(&config);
    crate::logging::save_log_config_to_disk(&config);
    backend.set_log_config(&config).await;
    info!("Log config updated");
    Ok(())
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
