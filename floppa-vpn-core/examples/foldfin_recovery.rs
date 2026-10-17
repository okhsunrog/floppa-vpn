//! Isolated WAN acceptance driver; the Python harness owns namespaces and fault injection.
use floppa_vpn_core::{
    actor::{
        TunnelActor,
        deployment::{ConfigSource, Deployment},
        handle::IntentRequest,
        types::CycleOutcome,
    },
    create_backend, get_platform,
};
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("floppa_vpn_core=info,foldfin=info")
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let config = std::fs::read_to_string(std::env::args().nth(1).ok_or("config path required")?)?;
    let handle = TunnelActor::spawn(
        create_backend(),
        Arc::new(get_platform()),
        None,
        Arc::new(|future| {
            tokio::spawn(future);
        }),
        Deployment {
            iface: floppa_vpn_core::protocol::InterfaceName::new("floppa77")
                .map_err(|e| std::io::Error::other(e.0))?,
            manage_dns: false,
            configs: ConfigSource::Ephemeral,
            resume_on_start: false,
            ..Deployment::default()
        },
    )
    .await
    .map_err(std::io::Error::other)?;
    let mut states = handle.states();
    let logger = tokio::spawn(async move {
        loop {
            let state = states.borrow_and_update().clone();
            println!(
                "{}",
                serde_json::json!({
                    "phase": state.phase, "epoch": state.epoch, "outcome_serial": state.outcome_serial,
                    "connected_at": state.connected_at, "assigned_ip": state.assigned_ip,
                    "server_endpoint": state.server_endpoint, "busy": state.busy,
                    "outcome": state.last_outcome
                })
            );
            if states.changed().await.is_err() {
                break;
            }
        }
    });
    let accepted = handle
        .set_intent(IntentRequest::Temporary {
            config,
            private_routes: vec![],
        })
        .await?;
    let outcome = handle.await_cycle(accepted.epoch).await?;
    if !matches!(outcome, CycleOutcome::Connected { .. }) {
        return Err(format!("initial connection failed: {outcome:?}").into());
    }
    tokio::signal::ctrl_c().await?;
    let down = handle.set_intent(IntentRequest::Down).await?;
    let outcome =
        tokio::time::timeout(Duration::from_secs(40), handle.await_cycle(down.epoch)).await??;
    if !matches!(outcome, CycleOutcome::Down) {
        return Err(format!("rollback failed: {outcome:?}").into());
    }
    logger.abort();
    println!("STOPPED");
    Ok(())
}
