//! What one `:vpn` service generation is holding, and how far along it is.
//!
//! Kept apart from the tarpc plumbing in `rpc_server.rs` so that it compiles — and its tests run —
//! on the host: tarpc is an Android-only dependency, and what an observation says about a
//! generation is exactly the kind of judgement that must not be checked only on a phone.

use crate::actor::types::TunnelParams;

/// How the tunnel on this service's descriptor was started, reported back with every
/// observation so the UI process learns it from the owner rather than guessing.
#[derive(Debug, Clone)]
pub struct Started {
    pub params: TunnelParams,
    pub autonomous: bool,
}

/// The descriptor's life in this service: not yet established, held, or already handed to a
/// tunnel. Three states rather than an `Option`, because "not yet" and "already used" both read
/// as `None` and mean opposite things to a start request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FdSlot {
    NotEstablished,
    Ready(std::os::fd::RawFd),
    Taken,
}

/// How far this generation has got, said outright.
///
/// `starting` used to be derived as "no tunnel and no error", which is also what a generation
/// that has been *stopped* looks like — so a stopped instance kept reporting itself as mid-start,
/// the observation classified as [`World::Dark`](crate::actor::types::World) instead of
/// `Clear`, and the actor waited out the whole darkness grace after a revoke before believing the
/// tunnel was gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationPhase {
    /// The socket is bound; `establish()` has not answered yet.
    Bound,
    /// The descriptor is in hand and a tunnel can be asked for.
    Established,
    /// A tunnel is running on the descriptor.
    Started,
    /// This generation is finished. It may still answer over an already-open connection, and what
    /// it says then is "there is nothing here" rather than "wait for me".
    Stopped,
}

/// What the service is holding before any tunnel exists.
///
/// This only has anywhere to live because the RPC server binds ahead of everything else — ahead
/// of `establish()` too. Before that, "the service is coming up", "the service is up and idle",
/// "the TUN could not be established" and "the service failed" were all a socket that would not
/// connect, and the caller could only wait and guess.
pub struct ServiceState {
    /// Identity of this service start, minted per start by whoever asked for it — the UI's
    /// [`ServiceGenerations`](crate::autostart::ServiceGenerations), or the reserved
    /// autonomous range for a start the system issued. Never an intent epoch.
    pub generation: u64,
    /// The descriptor handed over by `VpnService.Builder.establish()`, once it has been.
    tun_fd: std::sync::Mutex<FdSlot>,
    phase: std::sync::Mutex<GenerationPhase>,
    start_error: std::sync::Mutex<Option<String>>,
    /// Set once a tunnel is up on the descriptor.
    started: std::sync::Mutex<Option<Started>>,
}

impl ServiceState {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            tun_fd: std::sync::Mutex::new(FdSlot::NotEstablished),
            phase: std::sync::Mutex::new(GenerationPhase::Bound),
            start_error: std::sync::Mutex::new(None),
            started: std::sync::Mutex::new(None),
        }
    }

    /// The service has established its TUN; from now on a tunnel can be started on it.
    pub fn set_fd(&self, fd: std::os::fd::RawFd) {
        if let Ok(mut guard) = self.tun_fd.lock() {
            *guard = FdSlot::Ready(fd);
        }
        self.advance_to(GenerationPhase::Established);
    }

    /// Holding a descriptor a tunnel can be started on — and only that.
    ///
    /// A `Taken` slot is *not* ready: the descriptor has already been handed to a tunnel, so a
    /// start request against it can only fail. Reporting it as ready is what let a caller see
    /// `Ready` from an instance it had moved past and then get "the tunnel descriptor has
    /// already been used" back from `start_tunnel`.
    pub fn tun_ready(&self) -> bool {
        self.tun_fd
            .lock()
            .map(|g| matches!(*g, FdSlot::Ready(_)))
            .unwrap_or(false)
    }

    pub fn phase(&self) -> GenerationPhase {
        self.phase
            .lock()
            .map(|g| *g)
            .unwrap_or(GenerationPhase::Stopped)
    }

    /// Move forward, never back: a late `set_fd` cannot un-stop a stopped generation.
    pub fn advance_to(&self, phase: GenerationPhase) {
        if let Ok(mut guard) = self.phase.lock()
            && *guard != GenerationPhase::Stopped
        {
            *guard = phase;
        }
    }

    /// This generation is finished. Called from the teardown that owns it, so whatever it answers
    /// afterwards says "nothing here" rather than "still coming up".
    pub fn mark_stopped(&self) {
        if let Ok(mut guard) = self.phase.lock() {
            *guard = GenerationPhase::Stopped;
        }
    }

    /// Is this generation still on its way up?
    ///
    /// The one judgement the descriptor callbacks consult, and the reason it is a method rather
    /// than an expression at the call site: it used to be
    /// derived as "no tunnel and no error", which a *stopped* generation also satisfies — so a
    /// stopped instance kept claiming to be starting, the observation classified as dark rather
    /// than clear, and the actor sat out the whole darkness grace before believing the tunnel had
    /// gone.
    pub fn starting(&self) -> bool {
        matches!(
            self.phase(),
            GenerationPhase::Bound | GenerationPhase::Established
        ) && self.error().is_none()
    }

    pub fn take_fd(&self) -> Result<std::os::fd::RawFd, &'static str> {
        let mut guard = self
            .tun_fd
            .lock()
            .map_err(|_| "the descriptor lock is poisoned")?;
        match *guard {
            FdSlot::Ready(fd) => {
                *guard = FdSlot::Taken;
                Ok(fd)
            }
            FdSlot::NotEstablished => Err("the tunnel descriptor has not been established yet"),
            FdSlot::Taken => Err("the tunnel descriptor has already been used"),
        }
    }

    /// Record why this generation could not start, for whoever observes next. Public because the
    /// one failure the Rust side cannot see itself — `establish()` — is reported from Kotlin.
    pub fn set_error(&self, error: String) {
        if let Ok(mut guard) = self.start_error.lock() {
            *guard = Some(error);
        }
    }

    pub fn error(&self) -> Option<String> {
        self.start_error.lock().ok()?.clone()
    }

    pub fn set_started(&self, started: Started) {
        if let Ok(mut guard) = self.started.lock() {
            *guard = Some(started);
        }
    }

    pub fn started(&self) -> Option<Started> {
        self.started.lock().ok()?.clone()
    }
}

/// Which generation the `:vpn` process is serving, if any.
///
/// The process outlives the service instances inside it, and starting one is asynchronous: a
/// previous instance's teardown routinely arrives *after* the next one has been asked for. So the
/// current generation is held in one place that everything consults, rather than compared against
/// a global that each caller updates for itself — which is how a late `onDestroy` used to tear
/// down the instance that had replaced it.
#[derive(Default)]
pub struct ServiceRegistry {
    current: std::sync::Mutex<Option<std::sync::Arc<ServiceState>>>,
}

impl ServiceRegistry {
    /// Begin a generation, retiring whatever came before it.
    ///
    /// The predecessor is marked stopped rather than dropped: something may still be holding it,
    /// and what it says from now on must be "there is nothing here" rather than "wait for me".
    pub fn begin(&self, generation: u64) -> std::sync::Arc<ServiceState> {
        let state = std::sync::Arc::new(ServiceState::new(generation));
        if let Ok(mut guard) = self.current.lock() {
            if let Some(previous) = guard.take() {
                previous.mark_stopped();
            }
            *guard = Some(state.clone());
        }
        state
    }

    pub fn current(&self) -> Option<std::sync::Arc<ServiceState>> {
        self.current.lock().ok()?.clone()
    }

    /// The state for `generation`, and only if that is the one being served.
    ///
    /// Everything arriving from Kotlin goes through here: a descriptor, a start error or a
    /// teardown that names a generation we have moved past belongs to an instance that is already
    /// gone, and applying it to the current one is the bug this prevents.
    pub fn serving(&self, generation: u64) -> Option<std::sync::Arc<ServiceState>> {
        self.current().filter(|s| s.generation == generation)
    }

    /// End `generation` if it is the current one. Idempotent, and a no-op for any other.
    pub fn end(&self, generation: u64) -> bool {
        let Ok(mut guard) = self.current.lock() else {
            return false;
        };
        match guard.as_ref() {
            Some(state) if state.generation == generation => {
                state.mark_stopped();
                *guard = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_only_ever_answers_for_the_generation_it_is_serving() {
        let registry = ServiceRegistry::default();
        assert!(registry.current().is_none());

        let first = registry.begin(7);
        assert!(registry.serving(7).is_some());
        assert!(registry.serving(8).is_none(), "not this one");

        // A newer start retires the old one where it stands: whatever still holds a reference to
        // it now hears "nothing here" instead of "still coming up".
        let _second = registry.begin(8);
        assert_eq!(first.phase(), GenerationPhase::Stopped);
        assert!(registry.serving(7).is_none());
        assert!(registry.serving(8).is_some());

        // A teardown for the generation that has already been replaced must not touch the one
        // that replaced it — the case a late `onDestroy` produces on every reconnect.
        assert!(!registry.end(7));
        assert!(registry.serving(8).is_some());

        assert!(registry.end(8));
        assert!(registry.current().is_none());
        assert!(!registry.end(8), "and ending it twice is not an error");
    }

    #[test]
    fn a_descriptor_is_ready_only_while_it_is_still_available() {
        let service = ServiceState::new(7);
        assert!(!service.tun_ready(), "nothing established yet");
        assert!(service.starting());

        service.set_fd(3);
        assert!(service.tun_ready());
        assert_eq!(service.phase(), GenerationPhase::Established);
        assert!(
            service.starting(),
            "established but no tunnel asked for yet"
        );

        assert_eq!(service.take_fd(), Ok(3));
        assert!(
            !service.tun_ready(),
            "a descriptor already handed to a tunnel is not ready for another start"
        );
        assert!(service.take_fd().is_err(), "and it cannot be taken twice");
    }

    #[test]
    fn a_stopped_generation_says_there_is_nothing_here_rather_than_wait_for_me() {
        let service = ServiceState::new(7);
        service.set_fd(3);
        let _ = service.take_fd();
        service.advance_to(GenerationPhase::Started);
        assert!(!service.starting());

        service.mark_stopped();
        assert_eq!(service.phase(), GenerationPhase::Stopped);
        assert!(!service.starting());

        // Nothing moves it back: a late callback from the instance that is going away must not
        // make it look alive again.
        service.set_fd(4);
        assert_eq!(service.phase(), GenerationPhase::Stopped);
        assert!(!service.starting());
    }

    #[test]
    fn a_failed_start_is_not_a_start_still_in_progress() {
        let service = ServiceState::new(7);
        service.set_error("VpnService.Builder.establish() returned null".into());
        assert!(!service.starting());
        assert_eq!(
            service.error().as_deref(),
            Some("VpnService.Builder.establish() returned null")
        );
    }
}
