// consensus_rate_limiter.rs — Rate limit consensus messages per peer
//
// Token bucket per peer. Max 10 consensus requests per peer per second.
// Log but don't ban (could be legitimate retries).

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::warn;

/// Maximum consensus requests allowed per peer per second.
pub const MAX_REQUESTS_PER_SECOND: u32 = 10;

/// Duration of each rate-limit window.
pub const RATE_WINDOW: Duration = Duration::from_secs(1);

/// Token bucket for a single peer.
struct PeerBucket {
    /// Number of tokens available (refills up to MAX_REQUESTS_PER_SECOND).
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
            tokens: MAX_REQUESTS_PER_SECOND,
            last_refill: Instant::now(),
            total_requests: 0,
            rejected_requests: 0,
        }
    }

    /// Try to consume a token. Returns true if request is allowed.
    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_refill) >= RATE_WINDOW {
            self.tokens = MAX_REQUESTS_PER_SECOND;
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

/// Rate limiter for consensus messages.
///
/// # Usage
/// ```rust,no_run
/// let mut limiter = commputer_network::consensus_rate_limiter::ConsensusRateLimiter::new();
/// if limiter.check(12345u64, 100u64) {
///     // Process the message
/// } else {
///     // Drop the message (logged internally)
/// }
/// ```
pub struct ConsensusRateLimiter {
    /// Token buckets per peer (keyed by peer id hash).
    buckets: HashMap<u64, PeerBucket>,
}

impl ConsensusRateLimiter {
    /// Create a new rate limiter.
    pub fn new() -> Self {
        Self {
            buckets: HashMap::new(),
        }
    }

    /// Check whether a consensus message from `peer` should be processed.
    ///
    /// Returns `true` if the peer has not exceeded the rate limit.
    /// Vote deduplication is handled on the response side
    /// (in ConsensusManager::record_response), not here, because
    /// legitimate retries from the leader should not be blocked.
    pub fn check(&mut self, peer_hash: u64, height: u64) -> bool {
        let bucket = self.buckets.entry(peer_hash).or_insert_with(PeerBucket::new);
        if !bucket.try_consume() {
            warn!(
                peer = peer_hash,
                height = height,
                total_requests = bucket.total_requests,
                rejected = bucket.rejected_requests,
                "consensus_rate_limiter: rate limit exceeded for peer"
            );
            return false;
        }

        true
    }

    /// Returns statistics for a peer.
    pub fn peer_stats(&self, peer_hash: u64) -> Option<(u64, u64)> {
        self.buckets.get(&peer_hash).map(|b| (b.total_requests, b.rejected_requests))
    }

    /// Clear all state (used when the node resets).
    pub fn reset(&mut self) {
        self.buckets.clear();
    }
}

impl Default for ConsensusRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
    }

    #[test]
    fn same_peer_same_height_allowed_within_budget() {
        let mut limiter = ConsensusRateLimiter::new();
        // Retries from same peer at same height are allowed (dedup is on response side)
        assert!(limiter.check(1, 100));
        assert!(limiter.check(1, 100));
    }

    #[test]
    fn different_heights_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
        assert!(limiter.check(1, 101));
        assert!(limiter.check(1, 102));
    }

    #[test]
    fn different_peers_same_height_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
        assert!(limiter.check(2, 100));
        assert!(limiter.check(3, 100));
    }

    #[test]
    fn rate_limit_exceeded_in_same_window() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 42u64;

        for h in 0..MAX_REQUESTS_PER_SECOND as u64 {
            assert!(limiter.check(peer, h), "request {} should be allowed", h);
        }

        let over_limit = limiter.check(peer, MAX_REQUESTS_PER_SECOND as u64 + 100);
        assert!(!over_limit, "should be rate limited after {} requests", MAX_REQUESTS_PER_SECOND);
    }

    #[test]
    fn stats_tracked_correctly() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 99u64;

        for h in 0..5 {
            limiter.check(peer, h);
        }

        let (total, rejected) = limiter.peer_stats(peer).unwrap();
        assert_eq!(total, 5);
        assert_eq!(rejected, 0);
    }

    #[test]
    fn reset_clears_state() {
        let mut limiter = ConsensusRateLimiter::new();
        limiter.check(1, 100);
        limiter.check(2, 200);
        limiter.reset();

        // After reset, peer stats are gone
        assert!(limiter.peer_stats(1).is_none());
        assert!(limiter.peer_stats(2).is_none());
        // Requests still work
        assert!(limiter.check(1, 100));
    }
}
