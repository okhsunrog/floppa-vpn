//! The desktop ladder: seven steps, each recorded before it is applied.
//!
//! The order matters and is not arbitrary. The endpoint route must be pinned through the original
//! gateway *before* the tunnel routes are installed, or the tunnel's own traffic to the server
//! would be routed into the tunnel. DNS goes last so a failure there leaves a working tunnel.

use super::{AttemptCtx, verify};
use crate::vpn::actor::types::{AttemptError, AttemptPhase, DnsFailurePolicy, UpStatus};
use crate::vpn::platform::{Platform, PlatformError};
use crate::vpn::rollback::{RollbackStack, Step, StepKind, split_default};
use tracing::{error, info};

/// Map a platform failure onto the attempt vocabulary, keeping the distinction the platform drew:
/// a refused privilege will not be fixed by trying the next protocol, it will just ask again.
fn platform_error(step: StepKind, e: PlatformError) -> AttemptError {
    match e {
        PlatformError::PermissionDenied(_) => AttemptError::PermissionDenied,
        PlatformError::Unavailable(detail) => AttemptError::PlatformUnavailable { detail },
        PlatformError::Failed(detail) => AttemptError::Platform { step, detail },
    }
}

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
    let iface = ctx.iface.clone();

    ctx.phase(AttemptPhase::Preparing).await;

    // A missing privileged helper is worth knowing before anything is mutated, not halfway up.
    ctx.platform
        .preflight()
        .await
        .map_err(|e| platform_error(StepKind::PrepareLink, e))?;

    let endpoint_str = ctx.config.endpoint_str().to_string();
    let endpoint = tokio::net::lookup_host(&endpoint_str)
        .await
        .map_err(|e| AttemptError::ResolveFailed {
            host: endpoint_str.clone(),
            detail: e.to_string(),
        })?
        .next()
        .ok_or_else(|| AttemptError::ResolveFailed {
            host: endpoint_str.clone(),
            detail: "resolved to no addresses".into(),
        })?;
    let endpoint_ip = endpoint.ip();

    // 1. Link -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    stack.push(Step::PrepareLink {
        iface: iface.clone(),
    });
    ctx.platform
        .prepare_link(&iface)
        .await
        .map_err(|e| platform_error(StepKind::PrepareLink, e))?;
    stack.confirm_top(Step::PrepareLink {
        iface: iface.clone(),
    });

    // 2. Tunnel ---------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Starting).await;
    let tun_params = ctx.platform.tun_params();
    stack.push(Step::StartBackend {
        iface: iface.clone(),
    });
    let started = match ctx
        .backend
        .start(&ctx.config, iface.as_str(), &tun_params, endpoint)
        .await
    {
        // The kernel refuses fwmark without the capability; retry once without it rather than
        // failing the whole protocol over a routing nicety.
        Err(e)
            if tun_params.fwmark.is_some()
                && (e.contains("Operation not permitted") || e.contains("Permission denied")) =>
        {
            info!("tunnel start with fwmark was refused, retrying without it");
            let mut retry = tun_params;
            retry.fwmark = None;
            ctx.backend
                .start(&ctx.config, iface.as_str(), &retry, endpoint)
                .await
        }
        other => other,
    };
    started.map_err(|detail| AttemptError::Backend { detail })?;
    stack.confirm_top(Step::StartBackend {
        iface: iface.clone(),
    });

    // 3. Address --------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Configuring).await;
    let addr = ctx
        .config
        .address_network()
        .map_err(|detail| AttemptError::InvalidConfig { detail })?;
    stack.push(Step::Address {
        iface: iface.clone(),
        addr,
    });
    ctx.platform
        .configure_address(&iface, addr)
        .await
        .map_err(|e| platform_error(StepKind::Address, e))?;
    stack.confirm_top(Step::Address {
        iface: iface.clone(),
        addr,
    });

    // 4. Endpoint route -------------------------------------------------------------------
    // The gateway is read before the push so the undo can match on it. Without that, the undo
    // deletes any route to the endpoint — and after a roaming event, which is exactly when a
    // reconnect happens, that is the wrong route or none at all.
    bail_if_cancelled!(ctx);
    let gateway = ctx
        .platform
        .default_gateway(crate::vpn::platform::IpFamily::of(endpoint_ip))
        .await
        .unwrap_or_default();
    stack.push(Step::EndpointRoute {
        endpoint: endpoint_ip,
        gateway,
    });
    ctx.platform
        .add_endpoint_route(endpoint_ip, gateway.as_ref())
        .await
        .map_err(|e| platform_error(StepKind::EndpointRoute, e))?;
    stack.confirm_top(Step::EndpointRoute {
        endpoint: endpoint_ip,
        gateway,
    });

    // 5. Routes ---------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    let if_index = ctx.platform.interface_index(&iface).await;
    let routes = split_default(
        &ctx.config.allowed_ips_networks(),
        ctx.platform.ipv6_enabled().await,
    );
    stack.push(Step::Routes {
        iface: iface.clone(),
        routes: routes.clone(),
        if_index,
    });
    ctx.platform
        .add_routes(&iface, &routes, if_index)
        .await
        .map_err(|e| platform_error(StepKind::Routes, e))?;
    stack.confirm_top(Step::Routes {
        iface: iface.clone(),
        routes,
        if_index,
    });

    // 6. DNS ------------------------------------------------------------------------------
    // Captured before the mutation and owned by the step, so a second attempt can never snapshot
    // the resolver config this process itself wrote.
    bail_if_cancelled!(ctx);
    let dns_servers = ctx.config.dns_servers();
    if !dns_servers.is_empty() {
        match ctx.platform.capture_dns(&iface, if_index).await {
            Ok(snapshot) => {
                stack.push(Step::Dns {
                    iface: iface.clone(),
                    snapshot: snapshot.clone(),
                    if_index,
                });
                match ctx
                    .platform
                    .configure_dns(&iface, &dns_servers, if_index)
                    .await
                {
                    Ok(()) => stack.confirm_top(Step::Dns {
                        iface: iface.clone(),
                        snapshot,
                        if_index,
                    }),
                    Err(e) => {
                        // The step stays on the stack either way, so the undo still runs.
                        error!("failed to configure DNS: {e}");
                        if matches!(ctx.policy.dns_failure, DnsFailurePolicy::Fatal) {
                            return Err(platform_error(StepKind::Dns, e));
                        }
                    }
                }
            }
            Err(e) => error!("failed to capture DNS state, leaving DNS untouched: {e}"),
        }
    }

    // 7. Verify ---------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Verifying).await;
    verify(ctx).await?;

    Ok(UpStatus {
        epoch: ctx.epoch,
        protocol: ctx.protocol,
        params: Some(ctx.params.clone()),
        adopted: false,
        server_endpoint: endpoint_str,
        assigned_ip: ctx.config.address().to_string(),
        connected_at: chrono::Utc::now().timestamp(),
        dark_since: None,
        // Set by the actor when it accepts the result; an attempt cannot resolve its own waiter.
        resolved: false,
    })
}
