//! JNI entry points for the `:vpn` process.
//!
//! These functions are called by `FloppaVpnService` (Kotlin) in the separate
//! VPN process. They initialize the Rust runtime, start/stop the WireGuard
//! or VLESS tunnel, and run the tarpc RPC server.
//!
//! Two of them exist for the service to bring a tunnel up with no UI process at all — the
//! always-on, boot and lockdown starts the system issues with an empty intent:
//! `nativeLoadAutostart` reads the bundle the last successful connect wrote and says what TUN to
//! build, and `nativeStartTunnelFromBundle` starts the tunnel on it through the same path the RPC
//! `start_tunnel` uses.

use super::autostart::{self, AutostartBundle};
use super::rpc_server::{self, RpcServerHandle, ServiceState, Started};
use super::tunnel::{self, TunnelManager};
use crate::vpn::actor::types::TunnelParams;
use crate::vpn::state::ProtocolConfig;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned, JavaVM};
use std::net::SocketAddr;
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
    #[error("the autostart bundle cannot be encoded for the service: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("no autostart plan is prepared for generation {generation}")]
    NoPlan { generation: u64 },
    #[error("no service generation is serving yet")]
    NoService,
    #[error("generation {generation} is not serving (this process serves {current})")]
    WrongGeneration { generation: u64, current: u64 },
}

/// Global state for the VPN process
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
/// VpnService reference — Mutex so it can be updated when Android restarts the service
static VPN_SERVICE_REF: Mutex<Option<jni::objects::Global<JObject<'static>>>> = Mutex::new(None);
static TOKIO_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static TUNNEL_MANAGER: OnceLock<Arc<TunnelManager>> = OnceLock::new();
static RPC_HANDLE: Mutex<Option<RpcServerHandle>> = Mutex::new(None);
/// The generation behind [`RPC_HANDLE`], for a start that comes from inside this process rather
/// than over the socket.
static SERVICE_STATE: Mutex<Option<Arc<ServiceState>>> = Mutex::new(None);

/// What `nativeLoadAutostart` decided, held for the `nativeStartTunnelFromBundle` that follows
/// it — so the tunnel is built from exactly what the service was told to build a TUN for, with
/// no second read of the file in between.
struct PreparedAutostart {
    generation: u64,
    config: ProtocolConfig,
    endpoint: SocketAddr,
    params: TunnelParams,
}

static AUTOSTART: Mutex<Option<PreparedAutostart>> = Mutex::new(None);

/// How long an autonomous start waits for the resolver before dialling the stored literal. Under
/// lockdown nothing is allowed on the network yet, so this is mostly the time it takes to fail.
const AUTOSTART_RESOLVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Generation of the service instance that owns [`RPC_HANDLE`], or [`NO_GENERATION`] when none
/// does.
///
/// The `:vpn` process outlives the individual service instances inside it, so this global is
/// shared between them. Stopping is asynchronous: a previous instance's `onDestroy` can arrive
/// *after* the next one has already bound its socket, and without this it would tear down a server
/// it does not own. It is reset to the sentinel on every teardown, so a `nativeStop` that arrives
/// after its generation has gone matches nothing rather than matching whatever came next.
static SERVER_GENERATION: Mutex<u64> = Mutex::new(NO_GENERATION);

/// "Nothing is being served". No generation is ever minted as this — a UI one comes from
/// [`ServiceGenerations`](crate::vpn::autostart::ServiceGenerations) and counts up from a random
/// base, an autonomous one from the reserved range — so it can never be matched by mistake.
const NO_GENERATION: u64 = 0;

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
    socket_path: JString<'local>,
    generation: jlong,
) {
    env.with_env(|env: &mut Env<'local>| -> Result<(), EntryError> {
        let socket_path_str: String = socket_path.mutf8_chars(env)?.to_string();
        let generation = generation as u64;
        info!("nativeStartServer: socket={socket_path_str}, generation={generation}");

        // Socket protection for gotatun (WireGuard) and shoes-lite (VLESS).
        tunnel::set_socket_protect_callback(protect_socket_jni);
        shoes_lite::api::set_socket_protector(Arc::new(ShoesSocketProtector));

        let runtime = get_runtime();
        let tunnel_manager = get_tunnel_manager();
        // No descriptor yet: the socket is bound before `establish()` runs, so that a TUN that
        // cannot be established is reported over it (`nativeReportStartError`) instead of
        // leaving the UI to time out on a socket nobody bound.
        let service = Arc::new(rpc_server::ServiceState::new(generation));

        // Everything that can fail and does not need the bind comes before it, so that once the
        // socket is bound the only way out is the success path. (Should one of the locks below
        // still fail, the handle's `Drop` ends the accept loop rather than leaving it spinning.)
        let global_ref = env.new_global_ref(this)?;

        // Bind first. Nothing global is touched until the bind has succeeded, so a failed start
        // leaves the previous generation (if any) exactly as it was.
        let _enter = runtime.enter();
        let handle = rpc_server::start_server(&socket_path_str, tunnel_manager, service.clone())
            .map_err(EntryError::ServerStart)?;

        // Store/update the VpnService reference for protect() and shutdown calls.
        *VPN_SERVICE_REF.lock().map_err(|_| EntryError::Poisoned)? = Some(global_ref);
        *SERVICE_STATE.lock().map_err(|_| EntryError::Poisoned)? = Some(service);

        // Supersede the previous generation. Its socket file is ours now (see
        // `RpcServerHandle::shutdown`), so only the accept loop is stopped.
        let mut guard = RPC_HANDLE.lock().map_err(|_| EntryError::Poisoned)?;
        if let Some(old) = guard.take() {
            old.shutdown();
        }
        *guard = Some(handle);
        *SERVER_GENERATION.lock().map_err(|_| EntryError::Poisoned)? = generation;

        info!("tarpc RPC server started, waiting for the TUN and a tunnel request");
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// The generation currently serving, if it is `generation`.
fn serving(generation: u64) -> Result<Arc<ServiceState>, EntryError> {
    let current = *SERVER_GENERATION.lock().map_err(|_| EntryError::Poisoned)?;
    if current == NO_GENERATION || current != generation {
        return Err(EntryError::WrongGeneration {
            generation,
            current,
        });
    }
    SERVICE_STATE
        .lock()
        .map_err(|_| EntryError::Poisoned)?
        .clone()
        .ok_or(EntryError::NoService)
}

/// Called right after `VpnService.Builder.establish()` succeeded: hand the descriptor to the
/// generation `nativeStartServer` bound. From this moment the observation says `tun_ready` and
/// a `start_tunnel` request has something to run on.
///
/// Throws when `generation` is not the one serving, so a descriptor from a start that has
/// since been superseded is never adopted by the newer generation.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeSetTunFd<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
    tun_fd: jint,
) {
    env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;
        serving(generation)?.set_fd(tun_fd as RawFd);
        info!("nativeSetTunFd: generation {generation} holds fd {tun_fd}");
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// Called when `establish()` (or anything else between the bind and the descriptor) failed:
/// record the reason on the generation so the UI's next poll returns it as `start_error`.
///
/// This is the one failure the Rust side cannot see for itself. Best-effort: a generation that
/// is no longer serving has nobody left to tell, and the reason is already in the log.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeReportStartError<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
    message: JString<'local>,
) {
    let outcome = env.with_env(|env: &mut Env<'local>| -> Result<(), EntryError> {
        let message: String = message.mutf8_chars(env)?.to_string();
        let generation = generation as u64;
        error!("nativeReportStartError: generation {generation} could not start: {message}");
        serving(generation)?.set_error(message);
        Ok(())
    });
    log_outcome("nativeReportStartError", outcome.into_outcome());
}

/// Called in `FloppaVpnService.onDestroy()` / `onRevoke()`.
///
/// Stops the tunnel and tarpc server, and releases the service reference. Runs inside `with_env`
/// so a panic is caught and logged rather than unwinding into the JVM, which aborts the process.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStop<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;

        // Only tear down our own generation.
        //
        // Service instances share this process, and stopping one is asynchronous: the previous
        // instance's onDestroy routinely arrives after the next instance has already bound its
        // socket. Without this check it killed the new server roughly 150ms after it came up, and
        // the connect that was about to use it failed with "the connection was already shutdown"
        // — every time. What is compared is a *service generation*, minted once per start: an
        // intent epoch is shared by every pass of a cycle and restarts at 1 in every UI process,
        // so this check used to pass for exactly the instances it was written to reject.
        let mut serving = SERVER_GENERATION.lock().map_err(|_| EntryError::Poisoned)?;
        if *serving == NO_GENERATION || *serving != generation {
            info!(
                "nativeStop: ignoring a stop for generation {generation}; this process now serves {}",
                *serving
            );
            return Ok(());
        }
        // Cleared before anything is torn down, so a later stop for a generation that has already
        // gone — a linger timer racing onDestroy — matches nothing instead of matching its
        // successor.
        *serving = NO_GENERATION;
        drop(serving);

        info!("nativeStop: stopping tunnel and RPC server (generation {generation})");

        // Shutdown RPC server. This generation owns the socket path (it matched above and
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
        SERVICE_STATE
            .lock()
            .map_err(|_| EntryError::Poisoned)?
            .take();

        info!("nativeStop: cleanup complete");
        Ok(())
    });
    log_outcome("nativeStop", outcome.into_outcome());
}

/// Called in `FloppaVpnService.onStartCommand()` for a start the system issued with no
/// configuration — always-on VPN, boot, a lockdown restore — before anything else happens.
///
/// Reads the bundle the last successful connect wrote. Returns the TUN the service should build,
/// as the JSON form of the plugin's start payload (the same field names Kotlin reads out of a
/// start intent), with a fresh generation from the reserved autonomous range; or `null` when there is
/// nothing to restore, in which case the service stops and the reason is in the log.
///
/// This is the one moment an autonomous start can still resolve a name: no TUN exists yet. Under
/// lockdown it cannot, and the literal the last connect resolved to is what gets dialled.
///
/// `data_dir` is passed in because the `:vpn` process never initialises the UI's config-dir
/// resolver; it is the same directory (`applicationInfo.dataDir`) the UI writes into.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeLoadAutostart<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
) -> JString<'local> {
    env.with_env(
        |env: &mut Env<'local>| -> Result<JString<'local>, EntryError> {
            let dir: String = data_dir.mutf8_chars(env)?.to_string();
            let dir = std::path::Path::new(&dir);

            let Some(bundle) = autostart::load(dir) else {
                info!("autonomous start: no usable autostart bundle, nothing to restore");
                return Ok(JString::default());
            };

            let generation = autostart::next_autonomous_epoch(dir);
            let endpoint = get_runtime().block_on(autostart::resolve_endpoint(
                &bundle,
                AUTOSTART_RESOLVE_BUDGET,
            ));
            info!(
                protocol = %bundle.protocol(),
                %endpoint,
                generation,
                saved_at = bundle.saved_at,
                "autonomous start: rebuilding the last-good tunnel"
            );

            let AutostartBundle {
                tun,
                config,
                params,
                ..
            } = bundle;
            let plan = serde_json::to_string(&tun.with_generation(generation))?;
            *AUTOSTART.lock().map_err(|_| EntryError::Poisoned)? = Some(PreparedAutostart {
                generation,
                config,
                endpoint,
                params,
            });
            Ok(env.new_string(plan)?)
        },
    )
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Called after `nativeStartServer` on an autonomous start: bring the tunnel up on the descriptor
/// the service just bound its socket for, from what `nativeLoadAutostart` prepared.
///
/// The start itself runs on the runtime rather than on the service's main thread. Its outcome is
/// handled the way the RPC path's is — `start_error` recorded for whoever observes next, the
/// notification promoted on success — plus one thing the RPC path leaves to its caller: with no
/// UI process to react, a failed autonomous start stops the service itself, so it never sits
/// foreground holding a descriptor with a default route into nothing.
///
/// Throws when there is nothing to start from — no prepared plan for this generation, or no service
/// generation — so Kotlin cleans up the TUN it has already established.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeStartTunnelFromBundle<
    'local,
>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
) {
    env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;
        let plan = AUTOSTART
            .lock()
            .map_err(|_| EntryError::Poisoned)?
            .take_if(|p| p.generation == generation)
            .ok_or(EntryError::NoPlan { generation })?;
        let service = SERVICE_STATE
            .lock()
            .map_err(|_| EntryError::Poisoned)?
            .clone()
            .filter(|s| s.generation == generation)
            .ok_or(EntryError::NoService)?;
        let tunnel_manager = get_tunnel_manager();

        get_runtime().spawn(async move {
            let started = Started {
                params: plan.params,
                autonomous: true,
            };
            let result = rpc_server::bring_up(
                &service,
                &tunnel_manager,
                plan.config,
                plan.endpoint,
                started,
            )
            .await;
            if let Err(e) = result {
                error!("autonomous start failed: {e}; stopping the service");
                stop_vpn_service();
            }
        });
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}
