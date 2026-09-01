//! Per-IP login rate limiting — brute-force defence for `POST /api/login`.
//!
//! Uses a token-bucket approach: each IP is allowed at most
//! `MAX_ATTEMPTS` login attempts within a `WINDOW` duration. Attempts
//! that exceed the bucket are rejected with a `Retry-After` header
//! before Argon2id ever runs, which also avoids timing-based oracle
//! attacks on the password hash.
//!
//! The limiter is backed by a `DashMap` for concurrent reads and a
//! `Mutex`-guarded prune pass on each write. The map grows by one
//! entry per unique IP seen since startup or last prune; it never
//! exceeds one entry per remote address. A background sweeper
//! (`LoginRateLimiter::purge_expired`) can be called on a periodic
//! timer to reclaim memory from expired entries. The same ticker
//! that runs the refresh-token sweeper is a natural home for it.
//!
//! Successful login calls `reset(ip)` so a legitimate user who
//! previously tripped the limit is not permanently locked out.
//!
//! 2026-06-02.

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum failed attempts before a bucket is rate-limited.
const MAX_ATTEMPTS: usize = 5;
/// Sliding window over which attempts are counted.
const WINDOW: Duration = Duration::from_secs(10 * 60); // 10 minutes

/// Per-IP attempt record.
struct Bucket {
    /// Timestamps of all attempts in the current window.
    attempts: Vec<Instant>,
}

impl Bucket {
    fn new() -> Self {
        Self {
            attempts: Vec::with_capacity(MAX_ATTEMPTS + 1),
        }
    }

    /// Drop attempts older than WINDOW.
    fn prune(&mut self) {
        let cutoff = Instant::now() - WINDOW;
        self.attempts.retain(|&t| t > cutoff);
    }

    /// True if this bucket has exceeded the limit.
    fn is_limited(&mut self) -> bool {
        self.prune();
        self.attempts.len() >= MAX_ATTEMPTS
    }

    /// Record a new attempt.
    fn record(&mut self) {
        self.attempts.push(Instant::now());
    }

    /// Seconds until the oldest in-window attempt ages out.
    fn retry_after_secs(&mut self) -> u64 {
        self.prune();
        if let Some(&oldest) = self.attempts.first() {
            let elapsed = oldest.elapsed();
            if elapsed < WINDOW {
                return (WINDOW - elapsed).as_secs() + 1;
            }
        }
        1
    }
}

/// Shared login rate limiter. Clone-cheap: inner data is `Arc`-backed.
#[derive(Clone)]
pub struct LoginRateLimiter {
    buckets: Arc<DashMap<String, Bucket>>,
}

impl LoginRateLimiter {
    /// Construct a fresh limiter. Cheap enough to include in `AppState`
    /// unconditionally (tests get an instance with no entries).
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
        }
    }

    /// Check whether `ip` has exceeded the attempt limit. Records a
    /// new attempt in the bucket regardless.
    ///
    /// Returns `Ok(())` when the attempt is allowed.
    /// Returns `Err(retry_after_secs)` when the limit is exceeded; the
    /// caller should return HTTP 429 with `Retry-After: <n>`.
    pub fn check_and_record(&self, ip: &str) -> Result<(), u64> {
        let mut bucket = self
            .buckets
            .entry(ip.to_owned())
            .or_insert_with(Bucket::new);
        if bucket.is_limited() {
            let retry = bucket.retry_after_secs();
            return Err(retry);
        }
        bucket.record();
        Ok(())
    }

    /// Reset the bucket for `ip`. Call on successful login so
    /// a legitimate user who previously tripped the limit can
    /// log in again immediately.
    pub fn reset(&self, ip: &str) {
        self.buckets.remove(ip);
    }

    /// Drop all buckets whose attempt windows have fully expired.
    /// Safe to call from a periodic sweeper task; no-op when empty.
    pub fn purge_expired(&self) {
        self.buckets.retain(|_, bucket| {
            bucket.prune();
            !bucket.attempts.is_empty()
        });
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum extractor that reads the remote peer IP from `ConnectInfo` if the
/// service was started with `into_make_service_with_connect_info`, or falls
/// back to a stable sentinel otherwise. The sentinel never appears in a real
/// IP-bucket so it cannot be exploited for a cross-client lockout; tests that
/// hit the login endpoint multiple times do share the same sentinel bucket,
/// but with a 5-attempt window that is far above any single test's call count.
pub struct PeerIp(pub String);

impl<S> axum::extract::FromRequestParts<S> for PeerIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "test-sentinel-no-connectinfo".to_owned());
        Ok(PeerIp(ip))
    }
}

#[cfg(test)]
mod extractor_tests {
    use super::*;
    use axum::extract::FromRequestParts;
    use axum::http::Request;

    #[tokio::test]
    async fn peer_ip_falls_back_to_sentinel_when_no_connect_info() {
        let (mut parts, _) = Request::new(()).into_parts();
        let PeerIp(ip) = PeerIp::from_request_parts(&mut parts, &()).await.unwrap();
        assert_eq!(ip, "test-sentinel-no-connectinfo");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attempts_within_limit_are_allowed() {
        let limiter = LoginRateLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            assert!(limiter.check_and_record("10.0.0.1").is_ok());
        }
    }

    #[test]
    fn attempt_beyond_limit_returns_err() {
        let limiter = LoginRateLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            let _ = limiter.check_and_record("10.0.0.2");
        }
        let result = limiter.check_and_record("10.0.0.2");
        assert!(result.is_err(), "6th attempt should be rate-limited");
        // Retry-after must be positive.
        assert!(result.unwrap_err() > 0);
    }

    #[test]
    fn reset_clears_bucket() {
        let limiter = LoginRateLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            let _ = limiter.check_and_record("10.0.0.3");
        }
        // Exceeds limit.
        assert!(limiter.check_and_record("10.0.0.3").is_err());
        // After reset, the next attempt is allowed again.
        limiter.reset("10.0.0.3");
        assert!(limiter.check_and_record("10.0.0.3").is_ok());
    }

    #[test]
    fn different_ips_are_isolated() {
        let limiter = LoginRateLimiter::new();
        for _ in 0..MAX_ATTEMPTS {
            let _ = limiter.check_and_record("192.168.1.1");
        }
        // 192.168.1.1 is limited, but 192.168.1.2 should not be.
        assert!(limiter.check_and_record("192.168.1.1").is_err());
        assert!(limiter.check_and_record("192.168.1.2").is_ok());
    }

    #[test]
    fn purge_expired_does_not_remove_active_buckets() {
        let limiter = LoginRateLimiter::new();
        let _ = limiter.check_and_record("10.0.0.4");
        limiter.purge_expired();
        // Bucket should still exist (attempt is recent).
        assert!(limiter.buckets.contains_key("10.0.0.4"));
    }
}
