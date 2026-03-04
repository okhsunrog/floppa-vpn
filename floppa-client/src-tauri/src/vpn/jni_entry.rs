//! JNI entry points for the `:vpn` process.
//!
//! These functions are called by `FloppaVpnService` (Kotlin) in the separate
//! VPN process. They initialize the Rust runtime, start/stop the WireGuard
//! tunnel, and run the tarpc RPC server.

use super::rpc_server::{self, RpcServerHandle};
use super::state::WgConfig;
use super::tunnel::{self, TunnelManager};
use jni::objects::{JClass, JObject, JString};
use jni::sys::jint;
use jni::{Env, EnvUnowned, JavaVM};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, error, info, warn};

/// Global state for the VPN process
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
/// VpnService reference — Mutex so it can be updated when Android restarts the service
static VPN_SERVICE_REF: Mutex<Option<jni::objects::Global<JObject<'static>>>> = Mutex::new(None);
static TOKIO_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static TUNNEL_MANAGER: OnceLock<Arc<TunnelManager>> = OnceLock::new();
static RPC_HANDLE: Mutex<Option<RpcServerHandle>> = Mutex::new(None);

fn get_runtime() -> &'static tokio::runtime::Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime")
    })
}

fn get_tunnel_manager() -> Arc<TunnelManager> {
    TUNNEL_MANAGER
        .get_or_init(|| TunnelManager::new())
        .clone()
}

/// Protect a socket fd using VpnService.protect() via JNI.
///
/// Called from `AndroidUdpSocketFactory` when creating UDP sockets.
fn protect_socket_jni(fd: RawFd) -> bool {
    let vm = match JAVA_VM.get() {
        Some(vm) => vm,
        None => {
            error!("JavaVM not initialized");
            return false;
        }
    };
    let guard = match VPN_SERVICE_REF.lock() {
        Ok(g) => g,
        Err(_) => {
            error!("VPN_SERVICE_REF lock poisoned");
            return false;
        }
    };
    let service_ref = match guard.as_ref() {
        Some(r) => r,
        None => {
            error!("VpnService reference not set");
            return false;
        }
    };

    let result: Result<bool, jni::errors::Error> = vm.attach_current_thread(|env| {
        let result = env.call_method(
            service_ref.as_ref(),
            jni::jni_str!("protectSocket"),
            jni::jni_sig!("(I)Z"),
            &[(fd as i32).into()],
        )?;
        Ok(result.z()?)
    });

    match result {
        Ok(protected) => {
            if protected {
                debug!("Protected socket fd {fd}");
            } else {
                warn!("Failed to protect socket fd {fd}");
            }
            protected
        }
        Err(e) => {
            error!("JNI call to protectSocket failed: {e}");
            false
        }
    }
}

/// Stop the Android VPN service via JNI.
///
/// Calls `FloppaVpnService.shutdownService()` which handles stopForeground,
/// TUN close, and stopSelf. Called from the RPC `stop` handler after the
/// tunnel and RPC server are already stopped.
pub fn stop_vpn_service() {
    let vm = match JAVA_VM.get() {
        Some(vm) => vm,
        None => {
            warn!("stop_vpn_service: JavaVM not initialized");
            return;
        }
    };
    let guard = match VPN_SERVICE_REF.lock() {
        Ok(g) => g,
        Err(_) => {
            error!("stop_vpn_service: VPN_SERVICE_REF lock poisoned");
            return;
        }
    };
    let service_ref = match guard.as_ref() {
        Some(r) => r,
        None => {
            warn!("stop_vpn_service: VpnService reference not set");
            return;
        }
    };

    let result: Result<(), jni::errors::Error> = vm.attach_current_thread(|env| {
        env.call_method(
            service_ref.as_ref(),
            jni::jni_str!("shutdownService"),
            jni::jni_sig!("()V"),
            &[],
        )?;
        Ok(())
    });

    match result {
        Ok(()) => info!("VPN service shutdownService() called via JNI"),
        Err(e) => error!("Failed to call VPN service shutdownService(): {e}"),
    }
}

/// Called once in `FloppaVpnService.onCreate()`.
///
/// Initializes the Rust runtime, logging, and stores the JavaVM reference.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeInit<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    let _ = env.with_env(|env: &mut Env<'local>| -> Result<(), jni::errors::Error> {
        // Store JavaVM for later JNI calls
        if JAVA_VM.get().is_none() {
            let vm = env.get_java_vm()?;
            let _ = JAVA_VM.set(vm);
        }

        // Initialize logging via tracing-logcat
        {
            use tracing_logcat::{LogcatMakeWriter, LogcatTag};
            use tracing_subscriber::prelude::*;
            use tracing_subscriber::EnvFilter;

            let tag = LogcatTag::Fixed("FloppaVPN".to_owned());
            if let Ok(writer) = LogcatMakeWriter::new(tag) {
                let filter = EnvFilter::from_default_env();

                #[cfg(debug_assertions)]
                let filter = filter
                    .add_directive("floppa_client_lib=trace".parse().unwrap())
                    .add_directive("debug".parse().unwrap());

                #[cfg(not(debug_assertions))]
                let filter = filter
                    .add_directive("floppa_client_lib=debug".parse().unwrap())
                    .add_directive("gotatun=info".parse().unwrap())
                    .add_directive("tarpc=warn".parse().unwrap())
                    .add_directive("warn".parse().unwrap());

                let logcat_layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_file(false)
                    .with_line_number(false)
                    .with_writer(writer);

                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(logcat_layer)
                    .try_init();
            }
        }

        // Set a panic hook to ensure panics are logged to logcat
        std::panic::set_hook(Box::new(|info| {
            error!("{info}");
        }));

        info!("nativeInit: Rust runtime initialized in :vpn process");
        Ok(())
    });
}

/// Called in `FloppaVpnService.onStartCommand()` after TUN interface creation.
///
/// Starts the WireGuard tunnel and tarpc RPC server.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStartTunnel<'local>(
    mut env: EnvUnowned<'local>,
    this: JObject<'local>,
    tun_fd: jint,
    wg_config: JString<'local>,
    socket_path: JString<'local>,
) {
    let _ = env.with_env(|env: &mut Env<'local>| -> Result<(), jni::errors::Error> {
        // Store/update VpnService reference for protect() calls
        {
            let global_ref = env.new_global_ref(this)?;
            if let Ok(mut guard) = VPN_SERVICE_REF.lock() {
                *guard = Some(global_ref);
            }
        }

        // Extract Java strings
        let wg_config_str: String = wg_config.mutf8_chars(env)?.to_string();
        let socket_path_str: String = socket_path.mutf8_chars(env)?.to_string();

        info!(
            "nativeStartTunnel: fd={tun_fd}, socket={socket_path_str}"
        );

        // Set up socket protection callback (local JNI, no cross-process IPC)
        tunnel::set_socket_protect_callback(protect_socket_jni);

        let runtime = get_runtime();
        let tunnel_manager = get_tunnel_manager();

        // Parse config and start tunnel
        runtime.block_on(async {
            let config = match WgConfig::from_config_str(&wg_config_str) {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to parse WireGuard config: {e}");
                    return;
                }
            };

            if let Err(e) = tunnel_manager.start_with_fd(&config, tun_fd as RawFd).await {
                error!("Failed to start tunnel: {e}");
                return;
            }

            info!("Tunnel started successfully");

            // Start tarpc RPC server
            match rpc_server::start_server(&socket_path_str, tunnel_manager.clone()) {
                Ok(handle) => {
                    if let Ok(mut guard) = RPC_HANDLE.lock() {
                        if let Some(old) = guard.take() {
                            old.shutdown();
                        }
                        *guard = Some(handle);
                    }
                    info!("tarpc RPC server started");
                }
                Err(e) => {
                    error!("Failed to start tarpc server: {e}");
                }
            }
        });

        Ok(())
    });
}

/// Called in `FloppaVpnService.onDestroy()` / `onRevoke()`.
///
/// Stops the tunnel and tarpc server.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStop<'local>(
    _env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    info!("nativeStop: stopping tunnel and RPC server");

    // Shutdown RPC server
    if let Ok(mut guard) = RPC_HANDLE.lock() {
        if let Some(handle) = guard.take() {
            handle.shutdown();
        }
    }

    // Stop tunnel
    let runtime = get_runtime();
    let tunnel_manager = get_tunnel_manager();
    runtime.block_on(async {
        if let Err(e) = tunnel_manager.stop().await {
            error!("Failed to stop tunnel: {e}");
        }
    });

    info!("nativeStop: cleanup complete");
}
