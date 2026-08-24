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
use crate::vpn::actor::types::{AttemptError, AttemptPhase, UpStatus};
use crate::vpn::rollback::{RollbackStack, Step};
use crate::vpn::state::ProtocolConfig;
use tauri_plugin_vpn::VpnExt;
use tracing::{error, info};

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
    // Blocking plugin call, so it runs off the async runtime rather than stalling it.
    let app = ctx.app.clone();
    let granted = tokio::task::spawn_blocking(move || app.vpn().prepare())
        .await
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("consent check panicked: {e}"),
        })?
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("VPN prepare failed: {e}"),
        })?;
    if !granted {
        return Err(AttemptError::PermissionDenied);
    }

    // 2. Service --------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Starting).await;

    let vpn_config = build_config(ctx);
    stack.push(Step::AndroidService { epoch: ctx.epoch.0 });

    let app = ctx.app.clone();
    tokio::task::spawn_blocking(move || app.vpn().start(vpn_config))
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: format!("service start panicked: {e}"),
        })?
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;
    stack.confirm_top(Step::AndroidService { epoch: ctx.epoch.0 });

    // 3. Wait for the tunnel to come up ----------------------------------------------------
    // The service starts asynchronously and the tunnel appears only once the fd has been handed
    // to the Rust side, so the only signal available here is observation.
    ctx.phase(AttemptPhase::Configuring).await;
    wait_for_running(ctx).await?;

    // 4. Verify -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Verifying).await;
    verify(ctx).await?;

    Ok(UpStatus {
        epoch: ctx.epoch,
        protocol: ctx.protocol,
        params: Some(ctx.params.clone()),
        adopted: false,
        server_endpoint: ctx.config.endpoint_str().to_string(),
        assigned_ip: ctx.config.address().to_string(),
        connected_at: chrono::Utc::now().timestamp(),
        dark_since: None,
        resolved: false,
    })
}

async fn wait_for_running(ctx: &AttemptCtx) -> Result<(), AttemptError> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);
    // Bounded by the attempt budget rather than a second, independent timeout: two timers for one
    // operation is how a connect ends up abandoned by one and still running under the other.
    let deadline = std::time::Instant::now() + ctx.policy.attempt_budget / 2;

    loop {
        if ctx.cancelled() {
            return Err(AttemptError::Cancelled);
        }
        if ctx
            .backend
            .observe()
            .await
            .view
            .running_protocol()
            .is_some()
        {
            info!("tunnel is up");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            error!("the VPN service did not bring a tunnel up in time");
            return Err(AttemptError::PeerStartFailed {
                detail: "the VPN service did not report a running tunnel".into(),
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

    let protocol_config_str = match &ctx.config {
        ProtocolConfig::WireGuard(wg) => wg.to_config_str(),
        ProtocolConfig::AmneziaWg(awg) => awg.to_config_str(),
        ProtocolConfig::Vless(vless) => vless.uri.clone(),
    };
    let dns = match &ctx.config {
        ProtocolConfig::WireGuard(wg) => wg.dns.clone(),
        ProtocolConfig::AmneziaWg(awg) => awg.wg.dns.clone(),
        ProtocolConfig::Vless(vless) => vless.dns.clone(),
    };

    let mut config = tauri_plugin_vpn::VpnConfig {
        ipv4_addr: ctx.config.address().to_string(),
        ipv6_addr: None,
        routes: vec!["0.0.0.0/0".into(), "::/0".into()],
        dns,
        mtu: ctx.config.get_mtu() as u32,
        disallowed_apps: vec![],
        allowed_apps: vec![],
        protocol_config: Some(protocol_config_str),
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
