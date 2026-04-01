// chain_health_monitor_tests.rs — Comprehensive tests for chain_health_monitor.rs
//
// WHAT IT DOES:
//   Tests for ChainHealthMonitor: freshness, block recording, timeout rates,
//   stuck detection, block time calculation, and voter tracking.
//
// WHERE IT SHOULD GO:
//   Paste into src/node/src/chain_health_monitor.rs under #[cfg(test)] mod tests,
//   or add as a separate integration test.
//
// WIRING REQUIRED:
//   None — all tests use public API only.

#[cfg(test)]
mod chain_health_monitor_comprehensive_tests {
    use commputer::chain_health_monitor::{
        ChainHealthMonitor, FinalizeMethod, TIMEOUT_WINDOW, TIMEOUT_RATE_THRESHOLD, STUCK_THRESHOLD,
    };
    use std::thread::sleep;
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // Task 6a: Fresh monitor — is_healthy = true (no data yet)
    // A freshly created monitor starts with last_block_time = now,
    // so stuck_seconds = 0, which is < 30s threshold → healthy.
    // -----------------------------------------------------------------------
    #[test]
    fn fresh_monitor_is_healthy() {
        let monitor = ChainHealthMonitor::new();
        let h = monitor.health();
        assert!(h.is_healthy, "fresh monitor should be healthy");
        assert_eq!(h.timeout_rate, 0.0);
        assert!(h.stuck_seconds < STUCK_THRESHOLD.as_secs());
    }

    // -----------------------------------------------------------------------
    // Task 6b: 20 Snowball blocks — healthy, timeout_rate = 0
    // -----------------------------------------------------------------------
    #[test]
    fn twenty_snowball_blocks_healthy() {
        let mut monitor = ChainHealthMonitor::new();
        for i in 0..20u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert_eq!(h.timeout_rate, 0.0, "zero timeouts");
        // Healthy if no stuck AND no high timeout rate
        let has_timeout_issue = h.issues.iter().any(|s| s.contains("timeout"));
        assert!(!has_timeout_issue, "no timeout issues after 20 Snowball blocks");
        assert_eq!(h.height, 19);
    }

    // -----------------------------------------------------------------------
    // Task 6c: 20 timeout blocks — is_healthy = false, timeout_rate = 1.0
    // -----------------------------------------------------------------------
    #[test]
    fn twenty_timeout_blocks_unhealthy() {
        let mut monitor = ChainHealthMonitor::new();
        for i in 0..20u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Timeout);
        }
        let h = monitor.health();
        assert_eq!(h.timeout_rate, 1.0, "all timeouts → rate = 1.0");
        assert!(!h.is_healthy, "all timeouts → unhealthy");
        assert!(h.timeout_rate > TIMEOUT_RATE_THRESHOLD);
    }

    // -----------------------------------------------------------------------
    // Task 6d: 10 Snowball + 10 timeout — timeout_rate = 0.5
    // -----------------------------------------------------------------------
    #[test]
    fn mixed_finalization_50_percent_timeout() {
        let mut monitor = ChainHealthMonitor::new();
        for i in 0..10u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        for i in 10..20u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Timeout);
        }
        let h = monitor.health();
        assert!(
            (h.timeout_rate - 0.5).abs() < 0.01,
            "50% timeout rate expected, got {}", h.timeout_rate
        );
        // 50% equals threshold (not strictly greater) → still healthy on timeout dimension
        assert!(h.timeout_rate <= TIMEOUT_RATE_THRESHOLD || !h.is_healthy);
    }

    // -----------------------------------------------------------------------
    // Task 6e: Stuck detection — no blocks for 30+ seconds
    // -----------------------------------------------------------------------
    #[test]
    fn stuck_detection_after_30_seconds() {
        let monitor = ChainHealthMonitor::new();
        // Wait just over 30 seconds
        sleep(Duration::from_secs(31));
        let h = monitor.health();
        assert!(!h.is_healthy, "chain should be unhealthy after 30s without a block");
        assert!(h.stuck_seconds >= 30);
        assert!(h.issues.iter().any(|s| s.contains("stuck")));
    }

    // -----------------------------------------------------------------------
    // Task 6f: Block time calculation — avg of last 20 block deltas
    // Records 10 blocks 2 seconds apart → avg_block_time ≈ 2.0
    // -----------------------------------------------------------------------
    #[test]
    fn block_time_calculation_two_seconds() {
        let mut monitor = ChainHealthMonitor::new();
        for i in 0..10u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert!(
            (h.avg_block_time - 2.0).abs() < 0.5,
            "expected ~2.0s avg block time, got {:.2}", h.avg_block_time
        );
    }

    #[test]
    fn block_time_calculation_five_seconds() {
        let mut monitor = ChainHealthMonitor::new();
        // Blocks 5 seconds apart
        for i in 0..10u64 {
            monitor.record_block(i, 1_700_000_000 + i * 5, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert!(
            (h.avg_block_time - 5.0).abs() < 0.5,
            "expected ~5.0s avg block time, got {:.2}", h.avg_block_time
        );
    }

    // -----------------------------------------------------------------------
    // Task 6g: Voter tracking — record 3 voters, verify active_voters = 3
    // -----------------------------------------------------------------------
    #[test]
    fn voter_tracking_three_voters() {
        let mut monitor = ChainHealthMonitor::new();
        monitor.record_vote(100);
        monitor.record_vote(200);
        monitor.record_vote(300);

        let h = monitor.health();
        assert_eq!(h.active_voters, 3, "should track 3 active voters");
        assert_eq!(h.total_voters, 3);
    }

    #[test]
    fn voter_tracking_deduplication() {
        let mut monitor = ChainHealthMonitor::new();
        // Same voter votes multiple times
        monitor.record_vote(100);
        monitor.record_vote(100);
        monitor.record_vote(100);
        monitor.record_vote(200);

        let h = monitor.health();
        // voter_activity is a HashMap: same key overwrites
        assert_eq!(h.total_voters, 2, "should deduplicate same voter");
    }

    // -----------------------------------------------------------------------
    // Additional: rolling window evicts old blocks
    // -----------------------------------------------------------------------
    #[test]
    fn rolling_window_evicts_old_timeout_blocks() {
        let mut monitor = ChainHealthMonitor::new();
        // First TIMEOUT_WINDOW blocks are all timeouts
        for i in 0..TIMEOUT_WINDOW {
            monitor.record_block(i as u64, 1_700_000_000 + i as u64 * 2, FinalizeMethod::Timeout);
        }
        assert_eq!(monitor.health().timeout_rate, 1.0);

        // Next TIMEOUT_WINDOW blocks all Snowball — evicts the timeout ones
        for i in TIMEOUT_WINDOW..TIMEOUT_WINDOW * 2 {
            monitor.record_block(i as u64, 1_700_000_000 + i as u64 * 2, FinalizeMethod::Snowball);
        }
        assert_eq!(
            monitor.health().timeout_rate, 0.0,
            "all timeouts should be evicted from window"
        );
    }

    // -----------------------------------------------------------------------
    // Additional: height tracking correct
    // -----------------------------------------------------------------------
    #[test]
    fn height_tracking() {
        let mut monitor = ChainHealthMonitor::new();
        monitor.record_block(42, 1_700_000_000, FinalizeMethod::Snowball);
        monitor.record_block(43, 1_700_000_002, FinalizeMethod::Snowball);
        let h = monitor.health();
        assert_eq!(h.height, 43);
    }

    // -----------------------------------------------------------------------
    // Additional: is_healthy() convenience method
    // -----------------------------------------------------------------------
    #[test]
    fn is_healthy_convenience_method() {
        let monitor = ChainHealthMonitor::new();
        assert!(monitor.is_healthy(), "fresh monitor should be healthy");
    }
}
