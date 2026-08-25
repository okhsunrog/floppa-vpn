//! JNI entry points for the `:vpn` process — the process that owns the tunnel *and* the actor.
//!
//! Everything about a tunnel is decided here now: the intent, the status, the ladder, the reconnect
//! budget, the config store. The UI process holds a socket to this one and nothing else. That is
//! the whole point of the move: Android freezes the UI process in the background, so an actor
//! living there could not reconnect a tunnel that died while the phone was in a pocket, and could
//! not even watch one.
//!
//! What is left for Kotlin is what only Kotlin can do — say whether the app holds VPN consent, turn
//! a TUN spec into a file descriptor, and own the foreground notification — and each of those is
//! one call in one direction:
//!
//! ```text
//!   Kotlin  →  nativeInit            the process is up; boot the actor
//!           →  nativeSetTunFd        establish() succeeded, here is the descriptor
//!           →  nativeReportStartError establish() failed, here is why
//!           →  nativeNetworkChanged  the default network moved under a running tunnel
//!           →  nativeSystemStart     the system wants a tunnel (always-on, boot, lockdown)
//!           →  nativeServiceGone     this service instance is being destroyed
//!   Rust    →  hasConsent()          may we run a VPN at all?
//!           →  startGeneration()     establish a TUN for this generation
//!           →  setConnected()        what the notification should say
//!           →  shutdownService()     drop the notification and stop
//!           →  protectSocket()       keep the tunnel's own socket out of the tunnel
//! ```

use super::service_state::ServiceRegistry;
use super::tunnel::{self, TunnelManager};
use crate::vpn::actor::handle::{IntentRequest, TunnelHandle};
use crate::vpn::actor::types::Phase;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jlong};
use jni::{Env, EnvUnowned, JavaVM};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, error, info, warn};

/// What a JNI entry point can fail with.
///
/// `nativeInit` resolves these into a `java.lang.RuntimeException`, so Kotlin sees a boot that
/// failed as a thrown exception rather than as a call that returned normally.
#[derive(Debug, thiserror::Error)]
enum EntryError {
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
    #[error("a global lock in the :vpn process is poisoned")]
    Poisoned,
    #[error("failed to start the actor's socket: {0}")]
    ServerStart(String),
    #[error("the :vpn process has not been initialised")]
    NotBooted,
    #[error("generation {0} is not the one being served")]
    WrongGeneration(u64),
}

static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
/// The live `VpnService` instance. A `Mutex` rather than a `OnceLock`: the process outlives the
/// service instances inside it, and each new one replaces the reference.
static VPN_SERVICE_REF: Mutex<Option<jni::objects::Global<JObject<'static>>>> = Mutex::new(None);
static TOKIO_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Everything the boot builds, kept together so that "the process is up" is one check rather than
/// four that can disagree.
struct Booted {
    actor: TunnelHandle,
    services: Arc<ServiceRegistry>,
    tunnel_manager: Arc<TunnelManager>,
}

static BOOTED: OnceLock<Booted> = OnceLock::new();
static RPC_HANDLE: Mutex<Option<crate::vpn::rpc_server::RpcServerHandle>> = Mutex::new(None);

fn booted() -> Result<&'static Booted, EntryError> {
    BOOTED.get().ok_or(EntryError::NotBooted)
}

fn runtime() -> &'static tokio::runtime::Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("the :vpn process cannot run without a runtime")
    })
}

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

// ------------------------------------------------------------------ calls into the service

/// Run `call` against the live service instance.
fn with_service<T>(
    what: &str,
    call: impl FnOnce(&mut Env<'_>, &JObject<'static>) -> Result<T, jni::errors::Error>,
) -> Result<T, String> {
    let vm = JAVA_VM.get().ok_or("the JavaVM is not set")?;
    let guard = VPN_SERVICE_REF
        .lock()
        .map_err(|_| "the service reference is poisoned".to_string())?;
    let service = guard
        .as_ref()
        .ok_or_else(|| format!("{what}: there is no VPN service to ask"))?;
    let service = service.as_ref();
    vm.attach_current_thread(|env| call(env, service))
        .map_err(|e| format!("{what}: {e}"))
}

/// Protect a socket with `VpnService.protect()`, so the tunnel's own traffic bypasses the tunnel.
///
/// Called from the UDP factory on every bind — including the rebind a network change triggers.
fn protect_socket_jni(fd: RawFd) -> bool {
    let protected = with_service("protectSocket", |env, service| {
        env.call_method(
            service,
            jni::jni_str!("protectSocket"),
            jni::jni_sig!("(I)Z"),
            &[fd.into()],
        )?
        .z()
    });
    match protected {
        Ok(true) => {
            debug!("protected socket fd {fd}");
            true
        }
        Ok(false) => {
            warn!("VpnService.protect() refused fd {fd}");
            false
        }
        Err(e) => {
            error!("could not protect fd {fd}: {e}");
            false
        }
    }
}

/// Whether the app already holds VPN consent.
///
/// A question, not a dialog: only an activity can show one, and this process has none.
pub fn has_vpn_consent() -> Result<bool, String> {
    with_service("hasConsent", |env, service| {
        env.call_method(
            service,
            jni::jni_str!("hasConsent"),
            jni::jni_sig!("()Z"),
            &[],
        )?
        .z()
    })
}

/// Ask the service to establish a TUN for `generation`.
///
/// Returns as soon as the request is placed. The descriptor comes back asynchronously through
/// `nativeSetTunFd`, and a failure through `nativeReportStartError` — which is what lets the ladder
/// tell "still coming up" from "failed, and here is why" instead of waiting out a timeout.
pub fn start_generation(plan: &str, generation: u64) -> Result<(), String> {
    let plan = plan.to_string();
    with_service("startGeneration", |env, service| {
        let plan = env.new_string(&plan)?;
        env.call_method(
            service,
            jni::jni_str!("startGeneration"),
            jni::jni_sig!("(Ljava/lang/String;J)V"),
            &[(&plan).into(), (generation as jlong).into()],
        )?;
        Ok(())
    })
}

/// Stop the service: notification, descriptor and started lifecycle all go together.
pub fn stop_vpn_service() {
    match with_service("shutdownService", |env, service| {
        env.call_method(
            service,
            jni::jni_str!("shutdownService"),
            jni::jni_sig!("()V"),
            &[],
        )?;
        Ok(())
    }) {
        Ok(()) => info!("asked the VPN service to shut down"),
        Err(e) => warn!("could not ask the VPN service to shut down: {e}"),
    }
}

/// Tell the service what its notification should say.
///
/// Best-effort: a failure here costs a stale notification line, never the tunnel.
pub fn set_service_connected(connected: bool) {
    if let Err(e) = with_service("setConnected", |env, service| {
        env.call_method(
            service,
            jni::jni_str!("setConnected"),
            jni::jni_sig!("(Z)V"),
            &[connected.into()],
        )?;
        Ok(())
    }) {
        debug!("could not update the VPN notification: {e}");
    }
}

/// Bridges shoes-lite's SocketProtector trait to `VpnService.protect()`.
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

// ------------------------------------------------------------------------ entry points

/// Called from `FloppaVpnService.onCreate()`, for every service instance.
///
/// The first call boots the process: logging, the tunnel manager, the actor, and the socket the UI
/// reaches it through. Later calls only refresh the service reference — the process outlives its
/// service instances, and the actor outlives both.
///
/// `data_dir` is where the config store, the journal and the socket live. It is passed in because
/// this process has no Tauri to resolve it, and it is the same directory the UI process uses.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeInit<'local>(
    mut env: EnvUnowned<'local>,
    this: JObject<'local>,
    log_dir: JString<'local>,
    data_dir: JString<'local>,
) {
    env.with_env(|env: &mut Env<'local>| -> Result<(), EntryError> {
        if JAVA_VM.get().is_none() {
            let vm = env.get_java_vm()?;
            let _ = JAVA_VM.set(vm);
        }
        // Refreshed on every instance: a reference to a destroyed service protects no sockets and
        // shows no notifications.
        let global_ref = env.new_global_ref(this)?;
        *VPN_SERVICE_REF.lock().map_err(|_| EntryError::Poisoned)? = Some(global_ref);

        if BOOTED.get().is_some() {
            debug!("nativeInit: a new service instance in an already-running :vpn process");
            return Ok(());
        }

        let log_dir: String = log_dir.mutf8_chars(env)?.to_string();
        let data_dir: String = data_dir.mutf8_chars(env)?.to_string();
        crate::logging::init_tracing(
            std::path::Path::new(&log_dir),
            crate::logging::LogProcess::Vpn,
        );
        std::panic::set_hook(Box::new(|info| error!("{info}")));

        // Everything the actor persists — configs, the rollback journal — goes here, which is the
        // same directory the UI process reads.
        let dir = std::path::PathBuf::from(&data_dir);
        crate::vpn::config::init_config_dir(dir.clone());

        tunnel::set_socket_protect_callback(protect_socket_jni);
        shoes_lite::api::set_socket_protector(Arc::new(ShoesSocketProtector));

        let runtime = runtime();
        let _enter = runtime.enter();

        let tunnel_manager = TunnelManager::new();
        let services = Arc::new(ServiceRegistry::default());
        let backend: Arc<dyn crate::vpn::backend::VpnBackend> =
            Arc::new(crate::vpn::backend::AndroidServiceBackend::new(
                tunnel_manager.clone(),
                services.clone(),
            ));
        let platform = Arc::new(crate::vpn::platform::PlatformImpl::new());
        let journal = Some(crate::vpn::rollback::Journal::new(
            crate::vpn::rollback::Journal::default_path(&dir),
        ));
        let host = Arc::new(crate::vpn::host::service::JniServiceHost::new(
            services.clone(),
        ));
        let spawn: crate::vpn::actor::Spawn = {
            let handle = runtime.handle().clone();
            Arc::new(move |task| {
                handle.spawn(task);
            })
        };

        let actor =
            crate::vpn::actor::TunnelActor::spawn(backend, platform, journal, spawn, host.clone());

        // The notification is the only UI a tunnel has while the app is closed, so it follows the
        // actor's own phase rather than being set by whatever last touched the tunnel.
        {
            let mut states = actor.states();
            runtime.spawn(async move {
                let mut said = None;
                while states.changed().await.is_ok() {
                    let connected = states.borrow().phase == Phase::Connected;
                    if said != Some(connected) {
                        said = Some(connected);
                        set_service_connected(connected);
                    }
                }
            });
        }

        let socket = dir.join(crate::vpn::rpc::SOCKET_NAME);
        let handle = crate::vpn::rpc_server::serve(&socket.to_string_lossy(), actor.clone())
            .map_err(EntryError::ServerStart)?;
        *RPC_HANDLE.lock().map_err(|_| EntryError::Poisoned)? = Some(handle);

        let _ = BOOTED.set(Booted {
            actor,
            services,
            tunnel_manager,
        });
        info!("nativeInit: the :vpn process is up and the actor is serving");
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// `VpnService.Builder.establish()` succeeded: hand the descriptor to the generation that asked.
///
/// Throws when that generation is not the one being served, so a descriptor from a start that has
/// since been superseded is never adopted by the one that replaced it.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeSetTunFd<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
    tun_fd: jint,
) {
    env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;
        let service = booted()?
            .services
            .serving(generation)
            .ok_or(EntryError::WrongGeneration(generation))?;
        service.set_fd(tun_fd as RawFd);
        info!("nativeSetTunFd: generation {generation} holds fd {tun_fd}");
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// `establish()` — or anything between the request and the descriptor — failed. Record why, so the
/// ladder gets a reason on its next look instead of waiting out its budget.
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
        error!("generation {generation} could not establish a TUN: {message}");
        booted()?
            .services
            .serving(generation)
            .ok_or(EntryError::WrongGeneration(generation))?
            .set_error(message);
        Ok(())
    });
    log_outcome("nativeReportStartError", outcome.into_outcome());
}

/// The phone's default network changed under a running tunnel: rebind its socket in place.
///
/// The one recovery that belongs here rather than in the actor. The tunnel, its descriptor and its
/// routes are all still right, and only the socket underneath is bound to a network that no longer
/// exists — so this is a reflex, not a decision, and it cannot fight the actor over what should be
/// running. The actor's own recovery starts a whole cycle and is minutes away; this is a round trip.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeNetworkChanged<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;
        let booted = booted()?;
        if booted.services.serving(generation).is_none() {
            return Err(EntryError::WrongGeneration(generation));
        }
        info!("the network moved under generation {generation}; rebinding");
        let manager = booted.tunnel_manager.clone();
        runtime().spawn(async move {
            match manager.network_changed().await {
                Ok(()) => info!("the tunnel was rebound onto the new network"),
                Err(e) => warn!("could not rebind the tunnel after the network changed: {e}"),
            }
        });
        Ok(())
    });
    log_outcome("nativeNetworkChanged", outcome.into_outcome());
}

/// The system asked for a tunnel with nobody watching: always-on, boot, or a lockdown restore.
///
/// The second principal. The user's intent lives in the actor and persists across restarts, but
/// the system can want a tunnel when that intent says Down — the always-on toggle is the system's
/// decision, not the app's, and an app that fights it produces a restart loop. So a start the
/// system issued raises the intent to Up, from the order and rules the last connect recorded.
///
/// A wipe is what beats it: `Forget` clears the configs, and an actor with nothing to build from
/// stops rather than trying.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeSystemStart<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let actor = booted()?.actor.clone();
        let Some(request) = crate::vpn::autostart::last_intent() else {
            info!("the system asked for a tunnel, but nothing has been connected yet");
            stop_vpn_service();
            return Ok(());
        };
        info!("the system asked for a tunnel; raising the intent");
        runtime().spawn(async move {
            match actor.set_intent(request).await {
                Ok(accepted) => info!(epoch = %accepted.epoch, "the system's request was accepted"),
                Err(e) => {
                    error!("the system's request was refused: {e}");
                    stop_vpn_service();
                }
            }
        });
        Ok(())
    });
    log_outcome("nativeSystemStart", outcome.into_outcome());
}

/// This service instance is being destroyed — `onDestroy` or `onRevoke`.
///
/// Only its own generation is ended: service instances share this process and their teardown is
/// asynchronous, so a dying instance's callback routinely arrives after the next one has started.
/// Ending the wrong one killed the tunnel that had just replaced it.
///
/// The tunnel goes with the descriptor, because the descriptor goes with the service. What the
/// actor does about that is the actor's business: it observes a tunnel that is no longer running
/// and decides, exactly as it would for a tunnel that died any other way.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeServiceGone<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: jlong,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let generation = generation as u64;
        let booted = booted()?;
        if !booted.services.end(generation) {
            info!("nativeServiceGone: generation {generation} is not the one being served");
            return Ok(());
        }
        info!("nativeServiceGone: generation {generation} is gone; stopping its tunnel");
        let manager = booted.tunnel_manager.clone();
        runtime().spawn(async move {
            if let Err(e) = manager.stop().await {
                warn!("could not stop the tunnel of a service that went away: {e}");
            }
        });
        Ok(())
    });
    log_outcome("nativeServiceGone", outcome.into_outcome());
}

/// Called from `FloppaVpnService.onDestroy()` when the *process* is going away rather than one
/// service instance: release the service reference so a destroyed service is not pinned in the JVM.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeReleaseService<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        VPN_SERVICE_REF
            .lock()
            .map_err(|_| EntryError::Poisoned)?
            .take();
        Ok(())
    });
    log_outcome("nativeReleaseService", outcome.into_outcome());
}

/// Ask the actor to go down and stay down, from Kotlin's stop action.
#[unsafe(no_mangle)]
pub extern "C" fn Java_dev_okhsunrog_floppavpn_vpn_FloppaVpnService_nativeRequestStop<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
) {
    let outcome = env.with_env(|_env: &mut Env<'local>| -> Result<(), EntryError> {
        let actor = booted()?.actor.clone();
        runtime().spawn(async move {
            if let Err(e) = actor.set_intent(IntentRequest::Down).await {
                warn!("the stop request was refused: {e}");
            }
        });
        Ok(())
    });
    log_outcome("nativeRequestStop", outcome.into_outcome());
}
