//! Where this app's tunnel lives, decided once at startup.
//!
//! Two places it can be, and on Linux both are real. The **system service** holds it in a process
//! that outlives this one, which is what makes closing the window stop meaning "disconnect". **In
//! this process** is what the app has always done, what Windows and macOS do, and what a tarball
//! or an AppImage does — so it is not a legacy path and cannot be allowed to rot.
//!
//! Everything downstream of the choice is already transport-agnostic: `TunnelControl` has both
//! implementations and a Tauri command cannot tell them apart. What the rest of the app still
//! needs to know is what this module answers — because three things genuinely differ, and all
//! three are about *ownership* rather than about how a call is made.
//!
//! 1. **Exit must not take the tunnel down** when it is not ours. The whole feature is that it
//!    survives us.
//! 2. **The peer watcher runs where the actor is**, and nowhere else. Two of them would both see
//!    the same deleted peer and both ask the server to replace it.
//! 3. **A person should be able to see which it is**, because the promises differ. "Closing the
//!    window keeps the VPN up" is true in one mode and false in the other, and a UI that says it
//!    either way is lying half the time.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Who holds the tunnel this app is showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelOwner {
    /// The system service. The tunnel outlives this app.
    Service,
    /// This process. The tunnel goes when the app does.
    InProcess {
        /// Why it is not the service, when that is worth saying. `None` on the platforms where
        /// there is no service to be had and nothing has gone wrong.
        reason: Option<InProcessReason>,
    },
}

/// Why the tunnel ended up in this process on a platform that has a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InProcessReason {
    /// No service is installed or its socket is not enabled.
    NotInstalled,
    /// A service is running and this user may not talk to it — they are not in the `floppa` group.
    ///
    /// Kept apart from [`NotInstalled`](Self::NotInstalled) all the way to the screen. Falling
    /// back works, so nothing looks broken; it just quietly works worse forever, and the reason
    /// has to be visible or nobody will ever find it.
    NotPermitted,
}

impl TunnelOwner {
    /// Whether this app is responsible for taking the tunnel down when it quits.
    pub fn owns_the_tunnel(&self) -> bool {
        matches!(self, Self::InProcess { .. })
    }
}

/// Decide, on a platform where there is a service to look for.
///
/// By reachability rather than a full probe: this runs while a window is on screen, and the wait a
/// full probe can incur is a service cold start, not a call. A version mismatch is caught a moment
/// later by the mirror, which declines to adopt a state it cannot read.
#[cfg(target_os = "linux")]
pub async fn decide() -> TunnelOwner {
    use floppa_vpn_core::client_mode::{reach, system_socket};

    from_access(reach(&system_socket()).await)
}

/// The mapping, apart from the asking, so the distinction that matters can be tested.
#[cfg(target_os = "linux")]
fn from_access(access: floppa_vpn_core::client_mode::ServiceAccess) -> TunnelOwner {
    use floppa_vpn_core::client_mode::ServiceAccess;

    match access {
        ServiceAccess::Available => {
            tracing::info!("the tunnel service is holding the tunnel; this app is a client of it");
            TunnelOwner::Service
        }
        ServiceAccess::Forbidden { detail } => {
            tracing::warn!(
                "the tunnel service is running but this user may not use it ({detail}); \
                 falling back to running the tunnel in this process"
            );
            TunnelOwner::InProcess {
                reason: Some(InProcessReason::NotPermitted),
            }
        }
        // `reach` never answers `WrongVersion` — nothing has been said yet — and an unusable
        // socket is the same as none for the purpose of choosing.
        ServiceAccess::Absent | ServiceAccess::WrongVersion { .. } => TunnelOwner::InProcess {
            reason: Some(InProcessReason::NotInstalled),
        },
    }
}

/// On platforms with no service, there is nothing to decide and nothing to explain.
#[cfg(not(target_os = "linux"))]
pub async fn decide() -> TunnelOwner {
    TunnelOwner::InProcess { reason: None }
}

/// Making the service exist, from the app's side.
///
/// Nothing to do: under socket activation, connecting is what starts it, and the connection the
/// remote handle opens is that connection.
#[cfg(target_os = "linux")]
pub struct StartedByConnecting;

#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl floppa_vpn_core::remote::TunnelProcess for StartedByConnecting {
    async fn ensure_running(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use floppa_vpn_core::client_mode::ServiceAccess;

    #[test]
    fn a_reachable_service_holds_the_tunnel() {
        let owner = from_access(ServiceAccess::Available);
        assert_eq!(owner, TunnelOwner::Service);
        assert!(
            !owner.owns_the_tunnel(),
            "quitting must not take down a tunnel this process does not hold"
        );
    }

    /// The distinction the whole module exists to carry. Both end up running the tunnel here, so
    /// the *behaviour* is the same — and collapsing them would lose the only chance to tell
    /// somebody that a service they installed is one they are not allowed to use.
    #[test]
    fn being_refused_is_not_the_same_as_finding_nothing() {
        let refused = from_access(ServiceAccess::Forbidden {
            detail: "Permission denied".into(),
        });
        let nothing = from_access(ServiceAccess::Absent);

        assert!(refused.owns_the_tunnel() && nothing.owns_the_tunnel());
        assert_ne!(refused, nothing);
        assert_eq!(
            refused,
            TunnelOwner::InProcess {
                reason: Some(InProcessReason::NotPermitted)
            }
        );
        assert_eq!(
            nothing,
            TunnelOwner::InProcess {
                reason: Some(InProcessReason::NotInstalled)
            }
        );
    }
}
