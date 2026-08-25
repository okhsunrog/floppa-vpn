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
use crate::vpn::rollback::{RollbackStack, Step};
use tauri_plugin_vpn::VpnExt;
use tracing::{debug, error, info};

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

    let vpn_config = build_config(ctx);
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
        .start_tunnel(ctx.epoch.0, &ctx.config, endpoint)
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;

    // 5. Verify -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Verifying).await;
    verify(ctx).await?;

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
            if t.epoch == ctx.epoch.0 {
                if let Some(detail) = t.start_error {
                    error!("the VPN service reported a failed start: {detail}");
                    return Err(AttemptError::PeerStartFailed { detail });
                }
                return Ok(());
            }
            debug!(
                answered = t.epoch,
                wanted = ctx.epoch.0,
                "a previous service instance is still answering; waiting for ours"
            );
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

fn build_config(ctx: &AttemptCtx) -> tauri_plugin_vpn::VpnConfig {
    use crate::vpn::actor::types::SplitMode;

    // Resolvers only: `VpnService.Builder.addDnsServer` takes addresses, and a search domain on
    // the DNS line would just be logged as an invalid server on the Kotlin side.
    let dns_servers = ctx.config.dns_servers();
    let dns =
        (!dns_servers.is_empty()).then(|| floppa_tunnel_config::conf::comma_list(dns_servers));

    let mut config = tauri_plugin_vpn::VpnConfig {
        ipv4_addr: ctx.config.address(),
        ipv6_addr: None,
        routes: floppa_tunnel_config::route::CATCH_ALL
            .iter()
            .map(ToString::to_string)
            .collect(),
        dns,
        mtu: ctx.config.get_mtu() as u32,
        disallowed_apps: vec![],
        allowed_apps: vec![],
        epoch: ctx.epoch.0,
    };

    let apps = ctx.params.apps.clone();
    if !apps.is_empty() {
        match ctx.params.split_mode {
            SplitMode::Exclude => config.disallowed_apps = apps,
            SplitMode::Include => config.allowed_apps = apps,
            SplitMode::All => {}
        }
    }
    config
}
