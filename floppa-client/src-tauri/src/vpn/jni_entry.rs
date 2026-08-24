//! JNI entry points for the `:vpn` process.
//!
//! These functions are called by `FloppaVpnService` (Kotlin) in the separate
//! VPN process. They initialize the Rust runtime, start/stop the WireGuard
//! or VLESS tunnel, and run the tarpc RPC server.

use super::rpc_server::{self, RpcServerHandle};
use super::tunnel::{self, TunnelManager};
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jlong};
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

/// Generation of the service instance that owns [`RPC_HANDLE`].
///
/// The `:vpn` process outlives the individual service instances inside it, so this global is
/// shared between them. Stopping is asynchronous: a previous instance's `onDestroy` can arrive
/// *after* the next one has already bound its socket, and without this it would tear down a server
/// it does not own.
static SERVER_EPOCH: Mutex<u64> = Mutex::new(0);

fn get_runtime() -> &'static tokio::runtime::Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime")
    })
}

fn get_tunnel_manager() -> Arc<TunnelManager> {
    TUNNEL_MANAGER.get_or_init(TunnelManager::new).clone()
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
            &[fd.into()],
        )?;
        result.z()
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
    log_dir: JString<'local>,
) {
    let _ = env.with_env(|env: &mut Env<'local>| -> Result<(), jni::errors::Error> {
        // Store JavaVM for later JNI calls
        if JAVA_VM.get().is_none() {
            let vm = env.get_java_vm()?;
            let _ = JAVA_VM.set(vm);
        }

        // Initialize logging with file layer
        let log_dir_str: String = log_dir.mutf8_chars(env)?.to_string();
        crate::logging::init_tracing_vpn_process(std::path::Path::new(&log_dir_str));

        // Set a panic hook to ensure panics are logged to logcat
        std::panic::set_hook(Box::new(|info| {
            error!("{info}");
        }));

        info!("nativeInit: Rust runtime initialized in :vpn process");
        Ok(())
    });
}

/// Bridges shoes-lite's SocketProtector trait to the JNI VpnService.protect() callback.
struct ShoesSocketProtector;

impl shoes_lite::tun::SocketProtector for ShoesSocketProtector {
    fn protect(&self, fd: RawFd) -> std::io::Result<()> {
        if protect_socket_jni(fd) {
            Ok(())
        } else {
            Err(std::io::Error::other("VpnService.protect() failed"))
        }
    }
}

/// Called in `FloppaVpnService.onStartCommand()` after TUN interface creation.
///
/// Binds the RPC server and stops there. The tunnel itself is started by a later
/// [`start_tunnel`](crate::vpn::rpc::VpnRpc) call carrying a typed config.
///
/// That ordering is the point. Previously the tunnel was started first and the socket bound
/// afterwards, so a failed start left nothing listening — indistinguishable from a service that
/// had not come up yet. The only recourse was a blind timeout, and the reason for the failure was
/// logged here and never reached the caller. Binding first makes "up and idle", "failed, and here
/// is why" and "not up at all" three different observations.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStartServer<'local>(
    mut env: EnvUnowned<'local>,
    this: JObject<'local>,
    tun_fd: jint,
    socket_path: JString<'local>,
    epoch: jlong,
) {
    let _ = env.with_env(|env: &mut Env<'local>| -> Result<(), jni::errors::Error> {
        // Store/update VpnService reference for protect() calls
        {
            let global_ref = env.new_global_ref(this)?;
            if let Ok(mut guard) = VPN_SERVICE_REF.lock() {
                *guard = Some(global_ref);
            }
        }

        let socket_path_str: String = socket_path.mutf8_chars(env)?.to_string();
        let epoch = epoch as u64;
        info!("nativeStartServer: fd={tun_fd}, socket={socket_path_str}, epoch={epoch}");

        // Socket protection for gotatun (WireGuard) and shoes-lite (VLESS).
        tunnel::set_socket_protect_callback(protect_socket_jni);
        shoes_lite::api::set_socket_protector(Arc::new(ShoesSocketProtector));

        let runtime = get_runtime();
        let tunnel_manager = get_tunnel_manager();
        let service = Arc::new(rpc_server::ServiceState::new(epoch, tun_fd as RawFd));

        runtime.block_on(async {
            match rpc_server::start_server(&socket_path_str, tunnel_manager.clone(), service) {
                Ok(handle) => {
                    if let Ok(mut guard) = RPC_HANDLE.lock() {
                        if let Some(old) = guard.take() {
                            old.shutdown();
                        }
                        *guard = Some(handle);
                    }
                    if let Ok(mut guard) = SERVER_EPOCH.lock() {
                        *guard = epoch;
                    }
                    info!("tarpc RPC server started, waiting for a tunnel request");
                }
                Err(e) => error!("Failed to start tarpc server: {e}"),
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
    epoch: jlong,
) {
    let epoch = epoch as u64;

    // Only tear down our own generation.
    //
    // Service instances share this process, and stopping one is asynchronous: the previous
    // instance's onDestroy routinely arrives after the next instance has already bound its socket.
    // Without this check it killed the new server roughly 150ms after it came up, and the connect
    // that was about to use it failed with "the connection was already shutdown" — every time.
    if let Ok(current) = SERVER_EPOCH.lock()
        && *current != epoch
    {
        info!(
            "nativeStop: ignoring a stop for epoch {epoch}; this process now serves {}",
            *current
        );
        return;
    }

    info!("nativeStop: stopping tunnel and RPC server (epoch {epoch})");

    // Shutdown RPC server
    if let Ok(mut guard) = RPC_HANDLE.lock()
        && let Some(handle) = guard.take()
    {
        handle.shutdown();
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
