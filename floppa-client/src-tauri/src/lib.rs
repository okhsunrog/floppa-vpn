pub mod logging;
pub mod vpn;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;
#[allow(unused_imports)]
use tracing::{info, warn};
#[cfg(not(target_os = "android"))]
use vpn::create_backend;
use vpn::{PlatformImpl, get_platform};

/// Log directory, set once at startup. Used by log export commands.
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn get_log_dir() -> Option<&'static PathBuf> {
    LOG_DIR.get()
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, serde::Serialize)]
struct SingleInstancePayload {
    args: Vec<String>,
    cwd: String,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Setup Specta builder and register commands/events
    let specta_builder = tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            vpn::commands::get_device_id,
            vpn::commands::get_device_name,
            vpn::commands::tunnel_set_intent_up,
            vpn::commands::tunnel_set_intent_down,
            vpn::commands::tunnel_await_cycle,
            vpn::commands::tunnel_get_state,
            vpn::commands::import_config,
            vpn::commands::list_configs,
            vpn::commands::clear_configs,
            vpn::commands::forget_preferred_protocol,
            vpn::commands::get_installed_apps,
            vpn::commands::is_battery_optimization_disabled,
            vpn::commands::request_disable_battery_optimization,
            vpn::commands::are_notifications_enabled,
            vpn::commands::open_notification_settings,
            vpn::commands::get_safe_area_insets,
            vpn::commands::set_status_bar_style,
            vpn::commands::get_log_dir,
            vpn::commands::get_log_capture_status,
            vpn::commands::start_log_capture,
            vpn::commands::stop_log_capture,
            vpn::commands::export_logs,
            vpn::commands::get_log_config,
            vpn::commands::set_log_config,
        ])
        .events(tauri_specta::collect_events![])
        // specta rc.25 forbids exporting BigInt-style types (i64/u64/usize/…) by default.
        // We export them as TS `number` (as before the upgrade) — IDs/byte counters stay numbers.
        .dangerously_cast_bigints_to_number();

    // Export TypeScript bindings in debug mode on desktop.
    // Use if-let to avoid panicking when launched from a different working directory
    // (e.g. via deep-link handler where the second instance only needs single-instance relay).
    #[cfg(all(debug_assertions, not(target_os = "android")))]
    {
        if let Err(err) = specta_builder.export(
            specta_typescript::Typescript::default().header("/* eslint-disable */\n// @ts-nocheck"),
            "../src/bindings.ts",
        ) {
            eprintln!("Warning: Failed to export TypeScript bindings: {err}");
        }
    }

    // The platform is stateless and shared; the tunnel state lives inside the actor.
    let platform: Arc<PlatformImpl> = Arc::new(get_platform());

    // Create VPN backend (desktop — Android backend is created in setup() where app paths are available)
    #[cfg(not(target_os = "android"))]
    let backend = create_backend();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri::Emitter;
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            info!("Single-instance callback received: argv={argv:?}, cwd={cwd}");
            let payload = SingleInstancePayload { args: argv, cwd };
            if let Err(err) = app.emit("single-instance", payload) {
                warn!("Failed to emit single-instance event: {err}");
            }
        }));
    }

    // Register desktop backend before builder chain (Android backend registered in setup())
    #[cfg(not(target_os = "android"))]
    {
        builder = builder.manage(backend);
    }

    builder = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_log::Builder::new().skip_logger().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .manage(platform.clone())
        .invoke_handler(specta_builder.invoke_handler());

    // Desktop-only: fs and dialog plugins
    #[cfg(not(target_os = "android"))]
    {
        builder = builder
            .plugin(tauri_plugin_fs::init())
            .plugin(tauri_plugin_dialog::init());
    }

    // Android-only: android_fs plugin
    #[cfg(target_os = "android")]
    {
        builder = builder.plugin(tauri_plugin_android_fs::init());
    }

    // Desktop-only plugins (not needed on mobile)
    #[cfg(not(mobile))]
    {
        builder = builder
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))
            .plugin(tauri_plugin_process::init());
    }

    // Add VPN plugin on mobile platforms
    #[cfg(mobile)]
    {
        builder = builder.plugin(tauri_plugin_vpn::init());
    }

    let app = builder
        .setup(move |#[allow(unused_variables)] app| {
            // Set up log directory and init tracing with file logging
            let log_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data dir")
                .join("logs");
            logging::init_tracing(&log_dir);
            let _ = LOG_DIR.set(log_dir);
            info!("Logging initialized.");

            // Initialize config dir from Tauri path resolver
            if let Ok(config_dir) = app.path().app_config_dir() {
                vpn::config::init_config_dir(config_dir);
            }

            #[cfg(not(mobile))]
            {
                // Registering repoints the system-wide floppa:// handler at the current binary.
                // A debug build doing that hijacks deep links from the installed release app
                // (and a dev binary needs the vite dev server to even render), so debug builds
                // only register when explicitly asked to via FLOPPA_REGISTER_DEEP_LINK=1.
                if !cfg!(debug_assertions) || std::env::var_os("FLOPPA_REGISTER_DEEP_LINK").is_some()
                {
                    match app.deep_link().register_all() {
                        Ok(()) => info!("Deep-link schemes registered."),
                        Err(err) => warn!("Failed to register deep-link schemes: {err}"),
                    }
                } else {
                    info!(
                        "Debug build: skipping deep-link scheme registration (set FLOPPA_REGISTER_DEEP_LINK=1 to register)."
                    );
                }
            }

            app.deep_link().on_open_url(|event| {
                let urls = event.urls();
                info!("Deep-link event received in Rust runtime: {urls:?}");
            });

            // On Android, create the tarpc IPC backend now that we have the real data dir
            #[cfg(target_os = "android")]
            {
                let data_dir = app
                    .path()
                    .app_data_dir()
                    .expect("Failed to get app data dir");
                let socket_path = data_dir
                    .join(vpn::rpc::SOCKET_NAME)
                    .to_string_lossy()
                    .to_string();
                info!("Android tarpc socket path: {socket_path}");
                let backend = vpn::create_backend(socket_path);
                app.manage(backend);
            }

            // Spawn the tunnel actor. From here on it is the only thing that touches the tunnel:
            // every command is a message to it, and the published state is the only thing the UI
            // reads. Crash recovery and adoption of a surviving tunnel happen in its bootstrap.
            {
                let backend = app.state::<Arc<dyn vpn::VpnBackend>>().inner().clone();
                let platform = app.state::<Arc<PlatformImpl>>().inner().clone();
                let journal = vpn::config::config_dir().ok().map(|dir| {
                    vpn::rollback::Journal::new(vpn::rollback::Journal::default_path(&dir))
                });

                let handle = vpn::actor::TunnelActor::spawn(
                    backend,
                    platform,
                    journal,
                    #[cfg(target_os = "android")]
                    app.handle().clone(),
                );
                app.manage(handle);
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(
        |#[allow(unused_variables)] app_handle, #[allow(unused_variables)] event| {
            // Graceful teardown on desktop exit.
            //
            // Asking the actor to go down and waiting for it to actually be down, rather than
            // reaching past it into the tunnel: it holds the record of what was applied, so it is
            // the only thing that can undo exactly that. The previous version was gated on the
            // tunnel still reporting as running, which left routes and DNS behind whenever the
            // tunnel had already died.
            #[cfg(not(target_os = "android"))]
            if let tauri::RunEvent::Exit = event {
                use vpn::actor::handle::TunnelHandle;

                let handle = app_handle.state::<TunnelHandle>().inner().clone();
                tauri::async_runtime::block_on(async move {
                    let _ = handle
                        .set_intent(vpn::actor::handle::IntentRequest::Down)
                        .await;
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        handle.await_quiescent(),
                    )
                    .await
                    {
                        Ok(()) => info!("VPN cleanup complete"),
                        Err(_) => warn!("timed out waiting for the tunnel to settle on exit"),
                    }
                });
            }
        },
    );
}
