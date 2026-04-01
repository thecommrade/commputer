// sync_machine_comprehensive_tests.rs — Comprehensive tests for src/node/src/sync_machine.rs
//
// WHAT IT DOES:
//   Extended test suite covering the full lifecycle, batch calculation, peer
//   exhaustion, re-entry, median computation, and timeout detection.
//
// WHERE IT SHOULD GO:
//   Paste into src/node/src/sync_machine.rs under #[cfg(test)] mod tests, or
//   use as a standalone integration test file.
//
// WIRING REQUIRED:
//   In lib.rs for the node crate: no changes needed; add to Cargo.toml [[test]].

#[cfg(test)]
mod sync_machine_comprehensive_tests {
    use commputer::sync_machine::{
        SyncMachine, SyncState, SYNC_BATCH_SIZE, BATCH_TIMEOUT_SECS, MAX_PEER_FAILURES,
    };
    use libp2p::PeerId;

    // -----------------------------------------------------------------------
    // Task 3a: Full lifecycle — Idle -> QueryHeight -> Downloading -> Verifying -> Complete
    // -----------------------------------------------------------------------
    #[test]
    fn full_lifecycle_idle_to_complete() {
        let mut m = SyncMachine::new();
        assert_eq!(*m.state(), SyncState::Idle);

        // Idle -> QueryHeight
        m.start();
        assert_eq!(*m.state(), SyncState::QueryHeight);

        // Record height responses
        m.record_height(30);
        m.record_height(30);
        m.record_height(30);

        // QueryHeight -> Downloading
        let target = m.begin_downloading(0);
        assert_eq!(target, 30);
        assert_eq!(*m.state(), SyncState::Downloading);

        // Download batches: 1..10, 11..20, 21..30
        let b1 = m.next_batch(0);
        assert_eq!(b1, Some((1, 10)));
        let b2 = m.next_batch(10);
        assert_eq!(b2, Some((11, 20)));
        let b3 = m.next_batch(20);
        assert_eq!(b3, Some((21, 30)));

        // At target -> Verifying
        let none = m.next_batch(30);
        assert_eq!(none, None);
        assert_eq!(*m.state(), SyncState::Verifying);

        // Verifying -> Complete
        m.record_height(30);
        let done = m.complete_verification(30);
        assert!(done);
        assert_eq!(*m.state(), SyncState::Complete);
    }

    // -----------------------------------------------------------------------
    // Task 3b: Batch calculation — (1,10), (11,20), (21,30) for target=100
    // -----------------------------------------------------------------------
    #[test]
    fn batch_calculation_correct() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(100);
        m.begin_downloading(0);

        // SYNC_BATCH_SIZE = 10
        let b1 = m.next_batch(0);
        assert_eq!(b1, Some((1, 10)), "first batch should be (1, 10)");

        let b2 = m.next_batch(10);
        assert_eq!(b2, Some((11, 20)), "second batch should be (11, 20)");

        let b3 = m.next_batch(20);
        assert_eq!(b3, Some((21, 30)), "third batch should be (21, 30)");
    }

    // -----------------------------------------------------------------------
    // Task 3c: Batch at boundary — our_height=95, target=100 -> batch (96, 100)
    // -----------------------------------------------------------------------
    #[test]
    fn batch_at_boundary_clamped_to_target() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(100);
        m.begin_downloading(95); // our_height=95 at start of downloading

        let batch = m.next_batch(95);
        assert_eq!(
            batch,
            Some((96, 100)),
            "batch should be clamped to target, not (96, 105)"
        );
    }

    // -----------------------------------------------------------------------
    // Task 3d: All peers exhausted — 3 peers, 10 failures each
    // -----------------------------------------------------------------------
    #[test]
    fn all_peers_exhausted_select_peer_returns_none() {
        let mut m = SyncMachine::new();
        let peers: Vec<PeerId> = (0..3).map(|_| PeerId::random()).collect();

        // Record MAX_PEER_FAILURES for each peer
        for &peer in &peers {
            for _ in 0..MAX_PEER_FAILURES {
                m.record_batch_failure(peer);
            }
        }

        let result = m.select_peer(&peers);
        assert_eq!(result, None, "all peers exhausted → select_peer should return None");
    }

    #[test]
    fn peer_exhausted_after_max_failures_returns_true() {
        let mut m = SyncMachine::new();
        let peer = PeerId::random();

        for i in 0..MAX_PEER_FAILURES {
            let exhausted = m.record_batch_failure(peer);
            if i < MAX_PEER_FAILURES - 1 {
                assert!(!exhausted, "peer not exhausted at failure {}", i);
            } else {
                assert!(exhausted, "peer should be exhausted at failure {}", i);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Task 3e: Re-entry — Complete -> start() -> QueryHeight works
    // -----------------------------------------------------------------------
    #[test]
    fn reentry_complete_to_query_height() {
        let mut m = SyncMachine::new();

        // First sync cycle
        m.start();
        m.record_height(10);
        m.begin_downloading(0);
        m.next_batch(10); // → Verifying
        m.record_height(10);
        m.complete_verification(10);
        assert_eq!(*m.state(), SyncState::Complete);

        // Start again
        m.start();
        assert_eq!(*m.state(), SyncState::QueryHeight, "should re-enter QueryHeight from Complete");
    }

    // -----------------------------------------------------------------------
    // Task 3f: Median calculation — heights [50, 100, 200] -> target = 100
    // -----------------------------------------------------------------------
    #[test]
    fn median_calculation_three_responses() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(200);
        m.record_height(50);
        m.record_height(100);

        let target = m.begin_downloading(0);
        // Sorted: [50, 100, 200], mid=1 → 100
        assert_eq!(target, 100, "median of [50, 100, 200] should be 100");
    }

    #[test]
    fn median_calculation_even_count() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(100);
        m.record_height(200);

        let target = m.begin_downloading(0);
        // Sorted: [100, 200], mid=1 → 200
        assert_eq!(target, 200, "median of [100, 200] (mid=1) should be 200");
    }

    // -----------------------------------------------------------------------
    // Task 3g: Empty height responses — begin_downloading returns our_height
    // -----------------------------------------------------------------------
    #[test]
    fn empty_heights_begin_downloading_returns_our_height() {
        let mut m = SyncMachine::new();
        m.start();
        // No record_height calls — simulate timeout
        let target = m.begin_downloading(42);
        assert_eq!(target, 42, "empty responses → target should be our_height=42");
        assert_eq!(*m.state(), SyncState::Downloading);
    }

    // -----------------------------------------------------------------------
    // Task 3h: Batch timeout detection (BATCH_TIMEOUT_SECS)
    // Note: We cannot sleep in unit tests. Instead, verify the logic with mocked time.
    // We verify that batch_timed_out returns false immediately after next_batch.
    // -----------------------------------------------------------------------
    #[test]
    fn batch_not_timed_out_immediately() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(20);
        m.begin_downloading(0);
        m.next_batch(0);

        // Immediately after requesting batch: not timed out
        assert!(
            !m.batch_timed_out(),
            "batch should not be timed out immediately after next_batch"
        );
    }

    #[test]
    fn batch_timeout_only_in_downloading_state() {
        let m = SyncMachine::new();
        // In Idle state: batch_timed_out should always return false
        assert!(!m.batch_timed_out(), "should not time out in Idle state");
    }

    // -----------------------------------------------------------------------
    // Additional: select_peer skips exhausted, picks non-exhausted
    // -----------------------------------------------------------------------
    #[test]
    fn select_peer_skips_exhausted() {
        let mut m = SyncMachine::new();
        let bad_peer = PeerId::random();
        let good_peer = PeerId::random();

        // Exhaust bad_peer
        for _ in 0..MAX_PEER_FAILURES {
            m.record_batch_failure(bad_peer);
        }

        let available = vec![bad_peer, good_peer];
        let selected = m.select_peer(&available);
        assert_eq!(selected, Some(good_peer), "should skip exhausted peer");
    }

    // -----------------------------------------------------------------------
    // Additional: reset clears all state
    // -----------------------------------------------------------------------
    #[test]
    fn reset_returns_to_idle() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(100);
        m.begin_downloading(0);
        m.reset();
        assert_eq!(*m.state(), SyncState::Idle);
        assert_eq!(m.target_height(), 0);
    }

    // -----------------------------------------------------------------------
    // Additional: should_start_downloading with responses
    // -----------------------------------------------------------------------
    #[test]
    fn should_start_downloading_true_with_responses() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(50);
        assert!(
            m.should_start_downloading(0),
            "should start downloading after receiving height responses"
        );
    }

    #[test]
    fn verification_continues_when_behind() {
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(100);
        m.begin_downloading(0);
        m.next_batch(100); // caught up → Verifying

        // Peers report they are at 200 now
        m.record_height(200);
        let done = m.complete_verification(100);
        assert!(!done, "should not be complete if network advanced");
        assert_eq!(*m.state(), SyncState::Downloading);
        assert_eq!(m.target_height(), 200);
    }
}
