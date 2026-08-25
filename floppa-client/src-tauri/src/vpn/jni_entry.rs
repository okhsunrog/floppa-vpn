//! JNI entry points for the `:vpn` process.
//!
//! These functions are called by `FloppaVpnService` (Kotlin) in the separate
//! VPN process. They initialize the Rust runtime, start/stop the WireGuard
//! or VLESS tunnel, and run the tarpc RPC server.

use super::rpc_server::{self, RpcServerHandle};
use super::tunnel::{self, TunnelManager};
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned, JavaVM};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, error, info, warn};

/// What a JNI entry point can fail with.
///
/// `nativeStartServer` resolves these into a `java.lang.RuntimeException`, so Kotlin sees a start
/// that failed as a thrown exception rather than as a call that returned normally.
#[derive(Debug, thiserror::Error)]
enum EntryError {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
    #[error("a global lock in the :vpn process is poisoned")]
    Poisoned,
    #[error("failed to start the RPC server: {0}")]
    ServerStart(String),
}

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

/// Log whatever a JNI entry point that returns nothing to Java ended with.
///
/// `with_env` already catches panics — unwinding across the JNI boundary aborts the process — so
/// what is left is to say what happened instead of discarding it.
fn log_outcome<T>(entry: &str, outcome: jni::Outcome<T, EntryError>) {
    match outcome {
        jni::Outcome::Ok(_) => {}
        jni::Outcome::Err(e) => error!("{entry} failed: {e}"),
        jni::Outcome::Panic(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            error!("{entry} panicked: {msg}");
        }
    }
}

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

/// Tell the service its tunnel is up, so the notification stops claiming it before it is.
///
/// Best-effort: a failure here costs a stale notification line, never the tunnel.
pub fn set_service_connected(connected: bool) {
    let Some(vm) = JAVA_VM.get() else {
        return;
    };
    let Ok(guard) = VPN_SERVICE_REF.lock() else {
        return;
    };
    let Some(service_ref) = guard.as_ref() else {
        return;
    };

    let result: Result<(), jni::errors::Error> = vm.attach_current_thread(|env| {
        env.call_method(
            service_ref.as_ref(),
            jni::jni_str!("setConnected"),
            jni::jni_sig!("(Z)V"),
            &[connected.into()],
        )?;
        Ok(())
    });

    if let Err(e) = result {
        warn!("failed to update the VPN notification: {e}");
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
    let outcome = env.with_env(|env: &mut Env<'local>| -> Result<(), EntryError> {
        // Store JavaVM for later JNI calls
        if JAVA_VM.get().is_none() {
            let vm = env.get_java_vm()?;
            let _ = JAVA_VM.set(vm);
        }

        // Initialize logging with file layer
        let log_dir_str: String = log_dir.mutf8_chars(env)?.to_string();
        crate::logging::init_tracing(
            std::path::Path::new(&log_dir_str),
            crate::logging::LogProcess::Vpn,
        );

        // Set a panic hook to ensure panics are logged to logcat
        std::panic::set_hook(Box::new(|info| {
            error!("{info}");
        }));

        info!("nativeInit: Rust runtime initialized in :vpn process");
        Ok(())
    });
    log_outcome("nativeInit", outcome.into_outcome());
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
///
/// A failure to bind is thrown to Kotlin as a `RuntimeException`. It used to be logged and
/// swallowed, and the service — already foreground and holding an established TUN with a default
/// route into it — carried on as if it had started, with nothing listening on the socket and no
/// in-band way to stop it.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStartServer<'local>(
    mut env: EnvUnowned<'local>,
    this: JObject<'local>,
    tun_fd: jint,
    socket_path: JString<'local>,
    epoch: jlong,
) {
    env.with_env(|env: &mut Env<'local>| -> Result<(), EntryError> {
        let socket_path_str: String = socket_path.mutf8_chars(env)?.to_string();
        let epoch = epoch as u64;
        info!("nativeStartServer: fd={tun_fd}, socket={socket_path_str}, epoch={epoch}");

        // Socket protection for gotatun (WireGuard) and shoes-lite (VLESS).
        tunnel::set_socket_protect_callback(protect_socket_jni);
        shoes_lite::api::set_socket_protector(Arc::new(ShoesSocketProtector));

        let runtime = get_runtime();
        let tunnel_manager = get_tunnel_manager();
        let service = Arc::new(rpc_server::ServiceState::new(epoch, tun_fd as RawFd));

        // Everything that can fail and does not need the bind comes before it, so that once the
        // socket is bound the only way out is the success path. (Should one of the locks below
        // still fail, the handle's `Drop` ends the accept loop rather than leaving it spinning.)
        let global_ref = env.new_global_ref(this)?;

        // Bind first. Nothing global is touched until the bind has succeeded, so a failed start
        // leaves the previous generation (if any) exactly as it was.
        let _enter = runtime.enter();
        let handle = rpc_server::start_server(&socket_path_str, tunnel_manager, service)
            .map_err(EntryError::ServerStart)?;

        // Store/update the VpnService reference for protect() and shutdown calls.
        *VPN_SERVICE_REF.lock().map_err(|_| EntryError::Poisoned)? = Some(global_ref);

        // Supersede the previous generation. Its socket file is ours now (see
        // `RpcServerHandle::shutdown`), so only the accept loop is stopped.
        let mut guard = RPC_HANDLE.lock().map_err(|_| EntryError::Poisoned)?;
        if let Some(old) = guard.take() {
            old.shutdown();
        }
        *guard = Some(handle);
        *SERVER_EPOCH.lock().map_err(|_| EntryError::Poisoned)? = epoch;

        info!("tarpc RPC server started, waiting for a tunnel request");
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// Called in `FloppaVpnService.onDestroy()` / `onRevoke()`.
///
/// Stops the tunnel and tarpc server, and releases the service reference. Runs inside `with_env`
/// so a panic is caught and logged rather than unwinding into the JVM, which aborts the process.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStop<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    epoch: jlong,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let epoch = epoch as u64;

        // Only tear down our own generation.
        //
        // Service instances share this process, and stopping one is asynchronous: the previous
        // instance's onDestroy routinely arrives after the next instance has already bound its
        // socket. Without this check it killed the new server roughly 150ms after it came up, and
        // the connect that was about to use it failed with "the connection was already shutdown"
        // — every time.
        let current = *SERVER_EPOCH.lock().map_err(|_| EntryError::Poisoned)?;
        if current != epoch {
            info!(
                "nativeStop: ignoring a stop for epoch {epoch}; this process now serves {current}"
            );
            return Ok(());
        }

        info!("nativeStop: stopping tunnel and RPC server (epoch {epoch})");

        // Shutdown RPC server. This generation owns the socket path (the epoch matched above and
        // `nativeStartServer` runs on the same main thread), so unlinking is safe here and only
        // here.
        if let Some(handle) = RPC_HANDLE.lock().map_err(|_| EntryError::Poisoned)?.take() {
            handle.shutdown_and_unlink();
        }

        // Stop tunnel
        let runtime = get_runtime();
        let tunnel_manager = get_tunnel_manager();
        runtime.block_on(async {
            if let Err(e) = tunnel_manager.stop().await {
                error!("Failed to stop tunnel: {e}");
            }
        });

        // The service instance is going away; a global reference kept past this point pinned a
        // destroyed service in the JVM for the life of the process.
        VPN_SERVICE_REF
            .lock()
            .map_err(|_| EntryError::Poisoned)?
            .take();

        info!("nativeStop: cleanup complete");
        Ok(())
    });
    log_outcome("nativeStop", outcome.into_outcome());
}
