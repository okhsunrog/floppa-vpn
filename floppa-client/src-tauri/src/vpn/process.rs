//! Making the process that holds the actor exist, from the side that cannot do it itself.
//!
//! The `:vpn` process is a service, and only the UI process can ask Android for it: binding needs
//! a context, and the consent dialog needs an activity. So the two things the remote handle cannot
//! do are done here and nowhere else.
//!
//! Consent is asked for *here* rather than in the ladder, and that is the substantive change of the
//! move. The actor can check whether consent is held — a question needs no activity — but it can
//! only be *granted* in front of a person, and Android refuses to start an activity for a process
//! in the background. A reconnect at three in the morning that stops to ask for a dialog nobody can
//! see spends its whole budget on nothing; a reconnect that finds consent already held just works.
//! So the UI asks before it requests a tunnel, and the actor treats missing consent as a refusal.

use crate::vpn::remote::TunnelProcess;
use async_trait::async_trait;
use tauri_plugin_vpn::VpnExt;
use tracing::info;

pub struct ServiceProcess {
    app: tauri::AppHandle,
}

impl ServiceProcess {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

#[async_trait]
impl TunnelProcess for ServiceProcess {
    async fn ensure_running(&self) -> Result<(), String> {
        let granted = self
            .app
            .vpn()
            .prepare()
            .await
            .map_err(|e| format!("could not ask for VPN permission: {e}"))?;
        if !granted {
            return Err("VPN permission was refused".into());
        }
        info!("consent is held; making sure the VPN service is started");
        self.app
            .vpn()
            .start_service()
            .await
            .map_err(|e| format!("could not start the VPN service: {e}"))
    }
}
