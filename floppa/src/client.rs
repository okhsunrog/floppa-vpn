//! Driving the tunnel that the system service is holding.
//!
//! The other half of `connect`. Where [`crate::connect`] builds an actor in this process and holds
//! it for as long as the command runs, this asks one that is already running and then gets out of
//! the way: the tunnel is not this program's, and ending it is not what leaving the terminal
//! means any more.
//!
//! That difference is why `connect` has two shapes rather than a flag. A run given `--config` is
//! handed a tunnel to build out of a file and must not touch the machine's long-lived one; a run
//! given nothing is asking for the machine's tunnel, which is the service's. Deciding by what the
//! caller supplied rather than by a switch keeps `--config` meaning exactly what it always meant,
//! and keeps the integration harness — which runs in a container with no service at all — working
//! unchanged.

use anyhow::{Result, bail};
use std::sync::Arc;
use std::time::Duration;

use floppa_vpn_core::actor::Spawn;
use floppa_vpn_core::actor::handle::{IntentRequest, TunnelControl};
use floppa_vpn_core::actor::types::{CycleOutcome, Phase, SplitMode, TunnelParams, TunnelState};
use floppa_vpn_core::client_mode::{ServiceAccess, probe, system_socket};
use floppa_vpn_core::protocol::Protocol;
use floppa_vpn_core::remote::{RemoteActor, TunnelProcess};

/// How long to wait for the mirror to say anything at all before giving up on the service.
///
/// The mirror starts at `Unknown` — "nothing has been heard" — and every command here needs a
/// first answer before it can report anything true. The probe already proved the service answers,
/// so this only covers the moment between opening a second connection and the first state
/// arriving.
const FIRST_STATE: Duration = Duration::from_secs(10);

/// Under socket activation the socket is what starts the service, and a client connecting to it is
/// what makes that happen. There is nothing left for this to do.
struct StartedByConnecting;

#[async_trait::async_trait]
impl TunnelProcess for StartedByConnecting {
    async fn ensure_running(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Reach the service, or say why not in terms the person can act on.
pub async fn reach() -> Result<Arc<RemoteActor>> {
    let socket = system_socket();
    match probe(&socket).await {
        ServiceAccess::Available => {}
        ServiceAccess::Absent => bail!(
            "no tunnel service is running.\n\
             Start it with `systemctl start floppa-vpn.service`, or connect from a config file \
             with `sudo floppa connect --config <file>`."
        ),
        // Both of these have something specific to say, and saying it is the entire reason this
        // is not a boolean.
        other => bail!("{}", other.explain().expect("not a usable answer")),
    }

    let dir = socket
        .parent()
        .expect("the socket is inside a directory")
        .to_path_buf();
    let spawn: Spawn = Arc::new(|fut| {
        tokio::spawn(fut);
    });
    Ok(RemoteActor::new(
        &dir,
        Arc::new(StartedByConnecting),
        &spawn,
    ))
}

/// Give the service the credentials it will need when nobody is at this terminal.
///
/// The one thing a client can do for a service and not the other way round. Repairing a peer the
/// server deleted means talking to the server, and the service will have to do that with the app
/// closed and this shell long gone — but the credentials belong to a user and the place to keep
/// them belongs to root. So they travel in that direction, and only that direction: there is no
/// call to read a session back out, and there is not going to be one. What the socket grants is
/// the use of an account, never a copy of the token.
///
/// Best effort on purpose. A connect that is otherwise fine must not fail because a peer could not
/// be repaired *later*; the tunnel is what was asked for, and a service that ends up without a
/// session merely goes back to how it behaved before this existed.
pub async fn seed_session(
    remote: &RemoteActor,
    base_url: &str,
    token: &str,
    identity: &floppa_api_client::DeviceIdentity,
) {
    let session = floppa_provision::ServerSession::new(
        base_url.to_owned(),
        token.to_owned(),
        identity.device_id.clone(),
        identity.device_name.clone(),
    );
    let payload = match serde_json::to_string(&session) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::warn!("the session could not be serialised for the service: {e}");
            return;
        }
    };
    match remote.set_session(Some(payload)).await {
        Ok(()) => tracing::debug!("the service can now reach the server as this user"),
        Err(e) => tracing::warn!(
            "the service was not given a session, so it cannot replace a deleted peer on its own: {e}"
        ),
    }
}

/// Wait for the mirror to hold something other than "nothing has been heard".
async fn first_state(remote: &RemoteActor) -> Result<TunnelState> {
    let deadline = std::time::Instant::now() + FIRST_STATE;
    loop {
        let state = remote.snapshot();
        if state.phase != Phase::Unknown {
            return Ok(state);
        }
        if std::time::Instant::now() >= deadline {
            bail!("the tunnel service did not say what it is doing");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ask the service for a tunnel over `config_str`, and return once the cycle has settled.
///
/// Returns rather than waits: the tunnel outlives this command, which is the whole point of there
/// being a service. A caller who wants to watch it has `floppa status`.
pub async fn connect(remote: &RemoteActor, config_str: &str) -> Result<()> {
    let protocol = remote
        .import_config(config_str.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let accepted = remote
        .set_intent(IntentRequest::Up {
            order: vec![protocol],
            // Split tunnelling is an Android affair; on a desktop everything goes through.
            params: TunnelParams::new(SplitMode::All, Vec::new()),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!("Connecting over {protocol}...");
    match remote
        .await_cycle(accepted.epoch)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        CycleOutcome::Connected { protocol, .. } => {
            report(&remote.snapshot());
            println!("READY");
            eprintln!(
                "Connected over {protocol}. The service keeps it up; `floppa disconnect` stops it."
            );
            Ok(())
        }
        other => bail!("could not connect: {}", crate::connect::describe(&other)),
    }
}

/// Take the tunnel down and wait for the machine to be back as it was.
pub async fn disconnect(remote: &RemoteActor) -> Result<()> {
    let state = first_state(remote).await?;
    if state.phase == Phase::Disconnected {
        eprintln!("No tunnel is up.");
        return Ok(());
    }

    let down = remote
        .set_intent(IntentRequest::Down)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Awaited: the routes and the DNS come back in the service, and a command that returned first
    // would report success before the machine was actually restored.
    let _ = remote.await_cycle(down.epoch).await;
    eprintln!("Disconnected.");
    Ok(())
}

/// Put a config into the service's store, without connecting.
///
/// The gap this fills: `--config` means "build a tunnel here, out of this file", which is the one
/// shape that must not touch the machine's long-lived tunnel — so a person holding a `.conf` had
/// no way to hand it to the service at all, and `connect` would have sent them to the server for
/// one they already had. The store is root's, so this is the only way in.
pub async fn import(remote: &RemoteActor, config_str: &str) -> Result<()> {
    let protocol = remote
        .import_config(config_str.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("Stored a {protocol} config. `floppa connect` will use it.");
    Ok(())
}

/// Connect with whatever the service already holds.
///
/// The order is every protocol it has a config for. An empty order is *not* "you choose": the
/// store resolves a request by filtering it down to what it holds, so nothing in means nothing
/// out, and the cycle ends before it starts with "probe order is empty". Which of them leads is
/// still the store's call — `resolve_order` moves the one that last worked to the front — so this
/// says what is possible and lets the actor say what is preferable.
pub async fn connect_stored(remote: &RemoteActor) -> Result<()> {
    let order = first_state(remote).await?.configs.available;
    if order.is_empty() {
        bail!("the tunnel service holds no config; hand it one with `floppa import <file>`");
    }

    let accepted = remote
        .set_intent(IntentRequest::Up {
            order,
            params: TunnelParams::new(SplitMode::All, Vec::new()),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!("Connecting with the config the service already holds...");
    match remote
        .await_cycle(accepted.epoch)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        CycleOutcome::Connected { protocol, .. } => {
            report(&remote.snapshot());
            println!("READY");
            eprintln!(
                "Connected over {protocol}. The service keeps it up; `floppa disconnect` stops it."
            );
            Ok(())
        }
        other => bail!("could not connect: {}", crate::connect::describe(&other)),
    }
}

/// Show or change whether this machine reconnects its tunnel on boot.
pub async fn autostart(remote: &RemoteActor, set: Option<bool>) -> Result<()> {
    if let Some(enabled) = set {
        remote
            .set_resume_on_boot(enabled)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    let on = remote.resume_on_boot().await;
    println!("{}", if on { "on" } else { "off" });
    if on && set == Some(true) {
        eprintln!("The tunnel that last connected will be brought back at boot.");
    }
    Ok(())
}

/// Whether the service already holds something it could connect with.
pub async fn has_config(remote: &RemoteActor) -> bool {
    match first_state(remote).await {
        Ok(state) => !state.configs.available.is_empty(),
        Err(_) => false,
    }
}

/// Ask the service to bring back whatever it last had up.
///
/// What `floppa-vpn-autostart.service` runs at boot, and the reason it is a client command rather
/// than something the service does for itself: the service is started by its socket too, and every
/// `floppa status` would otherwise reconnect a VPN somebody had turned off. Being asked is
/// different from being started, and only a caller can tell the two apart.
pub async fn resume(remote: &RemoteActor, only_if_enabled: bool) -> Result<()> {
    // Asked of the service rather than read here: the preference lives beside the configs, in a
    // directory only root can read, for the same reason everything else about this does.
    if only_if_enabled && !remote.resume_on_boot().await {
        return Ok(());
    }
    let Some(accepted) = remote.resume().await.map_err(|e| anyhow::anyhow!("{e}"))? else {
        eprintln!("Nothing has connected on this machine yet; there is nothing to bring back.");
        return Ok(());
    };

    match remote
        .await_cycle(accepted.epoch)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        CycleOutcome::Connected { protocol, .. } => {
            eprintln!("Connected over {protocol}.");
            Ok(())
        }
        other => bail!("could not connect: {}", crate::connect::describe(&other)),
    }
}

/// What the service says the tunnel is doing.
pub async fn status(remote: &RemoteActor) -> Result<()> {
    let state = first_state(remote).await?;
    match state.phase {
        Phase::Connected => {
            let over = state
                .protocol
                .map(|p: Protocol| p.to_string())
                .unwrap_or_else(|| "an unnamed protocol".into());
            println!("connected over {over}");
            report(&state);
        }
        Phase::Disconnected => println!("disconnected"),
        // Everything else is the actor in the middle of something, and the phase is already the
        // word for it.
        other => println!("{other:?}"),
    }
    Ok(())
}

fn report(state: &TunnelState) {
    if let Some(summary) = state.configs.summaries.first() {
        eprintln!("VPN IP: {}", summary.address);
        eprintln!("Endpoint: {}", summary.server_endpoint);
    }
}
