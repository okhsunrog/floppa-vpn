//! Talking to the server as this device, from whichever process is doing the talking.
//!
//! Both callers need the same two things — a client authenticated as the signed-in user, and
//! somewhere for a fetched config to land — and they run in different processes: the UI when the
//! user is looking at the connection card, `:vpn` when a peer has to be replaced with nobody
//! looking. Neither of those is the tunnel's business, which is why none of this is in
//! `floppa-vpn-core`: the actor knows how to run a tunnel and nothing about who provisioned it.

use async_trait::async_trait;
use floppa_api_client::{ApiClient, ConfigSink, DeviceIdentity};
use tracing::{debug, warn};

use super::session;
use crate::vpn::actor::handle::TunnelHandle;
use crate::vpn::config::config_dir;

/// A client for the signed-in user, and the identity to introduce this device by.
///
/// `None` when there is nobody to be: signed out, or a session this build cannot read. Every
/// caller treats that the same way — it cannot talk to the server as anybody, so it must not try.
///
/// Read per call rather than held: the token is rewritten on every sliding refresh, and on Android
/// the process that writes it is not always the one reading it.
pub fn client() -> Option<(ApiClient, DeviceIdentity)> {
    let dir = match config_dir() {
        Ok(dir) => dir,
        Err(e) => {
            warn!("no config directory, so no session: {e}");
            return None;
        }
    };
    let session = session::load(&dir)?;
    match ApiClient::new(&session.base_url, &session.token) {
        Ok(client) => Some((client, session.identity())),
        Err(e) => {
            warn!("could not build an API client: {e}");
            None
        }
    }
}

/// The actor's config store, as somewhere for a fetched config to land.
///
/// On Android in the UI process this writes over the socket, which is the point: there is one
/// store, it lives with the actor, and a config fetched anywhere ends up in it.
pub struct ActorSink(pub TunnelHandle);

#[async_trait]
impl ConfigSink for ActorSink {
    async fn import(&self, raw: String) -> Result<(), String> {
        self.0
            .import_config(raw)
            .await
            .map(|protocol| debug!(%protocol, "a config from the server was stored"))
            .map_err(|e| e.to_string())
    }

    async fn has_any(&self) -> bool {
        !self.0.snapshot().configs.available.is_empty()
    }
}
