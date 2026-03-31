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
        let target = if self.height_responses.is_empty() {
            our_height
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

        info!(
            "[sync] begin_downloading: our_height={} target={}",
            our_height, target
        );

        self.target_height = target;
        self.height_responses.clear();
        self.state = SyncState::Downloading;
        self.state_entered_at = Instant::now();
        self.current_batch = None;

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
    pub fn record_batch_failure(&mut self, peer: PeerId) -> bool {
        let count = self.peer_failures.entry(peer).or_insert(0);
        *count += 1;
        if *count >= MAX_PEER_FAILURES {
            warn!(
                "[sync] peer {} exhausted after {} failures",
                peer, count
            );
            self.exhausted_peers.insert(peer);
            return true;
        }
        false
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
        let new_target = if self.height_responses.is_empty() {
            our_height
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

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
}
