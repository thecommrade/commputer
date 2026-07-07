// sync_rate_limiter.rs — Rate limit sync-protocol requests per peer.
//
// Token bucket per peer, mirroring `consensus_rate_limiter.rs`. The sync
// protocol serves up to 100 full blocks per GetBlocks request, so an unrated
// peer can amplify CPU/bandwidth by hammering GetBlocks. Cap it at
// MAX_SYNC_REQUESTS_PER_SECOND per peer. Log but don't ban (legitimate catch-up
// nodes issue bursts of GetBlocks during initial sync).
//
// WIRING (INERT): this is a standalone, unit-tested component. Wiring it into
// the `event_loop` GetBlocks handler is a one-line change reserved for the
// founder-gated protected enforcement batch; nothing here is threaded into the
// node yet.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::warn;

/// Maximum sync requests allowed per peer per second.
pub const MAX_SYNC_REQUESTS_PER_SECOND: u32 = 10;

/// Duration of each rate-limit window.
pub const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Token bucket for a single peer.
struct PeerBucket {
    /// Number of tokens available (refills up to MAX_SYNC_REQUESTS_PER_SECOND).
    tokens: u32,
    /// When the bucket was last refilled.
    last_refill: Instant,
    /// Total requests seen from this peer (for logging).
    total_requests: u64,
    /// Total requests rejected from this peer.
    rejected_requests: u64,
}

impl PeerBucket {
    fn new() -> Self {
        Self {
            tokens: MAX_SYNC_REQUESTS_PER_SECOND,
            last_refill: Instant::now(),
            total_requests: 0,
            rejected_requests: 0,
        }
    }

    /// Try to consume a token. Returns true if the request is allowed.
    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_refill) >= RATE_WINDOW {
            self.tokens = MAX_SYNC_REQUESTS_PER_SECOND;
            self.last_refill = now;
        }

        self.total_requests += 1;

        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            self.rejected_requests += 1;
            false
        }
    }
}

/// Rate limiter for sync-protocol (GetBlocks) requests.
///
/// # Usage
/// ```rust,no_run
/// let mut limiter = commputer_network::sync_rate_limiter::SyncRateLimiter::new();
/// if limiter.check(12345u64) {
///     // Serve the GetBlocks request
/// } else {
///     // Drop the request (logged internally)
/// }
/// ```
pub struct SyncRateLimiter {
    /// Token buckets per peer (keyed by peer id hash).
    buckets: HashMap<u64, PeerBucket>,
}

impl SyncRateLimiter {
    /// Create a new rate limiter.
    pub fn new() -> Self {
        Self {
            buckets: HashMap::new(),
        }
    }

    /// Check whether a sync request from `peer_hash` should be served.
    ///
    /// Returns `true` if the peer has not exceeded the rate limit.
    pub fn check(&mut self, peer_hash: u64) -> bool {
        let bucket = self.buckets.entry(peer_hash).or_insert_with(PeerBucket::new);
        if !bucket.try_consume() {
            warn!(
                peer = peer_hash,
                total_requests = bucket.total_requests,
                rejected = bucket.rejected_requests,
                "sync_rate_limiter: rate limit exceeded for peer"
            );
            return false;
        }

        true
    }

    /// Returns `(total_requests, rejected_requests)` for a peer.
    pub fn peer_stats(&self, peer_hash: u64) -> Option<(u64, u64)> {
        self.buckets.get(&peer_hash).map(|b| (b.total_requests, b.rejected_requests))
    }

    /// Clear all state (used when the node resets).
    pub fn reset(&mut self) {
        self.buckets.clear();
    }
}

impl Default for SyncRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_allowed() {
        let mut limiter = SyncRateLimiter::new();
        assert!(limiter.check(1));
    }

    #[test]
    fn different_peers_allowed() {
        let mut limiter = SyncRateLimiter::new();
        assert!(limiter.check(1));
        assert!(limiter.check(2));
        assert!(limiter.check(3));
    }

    #[test]
    fn rate_limit_exceeded_in_same_window() {
        let mut limiter = SyncRateLimiter::new();
        let peer = 42u64;

        for i in 0..MAX_SYNC_REQUESTS_PER_SECOND {
            assert!(limiter.check(peer), "request {} should be allowed", i);
        }

        assert!(
            !limiter.check(peer),
            "should be rate limited after {} requests",
            MAX_SYNC_REQUESTS_PER_SECOND
        );
    }

    #[test]
    fn one_peer_flood_does_not_block_others() {
        let mut limiter = SyncRateLimiter::new();
        let flooder = 7u64;
        // Exhaust the flooder's budget.
        for _ in 0..MAX_SYNC_REQUESTS_PER_SECOND {
            limiter.check(flooder);
        }
        assert!(!limiter.check(flooder), "flooder should be limited");
        // A different peer is unaffected.
        assert!(limiter.check(999), "other peers must not be starved");
    }

    #[test]
    fn stats_tracked_correctly() {
        let mut limiter = SyncRateLimiter::new();
        let peer = 99u64;
        for _ in 0..5 {
            limiter.check(peer);
        }
        let (total, rejected) = limiter.peer_stats(peer).unwrap();
        assert_eq!(total, 5);
        assert_eq!(rejected, 0);
    }

    #[test]
    fn reset_clears_state() {
        let mut limiter = SyncRateLimiter::new();
        limiter.check(1);
        limiter.check(2);
        limiter.reset();
        assert!(limiter.peer_stats(1).is_none());
        assert!(limiter.peer_stats(2).is_none());
        assert!(limiter.check(1));
    }
}
