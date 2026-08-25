//! Multi-user VLESS authenticator backed by DashMap.

use dashmap::DashMap;
use shoes_lite::speed_limit::Limiter;
use shoes_lite::vless::VlessAuthenticator;
use subtle::ConstantTimeEq;

/// Per-user info stored in the registry.
#[derive(Debug)]
pub struct UserInfo {
    pub user_id: i64,
    pub limiter: Limiter,
}

/// One user as loaded from the database for [`MultiUserAuthenticator::sync_users`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryUser {
    pub uuid: [u8; 16],
    pub user_id: i64,
    /// `None` = unlimited.
    pub speed_limit_mbps: Option<i32>,
}

/// Bytes moved for one user since the previous flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTraffic {
    pub user_id: i64,
    pub bytes_read: u64,
    pub bytes_written: u64,
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

    /// Synchronize the registry with a new set of users from the database.
    ///
    /// Uses diff-based update: preserves existing limiters (and their unflushed
    /// traffic counters), updates speed limits when changed, adds new users,
    /// and removes users no longer in the set.
    ///
    /// Returns the unflushed traffic of the users that were removed, so the
    /// caller can still record it — otherwise those bytes vanish with the limiter.
    pub fn sync_users(&self, new_users: Vec<RegistryUser>) -> Vec<UserTraffic> {
        use rustc_hash::FxHashSet;

        let new_uuids: FxHashSet<[u8; 16]> = new_users.iter().map(|u| u.uuid).collect();

        // Remove users not in the new set, draining their counters first
        let mut evicted = Vec::new();
        self.users.retain(|uuid, info| {
            if new_uuids.contains(uuid) {
                return true;
            }
            let (bytes_read, bytes_written) = info.limiter.swap_counters();
            if bytes_read > 0 || bytes_written > 0 {
                evicted.push(UserTraffic {
                    user_id: info.user_id,
                    bytes_read,
                    bytes_written,
                });
            }
            false
        });

        // Insert new users, update existing ones
        for user in new_users {
            let new_speed = mbps_to_bps(user.speed_limit_mbps);
            self.users
                .entry(user.uuid)
                .and_modify(|existing| {
                    existing.user_id = user.user_id;
                    existing.limiter.update_speed(new_speed);
                })
                .or_insert_with(|| UserInfo {
                    user_id: user.user_id,
                    limiter: Limiter::new(new_speed),
                });
        }

        evicted
    }

    /// Drain traffic counters from all users' limiters.
    ///
    /// Returns an entry for every user with non-zero traffic since the last call.
    pub fn flush_traffic(&self) -> Vec<UserTraffic> {
        let mut deltas = Vec::new();
        for entry in self.users.iter() {
            let (bytes_read, bytes_written) = entry.value().limiter.swap_counters();
            if bytes_read > 0 || bytes_written > 0 {
                deltas.push(UserTraffic {
                    user_id: entry.value().user_id,
                    bytes_read,
                    bytes_written,
                });
            }
        }
        deltas
    }
}

/// Convert an optional speed limit in Mbps to bytes per second.
fn mbps_to_bps(mbps: Option<i32>) -> f64 {
    match mbps {
        Some(m) => m as f64 * 125_000.0, // 1 Mbps = 125,000 bytes/sec
        None => f64::INFINITY,
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

    fn get_limiter(&self, uuid: &[u8; 16]) -> Option<Limiter> {
        self.users.get(uuid).map(|entry| entry.limiter.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(n: u8, speed: Option<i32>) -> RegistryUser {
        RegistryUser {
            uuid: [n; 16],
            user_id: n as i64,
            speed_limit_mbps: speed,
        }
    }

    #[test]
    fn resync_drops_missing_users_and_keeps_the_rest() {
        let auth = MultiUserAuthenticator::new();
        assert!(
            auth.sync_users(vec![user(1, Some(10)), user(2, None)])
                .is_empty()
        );
        assert!(auth.authenticate(&[1; 16]));
        assert!(auth.authenticate(&[2; 16]));
        assert!(!auth.authenticate(&[3; 16]));

        // User 2 gone: evicted, its (zero) traffic is not reported, user 1 survives.
        let evicted = auth.sync_users(vec![user(1, Some(20))]);
        assert!(evicted.is_empty());
        assert!(auth.authenticate(&[1; 16]));
        assert!(!auth.authenticate(&[2; 16]));
        assert!(auth.get_limiter(&[1; 16]).is_some());
        assert!(auth.get_limiter(&[2; 16]).is_none());
    }

    #[test]
    fn mbps_conversion() {
        assert_eq!(mbps_to_bps(Some(8)), 1_000_000.0);
        assert_eq!(mbps_to_bps(None), f64::INFINITY);
    }
}
