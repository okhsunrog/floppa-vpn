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

    /// Number of registered users.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Synchronize the registry with a new set of users from the database.
    ///
    /// Uses diff-based update: preserves existing limiters (and their unflushed
    /// traffic counters), updates speed limits when changed, adds new users,
    /// and removes users no longer in the set.
    pub fn sync_users(&self, new_users: Vec<([u8; 16], i64, Option<i32>)>) {
        use rustc_hash::FxHashSet;

        let new_uuids: FxHashSet<[u8; 16]> = new_users.iter().map(|(uuid, _, _)| *uuid).collect();

        // Remove users not in the new set
        self.users.retain(|uuid, _| new_uuids.contains(uuid));

        // Insert new users, update existing ones
        for (uuid, user_id, speed_limit_mbps) in new_users {
            let new_speed = mbps_to_bps(speed_limit_mbps);
            self.users
                .entry(uuid)
                .and_modify(|existing| {
                    existing.user_id = user_id;
                    existing.limiter.update_speed(new_speed);
                })
                .or_insert_with(|| UserInfo {
                    user_id,
                    limiter: Limiter::new(new_speed),
                });
        }
    }

    /// Drain traffic counters from all users' limiters.
    ///
    /// Returns `(user_id, bytes_read, bytes_written)` for users with non-zero traffic.
    pub fn flush_traffic(&self) -> Vec<(i64, u64, u64)> {
        let mut deltas = Vec::new();
        for entry in self.users.iter() {
            let (read, written) = entry.value().limiter.swap_counters();
            if read > 0 || written > 0 {
                deltas.push((entry.value().user_id, read, written));
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
