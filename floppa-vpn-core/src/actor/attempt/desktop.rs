//! The desktop ladder: each step is recorded before it is applied.
//!
//! Linux marks transport sockets before enabling policy routing. Other desktop platforms pin
//! an endpoint route before installing tunnel routes. DNS is applied last.

use super::{AttemptCtx, verify};
use crate::actor::types::{AttemptError, AttemptPhase, DnsFailurePolicy, UpStatus};
use crate::platform::PlatformError;
use crate::rollback::{RollbackStack, Step, StepKind, split_default};
use tracing::error;

/// Map a platform failure onto the attempt vocabulary, keeping the distinction the platform drew:
/// a refused privilege will not be fixed by trying the next protocol, it will just ask again.
fn platform_error(step: StepKind, e: PlatformError) -> AttemptError {
    match e {
        PlatformError::PermissionDenied(detail) => AttemptError::Backend {
            error: crate::backend::BackendError::PermissionDenied { detail },
        },
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
    endpoint: std::net::SocketAddr,
) -> Result<UpStatus, AttemptError> {
    let iface = ctx.iface.clone();

    ctx.phase(AttemptPhase::Preparing).await;

    // A missing privileged helper is worth knowing before anything is mutated, not halfway up.
    ctx.platform
        .preflight()
        .await
        .map_err(|e| platform_error(StepKind::PrepareLink, e))?;

    let endpoint_str = ctx.config.endpoint_str();
    let endpoint_ip = endpoint.ip();

    // 1. Link -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    stack
        .push(Step::PrepareLink {
            iface: iface.clone(),
        })
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("cannot persist rollback journal: {e}"),
        })?;
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
    stack
        .push(Step::StartBackend {
            iface: iface.clone(),
        })
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("cannot persist rollback journal: {e}"),
        })?;
    ctx.backend
        .start(&ctx.config, iface.as_str(), &tun_params, endpoint)
        .await
        .map_err(|error| AttemptError::Backend { error })?;
    stack.confirm_top(Step::StartBackend {
        iface: iface.clone(),
    });

    if matches!(ctx.config, crate::state::ProtocolConfig::Foldfin(_)) {
        ctx.platform
            .configure_mtu(&iface, ctx.config.get_mtu())
            .await
            .map_err(|e| platform_error(StepKind::PrepareLink, e))?;
    }

    // 3. Address --------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Configuring).await;
    let addresses = match &ctx.config {
        crate::state::ProtocolConfig::Foldfin(config) => config.addresses().to_vec(),
        _ => vec![
            ctx.config
                .address_network()
                .map_err(|e| AttemptError::InvalidConfig {
                    detail: e.to_string(),
                })?,
        ],
    };
    for addr in addresses {
        bail_if_cancelled!(ctx);
        stack.push(Step::Address {
            iface: iface.clone(),
            addr,
        }).map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("cannot persist rollback journal: {e}"),
        })?;
        ctx.platform
            .configure_address(&iface, addr)
            .await
            .map_err(|e| platform_error(StepKind::Address, e))?;
        stack.confirm_top(Step::Address {
            iface: iface.clone(),
            addr,
        });
    }

    // Only platforms without marked-socket routing pin a host route to the endpoint.
    if !ctx.platform.uses_policy_routing() {
        // 4. Endpoint route -------------------------------------------------------------------
        // The gateway is read before the push so the undo can match on it. Without that, the undo
        // deletes any route to the endpoint — and after a roaming event, which is exactly when a
        // reconnect happens, that is the wrong route or none at all.
        bail_if_cancelled!(ctx);
        let gateway = ctx
            .platform
            .default_gateway(crate::platform::IpFamily::of(endpoint_ip))
            .await
            .unwrap_or_default();
        stack
            .push(Step::EndpointRoute {
                endpoint: endpoint_ip,
                gateway,
            })
            .map_err(|e| AttemptError::PlatformUnavailable {
                detail: format!("cannot persist rollback journal: {e}"),
            })?;
        ctx.platform
            .add_endpoint_route(endpoint_ip, gateway.as_ref())
            .await
            .map_err(|e| platform_error(StepKind::EndpointRoute, e))?;
        stack.confirm_top(Step::EndpointRoute {
            endpoint: endpoint_ip,
            gateway,
        });
    }

    // 5. Routes ---------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    let if_index = ctx.platform.interface_index(&iface).await;
    // Both must hold: the host has to be able to carry IPv6 at all, and *this tunnel* has to
    // have an IPv6 address to carry it over. The second half was missing, and a tunnel with only
    // an IPv4 address still claimed `::/1` and `8000::/1` — which every IPv6-preferring client
    // then chose, and hung. See `ProtocolConfig::has_ipv6_address`.
    let ipv6 = ctx.config.has_ipv6_address() && ctx.platform.ipv6_enabled().await;
    let routes = if ctx.params.allow_lan {
        floppa_tunnel_config::route::exclude_local_networks(
            &ctx.config.allowed_ips_networks(),
            ipv6,
        )
    } else {
        split_default(&ctx.config.allowed_ips_networks(), ipv6)
    };
    let routes = floppa_tunnel_config::route::with_extra_routes(
        routes,
        &ctx.params.private_route_networks(),
    );
    let routes = floppa_tunnel_config::route::exclude_networks(
        routes,
        &ctx.params
            .excluded_routes
            .iter()
            .map(|r| r.parse().expect("validated exclusion"))
            .collect::<Vec<_>>(),
    );
    stack
        .push(Step::Routes {
            iface: iface.clone(),
            routes: routes.clone(),
            if_index,
        })
        .map_err(|e| AttemptError::PlatformUnavailable {
            detail: format!("cannot persist rollback journal: {e}"),
        })?;
    ctx.platform
        .add_routes(&iface, &routes, if_index)
        .await
        .map_err(|e| platform_error(StepKind::Routes, e))?;
    stack.confirm_top(Step::Routes {
        iface: iface.clone(),
        routes: routes.clone(),
        if_index,
    });

    if let Some(snapshot) = ctx
        .platform
        .capture_policy_routing(&routes)
        .await
        .map_err(|e| platform_error(StepKind::PolicyRouting, e))?
    {
        stack
            .push(Step::PolicyRouting {
                snapshot: snapshot.clone(),
            })
            .map_err(|e| AttemptError::PlatformUnavailable {
                detail: format!("cannot persist rollback journal: {e}"),
            })?;
        ctx.platform
            .apply_policy_routing(&snapshot)
            .await
            .map_err(|e| platform_error(StepKind::PolicyRouting, e))?;
        stack.confirm_top(Step::PolicyRouting { snapshot });
    }

    // 6. DNS ------------------------------------------------------------------------------
    // Captured before the mutation and owned by the step, so a second attempt can never snapshot
    // the resolver config this process itself wrote.
    bail_if_cancelled!(ctx);
    // An empty list and "do not touch DNS" reach the same place, and neither is an error: the
    // first is a config that names no servers, the second a caller that has said the machine's
    // resolution is not ours to change.
    let dns_servers = if ctx.manage_dns {
        ctx.config.dns_servers()
    } else {
        Vec::new()
    };
    if !dns_servers.is_empty() {
        match ctx.platform.capture_dns(&iface, if_index).await {
            Ok(snapshot) => {
                stack
                    .push(Step::Dns {
                        iface: iface.clone(),
                        snapshot: snapshot.clone(),
                        if_index,
                    })
                    .map_err(|e| AttemptError::PlatformUnavailable {
                        detail: format!("cannot persist rollback journal: {e}"),
                    })?;
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
    verify(ctx, endpoint).await?;

    Ok(UpStatus {
        epoch: ctx.epoch,
        protocol: ctx.protocol,
        params: Some(ctx.params.clone()),
        adopted: false,
        server_endpoint: endpoint_str,
        assigned_ip: ctx.config.address(),
        connected_at: chrono::Utc::now().timestamp(),
        dark_since: None,
        probing_since: None,
        // Set by the actor when it accepts the result; an attempt cannot resolve its own waiter.
        resolved: false,
    })
}
