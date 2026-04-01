# Fork Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove force-finalize, add fork detection circuit breaker, and implement chain wipe-and-resync so minority-fork nodes recover automatically.

**Architecture:** Four changes: (1) new `ForkDetector` struct triggers resync after 3 consecutive parent hash mismatches, (2) `ConsensusManager::try_finalize_round` returns a 3-state enum instead of fabricating votes, (3) `ChainState::reset_to_genesis` wipes blocks/accounts and the event loop orchestrates a full resync, (4) emergency production (30s any-validator bypass) is removed.

**Tech Stack:** Rust, existing crates (commputer-node, commputer-storage, commputer-consensus). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-01-fork-recovery-design.md`

---

## Context

The `commputer-node` crate is at `src/node/src/`. The event loop (`event_loop.rs`, ~2900 lines) drives block production, consensus, and sync. `ConsensusManager` (`consensus_manager.rs`) wraps Snowball voting per height. `ChainState` (`src/storage/src/state.rs`) holds all block and account state. `NodeStateMachine` (`node_state.rs`) tracks Syncing/Active/Stale.

Key existing behavior:
- `event_loop.rs:2364` gates block production on `node_state.is_active()`
- `event_loop.rs:2545` gates consensus voting on `node_state.is_active()`
- `event_loop.rs:2587` calls `consensus.try_finalize_round(height, peer_count)` which returns `bool`
- `event_loop.rs:2616-2648` handles fork detection with a 1-block reorg attempt
- `event_loop.rs:2417-2426` has emergency production bypass (any validator after 30s)
- `event_loop.rs:2430-2434` has 6s view change bypass of `has_active_vote`
- `consensus_manager.rs:336-352` fabricates Snowball votes on timeout
- `node_state.rs:86-94` has `force_active()` but no `force_syncing()`

## File Structure

```
src/node/src/
├── fork_detector.rs       -- NEW: ForkDetector struct + tests
├── node_state.rs          -- MODIFY: add force_syncing() + test
├── consensus_manager.rs   -- MODIFY: ConsensusRoundResult enum, remove vote fabrication, add clear()
├── event_loop.rs          -- MODIFY: wire fork detector, stall timer, resync, remove emergency production
├── lib.rs                 -- MODIFY: add `pub mod fork_detector;`
src/storage/src/
├── state.rs               -- MODIFY: add reset_to_genesis()
```

---

### Task 1: ForkDetector struct

**Files:**
- Create: `src/node/src/fork_detector.rs`
- Modify: `src/node/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Add to `src/node/src/fork_detector.rs`:

```rust
/// Default number of consecutive parent hash mismatches before triggering resync.
pub const DEFAULT_FORK_THRESHOLD: u32 = 3;

/// Tracks consecutive parent hash mismatches during block finalization.
/// After `threshold` consecutive mismatches, signals that the node should
/// wipe its chain and resync from peers.
pub struct ForkDetector {
    consecutive_mismatches: u32,
    threshold: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_mismatch_no_resync() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        assert!(!fd.should_resync());
    }

    #[test]
    fn threshold_triggers_resync() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_mismatch();
        assert!(fd.should_resync());
    }

    #[test]
    fn success_resets_counter() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_success();
        assert_eq!(fd.consecutive_mismatches(), 0);
        // One more mismatch after reset -- not enough
        fd.record_mismatch();
        assert!(!fd.should_resync());
    }

    #[test]
    fn reset_clears_state() {
        let mut fd = ForkDetector::new();
        fd.record_mismatch();
        fd.record_mismatch();
        fd.record_mismatch();
        assert!(fd.should_resync());
        fd.reset();
        assert!(!fd.should_resync());
        assert_eq!(fd.consecutive_mismatches(), 0);
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-node --lib fork_detector 2>&1`
Expected: FAIL (methods not implemented)

- [ ] **Step 3: Implement ForkDetector**

```rust
impl ForkDetector {
    pub fn new() -> Self {
        Self {
            consecutive_mismatches: 0,
            threshold: DEFAULT_FORK_THRESHOLD,
        }
    }

    /// Record a parent hash mismatch during block finalization.
    pub fn record_mismatch(&mut self) {
        self.consecutive_mismatches += 1;
        if self.consecutive_mismatches >= self.threshold {
            tracing::warn!(
                consecutive = self.consecutive_mismatches,
                threshold = self.threshold,
                "fork_detector: mismatch threshold reached, resync recommended"
            );
        }
    }

    /// Record a successful block application (resets the counter).
    pub fn record_success(&mut self) {
        self.consecutive_mismatches = 0;
    }

    /// Whether the node should wipe and resync from peers.
    pub fn should_resync(&self) -> bool {
        self.consecutive_mismatches >= self.threshold
    }

    /// Current count of consecutive mismatches (for logging).
    pub fn consecutive_mismatches(&self) -> u32 {
        self.consecutive_mismatches
    }

    /// Reset all state (used after a resync completes).
    pub fn reset(&mut self) {
        self.consecutive_mismatches = 0;
    }
}

impl Default for ForkDetector {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Add module to lib.rs**

In `src/node/src/lib.rs`, add:
```rust
pub mod fork_detector;
```

- [ ] **Step 5: Run tests, verify they pass**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-node --lib fork_detector 2>&1`
Expected: 4 tests PASS

- [ ] **Step 6: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/fork_detector.rs src/node/src/lib.rs
git commit -m "feat(node): add ForkDetector for chain resync triggering"
```

---

### Task 2: NodeStateMachine.force_syncing()

**Files:**
- Modify: `src/node/src/node_state.rs`

- [ ] **Step 1: Write failing test**

Add to the existing `mod tests` in `node_state.rs`:

```rust
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
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-node --lib node_state::tests::force_syncing 2>&1`
Expected: FAIL (method not found)

- [ ] **Step 3: Implement force_syncing**

Add to `impl NodeStateMachine` in `node_state.rs`, after the existing `force_active()` method (after line 94):

```rust
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
```

- [ ] **Step 4: Run tests, verify all pass**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-node --lib node_state 2>&1`
Expected: 7 tests PASS (6 existing + 1 new)

- [ ] **Step 5: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/node_state.rs
git commit -m "feat(node): add NodeStateMachine.force_syncing() for chain resync"
```

---

### Task 3: ConsensusRoundResult enum and remove force-finalize

**Files:**
- Modify: `src/node/src/consensus_manager.rs`

- [ ] **Step 1: Add ConsensusRoundResult enum**

Add after the `use` statements at the top of `consensus_manager.rs` (after line 10):

```rust
/// Result of a consensus finalization attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusRoundResult {
    /// Voting is still in progress, not yet converged.
    NotReady,
    /// Snowball voting converged -- block is finalized.
    Finalized,
    /// Consensus timed out without convergence -- node should consider resyncing.
    Stalled,
}
```

- [ ] **Step 2: Change try_finalize_round return type and remove vote fabrication**

Replace the `try_finalize_round` method (lines 316-358) with:

```rust
    /// Feed accumulated responses into the voter and reset for the next round.
    /// Returns the result of the finalization attempt.
    /// `peer_count` is used to scale the timeout to network size.
    pub fn try_finalize_round(&mut self, height: u64, peer_count: usize) -> ConsensusRoundResult {
        if let Some(state) = self.heights.get_mut(&height) {
            if state.voter.is_finalized() {
                return ConsensusRoundResult::NotReady;
            }

            // Try to finalize from accumulated peer votes FIRST.
            if !state.round_responses.is_empty() {
                let responses = std::mem::take(&mut state.round_responses);
                let finalized = state.voter.record_round(&responses);
                if finalized {
                    info!(
                        "Snowball finalized at height {}: {:?}",
                        height,
                        state.voter.finalized_hash()
                    );
                    return ConsensusRoundResult::Finalized;
                }
            }

            // Timeout detection -- signal stall instead of fabricating votes.
            let timeout = consensus_timeout_secs(peer_count);
            if let Some(start) = self.height_start_time.get(&height)
                && start.elapsed().as_secs() >= timeout {
                    warn!("Consensus stalled at height {} (timeout {}s, {} peers)",
                        height, timeout, peer_count);
                    return ConsensusRoundResult::Stalled;
                }

            ConsensusRoundResult::NotReady
        } else {
            ConsensusRoundResult::NotReady
        }
    }
```

- [ ] **Step 3: Add clear() method**

Add after the `cleanup_below` method (after line 393):

```rust
    /// Clear all consensus state. Used during chain resync.
    pub fn clear(&mut self) {
        self.heights.clear();
        self.height_start_time.clear();
        self.validator_blocks.clear();
        self.slashed_validators.clear();
        self.view_changes.clear();
        self.last_block_time.clear();
        self.checkpoint_votes.clear();
    }
```

- [ ] **Step 4: Fix all callers of try_finalize_round**

In `event_loop.rs`, line 2587 currently ignores the return value:
```rust
self.consensus.try_finalize_round(next_height, peer_count);
```

Change to (this is a temporary change -- Task 5 will add the full stall timer logic):
```rust
let _result = self.consensus.try_finalize_round(next_height, peer_count);
```

- [ ] **Step 5: Run full build to verify compilation**

Run: `cd /home/operator/Coin/src && cargo check 2>&1`
Expected: Compiles with no errors

- [ ] **Step 6: Write tests for new behavior**

Add to the existing `#[cfg(test)] mod tests` section in `consensus_manager.rs`. The file already has a `make_test_block(height)` helper (line 611) and `add_candidate` takes a single `Block` argument (derives height from `block.header.height`). Verify no existing test asserts on `bool` return value from `try_finalize_round` -- they should all still compile since the return value was previously discarded.

```rust
    #[test]
    fn real_votes_still_finalize() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        let height = 1;

        cm.add_candidate(block);

        // Simulate enough rounds of unanimous votes to finalize.
        for _ in 0..cm.params.decision_threshold {
            cm.record_response(height, hash);
            let result = cm.try_finalize_round(height, 1);
            if result == ConsensusRoundResult::Finalized {
                assert!(cm.finalized_at_height(height).is_some());
                return;
            }
        }
        // Should have finalized within decision_threshold rounds.
        panic!("expected finalization within {} rounds", cm.params.decision_threshold);
    }

    #[test]
    fn timeout_returns_stalled() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        cm.add_candidate(block);

        // The height_start_time is set by add_candidate. Wait for the timeout.
        // For 0 peers, consensus_timeout_secs returns 6s.
        // We can't easily wait 6s in a unit test, so set the start time to the past.
        cm.height_start_time.insert(1, std::time::Instant::now() - std::time::Duration::from_secs(10));

        let result = cm.try_finalize_round(1, 0);
        assert_eq!(result, ConsensusRoundResult::Stalled);

        // Verify no finalization happened (no fabricated votes).
        assert!(cm.finalized_at_height(1).is_none());
    }

    #[test]
    fn clear_wipes_all_heights() {
        let mut cm = ConsensusManager::new();
        let block1 = make_test_block(1);
        let block2 = make_test_block(2);
        cm.add_candidate(block1);
        cm.add_candidate(block2);
        assert!(cm.has_height(1));
        assert!(cm.has_height(2));

        cm.clear();
        assert!(!cm.has_height(1));
        assert!(!cm.has_height(2));
        assert!(cm.active_heights().is_empty());
    }
```

- [ ] **Step 7: Run tests, verify they pass**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-node --lib consensus_manager 2>&1`
Expected: Tests PASS

- [ ] **Step 8: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/consensus_manager.rs src/node/src/event_loop.rs
git commit -m "feat(consensus): replace force-finalize with ConsensusRoundResult enum

Remove vote fabrication on timeout. try_finalize_round now returns
NotReady/Finalized/Stalled instead of bool. Add clear() for resync."
```

---

### Task 4: ChainState::reset_to_genesis()

**Files:**
- Modify: `src/storage/src/state.rs`

- [ ] **Step 1: Write failing test**

Add to the existing `mod tests` in `state.rs`:

```rust
#[test]
fn reset_to_genesis() {
    let mut state = ChainState::new();

    // Apply a genesis block.
    let genesis = commputer_core::block::Block::default();
    state.apply_block_validated(&genesis).unwrap();
    assert_eq!(state.blocks.height(), 0);

    // Apply a few more blocks to get some state.
    // (Use the test helpers if available, or manually create blocks)
    let height_before = state.blocks.height();
    assert!(height_before >= 0);

    // Reset.
    state.reset_to_genesis().unwrap();

    // After reset: height 0, no accounts beyond genesis, counters zeroed.
    assert_eq!(state.blocks.height(), 0);
    assert_eq!(state.total_emitted, 0);
    assert_eq!(state.total_burned, 0);
    assert_eq!(state.current_epoch, 0);
    assert!(state.state_diffs.is_empty());
    assert!(state.validator_performance.is_empty());
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-storage --lib state::tests::reset_to_genesis 2>&1`
Expected: FAIL (method not found)

- [ ] **Step 3: Implement reset_to_genesis**

Add to `impl ChainState` in `state.rs`:

```rust
    /// Wipe all blocks and account state, reinitialize to genesis (height 0).
    /// Used during chain resync after fork detection.
    ///
    /// For RocksDB-backed state: clears all column families and reinitializes.
    /// For in-memory state: resets all HashMaps.
    ///
    /// Caller must also reset: consensus manager, mempool, sync_complete flag.
    pub fn reset_to_genesis(&mut self) -> Result<(), StateError> {
        info!("Resetting chain state to genesis");

        // Clear in-memory stores.
        self.accounts = AccountStore::new();
        self.blocks = BlockStore::new();
        self.total_emitted = 0;
        self.total_burned = 0;
        self.nerf_rate = NerfRate::INITIAL;
        self.current_epoch = 0;
        self.receipts = ReceiptStore::new();
        self.history = AccountHistoryIndex::new();
        self.cumulative_score = 0;
        self.state_diffs.clear();
        self.archived_accounts.clear();
        self.snapshot_height = 0;
        self.validator_performance.clear();

        // If RocksDB-backed, clear all column families.
        if let Some(ref rocks) = self.rocks {
            rocks.clear_all()
                .map_err(|e| StateError::StorageError(format!("failed to clear RocksDB: {}", e)))?;
        }

        info!("Chain state reset to genesis complete");
        Ok(())
    }
```

**Note:** This requires `RocksStore::clear_all()`. Check if it exists; if not, add it to `src/storage/src/rocks.rs`:

```rust
    /// Clear all data from all column families.
    pub fn clear_all(&self) -> Result<(), rocksdb::Error> {
        // Delete all keys in each column family by iterating and deleting.
        // This is safer than dropping and recreating CFs.
        for cf_name in &[CF_BLOCKS, CF_BLOCK_HEIGHTS, CF_ACCOUNTS, CF_META, CF_ARCHIVED] {
            if let Some(cf) = self.db.cf_handle(cf_name) {
                let mut batch = rocksdb::WriteBatch::default();
                let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
                for item in iter {
                    if let Ok((key, _)) = item {
                        batch.delete_cf(&cf, &key);
                    }
                }
                self.db.write(batch)?;
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run test, verify it passes**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-storage --lib state::tests::reset_to_genesis 2>&1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cd /home/operator/Coin && git add src/storage/src/state.rs src/storage/src/rocks.rs
git commit -m "feat(storage): add ChainState::reset_to_genesis() for chain resync"
```

---

### Task 5: Wire fork detector and stall timer into event loop

**Files:**
- Modify: `src/node/src/event_loop.rs`

This is the integration task. It wires the ForkDetector, stall timer, and resync logic into the event loop.

- [ ] **Step 1: Add fields to the event loop struct**

Add these fields to the `EventLoop` struct (near `node_state`, `sync_machine`, etc.):

```rust
    /// Fork detection circuit breaker.
    pub fork_detector: commputer::fork_detector::ForkDetector,
    /// Timestamp of the first consensus stall signal. None if no stall.
    pub stall_start: Option<std::time::Instant>,
```

Initialize in the constructor:
```rust
    fork_detector: commputer::fork_detector::ForkDetector::new(),
    stall_start: None,
```

- [ ] **Step 2: Add resync helper method**

Add a new method to `EventLoop`:

```rust
    /// Wipe chain state and re-enter sync mode.
    /// Called when fork detector or stall timer triggers.
    fn initiate_chain_resync(&mut self, reason: &str) {
        warn!("Initiating chain resync: {}", reason);

        // 1. Force node state to Syncing.
        self.node_state.force_syncing();

        // 2. Wipe chain state.
        if let Err(e) = self.state.reset_to_genesis() {
            tracing::error!("Failed to reset chain state: {}", e);
            return;
        }

        // 3. Clear consensus state.
        self.consensus.clear();

        // 4. Clear mempool and message dedup.
        self.pending_txs.clear();
        self.seen_tx_hashes.clear();
        self.mempool_added_at.clear();
        self.seen_message_ids.clear();

        // 5. Reset sync flag so SyncMachine drives again.
        self.sync_complete = false;

        // 6. Reset fork detector and stall timer.
        self.fork_detector.reset();
        self.stall_start = None;

        // 7. Reset voted peers tracking.
        self.voted_peers.clear();

        info!("Chain resync initiated. Waiting for sync from peers.");
    }
```

- [ ] **Step 3: Replace the fork detection path in try_apply_finalized**

Find the fork detection block (around line 2624-2648). Replace:

```rust
        if block.header.parent_hash != our_tip_hash {
            // Fork detected — this block extends a different chain.
            warn!("Fork detected at height {}: parent {} != our tip {}",
                height, block.header.parent_hash, our_tip_hash);

            // Attempt reorg: revert our tip and try to apply the fork block.
            let target = height.saturating_sub(1);
            match self.state.revert_to(target) {
                Ok(reverted) => {
                    info!("Reorg: reverted {} blocks to height {}", reverted, target);
                    match self.state.apply_block_validated(&block) {
                        Ok(()) => {
                            info!("Reorg: applied fork block {} at height {}", hash, height);
                            self.print_status();
                        }
                        Err(e) => {
                            warn!("Reorg failed: could not apply fork block: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Reorg failed: could not revert: {}", e);
                }
            }
            return;
        }
```

With:

```rust
        if block.header.parent_hash != our_tip_hash {
            warn!("Fork detected at height {}: parent {} != our tip {}",
                height, block.header.parent_hash, our_tip_hash);

            self.fork_detector.record_mismatch();

            if self.fork_detector.should_resync() {
                self.initiate_chain_resync(&format!(
                    "fork detector triggered after {} consecutive mismatches at height {}",
                    self.fork_detector.consecutive_mismatches(), height
                ));
            }
            return;
        }
```

**Note:** The `consecutive_mismatches()` getter was added in Task 1.

- [ ] **Step 4: Add fork_detector.record_success() on successful block application**

After the successful `apply_block_validated` call in `try_apply_finalized` (around line 2668), add:

```rust
    self.fork_detector.record_success();
```

- [ ] **Step 5: Wire stall timer into handle_consensus_tick**

In `handle_consensus_tick`, replace:
```rust
let _result = self.consensus.try_finalize_round(next_height, peer_count);
```

With:
```rust
use commputer::consensus_manager::ConsensusRoundResult;

let result = self.consensus.try_finalize_round(next_height, peer_count);
match result {
    ConsensusRoundResult::Finalized => {
        // Consensus is working -- reset stall timer.
        // Block application is handled by the existing finalized_heights loop below.
        self.stall_start = None;
    }
    ConsensusRoundResult::Stalled => {
        // Start or check stall timer.
        let stall_start = self.stall_start.get_or_insert_with(std::time::Instant::now);
        if stall_start.elapsed().as_secs() >= 60 {
            self.initiate_chain_resync(&format!(
                "consensus stall for {}s at height {}",
                stall_start.elapsed().as_secs(), next_height
            ));
            return;
        }
    }
    ConsensusRoundResult::NotReady => {
        // Normal in-progress voting -- don't touch stall timer.
    }
}
```

- [ ] **Step 6: Also reset stall_start on successful finalization in try_apply_finalized**

After the successful block application path (near `fork_detector.record_success()`), add:

```rust
    self.stall_start = None;
```

- [ ] **Step 7: Build and verify compilation**

Run: `cd /home/operator/Coin/src && cargo check 2>&1`
Expected: Compiles with no errors

- [ ] **Step 8: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/event_loop.rs src/node/src/fork_detector.rs
git commit -m "feat(node): wire fork detector and stall timer into event loop

Fork detector triggers chain resync after 3 consecutive parent hash
mismatches. Stall timer triggers resync after 60s of consensus timeout.
Both call initiate_chain_resync() which wipes state and re-enters sync."
```

---

### Task 6: Remove emergency production

**Files:**
- Modify: `src/node/src/event_loop.rs`

- [ ] **Step 1: Remove the 30s emergency production bypass**

Find in `handle_block_tick` (around line 2417-2426):

```rust
        if validators.len() >= 2 && seconds_waiting < 30 {
            // Normal leader election: only the elected leader produces.
            // After 30 seconds with no block, any validator can produce (emergency).
            if !commputer::leader::is_valid_leader(next_height, &our_addr, &validators, seconds_waiting) {
                return;
            }
        }
        if seconds_waiting >= 30 {
            warn!("Emergency block production: no block for {}s at height {}", seconds_waiting, next_height);
        }
```

Replace with:

```rust
        if validators.len() >= 2 {
            // Strict leader election: only the elected leader produces.
            // View change fallback handles leader unavailability (6s intervals).
            // If consensus stalls, the stall timer in handle_consensus_tick handles it.
            if !commputer::leader::is_valid_leader(next_height, &our_addr, &validators, seconds_waiting) {
                return;
            }
        }
```

**IMPORTANT:** Keep the 6-second view change bypass (lines 2430-2434) intact. That code:
```rust
        if seconds_waiting < 6
            && (self.consensus.has_active_vote(next_height) || self.consensus.has_height(next_height))
        {
            return;
        }
```
This is the view change mechanism, NOT emergency production. It allows a new leader to produce after 6s if the current leader's block hasn't been seen.

- [ ] **Step 2: Build and verify**

Run: `cd /home/operator/Coin/src && cargo check 2>&1`
Expected: Compiles with no errors

- [ ] **Step 3: Run full test suite**

Run: `cd /home/operator/Coin/src && cargo test 2>&1`
Expected: All existing tests pass

- [ ] **Step 4: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/event_loop.rs
git commit -m "fix(node): remove emergency block production (30s any-validator bypass)

Emergency production caused permanent forks by letting minority nodes
produce blocks unilaterally. Consensus stalls are now handled by the
stall timer which triggers a chain resync instead."
```

---

### Task 7: Build, test, deploy to testnet

- [ ] **Step 1: Full workspace build**

Run: `cd /home/operator/Coin/src && cargo build --release 2>&1`
Expected: Clean build

- [ ] **Step 2: Run full test suite**

Run: `cd /home/operator/Coin/src && cargo test 2>&1`
Expected: All tests pass

- [ ] **Step 3: Deploy to Optiplex**

```bash
scp -i ~/.ssh/id_claude /home/operator/Coin/src/target/release/commputer operator@198.51.100.51:~/commputer-new
ssh -i ~/.ssh/id_claude operator@198.51.100.51 "kill \$(pgrep commputer) 2>/dev/null; sleep 1; mv ~/commputer-new ~/commputer-bin && chmod +x ~/commputer-bin"
ssh -i ~/.ssh/id_claude operator@198.51.100.51 "rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test nohup ~/commputer-bin run --port 9002 > /tmp/commputer.log 2>&1 &"
```

- [ ] **Step 4: Deploy to Solarplexus**

```bash
scp -i ~/.ssh/id_claude /home/operator/Coin/src/target/release/commputer operator@198.51.100.11:~/commputer-new
ssh -i ~/.ssh/id_claude operator@198.51.100.11 "kill \$(pgrep commputer) 2>/dev/null; sleep 1; mv ~/commputer-new ~/commputer-bin && chmod +x ~/commputer-bin"
ssh -i ~/.ssh/id_claude operator@198.51.100.11 "rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test nohup ~/commputer-bin run --port 9003 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer.log 2>&1 &"
```

- [ ] **Step 5: Start laptop node**

```bash
rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test /home/operator/Coin/src/target/release/commputer run --port 9001 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer-laptop.log 2>&1 &
```

- [ ] **Step 6: Monitor for 2 minutes**

```bash
# Check all 3 nodes are producing and in sync
ssh -i ~/.ssh/id_claude operator@198.51.100.51 'tail -5 /tmp/commputer.log'
ssh -i ~/.ssh/id_claude operator@198.51.100.11 'tail -5 /tmp/commputer.log'
tail -5 /tmp/commputer-laptop.log
```

Verify: all 3 nodes at same height (within 2-3 blocks), no fork detections, no force-finalize messages.

- [ ] **Step 7: Test fork recovery**

Kill the laptop node, wait 60 seconds (let the other two advance), restart. Verify the laptop resyncs cleanly without forking:

```bash
kill $(pgrep -f "port 9001") 2>/dev/null
sleep 60
rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test /home/operator/Coin/src/target/release/commputer run --port 9001 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer-laptop.log 2>&1 &
sleep 30
tail -20 /tmp/commputer-laptop.log
```

Expected: Laptop syncs to current height, no "Fork detected" spam, no "force-finalizing" messages.

---

## Verification Checklist

1. `cargo test` -- all tests pass
2. `cargo check` -- clean build, no warnings
3. No `force-finalizing` in any node logs
4. No `Emergency block production` in any node logs
5. Fork recovery works: kill a node, restart, it resyncs
6. 3-node testnet runs stable for 5+ minutes
7. `ForkDetector` has 4 unit tests
8. `NodeStateMachine` has 7 unit tests (6 existing + 1 new)
9. `ConsensusManager` has 2+ new tests
