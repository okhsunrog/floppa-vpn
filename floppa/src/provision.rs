//! Getting this device a config from the server.
//!
//! The peer logic itself is in `floppa-api-client`, shared with the app; what is here is the
//! CLI's way of asking for one protocol at a time and putting the answer on stdout or into a
//! tunnel, rather than into a config store.

use anyhow::{Result, bail};
use floppa_api_client::{ApiClient, ConfigOutcome, DeviceIdentity, ProvisionApi, config_for_peer};
use floppa_vpn_core::protocol::Protocol;

/// This device's config for `protocol`, creating the peer if it has none.
pub async fn config_for(
    client: &ApiClient,
    protocol: Protocol,
    identity: &DeviceIdentity,
) -> Result<String> {
    // `None` for VLESS, which is provisioned per user and has no peer row. The conversion lives
    // in `floppa-provision` because it is the same question the peer watcher asks.
    let Some(peer_protocol) = floppa_provision::peer_protocol(protocol) else {
        // VLESS is per user: there is nothing to look up and nothing to create.
        return Ok(client.vless_config().await?.uri);
    };

    // Creating is always allowed: the CLI asks for one protocol and expects it, unlike the app,
    // which also provisions the *other* protocol as a bonus and must not conjure one for a server
    // that does not offer it. And the subscription is left to the server to rule on — the local
    // check is only there to save a round trip, and the CLI has not always made one.
    match config_for_peer(client, identity, true, peer_protocol, true).await {
        ConfigOutcome::Ready(config) => Ok(config),
        ConfigOutcome::Offline => bail!("Could not reach the server to get a {protocol} config"),
        ConfigOutcome::Failed(e) => bail!("{e}"),
        // Unreachable with `allow_create = true`, and better said than silently turned into an
        // empty config.
        ConfigOutcome::NotAsked => bail!("No {protocol} peer for this device, and none was made"),
    }
}
