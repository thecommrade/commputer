// consensus_rate_limiter.rs — Rate limit consensus messages per peer
//
// WHAT IT DOES:
//   Rate limits consensus protocol messages to protect against spam:
//   - Track votes per peer per height (reject duplicates)
//   - Max 10 consensus requests per peer per second
//   - Log but don't ban (could be legitimate retries)
//   - Uses a token bucket per peer for rate limiting
//
// WHERE IT SHOULD GO: src/network/src/consensus_rate_limiter.rs
//
// WIRING REQUIRED:
//   1. Add `pub mod consensus_rate_limiter;` to src/network/src/lib.rs
//   2. Instantiate ConsensusRateLimiter in the consensus protocol handler
//   3. Call limiter.check(peer, height) before processing any consensus message
//   4. Import: use commputer_network::consensus_rate_limiter::ConsensusRateLimiter;

use std::collections::{HashMap, HashSet};
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
        // Refill if window has elapsed
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

/// Tracks which (peer, height) pairs have already voted.
/// Prevents duplicate vote counting from the same peer at the same height.
type PeerVoteKey = (u64, u64); // (peer_id_hash, height)

/// Rate limiter for consensus messages.
///
/// # Usage
/// ```rust
/// let mut limiter = ConsensusRateLimiter::new();
/// if limiter.check(peer_id_hash, height) {
///     // Process the message
/// } else {
///     // Drop the message (logged internally)
/// }
/// ```
pub struct ConsensusRateLimiter {
    /// Token buckets per peer (keyed by peer id hash).
    buckets: HashMap<u64, PeerBucket>,
    /// Set of (peer, height) pairs that have already voted.
    seen_votes: HashSet<PeerVoteKey>,
    /// Maximum number of heights to track in seen_votes before pruning.
    max_tracked_heights: usize,
    /// The highest height we've seen (for pruning old entries).
    max_height: u64,
}

impl ConsensusRateLimiter {
    /// Create a new rate limiter.
    pub fn new() -> Self {
        Self {
            buckets: HashMap::new(),
            seen_votes: HashSet::new(),
            max_tracked_heights: 200,
            max_height: 0,
        }
    }

    /// Check whether a consensus message from `peer` at `height` should be processed.
    ///
    /// Returns `true` if:
    /// - This peer has not exceeded the rate limit, AND
    /// - This peer has not already voted at this height.
    ///
    /// Returns `false` and logs a warning otherwise.
    pub fn check(&mut self, peer_hash: u64, height: u64) -> bool {
        // Update max height for pruning
        if height > self.max_height {
            self.max_height = height;
            self.maybe_prune(height);
        }

        // Check for duplicate vote
        let vote_key = (peer_hash, height);
        if self.seen_votes.contains(&vote_key) {
            warn!(
                peer = peer_hash,
                height = height,
                "consensus_rate_limiter: duplicate vote from peer at height"
            );
            return false;
        }

        // Check rate limit
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

        // Record this vote
        self.seen_votes.insert(vote_key);
        true
    }

    /// Returns statistics for a peer.
    pub fn peer_stats(&self, peer_hash: u64) -> Option<(u64, u64)> {
        self.buckets.get(&peer_hash).map(|b| (b.total_requests, b.rejected_requests))
    }

    /// Returns the number of unique (peer, height) votes tracked.
    pub fn tracked_votes(&self) -> usize {
        self.seen_votes.len()
    }

    /// Prune vote records for heights below (max_height - window).
    fn maybe_prune(&mut self, current_height: u64) {
        if self.seen_votes.len() < self.max_tracked_heights {
            return;
        }
        // Keep only votes from the last 100 heights
        let cutoff = current_height.saturating_sub(100);
        self.seen_votes.retain(|&(_, h)| h > cutoff);
    }

    /// Clear all state (used when the node resets).
    pub fn reset(&mut self) {
        self.buckets.clear();
        self.seen_votes.clear();
        self.max_height = 0;
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
    fn duplicate_vote_rejected() {
        let mut limiter = ConsensusRateLimiter::new();
        // First vote: allowed
        assert!(limiter.check(1, 100));
        // Second vote from same peer at same height: rejected
        assert!(!limiter.check(1, 100), "duplicate vote should be rejected");
    }

    #[test]
    fn different_heights_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
        assert!(limiter.check(1, 101)); // different height, allowed
        assert!(limiter.check(1, 102));
    }

    #[test]
    fn different_peers_same_height_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
        assert!(limiter.check(2, 100)); // different peer, same height
        assert!(limiter.check(3, 100));
    }

    #[test]
    fn rate_limit_exceeded_in_same_window() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 42u64;

        // MAX_REQUESTS_PER_SECOND requests at different heights: all allowed
        for h in 0..MAX_REQUESTS_PER_SECOND as u64 {
            assert!(limiter.check(peer, h), "request {} should be allowed", h);
        }

        // Next request exceeds rate limit (but new height, so not duplicate)
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
    fn tracked_votes_count() {
        let mut limiter = ConsensusRateLimiter::new();

        for h in 0..5 {
            limiter.check(1, h);
            limiter.check(2, h);
        }

        assert_eq!(limiter.tracked_votes(), 10);
    }

    #[test]
    fn reset_clears_state() {
        let mut limiter = ConsensusRateLimiter::new();
        limiter.check(1, 100);
        limiter.check(2, 200);
        limiter.reset();

        // After reset, same peer+height allowed again
        assert!(limiter.check(1, 100), "should be allowed after reset");
        assert_eq!(limiter.tracked_votes(), 1);
    }
}
