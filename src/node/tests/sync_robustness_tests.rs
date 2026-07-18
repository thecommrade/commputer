//! Tier B (B-4) — sync state-machine robustness tests.
//!
//! Property/edge coverage of `SyncMachine` beyond the happy-path unit tests:
//! batch tiling covers the target range with no gaps/overlaps and clamps the
//! final batch; the target is the median of peer height responses; peers are
//! exhausted after exactly MAX_PEER_FAILURES and skipped by select_peer;
//! verification re-targets when peers advance; start() is guarded mid-sync;
//! reset() clears all transient state.
//!
//! New file, zero runtime behavior change. (Roadmap: src/staging/docs/wirein_roadmap.md B-4.)

use commputer::sync_machine::{SyncMachine, SyncState, MAX_PEER_FAILURES, SYNC_BATCH_SIZE};
use libp2p::PeerId;
use proptest::prelude::*;

/// A fresh machine driven into `Downloading` with `target` as the goal (one
/// peer reported `target`, so the median target is exactly `target`).
fn downloading_to(target: u64) -> SyncMachine {
    let mut m = SyncMachine::new();
    m.start();
    m.record_height(target);
    m.begin_downloading(0);
    m
}

proptest! {
    /// Iterating `next_batch` from `from` to `target` yields contiguous,
    /// non-overlapping batches of size <= SYNC_BATCH_SIZE that exactly cover
    /// (from, target], then `None` with a transition to Verifying.
    #[test]
    fn batches_tile_the_range_exactly(from in 0u64..500, span in 1u64..500) {
        let target = from + span;
        let mut m = downloading_to(target);
        prop_assert_eq!(m.target_height(), target);

        let mut height = from;
        let mut next_start = from + 1;
        let mut iters = 0;
        loop {
            iters += 1;
            prop_assert!(iters < 10_000, "iteration must terminate");
            match m.next_batch(height) {
                Some((start, end)) => {
                    prop_assert_eq!(start, next_start, "no gap/overlap between batches");
                    prop_assert!(end >= start, "batch is non-empty");
                    prop_assert!(end - start + 1 <= SYNC_BATCH_SIZE, "batch <= SYNC_BATCH_SIZE");
                    prop_assert!(end <= target, "batch must not overshoot target");
                    height = end;        // as if we applied the batch
                    next_start = end + 1;
                }
                None => {
                    prop_assert!(height >= target, "None only once caught up");
                    prop_assert_eq!(m.state(), &SyncState::Verifying);
                    break;
                }
            }
        }
        prop_assert_eq!(height, target, "coverage reaches exactly the target");
    }

    /// `begin_downloading` targets the median of the collected height responses.
    #[test]
    fn target_is_median_of_responses(heights in proptest::collection::vec(0u64..1000, 1..15)) {
        let mut m = SyncMachine::new();
        m.start();
        for h in &heights {
            m.record_height(*h);
        }
        let target = m.begin_downloading(0);
        let mut sorted = heights.clone();
        sorted.sort_unstable();
        let expected = sorted[sorted.len() / 2];
        prop_assert_eq!(target, expected);
        prop_assert_eq!(m.target_height(), expected);
    }

    /// A peer is exhausted after EXACTLY MAX_PEER_FAILURES failures, after which
    /// select_peer skips it (and returns None when no peers remain).
    #[test]
    fn peer_exhaustion_and_selection(n_peers in 1usize..6) {
        let mut m = SyncMachine::new();
        let peers: Vec<PeerId> = (0..n_peers).map(|_| PeerId::random()).collect();
        prop_assert_eq!(m.select_peer(&peers), Some(peers[0]));

        let mut exhausted_at = 0u32;
        for i in 1..=MAX_PEER_FAILURES {
            if m.record_batch_failure(peers[0]) {
                exhausted_at = i;
                break;
            }
        }
        prop_assert_eq!(exhausted_at, MAX_PEER_FAILURES, "exhaustion at the threshold");

        if n_peers > 1 {
            prop_assert_eq!(m.select_peer(&peers), Some(peers[1]), "skip the exhausted peer");
        } else {
            prop_assert_eq!(m.select_peer(&peers), None, "no peers left");
        }
    }
}

#[test]
fn complete_verification_retargets_when_peers_advanced() {
    let mut m = downloading_to(100);
    m.next_batch(100); // caught up to 100 → Verifying
    m.record_height(150); // peers now report a higher tip
    assert!(!m.complete_verification(100), "not done — peers advanced");
    assert_eq!(*m.state(), SyncState::Downloading);
    assert_eq!(m.target_height(), 150);
}

#[test]
fn start_is_ignored_while_actively_syncing() {
    let mut m = downloading_to(50);
    assert_eq!(*m.state(), SyncState::Downloading);
    m.start(); // must be a no-op mid-sync
    assert_eq!(*m.state(), SyncState::Downloading);
}

#[test]
fn reset_clears_all_transient_state() {
    // Exhaust the peer BEFORE entering Downloading: record_batch_failure counts in
    // any state, but the stall watchdog (MAX_STALL_BATCH_FAILURES) only arms while
    // Downloading toward a nonzero target — armed, the failure that exhausts a lone
    // peer would ALSO self-reset the machine (pinned by the watchdog test below),
    // which this test must sidestep to observe reset() itself doing the clearing.
    let mut m = SyncMachine::new();
    let peer = PeerId::random();
    for _ in 0..MAX_PEER_FAILURES {
        m.record_batch_failure(peer);
    }
    assert_eq!(m.select_peer(&[peer]), None, "peer exhausted");

    // Exhaustion survives entering a fresh download...
    m.start();
    m.record_height(100);
    m.begin_downloading(0);
    assert_eq!(*m.state(), SyncState::Downloading);
    assert_eq!(m.select_peer(&[peer]), None, "exhaustion persists across begin_downloading");

    // ...and reset() clears all of it.
    m.reset();
    assert_eq!(*m.state(), SyncState::Idle);
    assert_eq!(m.target_height(), 0);
    // Exhausted set cleared → the peer is selectable again.
    assert_eq!(m.select_peer(&[peer]), Some(peer));
}

#[test]
fn stall_watchdog_self_resets_on_the_exhausting_failure() {
    // Liveness watchdog (MAX_STALL_BATCH_FAILURES == MAX_PEER_FAILURES): while
    // Downloading toward a nonzero target with no forward progress, the failure
    // that exhausts a lone peer must ALSO trip the watchdog and reset the machine
    // — once a lone peer is exhausted, select_peer returns None and no further
    // failures are ever recorded, so firing any later would wedge the node in
    // Downloading forever.
    let mut m = downloading_to(100);
    let peer = PeerId::random();
    for _ in 0..MAX_PEER_FAILURES {
        m.record_batch_failure(peer);
    }
    assert_eq!(*m.state(), SyncState::Idle, "watchdog tore the wedged sync down");
    assert_eq!(m.target_height(), 0);
    assert_eq!(m.select_peer(&[peer]), Some(peer), "self-reset cleared the exhaustion");
}
