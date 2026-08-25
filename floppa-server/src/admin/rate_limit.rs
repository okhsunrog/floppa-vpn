//! Fixed-window rate limiting for the unauthenticated auth endpoints, and the client-IP
//! resolution the limiter keys on.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
};

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};

use crate::admin::error::ApiError;

/// Which endpoint a bucket protects. Part of the bucket key, so counters for different
/// endpoints never share a bucket even when the client key (IP, login) is the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitScope {
    /// Account registration, per IP.
    Register,
    /// Credential login, per IP.
    LoginIp,
    /// Credential login, per (normalized) login name — caps a distributed brute force of one
    /// account that rotates source addresses.
    LoginName,
    /// Deep-link login start page (mints a pending-state entry), per IP.
    TelegramStart,
    /// One-time login-code exchange, per IP.
    ExchangeCode,
}

impl RateLimitScope {
    /// Stable label for metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::LoginIp => "login_ip",
            Self::LoginName => "login_name",
            Self::TelegramStart => "telegram_start",
            Self::ExchangeCode => "exchange_code",
        }
    }
}

/// A request was rejected because its bucket is over its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited;

impl From<RateLimited> for ApiError {
    fn from(RateLimited: RateLimited) -> Self {
        ApiError::too_many_requests("Too many attempts, please try again later")
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    count: u32,
    window_start: DateTime<Utc>,
}

/// Fixed-window counters keyed by `(scope, client key)`. Expired buckets are dropped on every
/// check, so the map stays bounded by the number of distinct clients seen within one window.
#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<(RateLimitScope, String), Bucket>>,
}

impl RateLimiter {
    /// Count one attempt for `key` in `scope`; `Err` once more than `max` attempts fall inside
    /// the current window. Fixed window: it starts at the first attempt and ends `window`
    /// later regardless of further traffic, and attempts over `max` are rejected until then.
    pub fn check(
        &self,
        scope: RateLimitScope,
        key: impl Into<String>,
        max: u32,
        window: Duration,
    ) -> Result<(), RateLimited> {
        self.check_at(Utc::now(), scope, key.into(), max, window)
    }

    fn check_at(
        &self,
        now: DateTime<Utc>,
        scope: RateLimitScope,
        key: String,
        max: u32,
        window: Duration,
    ) -> Result<(), RateLimited> {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        buckets.retain(|_, b| now - b.window_start < window);
        let bucket = buckets.entry((scope, key)).or_insert(Bucket {
            count: 0,
            window_start: now,
        });
        bucket.count += 1;
        if bucket.count > max {
            crate::metrics::rate_limited(scope.as_str());
            return Err(RateLimited);
        }
        Ok(())
    }
}

/// The address a request really came from.
///
/// The server sits behind nginx on the same host, which overwrites `X-Real-IP` with the
/// connecting address and *appends* it to `X-Forwarded-For` — so the trustworthy element of
/// that list is the rightmost one, never the leftmost (which the client chooses freely). Those
/// headers are honoured only when the TCP peer is a loopback address, i.e. the request did come
/// through that proxy: anything that reaches the listener directly (a VPN client on the
/// interface, a local run, tests) is keyed by its own peer address, so a forged header cannot
/// pick the bucket. Without headers the peer address is used as well.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if !peer.ip().is_loopback() {
        return peer.ip();
    }
    let header_ip = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    header_ip("x-real-ip")
        .and_then(parse_forwarded_addr)
        .or_else(|| {
            header_ip("x-forwarded-for")
                .and_then(|v| v.rsplit(',').next())
                .and_then(parse_forwarded_addr)
        })
        .unwrap_or_else(|| peer.ip())
}

/// Accepts a bare IP (`1.2.3.4`, `2001:db8::1`) and the `ip:port` / `[ip]:port` forms some
/// proxies write.
fn parse_forwarded_addr(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    raw.parse::<IpAddr>()
        .ok()
        .or_else(|| raw.parse::<SocketAddr>().ok().map(|sa| sa.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// The proxy on the same host.
    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5555);
    /// A client that reached the listener directly.
    const DIRECT_PEER: SocketAddr =
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 9)), 5555);

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn x_real_ip_wins_over_forwarded_for() {
        let h = headers(&[
            ("x-real-ip", "203.0.113.7"),
            ("x-forwarded-for", "198.51.100.1, 203.0.113.8"),
        ]);
        assert_eq!(client_ip(&h, PEER), ip("203.0.113.7"));
    }

    #[test]
    fn forwarded_for_uses_the_rightmost_element() {
        let h = headers(&[("x-forwarded-for", "1.1.1.1, 2.2.2.2 , 203.0.113.8")]);
        assert_eq!(client_ip(&h, PEER), ip("203.0.113.8"));
        let h = headers(&[("x-forwarded-for", "2001:db8::1")]);
        assert_eq!(client_ip(&h, PEER), ip("2001:db8::1"));
        let h = headers(&[("x-forwarded-for", "spoofed, [2001:db8::2]:443")]);
        assert_eq!(client_ip(&h, PEER), ip("2001:db8::2"));
    }

    #[test]
    fn falls_back_to_the_peer_address() {
        assert_eq!(client_ip(&HeaderMap::new(), PEER), ip("127.0.0.1"));
        let h = headers(&[("x-forwarded-for", "not-an-ip"), ("x-real-ip", "")]);
        assert_eq!(client_ip(&h, PEER), ip("127.0.0.1"));
        assert_eq!(client_ip(&HeaderMap::new(), DIRECT_PEER), ip("10.0.0.9"));
    }

    #[test]
    fn proxy_headers_from_a_non_loopback_peer_are_ignored() {
        let h = headers(&[
            ("x-real-ip", "203.0.113.7"),
            ("x-forwarded-for", "198.51.100.1, 203.0.113.8"),
        ]);
        assert_eq!(client_ip(&h, DIRECT_PEER), ip("10.0.0.9"));
        // ...but honoured from the proxy, over v6 loopback too.
        let v6_proxy: SocketAddr = "[::1]:5555".parse().unwrap();
        assert_eq!(client_ip(&h, v6_proxy), ip("203.0.113.7"));
    }

    #[test]
    fn limiter_rejects_after_max_and_recovers_after_window() {
        let limiter = RateLimiter::default();
        let t0 = Utc::now();
        let window = Duration::minutes(15);
        let check = |at: DateTime<Utc>, key: &str| {
            limiter.check_at(at, RateLimitScope::LoginIp, key.into(), 3, window)
        };

        for _ in 0..3 {
            assert_eq!(check(t0, "a"), Ok(()));
        }
        assert_eq!(check(t0, "a"), Err(RateLimited));
        // Another key and another scope are independent buckets.
        assert_eq!(check(t0, "b"), Ok(()));
        assert_eq!(
            limiter.check_at(t0, RateLimitScope::LoginName, "a".into(), 3, window),
            Ok(())
        );
        // Still blocked just before the window ends, free again once it has passed.
        assert_eq!(
            check(t0 + window - Duration::seconds(1), "a"),
            Err(RateLimited)
        );
        assert_eq!(check(t0 + window, "a"), Ok(()));
    }

    #[test]
    fn expired_buckets_are_evicted() {
        let limiter = RateLimiter::default();
        let t0 = Utc::now();
        let window = Duration::minutes(1);
        for i in 0..50 {
            limiter
                .check_at(t0, RateLimitScope::Register, format!("k{i}"), 5, window)
                .unwrap();
        }
        assert_eq!(limiter.buckets.lock().unwrap().len(), 50);
        limiter
            .check_at(
                t0 + window,
                RateLimitScope::Register,
                "late".into(),
                5,
                window,
            )
            .unwrap();
        assert_eq!(limiter.buckets.lock().unwrap().len(), 1);
    }
}
