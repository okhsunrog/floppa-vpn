//! Multi-user VLESS authenticator backed by DashMap.

use dashmap::DashMap;
use shoes_lite::vless::VlessAuthenticator;
use subtle::ConstantTimeEq;

/// Per-user info stored in the registry.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub peer_id: i64,
    pub user_id: i64,
    pub speed_limit_mbps: Option<i32>,
}

/// Multi-user authenticator that checks UUIDs against a DashMap registry.
///
/// Authentication uses constant-time comparison for each entry to prevent
/// timing attacks from revealing which UUIDs are valid.
#[derive(Debug)]
pub struct MultiUserAuthenticator {
    /// Map from 16-byte VLESS UUID to user info.
    users: DashMap<[u8; 16], UserInfo>,
}

impl MultiUserAuthenticator {
    pub fn new() -> Self {
        Self {
            users: DashMap::new(),
        }
    }

    /// Insert or update a user.
    pub fn insert(&self, uuid: [u8; 16], info: UserInfo) {
        self.users.insert(uuid, info);
    }

    /// Remove a user by UUID.
    pub fn remove(&self, uuid: &[u8; 16]) {
        self.users.remove(uuid);
    }

    /// Look up user info by UUID (non-constant-time, for use after auth).
    pub fn get_user_info(&self, uuid: &[u8; 16]) -> Option<UserInfo> {
        self.users.get(uuid).map(|entry| entry.value().clone())
    }

    /// Number of registered users.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Clear all users (for full re-sync).
    pub fn clear(&self) {
        self.users.clear();
    }
}

impl VlessAuthenticator for MultiUserAuthenticator {
    fn authenticate(&self, uuid: &[u8; 16]) -> bool {
        // Iterate all entries with constant-time comparison to prevent timing attacks.
        // For typical floppa user counts (dozens to low hundreds), this is fine.
        let mut found = 0u8;
        for entry in self.users.iter() {
            found |= entry.key().ct_eq(uuid).unwrap_u8();
        }
        found == 1
    }
}
