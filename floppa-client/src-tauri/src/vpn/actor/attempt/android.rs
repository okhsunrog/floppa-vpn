//! The Android ladder.
//!
//! Everything the desktop ladder does step by step — address, routes, DNS — the VpnService does
//! atomically inside `Builder.establish()`, so there is one platform step here, not six. Its undo
//! is cross-process, and the OS can perform it unilaterally (revoked consent, low-memory kill), so
//! the undo verifies by observation rather than trusting its own return value.
//!
//! Consent is a genuine fork: the system dialog is shown by `prepare()`, and a refusal must abort
//! the whole cycle rather than being retried per protocol — otherwise a three-protocol order with a
//! reconnect budget is up to nine consent dialogs in a row.

use super::{AttemptCtx, verify};
use crate::vpn::actor::types::{AttemptError, AttemptPhase, UpStatus, WorldView};
use crate::vpn::actor::world::ServiceReadiness;
use crate::vpn::autostart::{self, AutostartBundle, TunSpec};
use crate::vpn::rollback::{RollbackStack, Step};
use tauri_plugin_vpn::VpnExt;
use tracing::{debug, error, info, warn};

macro_rules! bail_if_cancelled {
    ($ctx:expr) => {
        if $ctx.cancelled() {
            return Err(AttemptError::Cancelled);
        }
    };
}

pub(super) async fn ladder(
    ctx: &AttemptCtx,
    stack: &mut RollbackStack,
) -> Result<UpStatus, AttemptError> {
    ctx.phase(AttemptPhase::Preparing).await;
    bail_if_cancelled!(ctx);

    // 1. Consent --------------------------------------------------------------------------
    let granted = ctx
        .app
        .vpn()
        .prepare()
        .await
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("VPN prepare failed: {e}"),
        })?;
    if !granted {
        return Err(AttemptError::PermissionDenied);
    }

    // 2. Resolve the endpoint, before anything touches the system's DNS -------------------
    //
    // Once `Builder.establish()` has run, name resolution on this device is pointed at a tunnel
    // that does not exist yet, so resolving inside the service fails — reliably, not
    // intermittently. The desktop ladder has always resolved before starting for the same reason.
    // The resolved address is handed over with the config, so the service never needs DNS at all.
    bail_if_cancelled!(ctx);
    let host = ctx.config.endpoint_str();
    let endpoint = tokio::net::lookup_host(&host)
        .await
        .map_err(|e| AttemptError::ResolveFailed {
            host: host.clone(),
            detail: e.to_string(),
        })?
        .next()
        .ok_or_else(|| AttemptError::ResolveFailed {
            host: host.clone(),
            detail: "resolved to no addresses".into(),
        })?;
    info!(%host, %endpoint, "resolved the endpoint before establishing the tunnel");

    // 3. Service --------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Starting).await;

    let vpn_config = TunSpec::derive(&ctx.config, &ctx.params).with_epoch(ctx.epoch.0);
    stack.push(Step::AndroidService { epoch: ctx.epoch.0 });

    ctx.app
        .vpn()
        .start(vpn_config)
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;
    stack.confirm_top(Step::AndroidService { epoch: ctx.epoch.0 });

    // 4. Ask the service for a tunnel ------------------------------------------------------
    // The service binds its socket before starting anything, so waiting here means waiting for it
    // to be *reachable* — a bounded, observable thing — and then issuing a request whose failure
    // comes back as a reason. Previously this was a blind poll for a tunnel to appear, and a start
    // that failed was indistinguishable from one still in progress.
    ctx.phase(AttemptPhase::Configuring).await;
    wait_for_service(ctx).await?;
    ctx.backend
        .start_tunnel(ctx.epoch.0, &ctx.config, endpoint, &ctx.params)
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;

    // 5. Verify -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Verifying).await;
    verify(ctx).await?;

    // 6. Remember it for the service's own starts ------------------------------------------
    // Only a tunnel that verified is worth rebuilding without anyone watching. Best-effort: a
    // bundle that could not be written costs the next always-on start, never this connect.
    write_autostart_bundle(ctx, endpoint).await;

    Ok(UpStatus {
        epoch: ctx.epoch,
        protocol: ctx.protocol,
        params: Some(ctx.params.clone()),
        adopted: false,
        server_endpoint: ctx.config.endpoint_str(),
        assigned_ip: ctx.config.address(),
        connected_at: chrono::Utc::now().timestamp(),
        dark_since: None,
        resolved: false,
    })
}

/// Wait until *our* service instance is answering.
///
/// The epoch check is the whole point. Starting a tunnel stops the previous service first, and
/// that teardown is asynchronous: for a moment the dying instance still answers perfectly well.
/// Accepting any reply meant handing the tunnel request to a connection that was about to close,
/// which came back as "the connection to the server was already shutdown" — every time.
///
/// Only reachability is waited for, not a tunnel: the tunnel is requested afterwards and its
/// failure is returned rather than inferred. Bounded by a share of the attempt budget rather than
/// a timeout of its own — two independent timers for one operation is how a connect ends up
/// abandoned by one while still running under the other.
async fn wait_for_service(ctx: &AttemptCtx) -> Result<(), AttemptError> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(200);
    let deadline = std::time::Instant::now() + ctx.policy.attempt_budget / 2;

    loop {
        if ctx.cancelled() {
            return Err(AttemptError::Cancelled);
        }
        if let WorldView::Reachable(t) = ctx.backend.observe().await.view {
            match t.readiness_for(ctx.epoch.0) {
                ServiceReadiness::Ready => return Ok(()),
                ServiceReadiness::Failed(detail) => {
                    error!("the VPN service reported a failed start: {detail}");
                    return Err(AttemptError::PeerStartFailed { detail });
                }
                // Bound before `establish()`: answering is not yet holding a descriptor. A tunnel
                // request now would find nothing to run on; the next poll brings either the
                // descriptor or the reason there is none.
                ServiceReadiness::Establishing => {
                    debug!("our service is up; waiting for it to establish the TUN");
                }
                ServiceReadiness::OtherGeneration(answered) => {
                    debug!(
                        answered,
                        wanted = ctx.epoch.0,
                        "a previous service instance is still answering; waiting for ours"
                    );
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            error!("the VPN service never became reachable");
            return Err(AttemptError::PeerStartFailed {
                detail: "the VPN service did not come up".into(),
            });
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = ctx.cancel.cancelled() => return Err(AttemptError::Cancelled),
        }
    }
}

/// Write the last-good bundle the `:vpn` process rebuilds from when the system starts it without
/// the UI (always-on, boot, lockdown). Runs on a blocking thread: the file is small, but this task
/// shares its runtime with the actor's observer.
async fn write_autostart_bundle(ctx: &AttemptCtx, endpoint: std::net::SocketAddr) {
    let dir = match crate::vpn::config::config_dir() {
        Ok(dir) => dir,
        Err(e) => {
            warn!("not writing the autostart bundle: {e}");
            return;
        }
    };
    let bundle = AutostartBundle::new(
        ctx.config.clone(),
        endpoint,
        ctx.params.clone(),
        chrono::Utc::now().timestamp(),
    );
    let written = tokio::task::spawn_blocking(move || autostart::save(&dir, &bundle)).await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("failed to write the autostart bundle: {e}"),
        Err(e) => warn!("the autostart bundle writer did not finish: {e}"),
    }
}
