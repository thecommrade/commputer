//! Sync state machine for the Commputer L1 blockchain.
//!
//! Manages the block synchronization lifecycle: querying peers for their
//! chain height, downloading blocks in batches, and verifying completion.
//!
//! Where to wire in: `src/node/src/event_loop.rs` — replace or augment the
//! ad-hoc sync logic with a `SyncMachine` field on `EventLoop`. Drive it
//! from the sync tick handler.
//!
//! Existing file that needs changes: `src/node/src/event_loop.rs`

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use libp2p::PeerId;
use tracing::{debug, info, warn};

/// Number of blocks to request per batch.
pub const SYNC_BATCH_SIZE: u64 = 10;

/// Seconds before a batch request is considered timed out.
pub const BATCH_TIMEOUT_SECS: u64 = 10;

/// Seconds before a height query round is considered timed out.
pub const HEIGHT_QUERY_TIMEOUT_SECS: u64 = 5;

/// Number of failures before a peer is considered exhausted.
pub const MAX_PEER_FAILURES: u32 = 10;

/// Security bound: the maximum number of blocks ahead of our own height that a
/// single sync cycle will target. Peer-reported heights are self-attested and
/// unauthenticated; `should_start_downloading` returns true after a single
/// response, so one fast malicious peer replying `u64::MAX` can win the
/// `begin_downloading` median race and pin `target_height` at an unreachable
/// value, wedging the node in `Downloading` forever (the reset path only fires
/// when `target == 0`). Clamping the committed target to
/// `our_height + MAX_SYNC_TARGET_GAP` keeps it reachable: a genuinely
/// far-behind node simply syncs in windows across successive verify cycles,
/// while a bogus height can no longer create a permanent stall. Generous — far
/// larger than any honest single-cycle catch-up gap.
pub const MAX_SYNC_TARGET_GAP: u64 = 100_000;

/// Maximum number of `request_block` calls the out-of-order sync path may issue
/// in a SINGLE burst when it observes a block whose height is ahead of the tip.
/// Bounds the gap-request loop in `apply_synced_block` / `try_apply_finalized`
/// (SECURITY finding [1]): without it, a peer answering with header
/// `height = u64::MAX` drives ~1.8e19 synchronous `request_block` iterations on
/// the single-threaded event loop = permanent freeze + outbound amplification.
/// One burst is bounded to a batch; the loop re-fires on later blocks so a
/// genuinely far-behind node still catches up across successive ticks.
/// INERT until the protected event_loop gap-request hunks reference it.
pub const MAX_SYNC_GAP: u64 = SYNC_BATCH_SIZE;

/// Liveness watchdog bound: the number of consecutive batch failures — while
/// `Downloading` toward a nonzero target and WITHOUT our height advancing — after
/// which the machine tears down the current sync attempt and re-queries fresh
/// peer heights.
///
/// The `MAX_SYNC_TARGET_GAP` clamp keeps a bogus target *reachable* but does not
/// keep it *true*: on a short alpha chain a single malicious peer can still win
/// the median race and pin `target_height` ABOVE the real tip yet below the
/// clamp. The node then downloads the whole real chain and wedges — `next_batch`
/// never returns `None` (our_height < target forever), so `Verifying` (the only
/// path that re-queries heights) is never entered, and the `target == 0` reset is
/// never reached. Every further batch request for a block past the true tip times
/// out. Counting those no-progress failures and resetting once they cross this
/// bound breaks the wedge locally: the reset returns the machine to its initial
/// state so the event loop re-queries heights on the next tick, where an honest
/// majority can correct the target. Any forward progress (a batch that advances
/// our height) resets the counter, so a slow-but-genuine sync never trips it.
///
/// Kept at `MAX_PEER_FAILURES` so the watchdog also fires in the single-peer
/// case: a lone peer is marked exhausted after `MAX_PEER_FAILURES` failures, and
/// once exhausted `select_peer` returns `None` and no further failures are ever
/// recorded — the reset must therefore trigger on that same failure, not later.
pub const MAX_STALL_BATCH_FAILURES: u32 = MAX_PEER_FAILURES;

/// States of the sync lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncState {
    /// Not syncing. Waiting to be started or already up to date.
    Idle,
    /// Querying peers for their current chain height.
    QueryHeight,
    /// Downloading blocks from peers in batches.
    Downloading,
    /// Verifying that we have caught up; re-checking peer heights.
    Verifying,
    /// Sync is complete — we are at the target height.
    Complete,
}

/// State machine that drives the block sync lifecycle.
pub struct SyncMachine {
    /// Current state.
    state: SyncState,
    /// The height we are trying to reach.
    target_height: u64,
    /// Height responses collected from peers during `QueryHeight` / `Verifying`.
    height_responses: Vec<u64>,
    /// When the current state was entered (for timeout checks).
    state_entered_at: Instant,
    /// The block range `(start, end)` currently being downloaded.
    current_batch: Option<(u64, u64)>,
    /// Number of consecutive failures per peer.
    peer_failures: HashMap<PeerId, u32>,
    /// Peers that have been permanently excluded (>= MAX_PEER_FAILURES).
    exhausted_peers: HashSet<PeerId>,
    /// Highest `our_height` observed while downloading toward the current target.
    /// Used by the liveness watchdog to detect a no-progress stall.
    last_progress_height: u64,
    /// Batch failures accumulated since the last forward progress (or since the
    /// current target was committed). Reset to 0 whenever our height advances.
    /// Drives the `MAX_STALL_BATCH_FAILURES` watchdog reset.
    consecutive_stall_failures: u32,
}

impl SyncMachine {
    /// Create a new machine in the `Idle` state.
    pub fn new() -> Self {
        Self {
            state: SyncState::Idle,
            target_height: 0,
            height_responses: Vec::new(),
            state_entered_at: Instant::now(),
            current_batch: None,
            peer_failures: HashMap::new(),
            exhausted_peers: HashSet::new(),
            last_progress_height: 0,
            consecutive_stall_failures: 0,
        }
    }

    /// Return the current state.
    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Return the current target height.
    pub fn target_height(&self) -> u64 {
        self.target_height
    }

    /// Transition from `Idle` or `Complete` to `QueryHeight`.
    ///
    /// Clears any stale height responses and resets the state timer.
    /// Has no effect if the machine is already in another active state.
    pub fn start(&mut self) {
        match self.state {
            SyncState::Idle | SyncState::Complete => {
                info!("[sync] starting — entering QueryHeight");
                self.height_responses.clear();
                self.state = SyncState::QueryHeight;
                self.state_entered_at = Instant::now();
            }
            _ => {
                debug!("[sync] start() called in state {:?}, ignoring", self.state);
            }
        }
    }

    /// Record a height response from a peer during `QueryHeight`.
    pub fn record_height(&mut self, height: u64) {
        debug!("[sync] received height response: {}", height);
        self.height_responses.push(height);
    }

    /// Returns `true` when we should stop collecting heights and move on.
    ///
    /// True when we have at least one response, or the query timeout has elapsed.
    pub fn should_start_downloading(&self, our_height: u64) -> bool {
        let _ = our_height;
        if !self.height_responses.is_empty() {
            return true;
        }
        let elapsed = self.state_entered_at.elapsed().as_secs();
        elapsed >= HEIGHT_QUERY_TIMEOUT_SECS
    }

    /// Compute the median target height from collected responses, set it as the
    /// target, and transition to `Downloading`.
    ///
    /// Returns the computed target height.
    pub fn begin_downloading(&mut self, our_height: u64) -> u64 {
        let raw_target = if self.height_responses.is_empty() {
            our_height
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

        // Security (see MAX_SYNC_TARGET_GAP): clamp the peer-derived target to a
        // reachable window above our own height so a single peer reporting a
        // bogus height (e.g. u64::MAX) cannot pin us in Downloading forever.
        let target = raw_target.min(our_height.saturating_add(MAX_SYNC_TARGET_GAP));

        info!(
            "[sync] begin_downloading: our_height={} target={} (raw peer target={})",
            our_height, target, raw_target
        );

        self.target_height = target;
        self.height_responses.clear();
        self.state = SyncState::Downloading;
        self.state_entered_at = Instant::now();
        self.current_batch = None;
        // Fresh download attempt: reset the no-progress watchdog baseline.
        self.last_progress_height = our_height;
        self.consecutive_stall_failures = 0;

        target
    }

    /// Return the next batch `(start, end)` to request, or `None` if caught up.
    ///
    /// When caught up (`our_height >= target_height`), transitions to `Verifying`
    /// and clears height responses for re-collection.
    pub fn next_batch(&mut self, our_height: u64) -> Option<(u64, u64)> {
        if our_height >= self.target_height {
            info!(
                "[sync] reached target {} — entering Verifying",
                self.target_height
            );
            self.height_responses.clear();
            self.state = SyncState::Verifying;
            self.state_entered_at = Instant::now();
            self.current_batch = None;
            return None;
        }

        // Any forward progress toward the target clears the no-progress watchdog:
        // a genuine (if slow) sync that keeps acquiring blocks never trips it.
        if our_height > self.last_progress_height {
            self.last_progress_height = our_height;
            self.consecutive_stall_failures = 0;
        }

        let start = our_height + 1;
        let end = (start + SYNC_BATCH_SIZE - 1).min(self.target_height);
        debug!("[sync] next_batch: ({}, {})", start, end);
        self.current_batch = Some((start, end));
        self.state_entered_at = Instant::now();
        Some((start, end))
    }

    /// Returns `true` if the current batch has been in-flight longer than
    /// `BATCH_TIMEOUT_SECS`.
    pub fn batch_timed_out(&self) -> bool {
        if self.state != SyncState::Downloading {
            return false;
        }
        self.state_entered_at.elapsed().as_secs() >= BATCH_TIMEOUT_SECS
    }

    /// Record a batch failure for `peer`.
    ///
    /// Returns `true` if the peer has now reached `MAX_PEER_FAILURES` and has
    /// been added to the exhausted set.
    ///
    /// Also drives the liveness watchdog (see `MAX_STALL_BATCH_FAILURES`): while
    /// `Downloading` toward a nonzero target, batch failures that occur without
    /// any forward progress accumulate, and once they cross the bound the machine
    /// resets itself back to the initial state so the event loop re-queries fresh
    /// peer heights on the next tick. This is the local liveness fix for the
    /// "bogus target above the true tip but below the clamp" wedge — the reset is
    /// performed internally so it flows through this existing call site with no
    /// change to the (protected) caller and no signature change.
    pub fn record_batch_failure(&mut self, peer: PeerId) -> bool {
        let count = self.peer_failures.entry(peer).or_insert(0);
        *count += 1;
        let newly_exhausted = *count >= MAX_PEER_FAILURES;
        if newly_exhausted {
            warn!(
                "[sync] peer {} exhausted after {} failures",
                peer, count
            );
            self.exhausted_peers.insert(peer);
        }

        // Liveness watchdog: count no-progress batch failures and break the wedge
        // once they cross the bound. Guarded to the Downloading-with-target phase
        // so it never fires spuriously outside an active download.
        if self.state == SyncState::Downloading && self.target_height > 0 {
            self.consecutive_stall_failures = self.consecutive_stall_failures.saturating_add(1);
            if self.consecutive_stall_failures >= MAX_STALL_BATCH_FAILURES {
                warn!(
                    "[sync] no download progress after {} batch failures (stuck at height {}, target {}) — resetting to re-query peer heights",
                    self.consecutive_stall_failures, self.last_progress_height, self.target_height
                );
                self.reset();
            }
        }

        newly_exhausted
    }

    /// Returns `true` when verification data is ready: at least one height
    /// response has been collected, or the verification timeout has elapsed.
    pub fn verification_ready(&self) -> bool {
        if !self.height_responses.is_empty() {
            return true;
        }
        self.state_entered_at.elapsed().as_secs() >= HEIGHT_QUERY_TIMEOUT_SECS
    }

    /// Finalize the verification step.
    ///
    /// If `our_height` has caught up to the median of collected responses (or
    /// responses are empty), transitions to `Complete` and returns `true`.
    ///
    /// Otherwise, updates `target_height` to the new median and transitions back
    /// to `Downloading`, returning `false`.
    pub fn complete_verification(&mut self, our_height: u64) -> bool {
        let raw_target = if self.height_responses.is_empty() {
            our_height
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

        // Security (see MAX_SYNC_TARGET_GAP): clamp the peer-derived target so a
        // bogus verification-round height cannot re-poison the target and wedge
        // us back into an unreachable Downloading loop.
        let new_target = raw_target.min(our_height.saturating_add(MAX_SYNC_TARGET_GAP));

        self.height_responses.clear();

        if our_height >= new_target {
            info!("[sync] verification complete at height {}", our_height);
            self.state = SyncState::Complete;
            self.state_entered_at = Instant::now();
            true
        } else {
            info!(
                "[sync] not yet caught up (our={} target={}) — back to Downloading",
                our_height, new_target
            );
            self.target_height = new_target;
            self.state = SyncState::Downloading;
            self.state_entered_at = Instant::now();
            self.current_batch = None;
            // New download window: reset the no-progress watchdog baseline.
            self.last_progress_height = our_height;
            self.consecutive_stall_failures = 0;
            false
        }
    }

    /// Pick a peer from `available` that has not been exhausted.
    ///
    /// Returns `None` if all available peers are exhausted.
    pub fn select_peer(&self, available: &[PeerId]) -> Option<PeerId> {
        for &peer in available {
            if !self.exhausted_peers.contains(&peer) {
                return Some(peer);
            }
        }
        None
    }

    /// Reset the machine back to `Idle`, clearing all transient state.
    pub fn reset(&mut self) {
        info!("[sync] reset to Idle");
        self.state = SyncState::Idle;
        self.target_height = 0;
        self.height_responses.clear();
        self.state_entered_at = Instant::now();
        self.current_batch = None;
        self.peer_failures.clear();
        self.exhausted_peers.clear();
        self.last_progress_height = 0;
        self.consecutive_stall_failures = 0;
    }
}

impl Default for SyncMachine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: create a deterministic PeerId for tests.
    fn make_peer() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn starts_idle() {
        let machine = SyncMachine::new();
        assert_eq!(*machine.state(), SyncState::Idle);
    }

    #[test]
    fn start_transitions_to_query_height() {
        let mut machine = SyncMachine::new();
        machine.start();
        assert_eq!(*machine.state(), SyncState::QueryHeight);
    }

    #[test]
    fn downloading_produces_batches() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(100);
        let target = machine.begin_downloading(0);
        assert_eq!(target, 100);
        assert_eq!(*machine.state(), SyncState::Downloading);

        // First batch: blocks 1–10
        let batch1 = machine.next_batch(0);
        assert_eq!(batch1, Some((1, 10)));

        // Second batch: blocks 11–20
        let batch2 = machine.next_batch(10);
        assert_eq!(batch2, Some((11, 20)));
    }

    #[test]
    fn downloading_transitions_to_verifying_when_caught_up() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);

        // Consume all batches.
        machine.next_batch(0);  // (1, 10)
        // Now at target.
        let result = machine.next_batch(10);
        assert_eq!(result, None);
        assert_eq!(*machine.state(), SyncState::Verifying);
    }

    #[test]
    fn verification_completes_when_caught_up() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);
        // Drive to Verifying.
        machine.next_batch(10); // already at target → Verifying

        // Collect a height response and verify.
        machine.record_height(10);
        let done = machine.complete_verification(10);
        assert!(done);
        assert_eq!(*machine.state(), SyncState::Complete);
    }

    #[test]
    fn verification_continues_if_behind() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(100);
        machine.begin_downloading(0);
        // Simulate having downloaded to 100 and entering Verifying.
        machine.next_batch(100); // → Verifying

        // Peers now report height 200.
        machine.record_height(200);
        let done = machine.complete_verification(100);
        assert!(!done);
        assert_eq!(*machine.state(), SyncState::Downloading);
        assert_eq!(machine.target_height(), 200);
    }

    // ---- Security: target-height clamp (numeric bound ONLY) ----
    //
    // These two tests assert ONLY that the peer-derived target is numerically
    // clamped to `our_height + MAX_SYNC_TARGET_GAP`. They deliberately say
    // NOTHING about whether the node can escape a wedge: the clamp keeps a bogus
    // target reachable but not necessarily true, so on a short chain a target
    // above the real tip yet below the clamp still stalls the download. Liveness
    // out of that stall is covered separately by
    // `watchdog_resets_wedged_downloading_after_stall` below — do not read these
    // clamp tests as evidence the stall is resolved.

    #[test]
    fn begin_downloading_clamps_bogus_peer_height_to_numeric_bound() {
        let mut machine = SyncMachine::new();
        machine.start();
        // A single malicious peer reports u64::MAX (wins the median as sole
        // responder).
        machine.record_height(u64::MAX);
        let target = machine.begin_downloading(0);
        // Target must be clamped to a reachable window, never u64::MAX. (Numeric
        // bound only — says nothing about escaping a below-clamp bogus target.)
        assert_eq!(target, MAX_SYNC_TARGET_GAP);
        assert_eq!(machine.target_height(), MAX_SYNC_TARGET_GAP);
        assert!(machine.target_height() < u64::MAX);
    }

    #[test]
    fn complete_verification_clamps_bogus_peer_height_to_numeric_bound() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);
        machine.next_batch(10); // → Verifying
        // Malicious peer now reports u64::MAX during verification.
        machine.record_height(u64::MAX);
        let done = machine.complete_verification(10);
        // our_height (10) < clamped target (10 + GAP), so not done, but the
        // target is clamped and reachable rather than pinned at u64::MAX.
        // (Numeric bound only — not a liveness claim.)
        assert!(!done);
        assert_eq!(machine.target_height(), 10 + MAX_SYNC_TARGET_GAP);
        assert!(machine.target_height() < u64::MAX);
    }

    // ---- Security: liveness watchdog (escape the below-clamp wedge) ----

    /// NON-VACUOUS: drives the machine into the real wedge — a bogus target ABOVE
    /// the true tip but BELOW the clamp — then downloads the whole real chain and
    /// hammers batch failures with NO further progress, exactly as the event loop
    /// does on repeated batch timeouts. The watchdog must tear the attempt down
    /// (target back to 0, state back to the initial `Idle`) so the next tick
    /// re-queries fresh heights. Against the pre-watchdog code `record_batch_failure`
    /// only ever increments a per-peer counter and `next_batch` keeps returning
    /// `Some` forever (our_height < target), so `target_height()` stays at the
    /// bogus value and the state never leaves `Downloading` — this test fails.
    #[test]
    fn watchdog_resets_wedged_downloading_after_stall() {
        let mut machine = SyncMachine::new();
        machine.start();
        // Bogus peer target: real tip is 500, but a malicious peer reports 5_000
        // (above the tip, comfortably below MAX_SYNC_TARGET_GAP so the clamp does
        // NOT save us).
        let true_tip = 500u64;
        let bogus_target = 5_000u64;
        assert!(bogus_target < MAX_SYNC_TARGET_GAP);
        machine.record_height(bogus_target);
        let target = machine.begin_downloading(0);
        assert_eq!(target, bogus_target, "clamp leaves a below-clamp target intact");
        assert_eq!(*machine.state(), SyncState::Downloading);

        // Download the whole REAL chain up to the true tip — genuine progress each
        // batch (mirrors event_loop calling next_batch as our_height advances).
        let mut our_height = 0u64;
        while our_height < true_tip {
            let batch = machine.next_batch(our_height);
            assert!(batch.is_some(), "still below bogus target → always Some");
            our_height = batch.unwrap().1;
        }
        our_height = true_tip;

        // Now wedged at the true tip: next_batch never returns None (500 < 5000),
        // so Verifying / the target==0 reset are never reached.
        assert!(machine.next_batch(our_height).is_some());
        assert_eq!(*machine.state(), SyncState::Downloading);

        // Every batch for a block past the true tip times out. Feed no-progress
        // failures through the EXISTING call site until the watchdog fires.
        let peer = make_peer();
        let mut reset = false;
        for _ in 0..(MAX_STALL_BATCH_FAILURES as usize + 8) {
            // event loop requests the next batch each tick; our_height is stuck.
            let _ = machine.next_batch(our_height);
            machine.record_batch_failure(peer);
            if *machine.state() == SyncState::Idle {
                reset = true;
                break;
            }
        }

        assert!(reset, "watchdog must break the wedge, not stall forever");
        assert_eq!(machine.target_height(), 0, "reset clears the bogus target");
        assert_eq!(*machine.state(), SyncState::Idle, "back to initial state to re-query");
    }

    /// The watchdog must NOT punish a slow-but-genuine sync: as long as batches
    /// keep advancing our height, the no-progress counter resets and the machine
    /// stays in `Downloading` no matter how many total (interleaved) failures
    /// occur. Guards against an over-eager reset breaking legitimate catch-up.
    #[test]
    fn watchdog_tolerates_failures_interleaved_with_progress() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(1_000);
        machine.begin_downloading(0);
        let peer = make_peer();

        let mut our_height = 0u64;
        // Many more failures than the threshold, but a real block arrives between
        // each one, so the stall counter never accumulates.
        for _ in 0..(MAX_STALL_BATCH_FAILURES as usize * 3) {
            machine.record_batch_failure(peer);
            let batch = machine.next_batch(our_height).expect("below target");
            our_height = batch.1; // progress
        }
        assert_eq!(
            *machine.state(),
            SyncState::Downloading,
            "progress between failures must keep the sync alive"
        );
        assert_eq!(machine.target_height(), 1_000);
    }

    #[test]
    fn legit_target_within_window_unchanged() {
        // A realistic peer height within the window is used verbatim — the clamp
        // never rejects legitimate catch-up targets.
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(5_000);
        let target = machine.begin_downloading(0);
        assert_eq!(target, 5_000);
    }
}
