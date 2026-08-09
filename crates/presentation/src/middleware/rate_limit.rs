//! Per-IP sliding-window rate limiter for HTTP endpoints.
//!
//! # Design
//! `RateLimiter` is a pure decision engine: it holds a `Mutex<HashMap<String,
//! VecDeque<Instant>>>` of recent hit timestamps keyed by client identity.
//! The core `check` method accepts an injected `now: Instant` so the full
//! decision logic is exercisable in unit tests without any clock dependency.
//!
//! The axum middleware `rate_limit_mw` applies the limiter only to paths under
//! `/api/auth/`, returns HTTP 429 on over-limit, and derives the client key
//! from the TCP peer address — or, when `COXAGENT_TRUST_PROXY=1`, from the
//! leftmost `X-Forwarded-For` entry.
//!
//! # Constants
//! | Name | Value | Meaning |
//! |---|---|---|
//! | `AUTH_RATE_MAX` | 20 | Max requests per window |
//! | `AUTH_RATE_WINDOW` | 60 s | Sliding window length |

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Maximum auth requests per [`AUTH_RATE_WINDOW`] from a single client IP.
pub const AUTH_RATE_MAX: usize = 20;
/// Sliding-window length for the auth rate limiter.
pub const AUTH_RATE_WINDOW: Duration = Duration::from_secs(60);

/// Sliding-window rate limiter keyed by arbitrary string identities.
///
/// Thread-safe; clone-share via `Arc<RateLimiter>`.
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// Create a new, empty limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether `key` is within the limit.
    ///
    /// Records a new hit at `now`, evicts hits older than `window`, then
    /// returns `true` (request allowed) when the hit count (including this
    /// one) is ≤ `max`, or `false` (rate-limited) otherwise.
    ///
    /// `now` is injected so callers in tests can pass a fixed instant without
    /// any clock port or async machinery.
    pub fn check(&self, key: &str, max: usize, window: Duration, now: Instant) -> bool {
        let cutoff = now.checked_sub(window).unwrap_or(now);
        let mut guard = self
            .hits
            .lock()
            .expect("RateLimiter mutex should never be poisoned");
        let deque = guard.entry(key.to_owned()).or_default();

        // Evict stale hits that fall outside the current window.
        while deque.front().is_some_and(|t| *t <= cutoff) {
            deque.pop_front();
        }

        deque.push_back(now);
        deque.len() <= max
    }
}

/// Derive the client key for rate-limiting from the request.
///
/// When `COXAGENT_TRUST_PROXY=1` the leftmost `X-Forwarded-For` entry is used
/// (suitable for a Gateway-behind-LB topology).  Otherwise the TCP peer address
/// from [`ConnectInfo`] is used, preventing a self-reported header from being
/// trusted.
fn client_key(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(xff) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            let leftmost = xff.split(',').next().unwrap_or("").trim();
            if !leftmost.is_empty() {
                return leftmost.to_owned();
            }
        }
    }

    // Fall back to the TCP peer address injected by
    // `into_make_service_with_connect_info`.
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Axum middleware that applies `limiter` to all `/api/auth/` requests.
///
/// Returns `429 Too Many Requests` when the per-IP sliding window is exceeded.
/// All other paths pass through unconditionally.
pub async fn rate_limit_mw(
    req: Request,
    next: Next,
    limiter: std::sync::Arc<RateLimiter>,
    max: usize,
    window: Duration,
    trust_proxy: bool,
) -> Result<Response, StatusCode> {
    if req.uri().path().starts_with("/api/auth/") {
        let key = client_key(&req, trust_proxy);
        if !limiter.check(&key, max, window, Instant::now()) {
            tracing::warn!(
                client = %key,
                path = %req.uri().path(),
                "rate limit exceeded — returning 429"
            );
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const MAX: usize = 3;
    const WINDOW: Duration = Duration::from_secs(10);

    #[test]
    fn allows_requests_under_the_limit() {
        let rl = RateLimiter::new();
        let now = Instant::now();
        assert!(rl.check("alice", MAX, WINDOW, now));
        assert!(rl.check("alice", MAX, WINDOW, now));
        assert!(rl.check("alice", MAX, WINDOW, now));
    }

    #[test]
    fn denies_at_the_boundary() {
        let rl = RateLimiter::new();
        let now = Instant::now();
        // Fill up to the limit.
        for _ in 0..MAX {
            assert!(rl.check("bob", MAX, WINDOW, now));
        }
        // The next one must be denied.
        assert!(
            !rl.check("bob", MAX, WINDOW, now),
            "request at MAX+1 should be denied"
        );
    }

    #[test]
    fn window_rolls_over_and_allows_again() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();

        // Fill the window.
        for _ in 0..MAX {
            assert!(rl.check("carol", MAX, WINDOW, t0));
        }
        assert!(!rl.check("carol", MAX, WINDOW, t0), "should be denied before roll-over");

        // Advance past the window — all previous hits expire.
        let t1 = t0 + WINDOW + Duration::from_millis(1);
        assert!(
            rl.check("carol", MAX, WINDOW, t1),
            "should be allowed after window expires"
        );
    }

    #[test]
    fn two_keys_never_interfere() {
        let rl = RateLimiter::new();
        let now = Instant::now();

        // Exhaust the limit for "dave".
        for _ in 0..MAX {
            rl.check("dave", MAX, WINDOW, now);
        }
        assert!(!rl.check("dave", MAX, WINDOW, now), "dave should be denied");

        // "eve" starts fresh — must not be affected by dave's bucket.
        assert!(
            rl.check("eve", MAX, WINDOW, now),
            "eve should be allowed; buckets must be independent"
        );
    }
}
