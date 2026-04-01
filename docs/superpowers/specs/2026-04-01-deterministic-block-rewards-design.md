# Deterministic Block Rewards Design

**Date:** 2026-04-01
**Status:** Draft
**Scope:** Move coin emission from out-of-band epoch transitions into deterministic per-block rewards

---

## Problem

Coin emission currently happens as a side effect of epoch transitions. The event loop directly mutates account balances (`account.balance.checked_add(reward)`) outside of block application. This creates a fatal sync problem: when Node B downloads blocks from Node A, the blocks contain transactions that reference balances created by Node A's local epoch transition. Node B hasn't run the same epoch transition, so the balances don't exist and block validation fails.

Evidence from testnet: Solarplexus and Laptop failed to sync block 1 from Optiplex with "insufficient balance for validator registration: need 10000000 raw, have 0" because Optiplex's 28,538 COMME balance was created by a local epoch transition, not by any block.

### Why This Violates Blockchain Fundamentals

Every top L1 follows the same rule: **all state changes must be derivable from the blocks themselves.** If you can't replay every block from genesis and arrive at the same state, the chain is broken.

- **Bitcoin:** Block rewards are coinbase transactions IN the block. Every node that applies the block creates the coins deterministically.
- **Ethereum:** Validator rewards are protocol-level state transitions applied deterministically during block processing.
- **Solana:** Staking rewards are calculated deterministically from epoch state embedded in the chain.

---

## Design

### 1. Deterministic Block Reward

During `apply_block_validated`, credit `block_reward(height)` to `block.header.producer`. This happens BEFORE transactions are applied within the same block.

```
apply_block_validated(block):
  1. Validate block (parent hash, height, timestamp, etc.)
  2. Compute reward = block_reward(block.height)     // from halving schedule
  3. Credit reward to block.header.producer           // create account if needed
  4. Increment total_emitted by reward
  5. Apply all transactions in order                  // existing logic
  6. Store block, update chain tip
```

The reward amount is deterministic -- any node applying the same block at the same height computes the same reward from the halving schedule (`INITIAL_BLOCK_REWARD >> (height / HALVING_INTERVAL)`) and credits the same producer address from `block.header.producer`.

No new transaction type. No new block fields. The reward is implicit.

**Ordering matters:** Reward is credited BEFORE transactions. This ensures the block producer has balance for any transactions in the same block (e.g., ValidatorRegister).

**Implementation location:** `src/storage/src/state.rs` in `apply_block_validated`. Add reward logic before the transaction application loop.

**Halving schedule (unchanged):**
- `INITIAL_BLOCK_REWARD: u64 = 1_585_489_599` (~15.855 COMME/block)
- `HALVING_INTERVAL: u64 = 63_072_000` (~4 years at 2s blocks)
- `block_reward(height) = INITIAL_BLOCK_REWARD >> (height / HALVING_INTERVAL)`

### 2. Remove Out-of-Band Emission

The epoch transition in `event_loop.rs` currently:
1. Calculates per-validator rewards based on composite resource scores
2. Directly mutates account balances via `state.accounts.get_or_create().balance.checked_add()`
3. Increments `state.total_emitted`

**Remove:** All direct balance mutations from epoch transitions (step 2 and 3 above).

**Keep:** Everything that doesn't create coins:
- Proof-of-contribution scoring (composite resource scores)
- Compliance status updates (nerfing)
- Difficulty adjustments per channel
- Validator performance tracking
- Active validator set management
- Epoch summary logging

Epoch transitions become pure bookkeeping events. No state-mutating side effects. Coins enter circulation through exactly one path: block rewards in `apply_block_validated`.

**Files affected:**
- `src/node/src/event_loop.rs` -- remove balance mutation from `handle_epoch_transition` (around lines 2147-2230)
- `src/storage/src/state.rs` -- add block reward logic to `apply_block_validated`

### 3. Bootstrap Problem Resolution

The validator bootstrap problem ("need balance to register, need to produce to get balance") solves itself:

1. Bootstrap leader (seed node, no `--seeds`) produces block 1
2. `apply_block_validated` credits 15.855 COMME to the producer FIRST
3. Block 1 also contains the producer's ValidatorRegister transaction
4. ValidatorRegister passes the stake check because the producer now has balance
5. When other nodes sync block 1, they apply the same reward + same transaction = same state

No genesis allocations. No pre-mine. No special cases. The protocol itself bootstraps through its normal reward mechanism.

### 4. Emission Tracking

Currently `total_emitted` is incremented during epoch transitions. Move this to `apply_block_validated`:

```rust
// In apply_block_validated, after crediting reward:
self.total_emitted += reward;
```

This ensures `total_emitted` is part of deterministic block state, not a local side effect.

The `remaining_supply()` calculation (`TOTAL_SUPPLY - total_emitted`) continues to work unchanged.

---

## What This Does NOT Change

- Halving schedule (same formula, same intervals)
- Total supply cap (2B COMME)
- Fee burning (100% of fees burned, unchanged)
- Round-robin leader election (unchanged)
- Snowball consensus (unchanged)
- Fork recovery (unchanged)
- Block validation rules (unchanged, except adding reward credit)
- Proof-of-contribution scoring (still happens at epochs, just doesn't create coins)
- Compliance/nerfing (still tracked, affects epoch scores not block rewards)

## What This Changes

- Block rewards are deterministic and per-block (not batched at epochs)
- Every validator gets equal reward per block they produce (round-robin ensures equal turns)
- Epoch transitions no longer create coins
- `total_emitted` increments per-block, not per-epoch
- Validator registration works in the same block as the producer's first reward

---

## Edge Cases

**Block reward at supply cap:** When `block_reward(height)` returns 0 (all halvings exhausted) or remaining supply is 0, no reward is credited. The chain continues with transaction fees only (which are burned).

**Block produced by non-registered validator (bootstrap):** The producer address is credited regardless of validator status. They become a validator via ValidatorRegister in the same block.

**Syncing nodes:** Each synced block deterministically credits the reward to the producer. State is identical across all nodes that apply the same blocks.

**Epoch transition during sync:** Epochs still run for bookkeeping (scoring, compliance), but since they don't mutate balances, there's no state divergence.

**Existing testnet state:** This is a breaking change to block application logic. The testnet must be restarted from genesis.

---

## Testing Strategy

### Unit tests (state.rs)
- `test_block_reward_credited_to_producer` -- apply block, verify producer balance increased by `block_reward(height)`
- `test_reward_before_transactions` -- block with ValidatorRegister from producer succeeds (reward credited first)
- `test_total_emitted_incremented` -- after N blocks, `total_emitted` equals sum of rewards
- `test_no_reward_at_supply_cap` -- when remaining supply is 0, no reward credited
- `test_halving_affects_reward` -- blocks at different eras get different rewards

### Integration tests
- `test_sync_applies_same_rewards` -- two ChainStates apply same blocks, end up with identical state
- `test_epoch_no_balance_mutation` -- run epoch transition, verify no account balance changes

### Manual testnet
- Start 3-node testnet, verify all nodes produce blocks and sync
- Kill one node, restart, verify it syncs and state matches
- Verify block explorer shows consistent balances across nodes
