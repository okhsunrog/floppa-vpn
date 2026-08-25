//! The host as reached from inside the service itself.
//!
//! This is what the actor uses now that it lives in `:vpn`: the `VpnService` object is in this
//! process, so asking it for a descriptor is a JNI call rather than an intent to another process.
//!
//! Two of the three operations are synchronous questions and one is not. `establish()` has to run
//! on the main thread and can only answer by calling back — `nativeSetTunFd` on success,
//! `nativeReportStartError` on failure — so [`ServiceHost::start`] returns as soon as the request
//! is placed, and the ladder waits for the descriptor by *observing*, exactly as it did when the
//! request crossed a process boundary. Keeping that shape is deliberate: it is the shape that
//! makes "still coming up", "failed, and here is why" and "not there at all" three different
//! answers instead of one timeout.

use super::{HostError, ServiceHost};
use crate::vpn::autostart::TunSpec;
use crate::vpn::service_state::ServiceRegistry;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::info;

pub struct JniServiceHost {
    services: Arc<ServiceRegistry>,
}

impl JniServiceHost {
    pub fn new(services: Arc<ServiceRegistry>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl ServiceHost for JniServiceHost {
    /// Whether this app already holds VPN consent.
    ///
    /// A question, never a dialog: `VpnService.prepare` can be *checked* from anywhere and can only
    /// be *shown* from an activity, which this process does not have. So consent that is missing is
    /// reported as a refusal and the UI — which does have an activity — is what asks for it. The
    /// alternative is a background reconnect burning its whole budget on a dialog nobody can see.
    async fn consent(&self) -> Result<bool, HostError> {
        crate::vpn::jni_entry::has_vpn_consent().map_err(|detail| HostError::Unavailable { detail })
    }

    async fn start(&self, spec: TunSpec, generation: u64) -> Result<(), HostError> {
        // Registered before the request goes out: the answer comes back as a JNI call naming this
        // generation, and it has to find something to be recorded against.
        self.services.begin(generation);
        let plan = serde_json::to_string(&spec.with_generation(generation)).map_err(|e| {
            HostError::Unavailable {
                detail: format!("the TUN spec cannot be encoded for the service: {e}"),
            }
        })?;
        info!(generation, "asking the service to establish a TUN");
        crate::vpn::jni_entry::start_generation(&plan, generation).map_err(|detail| {
            // Nothing will call back for a request that was never placed.
            self.services.end(generation);
            HostError::Unavailable { detail }
        })
    }

    async fn stop(&self) -> Result<(), HostError> {
        crate::vpn::jni_entry::stop_vpn_service();
        Ok(())
    }
}
