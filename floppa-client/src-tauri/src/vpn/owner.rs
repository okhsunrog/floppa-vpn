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
    /// Service mode is selected, but the service cannot currently be used.
    ServiceUnavailable { problem: ServiceProblem },
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceProblem {
    NotPermitted,
    WrongVersion { theirs: u32 },
    Unresponsive,
}

impl TunnelOwner {
    /// Whether this app is responsible for taking the tunnel down when it quits.
    pub fn owns_the_tunnel(&self) -> bool {
        matches!(self, Self::InProcess { .. })
    }
}

/// Decide, on a platform where there is a service to look for.
///
/// Use a quick socket check during setup. RPC validates the version before commands;
/// the GUI's asynchronous ownership query supplies detailed status without blocking setup.
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
            tracing::warn!("the tunnel service is running but this user may not use it ({detail})");
            TunnelOwner::ServiceUnavailable {
                problem: ServiceProblem::NotPermitted,
            }
        }
        ServiceAccess::Absent => TunnelOwner::InProcess {
            reason: Some(InProcessReason::NotInstalled),
        },
        ServiceAccess::WrongVersion { theirs } => TunnelOwner::ServiceUnavailable {
            problem: ServiceProblem::WrongVersion { theirs },
        },
        ServiceAccess::Unresponsive { .. } => TunnelOwner::ServiceUnavailable {
            problem: ServiceProblem::Unresponsive,
        },
    }
}

/// Report service health without changing the owner selected at startup.
#[cfg(target_os = "linux")]
pub async fn inspect(owner: &TunnelOwner) -> TunnelOwner {
    use floppa_vpn_core::client_mode::{ServiceAccess, probe, system_socket};
    if owner.owns_the_tunnel() {
        return owner.clone();
    }
    match probe(&system_socket()).await {
        ServiceAccess::Absent => TunnelOwner::ServiceUnavailable {
            problem: ServiceProblem::Unresponsive,
        },
        access => from_access(access),
    }
}

/// On platforms with no service, there is nothing to decide and nothing to explain.
#[cfg(not(target_os = "linux"))]
pub async fn decide() -> TunnelOwner {
    TunnelOwner::InProcess { reason: None }
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

    /// Permission denial must never authorize a competing local actor.
    #[test]
    fn being_refused_is_not_the_same_as_finding_nothing() {
        let refused = from_access(ServiceAccess::Forbidden {
            detail: "Permission denied".into(),
        });
        let nothing = from_access(ServiceAccess::Absent);

        assert!(!refused.owns_the_tunnel());
        assert!(nothing.owns_the_tunnel());
        assert_ne!(refused, nothing);
        assert_eq!(
            refused,
            TunnelOwner::ServiceUnavailable {
                problem: ServiceProblem::NotPermitted
            }
        );
        assert_eq!(
            nothing,
            TunnelOwner::InProcess {
                reason: Some(InProcessReason::NotInstalled)
            }
        );
    }

    #[test]
    fn a_broken_or_incompatible_service_never_authorizes_a_local_actor() {
        for access in [
            ServiceAccess::WrongVersion { theirs: 999 },
            ServiceAccess::Unresponsive {
                detail: "timeout".into(),
            },
        ] {
            let owner = from_access(access);
            assert!(!owner.owns_the_tunnel());
            assert!(matches!(owner, TunnelOwner::ServiceUnavailable { .. }));
        }
    }
}
