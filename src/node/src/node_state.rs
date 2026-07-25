use tracing::{info, warn};

/// Threshold (in blocks) above which the node is considered stale.
/// If `network_height - our_height > STALE_THRESHOLD`, transition to Stale -> Syncing.
pub const STALE_THRESHOLD: u64 = 10;

/// Largest single-step gap between `our_height` and a freshly-reported network
/// tip that this state machine will honor. Any value beyond
/// `our_height + SANE_MAX_GAP` is clamped down to that ceiling, so no single
/// message (e.g. a gossip `height = u64::MAX`) can pin the sync target at an
/// unreachable value. Legitimate far-behind nodes still converge because the
/// ceiling ratchets upward as `our_height` advances through successive sync
/// batches.
///
/// RELATIONSHIP TO the event_loop `MAX_SYNC_WINDOW` (the PROTECTED per-advance
/// block clamp = 2000): this MUST be `>=` that window so the two clamps never
/// fight — a value the event_loop already admitted (`tip + MAX_SYNC_WINDOW`)
/// must not be re-shrunk here. Keep them EQUAL (both 2000): because the event
/// loop calls `set_our_height(self.state.blocks.height())` every sync tick,
/// `our_height == tip`, so `our_height + SANE_MAX_GAP == tip + MAX_SYNC_WINDOW`
/// and neither side ever re-clamps the other. (Making SANE_MAX_GAP smaller would
/// undershoot a target the event_loop meant to keep; larger is harmless — the
/// tighter event_loop clamp simply governs.)
pub const SANE_MAX_GAP: u64 = 2000;

/// Maximum number of distinct recent per-peer height reports retained for the
/// self-healing `recompute_network_height`. Fixed and small so a flood of peer
/// identities cannot grow memory; the oldest sample is evicted on overflow.
pub const MAX_PEER_HEIGHT_SAMPLES: usize = 64;

/// The three operational states of a Commputer node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// Node is catching up to the tip of the chain.
    Syncing,
    /// Node is at (or within threshold of) the network tip and participating normally.
    Active,
    /// Node has fallen behind by more than STALE_THRESHOLD blocks.
    /// This state transitions immediately into Syncing.
    Stale,
}

/// State machine that tracks whether this node is syncing, active, or stale.
///
/// Transition rules:
/// - Syncing -> Active:  `our_height >= network_height` AND `network_height > 0`
/// - Active  -> Stale:   `network_height - our_height > STALE_THRESHOLD`
/// - Stale   -> Syncing: immediately (Stale is a transient state)
///
/// `set_network_height` remains monotonic (that method never lowers the target),
/// but the additive self-healing path — `record_peer_height` +
/// `recompute_network_height` — MAY lower `network_height` back toward the real
/// tip. That decay partner is what stops a single stale/poison reading (e.g. an
/// orphan block or gossip `height = u64::MAX` that never applies) from pinning
/// the node out of `Active` forever. It only becomes EFFECTIVE once the PROTECTED
/// event_loop is rewired to (a) stop feeding a monotonic `self.network_height`
/// into `set_network_height` every tick and (b) instead feed authenticated peer
/// samples via `record_peer_height` and call `recompute_network_height` (see the
/// wiring note in this file's changeset).
pub struct NodeStateMachine {
    state: NodeState,
    our_height: u64,
    network_height: u64,
    /// Recent AUTHENTICATED per-peer height reports: `(peer_hash, last_height)`.
    /// Bounded to `MAX_PEER_HEIGHT_SAMPLES` (oldest evicted); folded into
    /// `network_height` by `recompute_network_height`. Populated only from trusted
    /// evidence (validated blocks / `GetHeight` replies), never raw gossip.
    peer_heights: Vec<(u64, u64)>,
}

impl NodeStateMachine {
    /// Create a new state machine. Starts in `Syncing` with both heights at 0.
    pub fn new() -> Self {
        Self {
            state: NodeState::Syncing,
            our_height: 0,
            network_height: 0,
            peer_heights: Vec::new(),
        }
    }

    /// Current state.
    pub fn state(&self) -> NodeState {
        self.state
    }

    /// Our local chain height.
    pub fn our_height(&self) -> u64 {
        self.our_height
    }

    /// Highest chain height observed from the network.
    pub fn network_height(&self) -> u64 {
        self.network_height
    }

    /// Returns `true` if the node is currently `Active`.
    pub fn is_active(&self) -> bool {
        self.state == NodeState::Active
    }

    /// Returns `true` if the node is currently `Syncing`.
    pub fn is_syncing(&self) -> bool {
        self.state == NodeState::Syncing
    }

    /// Update our local block height and re-evaluate the state.
    pub fn set_our_height(&mut self, height: u64) {
        self.our_height = height;
        self.check_transitions();
    }

    /// Update the observed network height. The value can only increase; smaller
    /// values are ignored. Re-evaluates the state after a valid update.
    ///
    /// The incoming value is first CLAMPED to `our_height + SANE_MAX_GAP` so that
    /// even before the decay path is wired, a single poison reading
    /// (`height = u64::MAX`) can raise the target by at most `SANE_MAX_GAP`
    /// instead of pinning it at an unreachable value (task item 3). Clamp-then-
    /// monotonic preserves the existing "this method never lowers the target"
    /// contract for its callers; the DECREASING / self-healing path lives in
    /// `recompute_network_height`, not here.
    pub fn set_network_height(&mut self, height: u64) {
        let clamped = height.min(self.our_height.saturating_add(SANE_MAX_GAP));
        if clamped <= self.network_height {
            return;
        }
        self.network_height = clamped;
        self.check_transitions();
    }

    /// Record an AUTHENTICATED height report from a specific peer, keyed by a
    /// stable per-peer hash. Callers MUST pass only heights backed by trusted
    /// evidence — a block that passed `validate_block_from_peer`, or a
    /// `SyncResponse::Height` reply to our OWN `GetHeight` probe — never an
    /// unsolicited/unvalidated gossip `height` field.
    ///
    /// This does not itself move `network_height`; call `recompute_network_height`
    /// to fold the samples in. The stored height is clamped to
    /// `our_height + SANE_MAX_GAP`, and the sample set is bounded to
    /// `MAX_PEER_HEIGHT_SAMPLES` (oldest evicted), so neither one peer nor a peer
    /// flood can inject an unreachable target or grow memory.
    pub fn record_peer_height(&mut self, peer_hash: u64, height: u64) {
        let clamped = height.min(self.our_height.saturating_add(SANE_MAX_GAP));
        if let Some(entry) = self.peer_heights.iter_mut().find(|(p, _)| *p == peer_hash) {
            entry.1 = clamped;
        } else {
            if self.peer_heights.len() >= MAX_PEER_HEIGHT_SAMPLES {
                self.peer_heights.remove(0);
            }
            self.peer_heights.push((peer_hash, clamped));
        }
    }

    /// Drop a peer's height sample (e.g. on disconnect) so a departed peer's
    /// stale reading cannot keep the target inflated.
    pub fn forget_peer_height(&mut self, peer_hash: u64) {
        self.peer_heights.retain(|(p, _)| *p != peer_hash);
    }

    /// Self-healing recompute of `network_height` from the live per-peer samples.
    /// Unlike `set_network_height`, this MAY LOWER `network_height` — it is the
    /// decay partner that keeps a single stale/poison reading from being
    /// permanent. Once the bad sample ages out (or is out-voted by honest peers)
    /// the target falls back to the true tip and the node can return to `Active`.
    ///
    /// Target = LOWER median of the recent samples: `hs[(len-1)/2]`, so raising
    /// the target requires a STRICT majority of samples above it — an even-split
    /// (or smaller) set of inflated readings cannot move it (the upper median
    /// `hs[len/2]` hands the even-split case to the liars). Never below
    /// `our_height`, and clamped to `our_height + SANE_MAX_GAP`. With no samples
    /// the target collapses to `our_height`. Re-evaluates state afterwards.
    pub fn recompute_network_height(&mut self) {
        let ceiling = self.our_height.saturating_add(SANE_MAX_GAP);
        // Peers BELOW our own height cannot be sync sources — drop them before
        // the median. Without this, one fresh-syncing peer drags the lower
        // median under our height, which reads as "network is behind us" and
        // (live 2026-07-25) escalated a merely-behind node into a destructive
        // resync that truncated the public chain from 1655 to 30. A peer must
        // claim AT LEAST our height to influence the target, which is exactly
        // the direction the existing raise-attack clamps already bound.
        let target = {
            let mut hs: Vec<u64> = self
                .peer_heights
                .iter()
                .map(|(_, h)| *h)
                .filter(|h| *h >= self.our_height)
                .collect();
            if hs.is_empty() {
                self.our_height
            } else {
                hs.sort_unstable();
                let median = hs[(hs.len() - 1) / 2];
                median.max(self.our_height).min(ceiling)
            }
        };
        // Deliberately bypasses the monotonic gate: this is the ONLY path that may
        // lower network_height, which is what un-pins a poisoned target.
        self.network_height = target;
        self.check_transitions();
    }

    /// Force the node into `Active` regardless of heights. Used when the node
    /// appears to be the sole participant on the network (solo-node timeout).
    pub fn force_active(&mut self) {
        if self.state != NodeState::Active {
            info!(
                previous_state = ?self.state,
                "node_state: force-transitioning to Active (solo-node or timeout)"
            );
            self.state = NodeState::Active;
        }
    }

    /// Force the node into `Syncing` and reset our_height to 0.
    /// Used during chain resync after fork detection or consensus stall.
    pub fn force_syncing(&mut self) {
        warn!(
            previous_state = ?self.state,
            "node_state: force-transitioning to Syncing (chain resync)"
        );
        self.state = NodeState::Syncing;
        self.our_height = 0;
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Evaluate whether the current heights require a state change and apply it.
    fn check_transitions(&mut self) {
        match self.state {
            NodeState::Syncing => {
                // Syncing -> Active when we have caught up and the network is non-trivial.
                if self.network_height > 0 && self.our_height >= self.network_height {
                    info!(
                        our_height = self.our_height,
                        network_height = self.network_height,
                        "node_state: Syncing -> Active"
                    );
                    self.state = NodeState::Active;
                }
            }

            NodeState::Active => {
                // Active -> Stale when we have fallen too far behind.
                if self.network_height > self.our_height
                    && self.network_height - self.our_height > STALE_THRESHOLD
                {
                    warn!(
                        our_height = self.our_height,
                        network_height = self.network_height,
                        threshold = STALE_THRESHOLD,
                        "node_state: Active -> Stale (behind by {} blocks)",
                        self.network_height - self.our_height,
                    );
                    self.state = NodeState::Stale;
                    // Stale is transient — immediately move to Syncing.
                    self.stale_to_syncing();
                }
            }

            NodeState::Stale => {
                // Should not remain in Stale after check_transitions; handle defensively.
                self.stale_to_syncing();
            }
        }
    }

    /// Transition from Stale to Syncing (always immediate).
    fn stale_to_syncing(&mut self) {
        warn!(
            our_height = self.our_height,
            network_height = self.network_height,
            "node_state: Stale -> Syncing"
        );
        self.state = NodeState::Syncing;
    }
}

impl Default for NodeStateMachine {
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

    #[test]
    fn starts_syncing() {
        let sm = NodeStateMachine::new();
        assert_eq!(sm.state(), NodeState::Syncing);
    }

    #[test]
    fn syncing_to_active_when_caught_up() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(100);
        sm.set_our_height(100);
        assert_eq!(sm.state(), NodeState::Active);
    }

    #[test]
    fn active_to_stale_to_syncing_when_behind() {
        let mut sm = NodeStateMachine::new();

        // Get to Active at height 10.
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert_eq!(sm.state(), NodeState::Active);

        // Network races ahead by more than STALE_THRESHOLD (10).
        sm.set_network_height(25); // 25 - 10 = 15 > 10
        // Should have gone Active -> Stale -> Syncing.
        assert_eq!(sm.state(), NodeState::Syncing);
    }

    #[test]
    fn active_stays_active_within_threshold() {
        let mut sm = NodeStateMachine::new();

        // Get to Active at height 10.
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert_eq!(sm.state(), NodeState::Active);

        // Network advances by exactly STALE_THRESHOLD (not greater).
        sm.set_network_height(20); // 20 - 10 = 10, not > 10
        assert_eq!(sm.state(), NodeState::Active);

        // Also check a sub-threshold advance.
        sm.set_network_height(18); // ignored (lower than 20)
        assert_eq!(sm.state(), NodeState::Active);
    }

    #[test]
    fn force_active() {
        let mut sm = NodeStateMachine::new();
        assert_eq!(sm.state(), NodeState::Syncing);
        sm.force_active();
        assert_eq!(sm.state(), NodeState::Active);
    }

    #[test]
    fn force_syncing() {
        let mut sm = NodeStateMachine::new();
        // Get to Active.
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert_eq!(sm.state(), NodeState::Active);

        // Force back to Syncing.
        sm.force_syncing();
        assert_eq!(sm.state(), NodeState::Syncing);
        assert_eq!(sm.our_height(), 0);
    }

    #[test]
    fn network_height_only_increases() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(100);
        assert_eq!(sm.network_height(), 100);

        sm.set_network_height(50); // should be ignored
        assert_eq!(sm.network_height(), 100);
    }

    // -- §0 decay-partner tests -------------------------------------------------

    #[test]
    fn set_network_height_clamps_single_poison() {
        let mut sm = NodeStateMachine::new();
        sm.set_our_height(5);
        // A single u64::MAX poison is clamped to our_height + SANE_MAX_GAP,
        // not pinned at an unreachable value.
        sm.set_network_height(u64::MAX);
        assert_eq!(sm.network_height(), 5 + SANE_MAX_GAP);
    }

    #[test]
    fn recompute_lowers_network_height_and_self_heals_to_active() {
        let mut sm = NodeStateMachine::new();
        // Reach Active at height 10.
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert_eq!(sm.state(), NodeState::Active);

        // Poison bumps the target (clamped to 10 + SANE_MAX_GAP) and demotes us.
        sm.set_network_height(u64::MAX);
        assert_eq!(sm.network_height(), 10 + SANE_MAX_GAP);
        assert_eq!(sm.state(), NodeState::Syncing);

        // Honest peers all report ~10. Recompute folds them in and DECREASES the
        // target back to the real tip — the monotonic value is no longer permanent.
        for p in 0..5u64 {
            sm.record_peer_height(p, 10);
        }
        sm.recompute_network_height();
        assert_eq!(sm.network_height(), 10);
        // our_height (10) >= network_height (10) -> back to Active. Self-healed.
        assert_eq!(sm.state(), NodeState::Active);
    }

    #[test]
    fn recompute_median_ignores_minority_poison() {
        let mut sm = NodeStateMachine::new();
        sm.set_our_height(100);
        // Three honest peers at 100, two liars at the clamp ceiling — a minority.
        sm.record_peer_height(1, 100);
        sm.record_peer_height(2, 100);
        sm.record_peer_height(3, 100);
        sm.record_peer_height(4, u64::MAX); // stored clamped to 100 + SANE_MAX_GAP
        sm.record_peer_height(5, u64::MAX); // stored clamped to 100 + SANE_MAX_GAP
        sm.recompute_network_height();
        // LOWER median of [100,100,100, 100+GAP, 100+GAP] = hs[(5-1)/2] = 100.
        // A minority of liars cannot raise the target.
        assert_eq!(sm.network_height(), 100);
    }

    #[test]
    fn lower_median_requires_majority() {
        let mut sm = NodeStateMachine::new();
        sm.set_our_height(100);
        // Exactly half the samples inflated: with the LOWER median, a 50% split
        // cannot raise the target — raising requires a STRICT majority. (The
        // upper median hs[len/2] would hand the even-split case to the liars.)
        sm.record_peer_height(1, 100);
        sm.record_peer_height(2, 100);
        sm.record_peer_height(3, 500);
        sm.record_peer_height(4, 500);
        sm.recompute_network_height();
        assert_eq!(sm.network_height(), 100);
        // A strict majority of samples (3 of 5) may raise it.
        sm.record_peer_height(5, 500);
        sm.recompute_network_height();
        assert_eq!(sm.network_height(), 500);
    }

    #[test]
    fn recompute_with_no_peers_collapses_to_our_height() {
        let mut sm = NodeStateMachine::new();
        sm.set_our_height(42);
        sm.set_network_height(u64::MAX); // clamped to 42 + SANE_MAX_GAP
        sm.recompute_network_height();   // no samples -> target = our_height
        assert_eq!(sm.network_height(), 42);
    }

    #[test]
    fn peer_height_samples_are_bounded() {
        let mut sm = NodeStateMachine::new();
        for p in 0..(MAX_PEER_HEIGHT_SAMPLES as u64 + 50) {
            sm.record_peer_height(p, 1);
        }
        // A flood of distinct peer identities never grows the sample set past the cap.
        assert!(sm.peer_heights.len() <= MAX_PEER_HEIGHT_SAMPLES);
    }
}
