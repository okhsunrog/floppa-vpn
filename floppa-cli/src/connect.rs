//! Connecting, over the same actor the app runs.
//!
//! This used to be a straight line: build a tunnel, set routes, set DNS, wait for a signal, undo.
//! It worked, and it had no answer to anything going wrong afterwards — a tunnel that stopped
//! passing traffic stayed "connected" until somebody noticed, and a teardown that half-failed
//! left the machine holding a route to a tunnel that no longer existed.
//!
//! The app already had all of that: a protocol ladder, a reconnect budget, silence detection, a
//! rollback journal that survives the process dying. Running the same actor here means it applies
//! to the CLI too, and it means the actor gets exercised on a desktop by every integration test
//! run rather than only by hand on a phone.

use anyhow::{Context, Result, bail};
use std::sync::Arc;
use tokio::sync::watch;

use floppa_vpn_core::actor::deployment::{ConfigSource, Deployment};
use floppa_vpn_core::actor::handle::IntentRequest;
use floppa_vpn_core::actor::types::{CycleOutcome, SplitMode, TunnelParams, TunnelState};
use floppa_vpn_core::actor::{Spawn, TunnelActor};
use floppa_vpn_core::protocol::InterfaceName;
use floppa_vpn_core::rollback::Journal;
use floppa_vpn_core::{create_backend, get_platform};

/// Bring up `config_str` and stay up until a signal says otherwise.
pub async fn run(
    config_str: &str,
    interface: &str,
    no_dns: bool,
    config_dir: &std::path::Path,
) -> Result<()> {
    let iface = InterfaceName::new(interface)
        .map_err(|e| anyhow::anyhow!("{interface} is not a usable interface name: {}", e.0))?;

    let deployment = Deployment {
        iface,
        manage_dns: !no_dns,
        // A run is handed its config, from a file or from the server. Persisting it would leave a
        // private key in the keyring of whoever the CLI is running as — under `sudo`, root's.
        configs: ConfigSource::Ephemeral,
    };

    let spawn: Spawn = Arc::new(|fut| {
        tokio::spawn(fut);
    });
    let handle = TunnelActor::spawn(
        create_backend(),
        Arc::new(get_platform()),
        // Durable across a crash: if this process dies mid-connect, the next run finds the steps
        // it had applied and undoes them instead of leaving the machine half-tunnelled.
        Some(Journal::new(Journal::default_path(config_dir))),
        spawn,
        deployment,
    );

    let protocol = handle
        .import_config(config_str.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("the config is not one this client can use")?;

    let accepted = handle
        .set_intent(IntentRequest::Up {
            order: vec![protocol],
            // Split tunnelling is an Android affair; on a desktop everything goes through.
            params: TunnelParams::new(SplitMode::All, Vec::new()),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!("Connecting over {protocol}...");
    match handle
        .await_cycle(accepted.epoch)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        CycleOutcome::Connected { protocol, .. } => {
            report(&handle.snapshot());
            // The integration harness waits for this exact line on stdout. Everything else the
            // CLI says goes to stderr.
            println!("READY");
            eprintln!("Connected over {protocol}. Press Ctrl+C to disconnect.");
        }
        other => bail!("could not connect: {}", describe(&other)),
    }

    // From here the actor is on its own: if the tunnel dies it reconnects, and if the far side
    // goes quiet it says so. Nothing here has to watch it — but a run that gives up for good
    // should end rather than sit at a prompt claiming to be connected.
    let signal = watch_until_stopped(handle.states()).await?;
    eprintln!("\n{signal}, disconnecting...");

    let down = handle
        .set_intent(IntentRequest::Down)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Awaited, not fired and forgotten: the routes and the DNS come back here, and a process that
    // exited first would leave them.
    let _ = handle.await_cycle(down.epoch).await;
    handle.await_quiescent().await;
    eprintln!("Disconnected.");
    Ok(())
}

fn report(state: &TunnelState) {
    if let Some(summary) = state.configs.summaries.first() {
        eprintln!("VPN IP: {}", summary.address);
        eprintln!("Endpoint: {}", summary.server_endpoint);
    }
}

fn describe(outcome: &CycleOutcome) -> String {
    match outcome {
        CycleOutcome::Exhausted { failures } => match failures.last() {
            Some(last) => format!("{} failed: {}", last.protocol, last.error),
            None => "no protocol was even tried".to_string(),
        },
        CycleOutcome::LostGaveUp { protocol, passes } => {
            format!("the {protocol} tunnel kept dropping; gave up after {passes} passes")
        }
        CycleOutcome::UnwindFailed => "a teardown could not be confirmed".to_string(),
        CycleOutcome::Cancelled => "cancelled".to_string(),
        CycleOutcome::Down => "the tunnel was asked to go down".to_string(),
        CycleOutcome::Connected { .. } => "connected".to_string(),
    }
}

/// Wait for a shutdown signal, or for the actor to give up on the tunnel for good.
async fn watch_until_stopped(mut states: watch::Receiver<TunnelState>) -> Result<String> {
    use floppa_vpn_core::actor::types::Phase;
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;

    loop {
        tokio::select! {
            r = tokio::signal::ctrl_c() => { r?; return Ok("SIGINT received".into()) }
            _ = terminate.recv() => return Ok("SIGTERM received".into()),
            _ = hangup.recv() => return Ok("SIGHUP received".into()),
            changed = states.changed() => {
                if changed.is_err() {
                    bail!("the tunnel actor stopped");
                }
                // Reconnecting is not stopping: only a settled Disconnected is, and it means the
                // actor has run out of ways to keep this tunnel up.
                if states.borrow_and_update().phase == Phase::Disconnected {
                    return Ok("the tunnel could not be kept up".into());
                }
            }
        }
    }
}
