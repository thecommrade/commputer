# Fork Recovery and Consensus Safety Design

**Date:** 2026-04-01
**Status:** Draft
**Scope:** Remove force-finalize, add fork detection, implement chain wipe-and-resync

---

## Problem

A node that falls behind in consensus force-finalizes its own block by fabricating Snowball votes. This creates a permanent fork: every subsequent network block fails the parent hash check, the reorg logic only reverts 1 block (useless when the fork point is thousands of blocks back), and the node loops on fork-detect/force-finalize forever.

Evidence from the 3-node testnet: the laptop forked at height 149 and accumulated 11,587 fork detections and 23,174 force-finalizations over 11,500+ blocks while Optiplex and Solarplexus ran correctly in lockstep.

### Root Cause Chain

1. Consensus times out at a height (node is slow, network latency, clock skew)
2. `ConsensusManager::try_finalize_round` fabricates `decision_threshold` rounds of fake votes for the local preference
3. The local preference differs from the network's agreed block
4. Node's chain tip diverges from the network
5. Every subsequent network block has a parent hash mismatch
6. Reorg attempts `revert_to(height - 1)` which only reverts 1 block -- fork point is far behind
7. Node force-finalizes its own block again at the next height
8. Permanent split-brain: node runs on a solo fork indefinitely

### Why This Violates Snowball Consensus

In Avalanche's Snowball protocol, finalization requires real convergence through repeated peer sampling. If consensus doesn't converge, the height is not finalized and the node waits. Avalanche nodes that fall behind enter a bootstrapping state and re-download the correct chain from peers. There is no force-finalize in the protocol.

---

## Design

Four changes that work together:

### 1. Fork Detection Circuit Breaker

A new `ForkDetector` struct tracks consecutive parent hash mismatches during block finalization.

**Interface:**

```rust
pub struct ForkDetector {
    consecutive_mismatches: u32,
    threshold: u32, // default: 3
}

impl ForkDetector {
    pub fn new() -> Self;
    pub fn record_mismatch(&mut self);
    pub fn record_success(&mut self); // resets counter to 0
    pub fn should_resync(&self) -> bool; // counter >= threshold
    pub fn reset(&mut self);
}
```

**Integration points:**
- `event_loop.rs`: In the fork detection path (currently line 2624), call `fork_detector.record_mismatch()` instead of attempting the shallow reorg
- On successful block application, call `fork_detector.record_success()`
- After each mismatch, check `fork_detector.should_resync()` and trigger resync if true
- When resync is triggered, call `node_state.force_syncing()` (new method, analogous to existing `force_active()`). The `NodeStateMachine` remains the single authority on node state transitions.

**Threshold: 3 consecutive mismatches.** One mismatch could be a race condition. Three is a pattern.

**File:** `src/node/src/fork_detector.rs`

### 2. Remove Force-Finalize

Replace the vote fabrication in `ConsensusManager::try_finalize_round` with a stall signal.

**Current behavior (lines 336-352 of consensus_manager.rs):**

```rust
// REMOVED: fabricates votes
let mut responses = HashMap::new();
responses.insert(hash, self.params.sample_size);
for _ in 0..self.params.decision_threshold {
    state.voter.record_round(&responses);
}
```

**New behavior:**

Change the return type from `bool` to a three-state enum:

```rust
pub enum ConsensusRoundResult {
    /// Voting is still in progress, not yet converged.
    NotReady,
    /// Snowball voting converged -- block is finalized.
    Finalized,
    /// Consensus timed out without convergence -- node should consider resyncing.
    Stalled,
}
```

When the timeout fires, return `Stalled` instead of fabricating votes. The event loop uses this signal to start a stall countdown.

**Stall ceiling: 60 seconds.** If no height finalizes within 60 seconds of the first `Stalled` signal, the node triggers a chain wipe and resync.

**Stall timer semantics:**
- `Finalized` -- resets the stall timer (consensus is working)
- `NotReady` -- does not advance or reset the timer (voting is in progress, not stalled)
- `Stalled` -- starts or advances the timer (consensus has timed out at this height)

This prevents false resyncs during slow-but-progressing voting rounds.

**File:** Modified `src/node/src/consensus_manager.rs`

### 3. Chain Wipe and Resync

When either trigger fires (3 fork mismatches OR 60s consensus stall), the event loop executes:

1. Log warning: "Fork/stall detected, initiating chain resync from peers"
2. Transition `node_state` to `Syncing` via `node_state.force_syncing()`
3. Call `self.state.reset_to_genesis()` -- clear all blocks and account state, reinitialize to genesis (height 0)
4. Call `self.consensus.clear()` -- wipe all consensus state for all heights
5. Clear `pending_txs` mempool (transactions will be re-received from peers after resync)
6. Set `self.sync_complete = false` so the SyncMachine is driven again
7. Reset `fork_detector` and stall timer
8. The existing `SyncMachine` takes over and downloads blocks from connected peers

**What is preserved:**
- Wallet identity and validator keypair
- Peer connections (libp2p swarm remains connected)
- Network-level state (peer addresses, connected peer set)
- Node configuration

**What is wiped:**
- Block storage (all blocks except genesis)
- Account state (balances, nonces, validator registry)
- State diffs, receipts, and account history index
- Consensus state (all in-flight votes)
- Transaction mempool (`pending_txs`)
- Fork detector and stall timer state
- `sync_complete` flag (set to false)

**New method required:**

```rust
// In src/storage/src/state.rs
impl ChainState {
    /// Wipe all blocks and account state, reinitialize to genesis.
    /// Preserves nothing -- caller must re-download from peers.
    ///
    /// RocksDB strategy: clear all column families (blocks, accounts,
    /// receipts, state_diffs, account_history_index), then replay
    /// apply_genesis() to reinitialize height 0 with genesis state.
    /// For in-memory storage, simply reset all HashMaps and re-apply genesis.
    pub fn reset_to_genesis(&mut self) -> Result<(), StateError>;
}
```

**New method required:**

```rust
// In src/node/src/consensus_manager.rs
impl ConsensusManager {
    /// Clear all consensus state. Used during chain resync.
    pub fn clear(&mut self);
}
```

**Files:** Modified `src/node/src/event_loop.rs`, `src/storage/src/state.rs`, `src/node/src/consensus_manager.rs`

### 4. Remove Emergency Production

Remove the two bypass paths in `handle_block_tick` that allow non-leaders to produce blocks:

**Remove (event_loop.rs line 2417-2426):**
- The `seconds_waiting >= 30` path that lets any validator produce (emergency production)

**Keep:**
- Round-robin leader election via `is_valid_leader()` with view change fallback every 6s
- The `seconds_waiting >= 6` bypass of `has_active_vote` -- this IS the view change mechanism (allows a new leader to produce when the current leader's block hasn't been seen). Removing it would break leader rotation recovery.
- The `validators.len() < 2` bootstrap bypass (needed for initial network formation before validator set is established)

If a node isn't the leader and consensus is stalled, the 60s stall detector (Section 2) handles it. The node doesn't try to produce its way out of the problem.

**File:** Modified `src/node/src/event_loop.rs`

---

## State Transitions

```
Normal operation:
  Active -> block produced -> consensus votes -> Finalized -> apply block -> Active

Fork detected:
  Active -> parent hash mismatch (x3) -> Syncing -> wipe chain -> sync from peers -> Active

Consensus stall:
  Active -> Stalled signal -> 60s countdown -> Syncing -> wipe chain -> sync from peers -> Active

Successful resync:
  Syncing -> download blocks from peers -> caught up to network -> Active
```

---

### NodeStateMachine Changes

Add a `force_syncing()` method to `NodeStateMachine` (in `src/node/src/node_state.rs`), analogous to the existing `force_active()`. This keeps `NodeStateMachine` as the single authority on node state transitions. Both the `ForkDetector` trigger and the stall timer trigger call `node_state.force_syncing()` rather than setting state directly.

```rust
/// Force the node into Syncing state (used during chain resync).
pub fn force_syncing(&mut self) {
    self.state = NodeState::Syncing;
    self.our_height = 0;
}
```

---

## What This Does NOT Change

- Snowball voting algorithm (untouched)
- Leader election logic (round-robin with view change, untouched)
- Sync machine (already handles batch downloads, untouched)
- Network transport (consensus request-response protocol, untouched)
- Rate limiter, eclipse detector (untouched)
- Block validation rules (untouched)
- Emission, fees, tokenomics (untouched)

---

## Edge Cases

**Solo node (0 peers):** Chain does not advance. This is correct -- a blockchain with one node isn't a network. The node waits in Active state until a peer connects.

**Bootstrap (< 2 validators):** The `validators.len() < 2` bypass still allows block production during initial network formation. Once 2+ validators exist, strict leader election takes over.

**Network partition heals:** When a partitioned node reconnects, it will receive blocks from the majority chain. If those blocks have mismatching parents (node was on wrong fork), the fork detector triggers after 3 mismatches, the node resyncs, and rejoins the correct chain.

**All nodes stall simultaneously:** If the entire network stalls (e.g., all nodes lose connectivity to each other), every node hits the 60s ceiling and enters sync mode. When connectivity returns, nodes discover peers, attempt to sync, find they're all at the same height, and the SyncMachine transitions through Downloading -> Verifying -> Complete (since `our_height >= target_height`). The node returns to Active. The next block tick (every 2s) fires, the elected leader produces a block, and consensus resumes normally. There is a brief window where all nodes are Active but waiting for their leader rotation slot -- this resolves within one block tick cycle.

**Rapid height changes during resync:** The sync machine downloads in batches of 10 with backpressure. While resyncing, `node_state` is `Syncing`, which prevents block production and consensus participation. The node silently catches up.

---

## Testing Strategy

### Unit tests (fork_detector.rs)
- `test_single_mismatch_no_resync` -- 1 mismatch, should_resync is false
- `test_threshold_triggers_resync` -- 3 mismatches, should_resync is true
- `test_success_resets_counter` -- 2 mismatches then success, counter resets
- `test_reset_clears_state` -- reset after mismatches, counter is 0

### Unit tests (consensus_manager.rs)
- `test_timeout_returns_stalled` -- verify Stalled is returned instead of force-finalizing
- `test_real_votes_still_finalize` -- normal Snowball path still works
- `test_clear_wipes_all_heights` -- verify clear() removes everything

### Integration tests (event_loop context)
- `test_fork_triggers_resync` -- simulate 3 parent hash mismatches, verify node enters Syncing
- `test_stall_triggers_resync` -- simulate 60s with no finalization, verify resync
- `test_resync_preserves_wallet` -- after wipe, wallet identity is intact
- `test_no_emergency_production` -- remove 30s bypass, verify non-leader doesn't produce

### Manual testnet validation
- Run 3-node testnet, kill one node for 2 minutes, restart, verify it resyncs correctly
- Run 3-node testnet, partition one node (block network), reconnect, verify recovery
- Run single node, verify it produces blocks with < 2 validators, stops producing when 2+ validators exist but it's not the leader
