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
use crate::actor::types::{AttemptError, AttemptPhase, UpStatus, WorldView};
use crate::actor::world::ServiceReadiness;
use crate::autostart::{self, TunSpec};
use crate::host::HostError;
use crate::rollback::{RollbackStack, Step};
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
    //
    // The one step of this ladder that waits on a person, and the one that could wait forever.
    // `prepare()` resolves from the activity result of the system consent dialog; when the UI
    // process is in the background Android refuses to start that activity, and when the activity
    // is recreated while the dialog is up the reply is lost — either way the future never
    // completes. Cancellation is checked *between* steps, so nothing rescued it: the attempt
    // budget moved the actor to Unwinding, Unwinding has no deadline and absorbs every intent,
    // and the app was stuck until it was restarted.
    //
    // So: honour cancellation here rather than only around here, and bound the wait. Both
    // endings are typed and both are fatal for the cycle — asking three protocols in a row for
    // consent that cannot be given is three dialogs nobody sees.
    let granted = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(AttemptError::Cancelled),
        answer = tokio::time::timeout(ctx.policy.consent_budget, ctx.host.consent()) => {
            match answer {
                Ok(Ok(granted)) => granted,
                Ok(Err(HostError::Refused)) => return Err(AttemptError::PermissionDenied),
                Ok(Err(HostError::Unavailable { detail })) => {
                    return Err(AttemptError::PlatformUnavailable { detail });
                }
                Err(_) => {
                    error!("the VPN consent dialog never answered");
                    return Err(AttemptError::ConsentUnavailable);
                }
            }
        }
    };
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
    let endpoint = resolve(&host).await?;
    info!(%host, %endpoint, "resolved the endpoint before establishing the tunnel");

    // 3. Service --------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Starting).await;

    let spec = TunSpec::derive(&ctx.config, &ctx.params);
    stack.push(Step::AndroidService {
        generation: ctx.generation,
    });

    ctx.host
        .start(spec, ctx.generation)
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;
    stack.confirm_top(Step::AndroidService {
        generation: ctx.generation,
    });

    // 4. Ask the service for a tunnel ------------------------------------------------------
    // The service binds its socket before starting anything, so waiting here means waiting for it
    // to be *reachable* — a bounded, observable thing — and then issuing a request whose failure
    // comes back as a reason. Previously this was a blind poll for a tunnel to appear, and a start
    // that failed was indistinguishable from one still in progress.
    ctx.phase(AttemptPhase::Configuring).await;
    wait_for_service(ctx).await?;
    ctx.backend
        .start_tunnel(ctx.generation, &ctx.config, endpoint, &ctx.params)
        .await
        .map_err(|e| AttemptError::PeerStartFailed {
            detail: e.to_string(),
        })?;

    // 5. Verify -----------------------------------------------------------------------------
    bail_if_cancelled!(ctx);
    ctx.phase(AttemptPhase::Verifying).await;
    verify(ctx, endpoint).await?;

    Ok(UpStatus {
        epoch: ctx.epoch,
        protocol: ctx.protocol,
        params: Some(ctx.params.clone()),
        adopted: false,
        server_endpoint: ctx.config.endpoint_str(),
        assigned_ip: ctx.config.address(),
        connected_at: chrono::Utc::now().timestamp(),
        dark_since: None,
        probing_since: None,
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
            match t.readiness_for(ctx.generation) {
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
                        wanted = ctx.generation,
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

/// Resolve the endpoint, falling back to where it resolved last time.
///
/// The fallback exists for exactly one situation, and it is the situation where a VPN matters
/// most: a start under lockdown. "Block connections without VPN" means nothing reaches the network
/// until the tunnel is up, so the resolver has nothing to answer with — and without an address the
/// tunnel cannot come up. A literal from the last successful connect breaks that circle. Outside
/// lockdown the cache is simply never reached, because DNS answers.
async fn resolve(host: &str) -> Result<std::net::SocketAddr, AttemptError> {
    let failed = |detail: String| {
        if let Some(known) = autostart::known_endpoint(host) {
            warn!(%host, %known, "could not resolve the endpoint ({detail}); using the last known address");
            Ok(known)
        } else {
            Err(AttemptError::ResolveFailed {
                host: host.to_string(),
                detail,
            })
        }
    };

    match tokio::net::lookup_host(host).await {
        Ok(mut addrs) => match addrs.next() {
            Some(addr) => {
                // Recorded on every success, so the cache is as fresh as the last connect. Cheap
                // and best-effort: it costs a small write only when the address actually changed.
                let (host, addr) = (host.to_string(), addr);
                tokio::task::spawn_blocking(move || autostart::remember_endpoint(&host, addr));
                Ok(addr)
            }
            None => failed("resolved to no addresses".into()),
        },
        Err(e) => failed(e.to_string()),
    }
}
