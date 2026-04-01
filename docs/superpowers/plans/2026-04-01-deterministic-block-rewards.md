# Deterministic Block Rewards Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move coin emission from out-of-band epoch transitions into deterministic per-block rewards credited during block application, fixing sync state divergence.

**Architecture:** Add a `credit_block_reward` method to `ChainState` that credits `min(block_reward(height), remaining_supply())` to `block.header.producer` before transactions. Call it from both `apply_block` and `apply_block_validated`. Remove balance mutations from epoch transitions. The `MiningReward` transaction type becomes a no-op (leave for backwards compat, remove synthetic creation).

**Tech Stack:** Rust, existing crates (commputer-storage, commputer-consensus, commputer-node). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-01-deterministic-block-rewards-design.md`

---

## Context

The `commputer-storage` crate is at `src/storage/src/`. `ChainState` in `state.rs` has two block application methods:
- `apply_block` (line 367) -- used in tests and genesis application, has StateDiff capture
- `apply_block_validated` (line 448) -- used by the live event loop, no StateDiff

The `commputer-consensus` crate has `EmissionSchedule::block_reward(height)` in `src/consensus/src/emission.rs`.

The `commputer-node` crate has the epoch transition in `src/node/src/event_loop.rs` (lines 2147-2228) that directly mutates account balances.

Key constants:
- `INITIAL_BLOCK_REWARD: u64 = 1_585_489_599` (~15.855 COMME)
- `HALVING_INTERVAL: u64 = 63_072_000`
- `TOTAL_SUPPLY: u64 = 200_000_000_000_000_000` (2B COMME in raw units)
- `MINIMUM_VALIDATOR_STAKE: u64 = 10_000_000`

## File Structure

```
src/storage/src/
├── state.rs         -- MODIFY: add credit_block_reward(), call from apply_block + apply_block_validated
src/node/src/
├── event_loop.rs    -- MODIFY: remove balance mutations from epoch transition
src/consensus/src/
├── emission.rs      -- READ ONLY: block_reward() already exists
```

---

### Task 1: Add credit_block_reward to ChainState

**Files:**
- Modify: `src/storage/src/state.rs`

- [ ] **Step 1: Write failing tests**

Add to the existing `mod tests` in `state.rs`. The tests will reference `credit_block_reward` which doesn't exist yet.

```rust
    #[test]
    fn block_reward_credited_to_producer() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        // Create a block at height 1 with a known producer.
        let producer = Address([1u8; 32]);
        let block = Block {
            header: BlockHeader {
                height: 1,
                parent_hash: genesis.hash(),
                producer,
                timestamp: 1000,
                ..Default::default()
            },
            transactions: vec![],
        };
        state.apply_block(&block).unwrap();

        // Producer should have received the block reward.
        let reward = commputer_consensus::emission::EmissionSchedule::new().block_reward(1);
        let account = state.accounts.get(&producer).expect("producer account should exist");
        assert!(account.balance.raw() >= reward, "producer should have at least the block reward");
        assert!(state.total_emitted >= reward, "total_emitted should include block reward");
    }

    #[test]
    fn no_reward_at_genesis() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        // Genesis block producer should NOT receive a reward.
        assert_eq!(state.total_emitted, 0, "no emission at genesis");
    }
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-storage --lib state::tests::block_reward_credited_to_producer 2>&1`
Expected: FAIL (reward not credited)

- [ ] **Step 3: Implement credit_block_reward**

Add to `impl ChainState` in `state.rs`, before `apply_block`:

```rust
    /// Credit the per-block reward to the block producer.
    /// Called during block application, BEFORE transactions.
    /// Skips genesis block (height 0). Caps reward to remaining supply.
    fn credit_block_reward(&mut self, block: &Block) {
        // No reward at genesis.
        if block.height() == 0 {
            return;
        }

        // Compute reward from halving schedule, capped to remaining supply.
        let schedule = commputer_consensus::emission::EmissionSchedule::new();
        let reward = schedule.block_reward(block.height());
        let remaining = self.remaining_supply();
        let actual_reward = reward.min(remaining);

        if actual_reward == 0 {
            return;
        }

        // Credit producer.
        let producer = block.header.producer;
        let account = self.accounts.get_or_create(producer);
        if let Some(new_balance) = account.balance.checked_add(
            commputer_core::token::Amount::from_raw(actual_reward),
        ) {
            account.balance = new_balance;
            // Track per-account mining total.
            account.total_mined = account.total_mined
                .checked_add(commputer_core::token::Amount::from_raw(actual_reward))
                .unwrap_or(account.total_mined);
        }

        // Update global emission counter.
        self.total_emitted = self.total_emitted.saturating_add(actual_reward);
    }
```

- [ ] **Step 4: Call credit_block_reward from apply_block**

In `apply_block` (line 367), add the call BEFORE the `before_states` capture (before line 380):

```rust
        // Credit per-block reward to producer before transactions.
        self.credit_block_reward(block);
```

Also add the producer to `before_states` for StateDiff capture. After the `credit_block_reward` call and before the transaction loop, insert:

```rust
        // Capture producer before-state for StateDiff (reward may have changed balance).
        if block.height() > 0 {
            let producer = block.header.producer;
            if let std::collections::hash_map::Entry::Vacant(e) = before_states.entry(producer) {
                // Producer was just created/credited -- capture the pre-reward state.
                // Since we already applied the reward, we need to compute what balance was before.
                let schedule = commputer_consensus::emission::EmissionSchedule::new();
                let reward = schedule.block_reward(block.height()).min(self.remaining_supply().saturating_add(
                    schedule.block_reward(block.height()).min(self.remaining_supply())
                ));
                // Actually, simpler: capture before_states BEFORE calling credit_block_reward.
            }
        }
```

**Wait -- this is getting complex. Simpler approach:** Move the `before_states` capture BEFORE `credit_block_reward`, and add the producer address to the list of addresses to track:

In `apply_block`, restructure the StateDiff section:

1. Capture producer's before-state
2. Call `credit_block_reward`
3. Capture transaction-involved before-states
4. Apply transactions
5. Build diff from all before-states

The cleanest implementation: collect ALL affected addresses (producer + transaction participants) before ANY state changes, capture their before-states, then apply reward + transactions, then diff.

- [ ] **Step 5: Call credit_block_reward from apply_block_validated**

In `apply_block_validated` (line 448), add the call BEFORE the transaction loop (before line 495):

```rust
        // Credit per-block reward to producer before transactions.
        self.credit_block_reward(block);
```

This is simpler since `apply_block_validated` doesn't have StateDiff.

- [ ] **Step 6: Run tests, verify they pass**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-storage --lib state::tests 2>&1`
Expected: All tests pass including new ones

- [ ] **Step 7: Commit**

```bash
cd /home/operator/Coin && git add src/storage/src/state.rs
git commit -m "feat(storage): add deterministic per-block reward in apply_block

Credit block_reward(height) to block.header.producer before transactions.
Skips genesis. Capped to remaining_supply(). Updates total_emitted and
per-account total_mined."
```

---

### Task 2: Remove out-of-band emission from epoch transitions

**Files:**
- Modify: `src/node/src/event_loop.rs`

- [ ] **Step 1: Read the epoch transition code**

Read `src/node/src/event_loop.rs` around lines 2147-2240. Understand the full reward distribution block.

- [ ] **Step 2: Remove balance mutations**

In the epoch transition handler, remove/comment out:
- The `if actual_emission > 0 { ... }` block that distributes rewards (lines 2147-2240ish)
- Specifically remove: `account.balance = new_balance` (line 2189)
- Remove: `account.total_mined` updates (lines 2190-2195)
- Remove: `self.state.emit(distributed)` (line 2228)
- Remove: synthetic `MiningReward` transaction creation (lines 2198-2214)
- Remove: `self.pending_txs.push(reward_tx)` (line 2214)

**Keep:**
- The `ChannelAllocation::from_demand` call (or remove if nothing uses the result)
- The epoch complete logging (lines 2220-2226) -- adjust to log "Epoch N complete (bookkeeping only, rewards via block production)"
- Compliance checks and logging
- Everything after the reward distribution (remaining supply warnings, etc.)

- [ ] **Step 3: Simplify the epoch handler**

Replace the removed reward block with a simple log:

```rust
        // Rewards are now credited per-block during apply_block_validated.
        // Epoch transitions handle bookkeeping only: scoring, compliance, difficulty.
        info!(
            "Epoch {} complete: {} validators (rewards via per-block production)",
            epoch, validator_count,
        );
```

- [ ] **Step 4: Build and verify**

Run: `cd /home/operator/Coin/src && cargo check 2>&1`
Expected: Compiles (may have unused variable warnings from removed code)

- [ ] **Step 5: Run tests**

Run: `cd /home/operator/Coin/src && cargo test --lib 2>&1`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
cd /home/operator/Coin && git add src/node/src/event_loop.rs
git commit -m "refactor(node): remove out-of-band emission from epoch transitions

Epoch transitions no longer mutate account balances. Rewards are now
credited per-block during apply_block_validated. Epochs remain for
bookkeeping: scoring, compliance, difficulty adjustments."
```

---

### Task 3: Fix StateDiff to capture block reward

**Files:**
- Modify: `src/storage/src/state.rs`

The `apply_block` method captures before/after states for StateDiff, but the producer's balance change from `credit_block_reward` must be included. The current code only captures before-states for transaction participants.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn block_reward_in_state_diff() {
        let mut state = ChainState::new();
        let genesis = genesis_block();
        state.apply_block(&genesis).unwrap();

        let producer = Address([1u8; 32]);
        let block = Block {
            header: BlockHeader {
                height: 1,
                parent_hash: genesis.hash(),
                producer,
                timestamp: 1000,
                ..Default::default()
            },
            transactions: vec![],
        };
        state.apply_block(&block).unwrap();

        // The StateDiff for height 1 should include the producer's reward.
        let diff = state.state_diffs.get(&1).expect("should have diff for height 1");
        assert!(diff.changes.contains_key(&producer), "diff should include producer");
        let change = &diff.changes[&producer];
        assert_eq!(change.old_balance, 0);
        assert!(change.new_balance > 0, "new balance should include reward");
    }
```

- [ ] **Step 2: Fix apply_block to capture producer before-state**

In `apply_block`, restructure the StateDiff capture. The key change: capture the producer's before-state BEFORE calling `credit_block_reward`.

Current order:
1. Capture before-states for transactions
2. Apply transactions
3. Build diff

New order:
1. Capture producer's before-state (if height > 0)
2. Call `credit_block_reward`
3. Capture before-states for transaction participants (skip producer if already captured)
4. Apply transactions
5. Build diff from all before-states

Move the `before_states` HashMap creation and producer capture BEFORE `credit_block_reward`:

```rust
        // Feature 181: Capture before-state for StateDiff.
        let mut before_states: HashMap<Address, (u64, u64)> = HashMap::new();

        // Capture producer before-state (reward will change their balance).
        if block.height() > 0 {
            let producer = block.header.producer;
            let (bal, nonce) = self.accounts.get(&producer)
                .map(|a| (a.balance.raw(), a.nonce))
                .unwrap_or((0, 0));
            before_states.insert(producer, (bal, nonce));
        }

        // Credit per-block reward to producer before transactions.
        self.credit_block_reward(block);

        // Capture before-states for transaction participants.
        for tx in &block.transactions {
            if let std::collections::hash_map::Entry::Vacant(e) = before_states.entry(tx.from) {
                // ... existing code
            }
            // ... existing recipient capture
        }
```

- [ ] **Step 3: Run tests, verify pass**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-storage --lib state::tests 2>&1`
Expected: All pass including new StateDiff test

- [ ] **Step 4: Commit**

```bash
cd /home/operator/Coin && git add src/storage/src/state.rs
git commit -m "fix(storage): capture block reward in StateDiff for revert support"
```

---

### Task 4: Build, test, deploy to testnet

- [ ] **Step 1: Full workspace build**

Run: `cd /home/operator/Coin/src && cargo build --release 2>&1`
Expected: Clean build

- [ ] **Step 2: Run full test suite**

Run: `cd /home/operator/Coin/src && cargo test 2>&1`
Expected: All tests pass

- [ ] **Step 3: Deploy Optiplex (seed, no --seeds)**

```bash
scp -i ~/.ssh/id_claude /home/operator/Coin/src/target/release/commputer operator@198.51.100.51:~/commputer-new
ssh -i ~/.ssh/id_claude operator@198.51.100.51 "pkill -9 commputer 2>/dev/null; sleep 2; mv ~/commputer-new ~/commputer-bin; chmod +x ~/commputer-bin; rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test nohup ~/commputer-bin run --port 9002 > /tmp/commputer.log 2>&1 &"
```

Wait 50 seconds for bootstrap, verify blocks producing:
```bash
ssh -i ~/.ssh/id_claude operator@198.51.100.51 'grep "Finalized" /tmp/commputer.log | tail -5'
```

- [ ] **Step 4: Deploy Solarplexus and Laptop**

```bash
scp -i ~/.ssh/id_claude /home/operator/Coin/src/target/release/commputer operator@198.51.100.11:~/commputer-new
ssh -i ~/.ssh/id_claude operator@198.51.100.11 "pkill -9 commputer 2>/dev/null; sleep 2; mv ~/commputer-new ~/commputer-bin; chmod +x ~/commputer-bin; rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test nohup ~/commputer-bin run --port 9003 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer.log 2>&1 &"

rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test /home/operator/Coin/src/target/release/commputer run --port 9001 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer-laptop.log 2>&1 &
```

- [ ] **Step 5: Verify sync works**

Wait 60 seconds, then check all 3 nodes:
```bash
ssh -i ~/.ssh/id_claude operator@198.51.100.51 'tail -5 /tmp/commputer.log'
ssh -i ~/.ssh/id_claude operator@198.51.100.11 'tail -5 /tmp/commputer.log'
tail -5 /tmp/commputer-laptop.log
```

**Critical verification:** No "insufficient balance for validator registration" errors on Solarplexus or Laptop. All 3 nodes at similar heights with Snowball consensus working.

- [ ] **Step 6: Test fork recovery**

Kill laptop, wait 60s, restart, verify clean resync:
```bash
kill $(pgrep -f "port 9001") 2>/dev/null
sleep 60
rm -rf ~/.commputer && COMMPUTER_WALLET_PASSWORD=test /home/operator/Coin/src/target/release/commputer run --port 9001 --seeds /ip4/198.51.100.51/tcp/9002 > /tmp/commputer-laptop.log 2>&1 &
sleep 30
tail -20 /tmp/commputer-laptop.log
```

Expected: Laptop syncs to current height, no fork errors.

---

## Verification Checklist

1. `cargo test` -- all tests pass
2. `cargo check` -- clean build
3. Block producer gets ~15.855 COMME per block (visible in logs)
4. No "insufficient balance" errors during sync
5. Epoch transitions log "bookkeeping only" -- no balance changes
6. 3-node testnet runs stable
7. `total_emitted` increments per block (visible in chain status logs)
8. Fork recovery still works (kill node, restart, resync)
9. No equivocation errors
10. StateDiff captures block reward (revert_block works correctly)
