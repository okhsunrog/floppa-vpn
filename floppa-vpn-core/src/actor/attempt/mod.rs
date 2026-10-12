//! One connection attempt, running as its own task.
//!
//! The attempt owns its rollback stack for its entire life and **never hands back a mess**: on
//! failure or cancellation it unwinds its own partial ladder to completion before reporting, and
//! only on success does it hand the stack to the actor. Exactly one owner holds a stack at any
//! instant, so an undo can never run concurrently with an apply.
//!
//! That single property replaces every per-error-path teardown in the old connect flow — eleven
//! hand-written cleanups across the desktop and Android branches, each of which had to remember
//! which subset of the work so far needed undoing, and one of which got it wrong.
//!
//! Cancellation is cooperative and checked *between* steps. The task is never dropped: dropping it
//! mid-step would abandon a half-applied change with nobody left holding the record of it.

#[cfg(not(target_os = "android"))]
mod desktop;

#[cfg(target_os = "android")]
mod android;

use super::handle::{AttemptReport, Command};
use super::types::{
    AttemptError, AttemptPhase, AttemptResult, IntentEpoch, Policy, TunnelParams, WorldView,
};
use crate::backend::VpnBackend;
use crate::platform::Platform;
use crate::protocol::{InterfaceName, Protocol};
use crate::rollback::{Journal, RollbackStack, unwind};
use crate::state::ProtocolConfig;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Everything one attempt needs. Assembled by the actor, moved into the task.
pub struct AttemptCtx {
    pub epoch: IntentEpoch,
    /// The identity of the `:vpn` service instance this attempt starts. Distinct from `epoch`,
    /// which every protocol and pass of one cycle shares — see
    /// [`ServiceGenerations`](crate::autostart::ServiceGenerations).
    #[cfg(target_os = "android")]
    pub generation: u64,
    pub index: usize,
    pub protocol: Protocol,
    pub config: ProtocolConfig,
    pub iface: InterfaceName,
    /// Whether this attempt may configure the machine's DNS. See
    /// [`Deployment::manage_dns`](crate::actor::deployment::Deployment::manage_dns).
    pub manage_dns: bool,
    pub params: TunnelParams,
    pub backend: Arc<dyn VpnBackend>,
    pub platform: Arc<dyn Platform>,
    pub journal: Option<Journal>,
    pub policy: Policy,
    pub cancel: CancellationToken,
    pub tx: mpsc::Sender<Command>,
    /// The service that owns the descriptor. Named as a dependency rather than reached through
    /// the UI process's `AppHandle`, so the same ladder runs wherever the actor is hosted.
    #[cfg(target_os = "android")]
    pub host: std::sync::Arc<dyn crate::host::ServiceHost>,
}

impl AttemptCtx {
    /// Report a cosmetic sub-phase. Never a reconcile input — it only moves the label.
    async fn phase(&self, phase: AttemptPhase) {
        let _ = self
            .tx
            .send(Command::AttemptProgress {
                epoch: self.epoch,
                index: self.index,
                phase,
            })
            .await;
    }

    /// The cancellation checkpoint, taken between steps.
    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Run one attempt to completion and report the outcome.
///
/// Always sends exactly one [`Command::AttemptDone`]: the actor registers the handle before this
/// task starts, so a silent return would leave the status waiting for a report that never comes.
pub async fn run(ctx: AttemptCtx) {
    let tx = ctx.tx.clone();
    let epoch = ctx.epoch;
    let index = ctx.index;

    let result = run_inner(ctx).await;

    let _ = tx
        .send(Command::AttemptDone(Box::new(AttemptReport {
            epoch,
            index,
            result,
        })))
        .await;
}

/// Report a failure without doing any work.
///
/// Used when the attempt cannot even begin — a protocol with no stored config. It still goes
/// through the normal report path rather than being synthesised by the actor, because the actor
/// registers the attempt handle first: a result that skipped the channel would be discarded as
/// stale and the status would wait forever.
pub async fn run_immediate_failure(
    tx: mpsc::Sender<Command>,
    epoch: IntentEpoch,
    index: usize,
    error: AttemptError,
) {
    let _ = tx
        .send(Command::AttemptDone(Box::new(AttemptReport {
            epoch,
            index,
            result: AttemptResult::Failed(error),
        })))
        .await;
}

async fn run_inner(mut ctx: AttemptCtx) -> AttemptResult {
    let mut stack = RollbackStack::new(ctx.journal.clone());

    match ladder(&mut ctx, &mut stack).await {
        Ok(view) => {
            info!(protocol = %ctx.protocol, "attempt established");
            AttemptResult::Established { view, stack }
        }
        Err(error) => {
            let cancelled = matches!(error, AttemptError::Cancelled);
            if cancelled {
                info!(protocol = %ctx.protocol, "attempt cancelled, unwinding");
            } else {
                warn!(protocol = %ctx.protocol, %error, "attempt failed, unwinding");
            }

            // Unwind before reporting. The actor's tables have no cleanup branches because this
            // line guarantees there is nothing left to clean up.
            let report = unwind(
                &mut stack,
                None,
                ctx.platform.as_ref(),
                ctx.backend.as_ref(),
                ctx.policy.undo_retries,
            )
            .await;
            if !report.is_clean() {
                warn!(residual = ?report.residual, "attempt rollback left residue");
            }

            if cancelled {
                AttemptResult::Cancelled
            } else {
                AttemptResult::Failed(error)
            }
        }
    }
}

async fn ladder(
    ctx: &mut AttemptCtx,
    stack: &mut RollbackStack,
) -> Result<super::types::UpStatus, AttemptError> {
    let endpoint = ctx.config.endpoint_str();
    let (host, _port) = floppa_net::split_endpoint(&endpoint)
        .map_err(|detail| AttemptError::InvalidConfig { detail })?;
    let resolver = floppa_net::resolver();
    let identity = ctx.config.endpoint_cache_identity();
    let candidates = resolver.candidates_for(&host, &identity);
    #[cfg(target_os = "android")]
    if candidates.borrow().is_empty()
        && let Some(known) = crate::autostart::known_endpoint(&endpoint)
    {
        // Preserve offline/lockdown starts across upgrades from the old Android cache.
        resolver.observe(&identity, vec![known.ip()], 60);
    }
    #[cfg(target_os = "android")]
    if host.parse::<std::net::IpAddr>().is_err() {
        let service = ctx.host.clone();
        let resolver = resolver.clone();
        let host = host.clone();
        let identity = identity.clone();
        tokio::spawn(async move {
            if let Ok(Ok(addresses)) =
                tokio::time::timeout(std::time::Duration::from_secs(4), service.resolve(&host))
                    .await
            {
                resolver.observe(&identity, addresses, 60);
            }
        });
    }

    connect_candidates(ctx, stack, resolver, candidates).await
}

pub(super) async fn connect_candidates(
    ctx: &mut AttemptCtx,
    stack: &mut RollbackStack,
    resolver: Arc<floppa_net::Resolver>,
    mut candidates: tokio::sync::watch::Receiver<Vec<std::net::IpAddr>>,
) -> Result<super::types::UpStatus, AttemptError> {
    let (host, port) = floppa_net::split_endpoint(&ctx.config.endpoint_str())
        .map_err(|detail| AttemptError::InvalidConfig { detail })?;
    let identity = ctx.config.endpoint_cache_identity();
    #[cfg(target_os = "android")]
    let mut generations = crate::autostart::ServiceGenerations::new();
    let mut tried = Vec::new();
    let mut refreshed_after_failure = false;
    let mut last_error = AttemptError::ResolveFailed {
        host: host.clone(),
        detail: "no usable addresses from DNS or cache".into(),
    };
    loop {
        let addresses = candidates.borrow_and_update().clone();
        for ip in addresses {
            if tried.contains(&ip) {
                continue;
            }
            tried.push(ip);
            if ctx.cancelled() {
                return Err(AttemptError::Cancelled);
            }
            let result = platform_ladder(ctx, stack, std::net::SocketAddr::new(ip, port)).await;
            match result {
                Ok(view) => {
                    if host.parse::<std::net::IpAddr>().is_err() {
                        resolver.remember(&identity, ip).await;
                    }
                    return Ok(view);
                }
                Err(error) => {
                    // Retrying a different address cannot repair permission, configuration or
                    // platform errors. Only a connection verification failure merits a rebuild.
                    if !matches!(error, AttemptError::VerifyFailed) {
                        return Err(error);
                    }
                    resolver.failed(&identity, ip);
                    let report = unwind(
                        stack,
                        None,
                        ctx.platform.as_ref(),
                        ctx.backend.as_ref(),
                        ctx.policy.undo_retries,
                    )
                    .await;
                    if !report.is_clean() {
                        return Err(error);
                    }
                    #[cfg(target_os = "android")]
                    {
                        ctx.generation = generations.mint();
                    }
                    last_error = error;
                }
            }
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(AttemptError::Cancelled),
            changed = candidates.changed() => if changed.is_err() {
                // Desktop DNS/DoH can have run while a failed TUN owned the routes. Once
                // rollback restored the underlay, allow one fresh discovery before giving up.
                if !tried.is_empty() && !refreshed_after_failure {
                    refreshed_after_failure = true;
                    candidates = resolver.refresh_for(&host, &identity);
                } else { return Err(last_error); }
            }
        }
    }
}

#[cfg(target_os = "android")]
use android::ladder as platform_ladder;
#[cfg(not(target_os = "android"))]
use desktop::ladder as platform_ladder;

/// Verification, shared by both ladders.
///
/// This is the step that decides whether a tunnel that *started* is actually carrying traffic —
/// the difference between "the interface exists" and "the peer answered".
///
/// `endpoint` is the address the ladder resolved before it built anything, and VLESS needs it for
/// the same reason the tunnel does: by now this device's DNS points into a tunnel that is not
/// carrying traffic yet, so resolving the server's name here fails — on Android it failed every
/// time, six seconds into a DNS query the tunnel under test was supposed to answer. The check
/// dials the literal, exactly as the running tunnel does.
pub(super) async fn verify(
    ctx: &AttemptCtx,
    endpoint: std::net::SocketAddr,
) -> Result<(), AttemptError> {
    match &ctx.config {
        ProtocolConfig::WireGuard(_) | ProtocolConfig::AmneziaWg(_) => {
            info!("tunnel up, waiting for a handshake");
            wait_for_handshake(ctx).await
        }
        ProtocolConfig::Vless(vless) => {
            info!("tunnel up, checking VLESS connectivity");
            let mut dialled = vless.clone();
            dialled.server_addr = endpoint.to_string();
            match dialled
                .to_shoes_config()
                .check_connectivity(ctx.policy.verify_vless)
                .await
            {
                Ok(()) => Ok(()),
                Err(e) => {
                    info!("VLESS connectivity check failed: {e}");
                    Err(AttemptError::VerifyFailed)
                }
            }
        }
    }
}

/// Poll until the tunnel has seen a recent inbound packet, or the budget runs out.
///
/// Cancellation is honoured here too: a user pressing Cancel during verification should not have
/// to wait out the full handshake timeout.
async fn wait_for_handshake(ctx: &AttemptCtx) -> Result<(), AttemptError> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);
    const RECENT_SECS: i64 = 10;

    let start = std::time::Instant::now();
    loop {
        if ctx.cancelled() {
            return Err(AttemptError::Cancelled);
        }
        // Unreachable reads as "no packet yet" and simply loops: an attempt that cannot ask is not
        // evidence of a handshake either way, and the budget below is what ends the wait.
        if let WorldView::Reachable(tunnel) = ctx.backend.observe().await.view
            && let Some(secs) = tunnel.last_packet_secs
            && secs < RECENT_SECS
        {
            return Ok(());
        }
        if start.elapsed() > ctx.policy.verify_wg {
            info!("no handshake within the budget — the peer is likely invalid");
            return Err(AttemptError::VerifyFailed);
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = ctx.cancel.cancelled() => return Err(AttemptError::Cancelled),
        }
    }
}
