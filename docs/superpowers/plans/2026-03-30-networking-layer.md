# Networking Layer Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ad-hoc networking with a resource-managed layer: node state machine, round-robin leader election, sync backpressure, consensus on request-response.

**Architecture:** Four independent deliverables in order: (1) leader election eliminates multi-producer forks, (2) node state machine eliminates sync flooding, (3) connection manager enforces stream budgets, (4) consensus moves from gossipsub to request-response for reliable voting. Each produces a deployable improvement.

**Tech Stack:** Rust, libp2p (gossipsub, request-response, kademlia, yamux, QUIC), tokio, serde_json

---

## Spec Reference

`docs/superpowers/specs/2026-03-30-networking-layer-design.md`

## Critical Files

| File | Lines | Role |
|------|-------|------|
| `src/node/src/event_loop.rs` | 2906 | Main event loop -- PROTECTED FILE, founder only |
| `src/node/src/consensus_manager.rs` | 1271 | Snowball consensus coordination |
| `src/network/src/transport.rs` | 797 | libp2p swarm construction |
| `src/network/src/sync_protocol.rs` | 131 | Sync request-response codec |
| `src/consensus/src/anchor.rs` | 135 | VRF-weighted anchor selection (preserved, not used for now) |

## File Structure (new/modified)

```
src/node/src/
├── leader.rs           -- NEW: round-robin leader election + view change
├── node_state.rs       -- NEW: Syncing/Active/Stale state machine
├── sync_machine.rs     -- NEW: sync state machine (QueryHeight/Downloading/Verifying/Complete)
├── consensus_manager.rs  -- MODIFY: accept leader context, scoped voting
├── event_loop.rs       -- MODIFY: integrate leader, state machine, new tick logic
src/network/src/
├── transport.rs        -- MODIFY: stream budgets, QUIC preference
├── sync_protocol.rs    -- UNCHANGED (messages same, config changes)
├── consensus_protocol.rs -- NEW: request-response codec for proposals + votes
├── lib.rs              -- MODIFY: add pub mod consensus_protocol
```

---

### Task 1: Round-Robin Leader Election

The most impactful change. Eliminates multi-producer forks immediately. Can be tested with current gossipsub voting before any other changes.

**Files:**
- Create: `src/node/src/leader.rs`
- Modify: `src/node/src/event_loop.rs` (handle_block_tick, ~lines 2108-2220)
- Modify: `src/node/src/lib.rs` (add `pub mod leader;`)

- [ ] **Step 1: Write failing tests for leader_for_height**

Create `src/node/src/leader.rs`:

```rust
use commputer_core::identity::Address;

/// Deterministic round-robin leader election.
/// All nodes independently compute the same leader for each height.
/// No communication needed.

/// Returns the leader for a given block height.
/// Validators are sorted by address bytes for deterministic ordering.
pub fn leader_for_height(height: u64, validators: &[Address]) -> Option<Address> {
    if validators.is_empty() {
        return None;
    }
    let mut sorted = validators.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    Some(sorted[height as usize % sorted.len()])
}

/// Returns the fallback leader after view change timeout.
/// Each 6-second window advances to the next validator in sorted order.
/// `seconds_since_expected` is time since the block was expected (height * 2s from genesis).
pub fn fallback_leader(
    height: u64,
    validators: &[Address],
    seconds_since_expected: u64,
) -> Option<Address> {
    if validators.is_empty() {
        return None;
    }
    let mut sorted = validators.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let primary = height as usize % sorted.len();
    let offset = (seconds_since_expected / 6) as usize;
    Some(sorted[(primary + offset) % sorted.len()])
}

/// Returns true if `address` is the valid leader for this height,
/// accounting for view change timeouts.
/// `seconds_waiting` is how long since the block was expected.
/// Tolerance of 3 seconds for clock skew.
pub fn is_valid_leader(
    height: u64,
    address: &Address,
    validators: &[Address],
    seconds_waiting: u64,
) -> bool {
    if validators.is_empty() {
        return false;
    }
    let mut sorted = validators.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let primary = height as usize % sorted.len();

    // Check primary leader.
    if sorted[primary] == *address && seconds_waiting < 9 {
        // Primary valid for first 6s + 3s tolerance.
        return true;
    }

    // Check fallback leaders (view changes).
    // Each 6-second window enables the next validator.
    // With 3s tolerance, check current and previous window.
    if seconds_waiting >= 3 {
        let max_offset = ((seconds_waiting + 3) / 6) as usize;
        for offset in 0..=max_offset {
            let idx = (primary + offset) % sorted.len();
            if sorted[idx] == *address {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(id: u8) -> Address {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        Address(bytes)
    }

    #[test]
    fn test_round_robin_basic() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // Sorted: addr(1), addr(2), addr(3)
        assert_eq!(leader_for_height(0, &validators), Some(addr(1)));
        assert_eq!(leader_for_height(1, &validators), Some(addr(2)));
        assert_eq!(leader_for_height(2, &validators), Some(addr(3)));
        assert_eq!(leader_for_height(3, &validators), Some(addr(1))); // Wraps
    }

    #[test]
    fn test_round_robin_deterministic() {
        let validators = vec![addr(3), addr(1), addr(2)]; // Unsorted input
        let a = leader_for_height(5, &validators);
        let b = leader_for_height(5, &validators);
        assert_eq!(a, b); // Same result regardless of input order
    }

    #[test]
    fn test_round_robin_empty() {
        assert_eq!(leader_for_height(0, &[]), None);
    }

    #[test]
    fn test_round_robin_single_validator() {
        let validators = vec![addr(1)];
        assert_eq!(leader_for_height(0, &validators), Some(addr(1)));
        assert_eq!(leader_for_height(100, &validators), Some(addr(1)));
    }

    #[test]
    fn test_fallback_leader() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // Primary for height 0 = addr(1).
        // After 6s, fallback = addr(2).
        // After 12s, fallback = addr(3).
        assert_eq!(fallback_leader(0, &validators, 0), Some(addr(1)));
        assert_eq!(fallback_leader(0, &validators, 6), Some(addr(2)));
        assert_eq!(fallback_leader(0, &validators, 12), Some(addr(3)));
        assert_eq!(fallback_leader(0, &validators, 18), Some(addr(1))); // Wraps
    }

    #[test]
    fn test_is_valid_leader_primary() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // addr(1) is primary for height 0, valid for first ~9 seconds.
        assert!(is_valid_leader(0, &addr(1), &validators, 0));
        assert!(is_valid_leader(0, &addr(1), &validators, 5));
        assert!(is_valid_leader(0, &addr(1), &validators, 8)); // Within 3s tolerance
    }

    #[test]
    fn test_is_valid_leader_fallback() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // After 6+ seconds, addr(2) becomes valid fallback for height 0.
        assert!(is_valid_leader(0, &addr(2), &validators, 7));
        assert!(is_valid_leader(0, &addr(2), &validators, 10));
    }

    #[test]
    fn test_is_valid_leader_rejects_wrong() {
        let validators = vec![addr(1), addr(2), addr(3)];
        // addr(3) is NOT valid at height 0, time 0.
        assert!(!is_valid_leader(0, &addr(3), &validators, 0));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cd src && cargo test -p commputer --lib leader::tests -- --nocapture`
Expected: All 7 tests pass.

- [ ] **Step 3: Add module to lib.rs**

In `src/node/src/lib.rs`, add:
```rust
pub mod leader;
```

- [ ] **Step 4: Run full test suite**

Run: `cd src && cargo test`
Expected: All existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/node/src/leader.rs src/node/src/lib.rs
git commit -m "feat(consensus): round-robin leader election with view change fallback"
```

---

### Task 2: Wire Leader Election into Block Production

Modify `handle_block_tick` in event_loop.rs so only the elected leader produces blocks.

**Files:**
- Modify: `src/node/src/event_loop.rs` (handle_block_tick, ~lines 2159-2220)

- [ ] **Step 1: Add active_validators helper method**

Add this method to `impl EventLoop` (near the other helper methods, around line 2500):

```rust
/// Returns sorted list of active validator addresses.
/// Used for leader election — all nodes compute the same list.
fn active_validators(&self) -> Vec<Address> {
    let mut validators: Vec<Address> = self.state.accounts.iter()
        .filter(|a| a.is_validator)
        .map(|a| a.address)
        .collect();
    validators.sort_by(|a, b| a.0.cmp(&b.0));
    validators
}
```

- [ ] **Step 2: Replace the block rotation logic in handle_block_tick**

Replace the existing block rotation block (approximately lines 2177-2220 — the section starting with `// Item 52: Block production fairness` through the end of the rotation check) with:

```rust
        // Leader election: only produce if we're the elected leader for this height.
        let validators = self.active_validators();
        let our_addr = *self.wallet.address();
        let seconds_waiting = self.last_block_seen_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);

        if !crate::leader::is_valid_leader(next_height, &our_addr, &validators, seconds_waiting) {
            return;
        }
```

This replaces the old `active_validators.len() >= 10` rotation check AND the view change timeout check — both are now handled by `is_valid_leader`.

- [ ] **Step 3: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build with no errors.

- [ ] **Step 4: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass. No behavior change for existing tests (they don't exercise multi-node block production).

- [ ] **Step 5: Commit**

```bash
git add src/node/src/event_loop.rs
git commit -m "feat(consensus): wire round-robin leader election into block production"
```

---

### Task 3: Validate Leader in Block Proposals

When receiving a block from a peer, verify the producer is the valid leader for that height. Reject blocks from non-leaders.

**Files:**
- Modify: `src/node/src/event_loop.rs` (validate_block_from_peer, ~line 1381)

- [ ] **Step 1: Add leader validation to validate_block_from_peer**

After the existing timestamp validation (around line 1420, after the timestamp-before-parent check), add:

```rust
        // Leader election validation: reject blocks from non-leaders.
        let validators = self.active_validators();
        if !validators.is_empty() {
            let seconds_since_parent = if let Some(parent) = self.state.blocks.get(&block.header.parent_hash) {
                block.header.timestamp.saturating_sub(parent.header.timestamp)
            } else {
                0 // Can't verify timing without parent — allow it (sync may deliver out of order)
            };
            if !crate::leader::is_valid_leader(
                block.height(),
                &block.header.producer,
                &validators,
                seconds_since_parent,
            ) {
                warn!("Rejected block from {}: not the valid leader for height {} (waited {}s)",
                    source, block.height(), seconds_since_parent);
                self.adjust_peer_score(source, -10);
                return false;
            }
        }
```

- [ ] **Step 2: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build.

- [ ] **Step 3: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/node/src/event_loop.rs
git commit -m "feat(consensus): validate leader election in received blocks"
```

---

### Task 4: Node State Machine

Replace the `sync_complete` boolean with a proper Syncing/Active/Stale state machine.

**Files:**
- Create: `src/node/src/node_state.rs`
- Modify: `src/node/src/lib.rs` (add module)
- Modify: `src/node/src/event_loop.rs` (replace sync_complete usage)

- [ ] **Step 1: Create node_state.rs with tests**

```rust
use tracing::{info, warn};

/// Node operating state. Determines what the node is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// Downloading blocks. No block production, no consensus voting, no gossip publishing.
    Syncing,
    /// Caught up with network. Full participation.
    Active,
    /// Was Active, fell behind. Transitioning to Syncing.
    Stale,
}

/// How far behind before an Active node becomes Stale.
pub const STALE_THRESHOLD: u64 = 10;

/// Manages node state transitions.
pub struct NodeStateMachine {
    state: NodeState,
    /// Our current chain height.
    our_height: u64,
    /// Highest height observed from the network.
    network_height: u64,
}

impl NodeStateMachine {
    pub fn new() -> Self {
        Self {
            state: NodeState::Syncing,
            our_height: 0,
            network_height: 0,
        }
    }

    pub fn state(&self) -> NodeState {
        self.state
    }

    pub fn our_height(&self) -> u64 {
        self.our_height
    }

    pub fn network_height(&self) -> u64 {
        self.network_height
    }

    /// Update our chain height. May trigger state transitions.
    pub fn set_our_height(&mut self, height: u64) {
        self.our_height = height;
        self.check_transitions();
    }

    /// Update the observed network height. May trigger state transitions.
    pub fn set_network_height(&mut self, height: u64) {
        if height > self.network_height {
            self.network_height = height;
            self.check_transitions();
        }
    }

    /// Force transition to Active (e.g., first/solo node after timeout).
    pub fn force_active(&mut self) {
        if self.state != NodeState::Active {
            info!("Node state: {:?} -> Active (forced)", self.state);
            self.state = NodeState::Active;
        }
    }

    /// Check and apply state transitions.
    fn check_transitions(&mut self) {
        match self.state {
            NodeState::Syncing => {
                if self.our_height >= self.network_height && self.network_height > 0 {
                    info!("Node state: Syncing -> Active (caught up at height {})", self.our_height);
                    self.state = NodeState::Active;
                }
            }
            NodeState::Active => {
                if self.network_height > self.our_height + STALE_THRESHOLD {
                    warn!("Node state: Active -> Stale (behind by {} blocks)",
                        self.network_height - self.our_height);
                    self.state = NodeState::Stale;
                    // Stale immediately transitions to Syncing.
                    info!("Node state: Stale -> Syncing");
                    self.state = NodeState::Syncing;
                }
            }
            NodeState::Stale => {
                // Stale always transitions to Syncing immediately.
                self.state = NodeState::Syncing;
            }
        }
    }

    /// Whether the node should produce blocks and vote.
    pub fn is_active(&self) -> bool {
        self.state == NodeState::Active
    }

    /// Whether the node is syncing (should download blocks, not participate).
    pub fn is_syncing(&self) -> bool {
        self.state == NodeState::Syncing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_syncing() {
        let sm = NodeStateMachine::new();
        assert_eq!(sm.state(), NodeState::Syncing);
        assert!(!sm.is_active());
        assert!(sm.is_syncing());
    }

    #[test]
    fn syncing_to_active_when_caught_up() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(100);
        assert!(sm.is_syncing());
        sm.set_our_height(100);
        assert!(sm.is_active());
    }

    #[test]
    fn active_to_stale_to_syncing_when_behind() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert!(sm.is_active());

        // Fall behind by more than STALE_THRESHOLD.
        sm.set_network_height(25);
        assert!(sm.is_syncing()); // Stale -> Syncing immediately
    }

    #[test]
    fn active_stays_active_within_threshold() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(10);
        sm.set_our_height(10);
        assert!(sm.is_active());

        // Fall behind by less than threshold.
        sm.set_network_height(18);
        assert!(sm.is_active()); // Still active (only 8 behind, threshold is 10)
    }

    #[test]
    fn force_active() {
        let mut sm = NodeStateMachine::new();
        assert!(sm.is_syncing());
        sm.force_active();
        assert!(sm.is_active());
    }

    #[test]
    fn network_height_only_increases() {
        let mut sm = NodeStateMachine::new();
        sm.set_network_height(100);
        sm.set_network_height(50); // Ignored — can't go backwards.
        assert_eq!(sm.network_height(), 100);
    }
}
```

- [ ] **Step 2: Add module to lib.rs**

In `src/node/src/lib.rs`, add:
```rust
pub mod node_state;
```

- [ ] **Step 3: Run tests**

Run: `cd src && cargo test -p commputer --lib node_state::tests -- --nocapture`
Expected: All 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/node/src/node_state.rs src/node/src/lib.rs
git commit -m "feat(node): add NodeStateMachine (Syncing/Active/Stale)"
```

---

### Task 5: Wire Node State Machine into Event Loop

Replace `sync_complete` and `partition_detected` with the NodeStateMachine.

**Files:**
- Modify: `src/node/src/event_loop.rs`

- [ ] **Step 1: Add NodeStateMachine field to EventLoop struct**

In the EventLoop struct definition (~line 65), add:
```rust
    /// Node operating state: Syncing, Active, or Stale.
    pub node_state: crate::node_state::NodeStateMachine,
```

In `EventLoop::new()` (~line 150), add to the struct initializer:
```rust
            node_state: crate::node_state::NodeStateMachine::new(),
```

- [ ] **Step 2: Replace sync_complete checks with node_state.is_active()**

In `handle_block_tick` (~line 2130), replace:
```rust
        if !self.sync_complete {
            return;
        }
```
with:
```rust
        if !self.node_state.is_active() {
            return;
        }
```

In `handle_consensus_tick` (~line 2319), add at the top:
```rust
        if !self.node_state.is_active() {
            return;
        }
```

- [ ] **Step 3: Update network_height to feed the state machine**

Everywhere `self.network_height` is updated (search for `self.network_height =`), also call:
```rust
self.node_state.set_network_height(height);
```

Everywhere a block is applied (after `apply_block_validated` succeeds), also call:
```rust
self.node_state.set_our_height(self.state.blocks.height());
```

- [ ] **Step 4: Replace the 30-second solo-node timeout**

Find the "No network blocks found after 30s" logic and replace with:
```rust
self.node_state.force_active();
```

- [ ] **Step 5: Replace partition_detected with node_state**

Remove the `partition_detected` check from `handle_block_tick`. The node state machine now handles this — if all peers disconnect and network_height advances beyond our height, we go Stale -> Syncing automatically.

- [ ] **Step 6: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build. Some warnings about unused `sync_complete` and `partition_detected` fields are OK.

- [ ] **Step 7: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/node/src/event_loop.rs
git commit -m "feat(node): wire NodeStateMachine into event loop, replacing sync_complete"
```

---

### Task 6: Sync State Machine

Replace the ad-hoc 5-second sync timer with a proper state machine.

**Files:**
- Create: `src/node/src/sync_machine.rs`
- Modify: `src/node/src/lib.rs`

- [ ] **Step 1: Create sync_machine.rs**

```rust
use std::collections::HashSet;
use std::time::Instant;
use tracing::{info, debug, warn};

/// Sync state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Not syncing (node is Active).
    Idle,
    /// Querying peers for their chain height.
    QueryHeight,
    /// Downloading blocks in batches.
    Downloading,
    /// Verifying we're caught up before transitioning to Active.
    Verifying,
    /// Sync complete.
    Complete,
}

/// Batch size for block downloads.
pub const SYNC_BATCH_SIZE: u64 = 10;

/// Timeout for a single batch request (seconds).
pub const BATCH_TIMEOUT_SECS: u64 = 5;

/// Timeout for height queries (seconds).
pub const HEIGHT_QUERY_TIMEOUT_SECS: u64 = 5;

/// Max failed attempts on a peer before rotating.
pub const MAX_PEER_FAILURES: u32 = 3;

/// Manages the sync lifecycle.
pub struct SyncMachine {
    state: SyncState,
    /// Target height we're syncing to.
    target_height: u64,
    /// Heights we've received from peers during QueryHeight.
    height_responses: Vec<u64>,
    /// When we entered the current state.
    state_entered_at: Instant,
    /// Current batch: (start, end) inclusive.
    current_batch: Option<(u64, u64)>,
    /// Peer we're currently syncing from.
    current_peer: Option<libp2p::PeerId>,
    /// Failed sync attempts per peer.
    peer_failures: std::collections::HashMap<libp2p::PeerId, u32>,
    /// Peers we've tried and exhausted.
    exhausted_peers: HashSet<libp2p::PeerId>,
}

impl SyncMachine {
    pub fn new() -> Self {
        Self {
            state: SyncState::Idle,
            target_height: 0,
            height_responses: Vec::new(),
            state_entered_at: Instant::now(),
            current_batch: None,
            current_peer: None,
            peer_failures: std::collections::HashMap::new(),
            exhausted_peers: HashSet::new(),
        }
    }

    pub fn state(&self) -> SyncState {
        self.state
    }

    pub fn target_height(&self) -> u64 {
        self.target_height
    }

    /// Begin syncing. Transitions from Idle to QueryHeight.
    pub fn start(&mut self) {
        if self.state == SyncState::Idle || self.state == SyncState::Complete {
            info!("Sync: starting (QueryHeight)");
            self.state = SyncState::QueryHeight;
            self.height_responses.clear();
            self.state_entered_at = Instant::now();
            self.exhausted_peers.clear();
            self.peer_failures.clear();
        }
    }

    /// Record a height response from a peer.
    pub fn record_height(&mut self, height: u64) {
        if self.state == SyncState::QueryHeight {
            self.height_responses.push(height);
        }
    }

    /// Returns true if we should transition from QueryHeight to Downloading.
    /// Call this after recording height responses.
    pub fn should_start_downloading(&self, our_height: u64) -> bool {
        if self.state != SyncState::QueryHeight {
            return false;
        }
        // Start when we have at least 1 response or timeout.
        !self.height_responses.is_empty()
            || self.state_entered_at.elapsed().as_secs() >= HEIGHT_QUERY_TIMEOUT_SECS
    }

    /// Compute target and transition to Downloading. Returns the target height.
    pub fn begin_downloading(&mut self, our_height: u64) -> u64 {
        // Take median of responses.
        let target = if self.height_responses.is_empty() {
            our_height // No responses — we might be alone.
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort();
            sorted[sorted.len() / 2]
        };
        self.target_height = target;
        self.state = SyncState::Downloading;
        self.state_entered_at = Instant::now();
        info!("Sync: target height = {}, starting download from height {}", target, our_height);
        target
    }

    /// Returns the next batch to request, or None if caught up.
    pub fn next_batch(&mut self, our_height: u64) -> Option<(u64, u64)> {
        if self.state != SyncState::Downloading {
            return None;
        }
        if our_height >= self.target_height {
            // Caught up — transition to Verifying.
            self.state = SyncState::Verifying;
            self.height_responses.clear();
            self.state_entered_at = Instant::now();
            info!("Sync: reached target height {}, verifying", self.target_height);
            return None;
        }
        let start = our_height + 1;
        let end = std::cmp::min(start + SYNC_BATCH_SIZE - 1, self.target_height);
        self.current_batch = Some((start, end));
        self.state_entered_at = Instant::now();
        Some((start, end))
    }

    /// Check if the current batch has timed out.
    pub fn batch_timed_out(&self) -> bool {
        self.state == SyncState::Downloading
            && self.state_entered_at.elapsed().as_secs() >= BATCH_TIMEOUT_SECS
    }

    /// Record a batch failure. Returns true if we should rotate peers.
    pub fn record_batch_failure(&mut self, peer: libp2p::PeerId) -> bool {
        let count = self.peer_failures.entry(peer).or_insert(0);
        *count += 1;
        if *count >= MAX_PEER_FAILURES {
            self.exhausted_peers.insert(peer);
            warn!("Sync: peer {} exhausted after {} failures", peer, count);
            true
        } else {
            false
        }
    }

    /// Returns true if verification is ready (got height responses or timed out).
    pub fn verification_ready(&self) -> bool {
        self.state == SyncState::Verifying
            && (!self.height_responses.is_empty()
                || self.state_entered_at.elapsed().as_secs() >= HEIGHT_QUERY_TIMEOUT_SECS)
    }

    /// Complete verification. Returns true if sync is complete, false if more downloading needed.
    pub fn complete_verification(&mut self, our_height: u64) -> bool {
        if self.height_responses.is_empty() {
            // No responses — assume we're caught up.
            self.state = SyncState::Complete;
            info!("Sync: complete at height {}", our_height);
            return true;
        }
        let mut sorted = self.height_responses.clone();
        sorted.sort();
        let median = sorted[sorted.len() / 2];
        if our_height >= median {
            self.state = SyncState::Complete;
            info!("Sync: complete at height {} (network at {})", our_height, median);
            true
        } else {
            // Network advanced — keep syncing.
            self.target_height = median;
            self.state = SyncState::Downloading;
            self.state_entered_at = Instant::now();
            info!("Sync: network advanced to {}, continuing download", median);
            false
        }
    }

    /// Select a peer to sync from. Avoids exhausted peers.
    pub fn select_peer(&self, available: &[libp2p::PeerId]) -> Option<libp2p::PeerId> {
        available.iter()
            .find(|p| !self.exhausted_peers.contains(p))
            .copied()
    }

    /// Reset to Idle (called when node transitions to Active).
    pub fn reset(&mut self) {
        self.state = SyncState::Idle;
        self.current_batch = None;
        self.current_peer = None;
        self.height_responses.clear();
        self.peer_failures.clear();
        self.exhausted_peers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_idle() {
        let sm = SyncMachine::new();
        assert_eq!(sm.state(), SyncState::Idle);
    }

    #[test]
    fn start_transitions_to_query_height() {
        let mut sm = SyncMachine::new();
        sm.start();
        assert_eq!(sm.state(), SyncState::QueryHeight);
    }

    #[test]
    fn downloading_produces_batches() {
        let mut sm = SyncMachine::new();
        sm.start();
        sm.record_height(100);
        sm.begin_downloading(0);
        assert_eq!(sm.state(), SyncState::Downloading);

        let batch = sm.next_batch(0);
        assert_eq!(batch, Some((1, 10)));

        let batch = sm.next_batch(10);
        assert_eq!(batch, Some((11, 20)));
    }

    #[test]
    fn downloading_transitions_to_verifying_when_caught_up() {
        let mut sm = SyncMachine::new();
        sm.start();
        sm.record_height(5);
        sm.begin_downloading(0);

        let batch = sm.next_batch(5);
        assert_eq!(batch, None);
        assert_eq!(sm.state(), SyncState::Verifying);
    }

    #[test]
    fn verification_completes_when_caught_up() {
        let mut sm = SyncMachine::new();
        sm.state = SyncState::Verifying;
        sm.record_height(100);
        assert!(sm.complete_verification(100));
        assert_eq!(sm.state(), SyncState::Complete);
    }

    #[test]
    fn verification_continues_if_behind() {
        let mut sm = SyncMachine::new();
        sm.state = SyncState::Verifying;
        sm.record_height(200);
        assert!(!sm.complete_verification(100));
        assert_eq!(sm.state(), SyncState::Downloading);
        assert_eq!(sm.target_height(), 200);
    }
}
```

- [ ] **Step 2: Add module to lib.rs**

In `src/node/src/lib.rs`, add:
```rust
pub mod sync_machine;
```

- [ ] **Step 3: Run tests**

Run: `cd src && cargo test -p commputer --lib sync_machine::tests -- --nocapture`
Expected: All 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/node/src/sync_machine.rs src/node/src/lib.rs
git commit -m "feat(node): add SyncMachine state machine with backpressure"
```

---

### Task 7: Connection Manager Configuration

Configure libp2p with explicit stream budgets and QUIC preference.

**Files:**
- Modify: `src/network/src/transport.rs`
- Modify: `src/network/src/sync_protocol.rs`

- [ ] **Step 1: Set yamux max streams**

In `transport.rs`, find where yamux is configured (line ~199):
```rust
yamux::Config::default,
```

Replace with a function that sets max streams:
```rust
|| {
    let mut cfg = yamux::Config::default();
    cfg.set_max_num_streams(24);
    cfg
},
```

Do the same for the relay client yamux config (~line 202).

- [ ] **Step 2: Reduce sync protocol concurrent streams**

In `sync_protocol.rs`, line 126, change:
```rust
.with_max_concurrent_streams(256);
```
to:
```rust
.with_max_concurrent_streams(4);
```

- [ ] **Step 3: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build.

- [ ] **Step 4: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/network/src/transport.rs src/network/src/sync_protocol.rs
git commit -m "feat(network): enforce stream budgets — 24 per connection, 4 for sync"
```

---

### Task 8: Consensus Request-Response Protocol

Create a new request-response protocol for direct block proposals and votes. This replaces gossipsub for consensus.

**Files:**
- Create: `src/network/src/consensus_protocol.rs`
- Modify: `src/network/src/lib.rs` (add module)
- Modify: `src/network/src/transport.rs` (add to CommpBehaviour)

- [ ] **Step 1: Create consensus_protocol.rs**

```rust
//! Direct request-response protocol for consensus.
//! Block proposals and votes go here — NOT through gossipsub.
//! Guarantees delivery to specific peers, no dedup issues.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

pub const CONSENSUS_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/consensus/1");

/// A consensus request from the leader to a peer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConsensusRequest {
    /// Leader sends full block proposal.
    BlockProposal { block_bytes: Vec<u8>, height: u64 },
    /// Leader requests a vote from a peer that hasn't responded.
    VoteRequest { height: u64, block_hash: [u8; 32] },
}

/// A consensus response from a peer to the leader.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConsensusResponse {
    /// Peer validates and votes.
    Vote {
        height: u64,
        preference: [u8; 32],
        accept: bool,
    },
    /// Peer is not ready (still syncing).
    NotReady { height: u64 },
}

/// Codec for the consensus protocol — same pattern as sync protocol.
#[derive(Debug, Clone, Default)]
pub struct ConsensusCodec;

#[async_trait]
impl request_response::Codec for ConsensusCodec {
    type Protocol = StreamProtocol;
    type Request = ConsensusRequest;
    type Response = ConsensusResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 10 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "request too large"));
        }
        let mut buf = vec![0u8; len];
        io.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 10 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "response too large"));
        }
        let mut buf = vec![0u8; len];
        io.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (data.len() as u32).to_be_bytes();
        io.write_all(&len).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&resp)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (data.len() as u32).to_be_bytes();
        io.write_all(&len).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }
}

/// Create the request-response behaviour for consensus.
pub fn consensus_behaviour() -> request_response::Behaviour<ConsensusCodec> {
    let config = request_response::Config::default()
        .with_max_concurrent_streams(4);
    request_response::Behaviour::new(
        [(CONSENSUS_PROTOCOL, request_response::ProtocolSupport::Full)],
        config,
    )
}
```

- [ ] **Step 2: Add module to lib.rs**

In `src/network/src/lib.rs`, add:
```rust
pub mod consensus_protocol;
```

- [ ] **Step 3: Add consensus behaviour to CommpBehaviour**

In `src/network/src/transport.rs`, add to the `CommpBehaviour` struct:
```rust
pub consensus: libp2p::request_response::Behaviour<crate::consensus_protocol::ConsensusCodec>,
```

And in the swarm builder where behaviours are constructed, add the consensus behaviour alongside the sync behaviour.

- [ ] **Step 4: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build.

- [ ] **Step 5: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/network/src/consensus_protocol.rs src/network/src/lib.rs src/network/src/transport.rs
git commit -m "feat(network): add consensus request-response protocol for direct proposals + votes"
```

---

### Task 9: Wire Consensus Protocol into Event Loop

The leader sends block proposals directly to peers via the new consensus request-response protocol. Peers validate and vote back directly. This replaces gossipsub for consensus.

**Files:**
- Modify: `src/node/src/event_loop.rs`

This is the largest task. It modifies the protected event_loop.rs extensively.

- [ ] **Step 1: Add consensus request-response event handling**

In the main swarm event match (alongside the existing sync protocol handler), add a handler for consensus protocol events. When a `ConsensusRequest::BlockProposal` arrives:
1. Deserialize the block
2. Validate it (validate_block_from_peer)
3. If valid and node is Active, respond with `ConsensusResponse::Vote { accept: true }`
4. If node is Syncing, respond with `ConsensusResponse::NotReady`

When a `ConsensusResponse::Vote` arrives (we're the leader):
1. Feed the vote into the Snowball voter
2. Try to finalize

- [ ] **Step 2: Replace gossipsub consensus publishing with direct sends**

In `handle_consensus_tick`, replace `publish_consensus_message` calls with direct request-response sends to each connected Active peer.

The leader:
1. Serializes the block to bytes
2. Sends `ConsensusRequest::BlockProposal` to each peer in `peer_ips`
3. On receiving votes back, feeds them into `consensus.record_response()`
4. On Snowball finalization, broadcasts compact `BlockAnnounce` on gossipsub (kept for block announcements only)

- [ ] **Step 3: Remove consensus gossipsub topic subscription**

Stop subscribing to `commputer/consensus/0.1`. Keep block and tx topics.

- [ ] **Step 4: Build and verify**

Run: `cd src && cargo build --release 2>&1 | tail -5`
Expected: Clean build.

- [ ] **Step 5: Run full test suite**

Run: `cd src && cargo test`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/node/src/event_loop.rs
git commit -m "feat(consensus): replace gossipsub voting with direct request-response"
```

---

### Task 10: Deploy and Test 2-Node Testnet

Verify the complete networking redesign works on the real testnet.

- [ ] **Step 1: Build release binary**

Run: `cd src && cargo build --release`

- [ ] **Step 2: Deploy to Optiplex**

```bash
scp ~/Coin/src/target/release/commputer newserver:~/commputer-bin
```

- [ ] **Step 3: Start Optiplex (seed)**

```bash
ssh newserver
rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test ~/commputer-bin run --port 9000
```

- [ ] **Step 4: Start Laptop (connects to Optiplex)**

```bash
rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test ~/Coin/src/target/release/commputer run --port 9001 --seeds /ip4/198.51.100.51/tcp/9000
```

- [ ] **Step 5: Verify**

Check for:
- Both nodes connected (Peers: 1)
- Round-robin block production (alternating producers)
- Snowball-finalized every block (no timeout-finalize)
- No yamux stream errors
- No fork after 500+ blocks
- Clean sync if one node restarts

- [ ] **Step 6: Commit tag**

```bash
git tag testnet-v0.2.0 -m "Networking redesign: leader election, state machine, stream budgets, direct consensus"
```

---

## Verification

1. `cargo test` — all existing tests + ~20 new tests pass
2. `cargo build --release` — clean build, no warnings
3. 2-node testnet: alternating leaders, Snowball-finalized, no forks after 500+ blocks
4. Node restart: syncs via state machine, catches up, re-enters Active
5. No yamux stream exhaustion
6. No gossipsub rate limit issues during sync
7. Consensus votes delivered reliably (no asymmetry)
