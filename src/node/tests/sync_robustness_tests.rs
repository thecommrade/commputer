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

use commputer::sync_machine::{
    SyncMachine, SyncState, MAX_PEER_FAILURES, MAX_STALL_BATCH_FAILURES, SYNC_BATCH_SIZE,
};
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
            // Rotation still holds: while a HEALTHY alternative exists, an
            // exhausted peer is skipped. This is the assertion that keeps
            // `exhausted_peers` meaningful.
            prop_assert_eq!(m.select_peer(&peers), Some(peers[1]), "skip the exhausted peer");
        } else {
            // QC-024 CONTRACT CHANGE: with every peer exhausted, `select_peer`
            // now FAILS OPEN and returns the peer anyway instead of `None`.
            // Returning `None` made the driver send no request AND record no
            // failure, so the stall watchdog could never trip and the node wedged
            // SILENTLY until a restart — the same class of unrecoverable stall
            // QC-024 exists to remove. Retrying a peer that previously failed can
            // only succeed or fail again, and failing again keeps the watchdog
            // alive; asking nobody guarantees neither. Liveness beats tidiness.
            prop_assert_eq!(
                m.select_peer(&peers),
                Some(peers[0]),
                "fail open: a previously-failed peer beats asking nobody"
            );
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
    // QC-024: `select_peer` now FAILS OPEN, so "returns None" is no longer a valid
    // probe for "this peer is exhausted" — with nothing else available it hands
    // the exhausted peer back on purpose (see peer_exhaustion_and_selection).
    // Probe with a HEALTHY alternative instead: if the exhausted peer is genuinely
    // tracked, selection skips it in favour of `healthy`. That observes the same
    // internal state this test has always been about, and survives the contract
    // change without weakening the assertion.
    let healthy = PeerId::random();
    for _ in 0..MAX_PEER_FAILURES {
        m.record_batch_failure(peer);
    }
    assert_eq!(
        m.select_peer(&[peer, healthy]),
        Some(healthy),
        "peer exhausted → skipped while a healthy alternative exists"
    );

    // Exhaustion survives entering a fresh download...
    m.start();
    m.record_height(100);
    m.begin_downloading(0);
    assert_eq!(*m.state(), SyncState::Downloading);
    assert_eq!(
        m.select_peer(&[peer, healthy]),
        Some(healthy),
        "exhaustion persists across begin_downloading"
    );

    // ...and reset() clears all of it.
    m.reset();
    assert_eq!(*m.state(), SyncState::Idle);
    assert_eq!(m.target_height(), 0);
    // Exhausted set cleared → the previously-exhausted peer is preferred again
    // (it is first in the slice, so selecting it proves it is no longer skipped).
    assert_eq!(m.select_peer(&[peer, healthy]), Some(peer));
}

#[test]
fn stall_watchdog_fires_even_after_the_lone_peer_is_exhausted() {
    // QC-024 REWRITE. This test used to assert that the failure which exhausts a
    // lone peer must ALSO trip the watchdog, and the thresholds were pinned equal
    // to force that. The stated reason was: once a lone peer is exhausted,
    // select_peer returns None, no further failures are ever recorded, so a
    // watchdog that fired later would never fire at all.
    //
    // That constraint is gone. select_peer now FAILS OPEN, so failures keep being
    // recorded past exhaustion, which lets the two thresholds be decoupled
    // (MAX_PEER_FAILURES=3 rotates; MAX_STALL_BATCH_FAILURES=10 resets). The new
    // property is strictly stronger and is what the fix is for: exhaustion no
    // longer silences failure accounting, so the watchdog reliably arrives.
    assert!(
        MAX_PEER_FAILURES < MAX_STALL_BATCH_FAILURES,
        "rotation must come strictly before the whole-machine reset, or a peer is \
         only ever exhausted by the same call that clears the exhausted set"
    );

    let mut m = downloading_to(100);
    let peer = PeerId::random();

    // Exhaust the lone peer. The machine must NOT reset yet — rotation is not a
    // reset, and with only one peer there is nobody to rotate to.
    for _ in 0..MAX_PEER_FAILURES {
        m.record_batch_failure(peer);
    }
    assert_eq!(
        *m.state(),
        SyncState::Downloading,
        "exhausting a peer rotates; it must not tear down the download"
    );
    assert_eq!(
        m.select_peer(&[peer]),
        Some(peer),
        "fail open: the lone exhausted peer is still handed back, so the driver \
         keeps requesting and keeps recording failures"
    );

    // Keep failing. Because failures still accrue, the watchdog is reachable.
    for _ in MAX_PEER_FAILURES..MAX_STALL_BATCH_FAILURES {
        m.record_batch_failure(peer);
    }
    assert_eq!(*m.state(), SyncState::Idle, "watchdog tore the wedged sync down");
    assert_eq!(m.target_height(), 0);
    assert_eq!(m.select_peer(&[peer]), Some(peer), "self-reset cleared the exhaustion");
}
