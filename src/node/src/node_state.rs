use tracing::{info, warn};

/// Threshold (in blocks) above which the node is considered stale.
/// If `network_height - our_height > STALE_THRESHOLD`, transition to Stale -> Syncing.
pub const STALE_THRESHOLD: u64 = 10;

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
/// `network_height` can never decrease; lower values are silently ignored.
pub struct NodeStateMachine {
    state: NodeState,
    our_height: u64,
    network_height: u64,
}

impl NodeStateMachine {
    /// Create a new state machine. Starts in `Syncing` with both heights at 0.
    pub fn new() -> Self {
        Self {
            state: NodeState::Syncing,
            our_height: 0,
            network_height: 0,
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
    pub fn set_network_height(&mut self, height: u64) {
        if height <= self.network_height {
            return;
        }
        self.network_height = height;
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
    fn network_height_only_increases() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(100);
        assert_eq!(sm.network_height(), 100);

        sm.set_network_height(50); // should be ignored
        assert_eq!(sm.network_height(), 100);
    }
}
