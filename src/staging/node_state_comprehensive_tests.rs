// node_state_comprehensive_tests.rs — Comprehensive tests for src/node/src/node_state.rs
//
// WHAT IT DOES:
//   Extended tests for NodeStateMachine covering cycling, boundary conditions,
//   monotonic height enforcement, idempotency, and concurrent-ish patterns.
//
// WHERE IT SHOULD GO:
//   Add into src/node/src/node_state.rs under #[cfg(test)] mod tests,
//   or compile as a standalone integration test.
//
// WIRING REQUIRED:
//   In Cargo.toml for the node crate, add an [[test]] entry pointing here, OR
//   paste the inner functions into the existing tests module in node_state.rs.

#[cfg(test)]
mod node_state_comprehensive_tests {
    use commputer::node_state::{NodeStateMachine, NodeState, STALE_THRESHOLD};

    // -----------------------------------------------------------------------
    // Task 2a: Rapid state cycling Syncing->Active->Stale->Syncing 100 times
    // -----------------------------------------------------------------------
    #[test]
    fn rapid_state_cycling_100_times() {
        let mut sm = NodeStateMachine::new();
        for i in 0u64..100 {
            // Force into Active
            sm.set_network_height(i * 100 + 100);
            sm.set_our_height(i * 100 + 100);
            assert_eq!(sm.state(), NodeState::Active, "cycle {}: should be Active", i);

            // Force into Stale→Syncing by advancing network height far ahead
            sm.set_network_height(i * 100 + 100 + STALE_THRESHOLD + 1);
            assert_eq!(
                sm.state(), NodeState::Syncing,
                "cycle {}: should be Syncing after going stale", i
            );
        }
    }

    // -----------------------------------------------------------------------
    // Task 2b: Boundary — exactly STALE_THRESHOLD behind = Active; 11 = Syncing
    // -----------------------------------------------------------------------
    #[test]
    fn stale_threshold_boundary_exact() {
        let mut sm = NodeStateMachine::new();

        // Bring to Active at height 100
        sm.set_network_height(100);
        sm.set_our_height(100);
        assert_eq!(sm.state(), NodeState::Active);

        // Advance network by exactly STALE_THRESHOLD (not >)
        sm.set_network_height(100 + STALE_THRESHOLD);
        assert_eq!(
            sm.state(), NodeState::Active,
            "exactly {} behind should stay Active", STALE_THRESHOLD
        );
    }

    #[test]
    fn stale_threshold_boundary_one_over() {
        let mut sm = NodeStateMachine::new();

        sm.set_network_height(100);
        sm.set_our_height(100);
        assert_eq!(sm.state(), NodeState::Active);

        // Advance by STALE_THRESHOLD + 1 → triggers Stale→Syncing
        sm.set_network_height(100 + STALE_THRESHOLD + 1);
        assert_eq!(
            sm.state(), NodeState::Syncing,
            "{} behind should transition to Syncing", STALE_THRESHOLD + 1
        );
    }

    // -----------------------------------------------------------------------
    // Task 2c: network_height monotonic — set 100, set 50, set 200
    // -----------------------------------------------------------------------
    #[test]
    fn network_height_monotonic() {
        let mut sm = NodeStateMachine::new();

        sm.set_network_height(100);
        assert_eq!(sm.network_height(), 100, "after set 100");

        sm.set_network_height(50); // should be ignored
        assert_eq!(sm.network_height(), 100, "after set 50 (ignored)");

        sm.set_network_height(200);
        assert_eq!(sm.network_height(), 200, "after set 200");
    }

    // -----------------------------------------------------------------------
    // Task 2d: force_active idempotent — call 10 times, still Active
    // -----------------------------------------------------------------------
    #[test]
    fn force_active_idempotent() {
        let mut sm = NodeStateMachine::new();
        assert_eq!(sm.state(), NodeState::Syncing);

        for _ in 0..10 {
            sm.force_active();
            assert_eq!(sm.state(), NodeState::Active, "should remain Active after force_active");
        }
    }

    // -----------------------------------------------------------------------
    // Task 2e: Interleaved set_our_height and set_network_height
    // Simulates out-of-order updates from async sources
    // -----------------------------------------------------------------------
    #[test]
    fn interleaved_height_updates() {
        let mut sm = NodeStateMachine::new();

        // Interleave: network advances, then we catch up
        sm.set_network_height(10);
        assert_eq!(sm.state(), NodeState::Syncing); // haven't caught up yet

        sm.set_our_height(5);
        assert_eq!(sm.state(), NodeState::Syncing); // still behind

        sm.set_network_height(15);
        assert_eq!(sm.state(), NodeState::Syncing); // even further behind

        sm.set_our_height(15);
        assert_eq!(sm.state(), NodeState::Active); // caught up

        sm.set_our_height(10); // our_height can go "down" if we call it (no guard)
        // Network=15, ours=10: 15-10=5 <= STALE_THRESHOLD(10), still Active
        assert_eq!(sm.state(), NodeState::Active, "5 blocks behind should stay Active");

        sm.set_our_height(1); // Network=15, ours=1: 14 > STALE_THRESHOLD
        assert_eq!(sm.state(), NodeState::Syncing, "14 blocks behind should go Syncing");
    }

    // -----------------------------------------------------------------------
    // Task 2f: Zero heights — both at 0, should stay Syncing
    // -----------------------------------------------------------------------
    #[test]
    fn zero_heights_stays_syncing() {
        let sm = NodeStateMachine::new();
        assert_eq!(sm.state(), NodeState::Syncing);
        assert_eq!(sm.our_height(), 0);
        assert_eq!(sm.network_height(), 0);
        // Both at 0: network_height is 0, so Syncing -> Active guard requires network_height > 0
        // Therefore should stay Syncing
        assert!(!sm.is_active(), "should not be active when network_height=0");
    }

    #[test]
    fn zero_network_height_prevents_active() {
        let mut sm = NodeStateMachine::new();
        sm.set_our_height(100); // We have blocks, but network reports 0
        // network_height is still 0 — guard: network_height > 0 required for Active
        assert_eq!(sm.state(), NodeState::Syncing, "should stay Syncing when network_height=0");
    }

    // -----------------------------------------------------------------------
    // Additional: force_active then Stale check
    // -----------------------------------------------------------------------
    #[test]
    fn force_active_then_can_go_stale() {
        let mut sm = NodeStateMachine::new();
        sm.force_active();
        assert_eq!(sm.state(), NodeState::Active);

        // Now simulate falling behind
        sm.set_network_height(STALE_THRESHOLD + 2);
        // our_height is still 0: 12 > 10 → should go Stale→Syncing
        assert_eq!(sm.state(), NodeState::Syncing);
    }

    // -----------------------------------------------------------------------
    // Additional: is_syncing helper
    // -----------------------------------------------------------------------
    #[test]
    fn is_syncing_helper_correct() {
        let mut sm = NodeStateMachine::new();
        assert!(sm.is_syncing());

        sm.set_network_height(10);
        sm.set_our_height(10);
        assert!(!sm.is_syncing());
        assert!(sm.is_active());
    }
}
