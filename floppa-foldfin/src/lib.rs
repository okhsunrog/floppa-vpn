//! Floppa's device access adapter for the Foldfin listener.
//!
//! Feed committed control-plane grants into this registry. PostgreSQL, subscription
//! checks, address allocation and OS routes belong to the host, not the protocol engine.
#[cfg(feature = "node")]
pub mod network;
#[cfg(feature = "node")]
pub mod node;
#[cfg(feature = "node")]
mod policy;
pub mod tls;
use foldfin::{
    authorization::{
        AccessGrant, AccessLease, AccessRequest, Authorizer, LeaseState, RequestedMode,
    },
    server::{Server, VpnConfig},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock, Weak},
    time::Instant,
};
use tokio::sync::{Semaphore, watch};

/// A high-entropy credential is stored by its SHA-256 digest, never plaintext.
/// Digest of raw token bytes, excluding the `Bearer ` prefix.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenDigest([u8; 32]);
impl TokenDigest {
    pub fn from_token(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }
    /// Import the digest recorded by the provisioning service.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl std::fmt::Debug for TokenDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenDigest([REDACTED])")
    }
}

/// Trusted control-plane decision for an installation in a region.
/// Construct only after checking account, subscription, device ownership and region entitlement.
pub struct DeviceGrant {
    pub device_id: String,
    pub token_digest: TokenDigest,
    pub expires_at: Instant,
    pub proxy: bool,
    pub allow_private_proxy: bool,
    /// The IP policy and packet factory enforce this device's source/destination prefixes.
    pub vpn: Option<VpnConfig>,
}
struct Entry {
    device_id: String,
    grant: AccessGrant,
    revoke: watch::Sender<LeaseState>,
}
#[derive(Default)]
struct RegistryState {
    entries: HashMap<TokenDigest, Entry>,
    // Keep the VPN slot across rotation/revoke until every old session has released it.
    device_slots: HashMap<String, Weak<Semaphore>>,
}
#[derive(Default)]
pub struct DeviceRegistry(RwLock<RegistryState>);

impl DeviceRegistry {
    /// Replace a device credential atomically; closes streams using its previous grant.
    /// Call on actual rotation/policy changes, not every unchanged polling snapshot.
    pub fn install(&self, device: DeviceGrant) -> Result<(), String> {
        if device.device_id.is_empty() || device.expires_at <= Instant::now() {
            return Err("invalid/expired device grant".into());
        }
        if let Some(vpn) = &device.vpn {
            vpn.policy.validate().map_err(|e| e.to_string())?;
        }
        let peer =
            foldfin::admission::PeerId::new(device.device_id.clone()).map_err(|e| e.to_string())?;
        let mut state = self.0.write().unwrap();
        let slot = state
            .device_slots
            .get(&device.device_id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(Semaphore::new(1)));
        state
            .device_slots
            .insert(device.device_id.clone(), Arc::downgrade(&slot));
        let entries = &mut state.entries;
        if entries
            .get(&device.token_digest)
            .is_some_and(|entry| entry.device_id != device.device_id)
        {
            return Err("credential digest already belongs to another device".into());
        }
        entries.retain(|_, entry| {
            if entry.device_id == device.device_id {
                entry.revoke.send_replace(LeaseState::Revoked);
                false
            } else {
                true
            }
        });
        let (revoke, lease) = AccessLease::new(Some(device.expires_at));
        entries.insert(
            device.token_digest,
            Entry {
                device_id: device.device_id,
                grant: AccessGrant {
                    peer: Some(peer),
                    proxy: device.proxy,
                    allow_private: device.allow_private_proxy,
                    vpn: device.vpn,
                    vpn_slots: slot,
                    lease,
                    session: None,
                },
                revoke,
            },
        );
        Ok(())
    }
    /// Renew a still-valid control-plane lease without interrupting its streams.
    /// Policy/credential changes use install(); an expired/revoked grant cannot be revived.
    pub fn renew(&self, device_id: &str, expires_at: Instant) -> Result<(), String> {
        if expires_at <= Instant::now() {
            return Err("lease deadline already passed".into());
        }
        // Serialize renewal with rotation/revocation, including the lease update.
        let mut state = self.0.write().unwrap();
        let entry = state
            .entries
            .values_mut()
            .find(|entry| entry.device_id == device_id && entry.grant.valid())
            .ok_or("no active device lease")?;
        entry.revoke.send_replace(LeaseState::Active {
            expires_at: Some(expires_at),
        });
        Ok(())
    }

    /// Device deletion, logout-all, subscription expiry or policy withdrawal.
    pub fn revoke(&self, device_id: &str) {
        self.0.write().unwrap().entries.retain(|_, entry| {
            if entry.device_id == device_id {
                entry.revoke.send_replace(LeaseState::Revoked);
                false
            } else {
                true
            }
        });
    }
    /// Expiration still closes existing streams when the control-plane sync is unavailable.
    pub fn prune_expired(&self) {
        let mut state = self.0.write().unwrap();
        state.entries.retain(|_, entry| entry.grant.valid());
        state.device_slots.retain(|_, slot| slot.strong_count() > 0);
    }

    /// The host supplies prepared TLS, listener, packet devices and shutdown.
    pub fn server(
        self: &Arc<Self>,
        tls: Arc<foldfin::server::TlsConfig>,
    ) -> foldfin::error::Result<Server> {
        Server::with_authorizer(tls, self.clone())
    }
}
impl Authorizer for DeviceRegistry {
    fn authorize(
        &self,
        request: AccessRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = foldfin::error::Result<Option<AccessGrant>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let Some(token) = request.authorization().strip_prefix(b"Bearer ") else {
                return Ok(None);
            };
            if token.is_empty() || token.len() > 512 {
                return Ok(None);
            }
            let digest = TokenDigest(Sha256::digest(token).into());
            let selected = {
                let state = self.0.read().unwrap();
                state
                    .entries
                    .get(&digest)
                    .filter(|entry| {
                        entry.grant.valid()
                            && match request.mode {
                                RequestedMode::Tcp { .. } => entry.grant.proxy,
                                RequestedMode::Ip => entry.grant.vpn.is_some(),
                            }
                    })
                    .map(|entry| entry.grant.clone())
            };
            Ok(selected)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_replacement_reuses_the_vpn_slot_until_old_ownership_is_released() {
        let registry = DeviceRegistry::default();
        let install = |token: &str| {
            registry
                .install(DeviceGrant {
                    device_id: "device".into(),
                    token_digest: TokenDigest::from_token(token),
                    expires_at: Instant::now() + std::time::Duration::from_secs(30),
                    proxy: true,
                    allow_private_proxy: false,
                    vpn: None,
                })
                .unwrap()
        };
        install("old");
        let slot = registry.0.read().unwrap().entries[&TokenDigest::from_token("old")]
            .grant
            .vpn_slots
            .clone();
        let held = slot.clone().try_acquire_owned().unwrap();
        registry.revoke("device");
        install("new");
        let replacement = registry.0.read().unwrap().entries[&TokenDigest::from_token("new")]
            .grant
            .vpn_slots
            .clone();
        assert!(Arc::ptr_eq(&slot, &replacement));
        assert!(replacement.clone().try_acquire_owned().is_err());
        drop(held);
        assert!(replacement.try_acquire_owned().is_ok());
    }
}
