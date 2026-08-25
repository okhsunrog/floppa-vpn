//! What the tunnel needs from the thing hosting it — everything the actor cannot do itself.
//!
//! On Android that is the `VpnService`: only it can ask for consent, and only it can turn a
//! [`TunSpec`] into a file descriptor with `Builder.establish()`. Until now the actor reached
//! those through `tauri::AppHandle` and the plugin, which pinned it to the UI process — the one
//! process Android freezes in the background, and the one the tunnel is *not* in.
//!
//! Naming the dependency instead of the process is what lets the same actor run in either. The UI
//! process implements this over the plugin's intent path ([`plugin`]); the `:vpn` process
//! implements it with JNI calls to the service it already lives inside.
//!
//! Desktop has no such host: its ladder configures the machine itself, step by step, through
//! [`Platform`](super::platform::Platform).

#[cfg(target_os = "android")]
pub mod plugin;

use super::autostart::TunSpec;
use async_trait::async_trait;

/// Why the host could not do what the tunnel asked.
///
/// One variant per *decision* a caller makes, not per call site: a refusal ends the whole cycle,
/// an unavailable host ends this attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The user said no, or a policy says no.
    #[error("VPN permission was refused")]
    Refused,
    /// The host could not be reached or did not answer: no activity to show a dialog in, a
    /// service that would not start, a JNI call that failed.
    #[error("the VPN host is unavailable: {detail}")]
    Unavailable { detail: String },
}

/// The service that owns the tunnel's descriptor, seen from the actor.
#[async_trait]
pub trait ServiceHost: Send + Sync {
    /// Whether this app may run a VPN, asking the user if it has to.
    ///
    /// May block on a person, and may never answer at all — Android refuses to start the consent
    /// activity for a background process — so every caller bounds it. Answering `Ok(false)` is a
    /// refusal; being unable to ask is [`HostError::Unavailable`], and the two must not be
    /// confused: a refusal ends the cycle, an unanswerable dialog is worth retrying later.
    async fn consent(&self) -> Result<bool, HostError>;

    /// Make sure a service instance is running for `generation`, holding a TUN built from `spec`.
    async fn start(&self, spec: TunSpec, generation: u64) -> Result<(), HostError>;

    /// Stop the service out of band — the path that still works when its socket does not.
    async fn stop(&self) -> Result<(), HostError>;
}
