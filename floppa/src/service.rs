//! The tunnel as a system service.
//!
//! The same binary, started a different way. What it holds is what the app used to hold in its own
//! process: the actor, the backends, the platform layer, the rollback journal and the config
//! store — and it keeps holding them when every client has gone away, which is the whole point.
//! Closing a window, or logging out, or never logging in at all, stops being something the tunnel
//! has an opinion about.
//!
//! It is a **system** service and not a per-user one, because a machine has one default route. Two
//! users cannot each have their own VPN in any sense that means anything, and pretending otherwise
//! would buy a second actor fighting the first for the same interface.
//!
//! # What it is not, yet
//!
//! Clients cannot drive it: `floppa connect` and the app still run their own actor. That is the
//! next piece. The watcher is started here all the same — it no-ops while there is no session to
//! repair peers with, and starting it where the actor is, rather than remembering to later, is the
//! rule this codebase already keeps.

use anyhow::{Context, Result, bail};
use std::sync::Arc;

use floppa_vpn_core::actor::deployment::Deployment;
use floppa_vpn_core::actor::handle::{IntentRequest, TunnelHandle};
use floppa_vpn_core::actor::{Spawn, TunnelActor};
use floppa_vpn_core::rollback::Journal;
use floppa_vpn_core::rpc::{SOCKET_NAME, SYSTEM_SOCKET_DIR, SYSTEM_STATE_DIR};
use floppa_vpn_core::{create_backend, get_platform, rpc_listener, rpc_server};

/// Hold the tunnel until the system asks for it back.
pub async fn run() -> Result<()> {
    // Before anything is created, so a mistaken invocation leaves no directories behind and no
    // half-initialised state. The check is `geteuid` rather than an attempt that fails somewhere
    // later: the failure would otherwise land in the middle of a connect, as a permission error on
    // a route, with nothing saying what the real problem was.
    if !is_root() {
        bail!(
            "`floppa service` configures the machine's routing and DNS and must run as root — \
             it is normally started by systemd, not by hand"
        );
    }

    let state_dir = std::path::Path::new(SYSTEM_STATE_DIR);
    create_private_dir(state_dir)?;
    floppa_vpn_core::config::init_config_dir(state_dir.to_path_buf());
    // Both before the actor exists, because the store is the first thing it reads.
    floppa_vpn_core::config::use_file_storage_only();

    let spawn: Spawn = Arc::new(|fut| {
        tokio::spawn(fut);
    });

    let handle = TunnelActor::spawn(
        create_backend(),
        Arc::new(get_platform()),
        // Durable across a crash. It matters more here than anywhere else: a service that is
        // killed mid-connect is restarted by systemd within seconds, and without the journal the
        // machine would be left holding routes to a tunnel that no longer exists while the next
        // run built another one beside them.
        Some(Journal::new(Journal::default_path(state_dir))),
        spawn.clone(),
        // `Persisted`, unlike a one-shot `floppa connect`: this process is what a machine has
        // instead of a logged-in user, so what it is told to keep, it keeps.
        Deployment::default(),
    );

    floppa_provision::watcher::watch(handle.clone(), spawn.clone(), env!("CARGO_PKG_VERSION"));
    remember_what_connects(handle.clone(), &spawn);

    let server = start_server(handle.clone())?;

    let signal = wait_for_shutdown().await?;
    tracing::info!("{signal}; taking the tunnel down");

    // Stop accepting before unwinding: a client that connected during the teardown would be told
    // about a tunnel that is on its way out, and could raise an intent against an actor that is
    // about to stop answering.
    server.shutdown();
    take_down(&handle).await;
    Ok(())
}

/// Write down what just connected, so a start with nobody watching can ask for it again.
///
/// Android does this inside the actor, off `Effect::RememberWinner`, because there the actor and
/// the thing that restarts it are the same process. Here they are not — systemd restarts this, and
/// the actor has no opinion about systemd — so it is done from outside, off the published state,
/// which carries everything the record needs.
///
/// The winner leads the order: it is the protocol that actually carried a tunnel on this network,
/// and trying it first is what makes a resume fast rather than a fresh walk down the ladder.
///
/// Written only when it changes. The state publishes on every traffic sample, and a file rewritten
/// once a second for the life of a connection would be a great deal of writing to say the same
/// thing.
fn remember_what_connects(handle: TunnelHandle, spawn: &Spawn) {
    use floppa_vpn_core::actor::types::{IntentView, Phase};

    let mut states = handle.states();
    spawn(Box::pin(async move {
        let mut written: Option<Vec<floppa_vpn_core::protocol::Protocol>> = None;
        while states.changed().await.is_ok() {
            let state = states.borrow_and_update().clone();
            if state.phase != Phase::Connected || state.intent != IntentView::Up {
                continue;
            }
            let (Some(winner), Some(params)) = (state.protocol, state.params.clone()) else {
                continue;
            };
            let mut order = vec![winner];
            order.extend(state.intent_order.iter().copied().filter(|p| *p != winner));
            if written.as_ref() == Some(&order) {
                continue;
            }
            let at = chrono::Utc::now().timestamp();
            let recorded = order.clone();
            tokio::task::spawn_blocking(move || {
                floppa_vpn_core::autostart::remember(recorded, params, at)
            });
            written = Some(order);
        }
    }));
}

/// Serve on systemd's socket if there is one, and bind for ourselves if not.
///
/// Both, because the unit file is not the only way this runs: it is also how the service is tried
/// out before anything is installed, and a mode that only works under systemd is a mode that
/// cannot be debugged.
fn start_server(handle: TunnelHandle) -> Result<rpc_server::RpcServerHandle> {
    let session: Option<Arc<dyn rpc_server::SessionSink>> = Some(Arc::new(StateDirSession));

    if let Some(listener) =
        rpc_listener::inherited_listener().context("reading the socket systemd passed")?
    {
        return Ok(rpc_server::serve_on_listener(listener, handle, session));
    }

    // Root only, both of them. Who *else* may drive the tunnel is a question the `.socket` unit
    // answers, with a group and a mode, and it answers it before the socket exists — which is the
    // point of letting systemd own it. This branch has no unit to read that from, so it opens the
    // door to nobody rather than guessing: a bare `create_dir_all` leaves `0755` after umask, and
    // a socket bound inside it is one any local user can connect to and steer the machine's
    // routing with.
    //
    // The directory carries it as well as the socket. A mode on the socket alone is applied after
    // `bind` returns, and there is a moment before that where it is already listening.
    let dir = std::path::Path::new(SYSTEM_SOCKET_DIR);
    create_private_dir(dir)?;
    let socket = dir.join(SOCKET_NAME);
    let server = rpc_server::serve(&socket.to_string_lossy(), handle, session)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    restrict_to_owner(&socket)?;
    Ok(server)
}

/// The server session, kept in the state directory on a client's behalf.
///
/// This is the half of the arrangement the client cannot do for itself. The store is root-owned,
/// so an unprivileged program cannot write it; and it has to be readable at boot, before anyone
/// has logged in, which rules out anybody's keyring. The client has the credentials and no way to
/// keep them here; this process has the place and no way to obtain them.
///
/// It goes through `floppa-provision` rather than writing the file directly, so the shape and the
/// filename have exactly one definition — the same one the peer watcher in this process reads
/// back a moment later.
struct StateDirSession;

impl rpc_server::SessionSink for StateDirSession {
    fn store(&self, session: Option<String>) -> Result<(), String> {
        let dir = floppa_vpn_core::config::config_dir()?;
        let parsed = match session {
            Some(raw) => Some(
                serde_json::from_str::<floppa_provision::ServerSession>(&raw)
                    .map_err(|e| format!("the session is not one this build understands: {e}"))?,
            ),
            None => None,
        };
        floppa_provision::session::store(&dir, parsed)
    }
}

/// Make a path readable and writable by its owner and nobody else.
fn restrict_to_owner(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {}", path.display()))
}

/// Ask the actor to go down, and wait for it to actually be down.
///
/// Waited on rather than fired off, exactly as the app does on exit: the actor holds the record of
/// what was applied, so it is the only thing that can undo precisely that, and a process that
/// exited first would leave the machine holding it.
async fn take_down(handle: &TunnelHandle) {
    let _ = handle.set_intent(IntentRequest::Down).await;
    match tokio::time::timeout(std::time::Duration::from_secs(10), handle.await_quiescent()).await {
        Ok(()) => tracing::info!("the tunnel is down and the machine is back as it was"),
        // Said plainly rather than swallowed: what is left behind is a route or a resolv.conf, and
        // the journal is what the next start will use to find it.
        Err(_) => tracing::warn!(
            "timed out waiting for the tunnel to settle; the rollback journal holds what was applied"
        ),
    }
    match tokio::time::timeout(std::time::Duration::from_secs(5), handle.flush_configs()).await {
        Ok(()) => tracing::debug!("the config store is flushed"),
        Err(_) => tracing::warn!("timed out waiting for the config store to flush"),
    }
}

/// The signals a service is stopped by. `SIGHUP` is included because losing a terminal is a
/// perfectly ordinary way to end a run started by hand.
async fn wait_for_shutdown() -> Result<&'static str> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => { r?; Ok("SIGINT received") }
        _ = terminate.recv() => Ok("SIGTERM received"),
        _ = hangup.recv() => Ok("SIGHUP received"),
    }
}

fn is_root() -> bool {
    // SAFETY: `geteuid` reads this process's own effective uid and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Create a directory only its owner may enter, or leave alone the one that is already there.
///
/// Left alone deliberately: under systemd this is `RuntimeDirectory=` or `StateDirectory=`, made
/// with whatever ownership the unit asked for, and reaching in to narrow it would undo the
/// packaging's own decision about who may reach the socket.
fn create_private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("could not create {}", dir.display()))
}
