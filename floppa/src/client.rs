//! Select one owner: an available system service, otherwise this standalone process.
//! An inaccessible or incompatible service is an error, never permission to start another owner.

use anyhow::{Result, bail};
use std::sync::Arc;
use std::time::Duration;

use floppa_vpn_core::actor::Spawn;
use floppa_vpn_core::actor::handle::{IntentRequest, TunnelHandle};
use floppa_vpn_core::actor::types::{CycleOutcome, Phase, TunnelState};
use floppa_vpn_core::client_mode::{ServiceAccess, probe, system_remote, system_socket};
use floppa_vpn_core::protocol::Protocol;
use floppa_vpn_core::remote::RemoteActor;

/// How long to wait for the mirror to say anything at all before giving up on the service.
///
/// The mirror starts at `Unknown` — "nothing has been heard" — and every command here needs a
/// first answer before it can report anything true. The probe already proved the service answers,
/// so this only covers the moment between opening a second connection and the first state
/// arriving.
const FIRST_STATE: Duration = Duration::from_secs(10);

/// Reach the service, or say why not in terms the person can act on.
pub struct ClientOwner {
    handle: TunnelHandle,
    remote: Option<Arc<RemoteActor>>,
}
impl std::ops::Deref for ClientOwner {
    type Target = TunnelHandle;
    fn deref(&self) -> &TunnelHandle {
        &self.handle
    }
}
impl ClientOwner {
    async fn set_session(&self, payload: Option<String>) -> Result<(), String> {
        if let Some(remote) = &self.remote {
            return remote.set_session(payload).await.map_err(|e| e.to_string());
        }
        let session = payload
            .map(|p| serde_json::from_str(&p))
            .transpose()
            .map_err(|e| e.to_string())?;
        floppa_provision::session::store(&floppa_vpn_core::config::config_dir()?, session)
    }
    async fn resume_on_boot(&self) -> Result<bool> {
        Ok(self
            .get_settings()
            .await?
            .document
            .settings
            .startup
            .connect_on_boot)
    }
}

pub async fn reach() -> Result<ClientOwner> {
    reach_for_connect(floppa_vpn_core::protocol::InterfaceName::DEFAULT, false).await
}

pub async fn reach_for_connect(interface: &str, no_dns: bool) -> Result<ClientOwner> {
    let spawn: Spawn = Arc::new(|fut| {
        tokio::spawn(fut);
    });
    match probe(&system_socket()).await {
        ServiceAccess::Available => {
            if no_dns || interface != floppa_vpn_core::protocol::InterfaceName::DEFAULT {
                bail!("--interface and --no-dns cannot override the system service deployment");
            }
            eprintln!("Mode: systemd service");
            let remote = system_remote(&spawn);
            Ok(ClientOwner {
                handle: TunnelHandle::remote(remote.clone()),
                remote: Some(remote),
            })
        }
        ServiceAccess::Absent => {
            eprintln!("Mode: standalone (this process owns the tunnel)");
            let dir = floppa_vpn_core::config::config_dir().map_err(anyhow::Error::msg)?;
            let handle = floppa_vpn_core::actor::TunnelActor::spawn(
                floppa_vpn_core::create_backend(),
                Arc::new(floppa_vpn_core::get_platform()),
                Some(floppa_vpn_core::rollback::Journal::new(
                    floppa_vpn_core::rollback::Journal::default_path(&dir),
                )),
                spawn.clone(),
                floppa_vpn_core::actor::deployment::Deployment {
                    iface: floppa_vpn_core::protocol::InterfaceName::new(interface)
                        .map_err(|e| anyhow::anyhow!("{}", e.0))?,
                    manage_dns: !no_dns,
                    ..Default::default()
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
            floppa_provision::watcher::watch(handle.clone(), spawn, env!("CARGO_PKG_VERSION"));
            Ok(ClientOwner {
                handle,
                remote: None,
            })
        }
        other => bail!("{}", other.explain().expect("not a usable answer")),
    }
}

async fn finish_connect(
    owner: &ClientOwner,
    accepted: floppa_vpn_core::actor::types::IntentAccepted,
) -> Result<()> {
    if owner.remote.is_none() {
        return crate::connect::hold(&owner.handle, accepted).await;
    }
    match owner.await_cycle(accepted.epoch).await {
        Ok(CycleOutcome::Connected { protocol, .. }) => {
            println!("READY");
            eprintln!(
                "Connected over {protocol}. The service keeps it up; `floppa disconnect` stops it."
            );
            Ok(())
        }
        Err(floppa_vpn_core::actor::types::IntentError::CycleStillRunning) => {
            eprintln!(
                "The service is still connecting or waiting for a network; use `floppa status`."
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
        Ok(other) => bail!("could not connect: {}", crate::connect::describe(&other)),
    }
}

pub async fn connect_temporary(owner: &ClientOwner, raw: String) -> Result<()> {
    let accepted = owner
        .set_intent(IntentRequest::Temporary {
            config: raw,
            private_routes: vec![],
        })
        .await?;
    finish_connect(owner, accepted).await
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
    remote: &ClientOwner,
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
async fn first_state(remote: &ClientOwner) -> Result<TunnelState> {
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
pub async fn connect(
    remote: &ClientOwner,
    config_str: &str,
    private_routes: Vec<String>,
    excluded_routes: Option<Vec<String>>,
) -> Result<()> {
    if let Some(routes) = excluded_routes {
        update_routes(remote, routes).await?;
    }
    remote.import_config(config_str.to_string()).await?;
    let accepted = remote
        .set_intent(IntentRequest::Configured { private_routes })
        .await?;
    finish_connect(remote, accepted).await
}

async fn update_routes(owner: &ClientOwner, routes: Vec<String>) -> Result<()> {
    let mut state = owner.get_settings().await?;
    state.document.settings.routing.excluded_routes = routes;
    owner
        .update_settings(state.document.revision, state.document.settings)
        .await?;
    Ok(())
}

/// Take the tunnel down and wait for the machine to be back as it was.
pub async fn disconnect(remote: &ClientOwner) -> Result<()> {
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

/// Persist credentials in the selected owner, without connecting.
pub async fn import(remote: &ClientOwner, config_str: &str) -> Result<()> {
    let protocol = remote
        .import_config(config_str.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("Stored a {protocol} config. `floppa connect` will use it.");
    Ok(())
}

/// Connect using owner settings and stored credentials.
pub async fn connect_stored(
    remote: &ClientOwner,
    excluded_routes: Option<Vec<String>>,
) -> Result<()> {
    if let Some(routes) = excluded_routes {
        update_routes(remote, routes).await?;
    }
    let saved = match &remote.remote {
        Some(service) => service.saved_intent().await?,
        None => floppa_vpn_core::autostart::last_intent(),
    };
    let private_routes = match saved {
        Some(IntentRequest::Configured { private_routes }) => private_routes,
        _ => vec![],
    };
    let accepted = remote
        .set_intent(IntentRequest::Configured { private_routes })
        .await?;
    finish_connect(remote, accepted).await
}

/// Show or change whether this machine reconnects its tunnel on boot.
pub async fn autostart(remote: &ClientOwner, set: Option<bool>) -> Result<()> {
    if let Some(enabled) = set {
        let mut state = remote.get_settings().await?;
        state.document.settings.startup.connect_on_boot = enabled;
        remote
            .update_settings(state.document.revision, state.document.settings)
            .await?;
    }
    let on = remote.resume_on_boot().await?;
    println!("{}", if on { "on" } else { "off" });
    if on && set == Some(true) {
        eprintln!("The tunnel that last connected will be brought back at boot.");
    }
    Ok(())
}

/// Whether the service already holds something it could connect with.
pub async fn has_config(remote: &ClientOwner) -> bool {
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
pub async fn resume(remote: &ClientOwner, only_if_enabled: bool) -> Result<()> {
    if only_if_enabled && !remote.resume_on_boot().await? {
        return Ok(());
    }
    if let Some(service) = &remote.remote {
        let Some(accepted) = service.resume().await? else {
            eprintln!("No saved connection.");
            return Ok(());
        };
        finish_connect(remote, accepted).await
    } else {
        connect_stored(remote, None).await
    }
}

/// What the service says the tunnel is doing.
pub async fn status(remote: &ClientOwner) -> Result<()> {
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
