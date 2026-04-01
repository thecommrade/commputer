// consensus_rate_limiter_tests.rs — Comprehensive tests for consensus_rate_limiter.rs
//
// WHAT IT DOES:
//   Tests for ConsensusRateLimiter covering first-request allowance, rate limits,
//   duplicate vote detection, refresh behavior, and cleanup.
//
// WHERE IT SHOULD GO:
//   Paste into src/network/src/consensus_rate_limiter.rs under #[cfg(test)] mod tests.
//
// WIRING REQUIRED:
//   None — all tests use only the public API.

#[cfg(test)]
mod consensus_rate_limiter_comprehensive_tests {
    use commputer_network::consensus_rate_limiter::{ConsensusRateLimiter, MAX_REQUESTS_PER_SECOND};
    use std::thread::sleep;
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // Task 5a: First request from peer is allowed
    // -----------------------------------------------------------------------
    #[test]
    fn first_request_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100), "first request should be allowed");
    }

    // -----------------------------------------------------------------------
    // Task 5b: 10 requests in 1 second — all allowed (MAX=10)
    // -----------------------------------------------------------------------
    #[test]
    fn ten_requests_all_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 42u64;
        for h in 0..(MAX_REQUESTS_PER_SECOND as u64) {
            assert!(
                limiter.check(peer, h),
                "request {} should be allowed", h
            );
        }
        let (total, rejected) = limiter.peer_stats(peer).unwrap();
        assert_eq!(total, MAX_REQUESTS_PER_SECOND as u64);
        assert_eq!(rejected, 0);
    }

    // -----------------------------------------------------------------------
    // Task 5c: 11th request is rejected (same peer, same second, different heights)
    // -----------------------------------------------------------------------
    #[test]
    fn eleventh_request_rejected() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 99u64;

        // Use heights beyond seen_votes to avoid duplicate rejection
        for h in 1000..(1000 + MAX_REQUESTS_PER_SECOND as u64) {
            assert!(limiter.check(peer, h), "request {} should be allowed", h);
        }
        // 11th at a new height — rate limited
        let result = limiter.check(peer, 9999);
        assert!(!result, "11th request should be rejected by rate limiter");
    }

    // -----------------------------------------------------------------------
    // Task 5d: After 1 second, allowance refreshes
    // -----------------------------------------------------------------------
    #[test]
    fn allowance_refreshes_after_one_second() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 77u64;

        // Exhaust the bucket
        for h in 0..(MAX_REQUESTS_PER_SECOND as u64) {
            limiter.check(peer, h + 2000);
        }
        // Should be rejected now
        assert!(!limiter.check(peer, 9000), "should be rate limited");

        // Wait >1 second for window to reset
        sleep(Duration::from_millis(1100));

        // Bucket should have refilled — new height, so not a duplicate
        assert!(limiter.check(peer, 9001), "should be allowed after window reset");
    }

    // -----------------------------------------------------------------------
    // Task 5e: Duplicate vote at same height — rejected
    // -----------------------------------------------------------------------
    #[test]
    fn duplicate_vote_same_height_rejected() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 500), "first vote at height 500 allowed");
        assert!(!limiter.check(1, 500), "duplicate vote at height 500 rejected");
    }

    // -----------------------------------------------------------------------
    // Task 5f: Different heights from same peer — allowed
    // -----------------------------------------------------------------------
    #[test]
    fn different_heights_from_same_peer_allowed() {
        let mut limiter = ConsensusRateLimiter::new();
        let peer = 55u64;
        // Each different height: allowed (within rate limit)
        for h in 0..(MAX_REQUESTS_PER_SECOND as u64) {
            assert!(
                limiter.check(peer, h + 100),
                "height {} should be allowed for peer {}", h + 100, peer
            );
        }
    }

    // -----------------------------------------------------------------------
    // Task 5g: Cleanup — tracked_votes shrinks after prune
    // -----------------------------------------------------------------------
    #[test]
    fn cleanup_prunes_old_entries() {
        let mut limiter = ConsensusRateLimiter::new();

        // Add 210 entries for many heights with peer 1
        // Each call to check(peer, h) at a new height records (peer, h) in seen_votes
        // We need separate peers to avoid rate limiting on the same peer
        for h in 0u64..210 {
            let peer = h % 20; // 20 peers, each gets ~10 heights before being rate limited
            limiter.check(peer, h);
        }

        // Now trigger pruning with a very high height
        // The pruner keeps only heights > current_height - 100
        // If we push height = 300, cutoff = 200, so heights 0..200 are pruned
        limiter.check(99, 300);

        // After prune, seen_votes should contain far fewer entries than 210
        let tracked = limiter.tracked_votes();
        assert!(
            tracked < 210,
            "pruning should have reduced tracked votes, got {}",
            tracked
        );
    }

    // -----------------------------------------------------------------------
    // Additional: Different peers at same height are independent
    // -----------------------------------------------------------------------
    #[test]
    fn different_peers_same_height_independent() {
        let mut limiter = ConsensusRateLimiter::new();
        for peer in 1..=10u64 {
            assert!(
                limiter.check(peer, 777),
                "peer {} at height 777 should be allowed", peer
            );
        }
        assert_eq!(limiter.tracked_votes(), 10);
    }

    // -----------------------------------------------------------------------
    // Additional: reset clears all state
    // -----------------------------------------------------------------------
    #[test]
    fn reset_allows_previously_seen_votes() {
        let mut limiter = ConsensusRateLimiter::new();
        assert!(limiter.check(1, 100));
        assert!(!limiter.check(1, 100), "duplicate rejected before reset");

        limiter.reset();
        assert!(limiter.check(1, 100), "allowed again after reset");
        assert_eq!(limiter.tracked_votes(), 1);
    }

    // -----------------------------------------------------------------------
    // Additional: peer_stats returns None for unknown peer
    // -----------------------------------------------------------------------
    #[test]
    fn peer_stats_none_for_unknown() {
        let limiter = ConsensusRateLimiter::new();
        assert!(limiter.peer_stats(999).is_none());
    }
}
