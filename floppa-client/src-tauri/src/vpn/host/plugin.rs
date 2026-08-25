//! The host as reached from the UI process: through the Tauri plugin's intent path.
//!
//! Every call here needs an `AppHandle`, which is exactly why it lives outside the actor.

use super::{HostError, ServiceHost};
use crate::vpn::autostart::TunSpec;
use async_trait::async_trait;
use tauri_plugin_vpn::VpnExt;

pub struct PluginHost {
    app: tauri::AppHandle,
}

impl PluginHost {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

#[async_trait]
impl ServiceHost for PluginHost {
    async fn consent(&self) -> Result<bool, HostError> {
        self.app
            .vpn()
            .prepare()
            .await
            .map_err(|e| HostError::Unavailable {
                detail: format!("VPN prepare failed: {e}"),
            })
    }

    async fn start(&self, spec: TunSpec, generation: u64) -> Result<(), HostError> {
        self.app
            .vpn()
            .start(spec.with_generation(generation))
            .await
            .map_err(|e| HostError::Unavailable {
                detail: e.to_string(),
            })
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.app
            .vpn()
            .stop()
            .await
            .map_err(|e| HostError::Unavailable {
                detail: e.to_string(),
            })
    }
}
