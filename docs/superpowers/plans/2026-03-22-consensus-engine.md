# Consensus Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the consensus engine that produces blocks, validates them, manages epochs, and selects block producers — the core that makes the Commputer chain functional.

**Architecture:** This is Phase 1 of Snowstorm — a CRS-weighted block producer selection with round-robin fallback, block validation, and epoch management. Full Snowball finality and DAG structure come in Phase 2 (separate plan). The engine ties together the state layer (StateManager) and proof channel (CpuProofEngine) into a working chain that can produce and validate blocks.

**Tech Stack:** Rust, blake3, rand, existing types from commputer-core (Block, Transaction, ProofChallenge, ProofResult), StateManager, CpuProofEngine.

---

## Context

We have:
- `commputer-core` — 55 tests, all types + state management (StateManager, InMemoryStore, accounts, emission, validators, tx pool, grace, state root hashing, block application)
- `commputer-proofs` — 14 tests, CPU proof channel (generate, execute, verify) + dispatcher
- `commputer-consensus` — scaffolded, empty (engine.rs, epoch.rs, producer.rs, validation.rs)

The consensus engine orchestrates everything: it picks who produces the next block, assembles block contents (pending txs + verified proofs), validates incoming blocks from peers, and manages epoch transitions for emission recalculation.

## Scope — Phase 1 (this plan)

- Composite Resource Score (CRS) calculation
- Block producer selection (CRS-weighted, deterministic per round)
- Block assembly (pull from tx pool + aggregate proof results)
- Block validation (verify transactions, proofs, state transitions, signatures)
- Epoch management (recalculate emission rates per epoch boundary)
- Single-node chain operation (produce + validate own blocks, foundation for networking)

## NOT in scope (Phase 2)

- Snowball finality voting
- DAG structure with cross-channel references
- VRF-based leader election
- Multi-node consensus (requires networking)
- Per-channel difficulty adjustment

## File Structure

```
commputer-consensus/src/
├── lib.rs          -- Re-exports, ConsensusConfig
├── engine.rs       -- ConsensusEngine: produce_block, validate_block, main loop
├── producer.rs     -- CRS calculation, block producer selection
├── validation.rs   -- Block validation rules
├── epoch.rs        -- Epoch boundary detection, emission recalculation
```

## Critical Files (existing, to reference)

- `commputer-core/src/state/mod.rs` — StateManager (apply_block, apply_transaction, get_balance, compute_state_root)
- `commputer-core/src/block.rs` — Block, BlockHeader, GenesisConfig
- `commputer-core/src/identity.rs` — ValidatorIdentity, ChannelScores, diversity_multiplier()
- `commputer-core/src/tokenomics.rs` — hybrid_emission_rate(), HolderTier
- `commputer-core/src/proof.rs` — ProofChallenge, ProofResult, VerifiedProofResult
- `commputer-proofs/src/cpu.rs` — CpuProofEngine (generate, execute, verify)
- `commputer-proofs/src/verifier.rs` — verify_proof() dispatcher

---

### Task 1: Composite Resource Score (CRS)

**Files:**
- Modify: `commputer-consensus/src/producer.rs`

- [ ] **Step 1: Write failing tests**
  - `test_crs_single_channel` — validator with only CPU score gets CRS proportional to CPU
  - `test_crs_multi_channel` — validator active on all 5 channels gets higher CRS than single-channel
  - `test_crs_sublinear_scaling` — doubling a score does NOT double CRS (R^0.7 diminishing returns)
  - `test_crs_diversity_bonus` — 5-channel validator gets multiplier vs 1-channel
  - `test_crs_zero_scores` — validator with all zeros gets CRS of 0

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement compute_crs()**
  - `CRS = sum(w_x * R_x^0.7) * diversity_multiplier`
  - Weights: equal (0.2 each) for Phase 1
  - Uses existing `ChannelScores` from identity.rs and `diversity_multiplier()` method

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): composite resource score calculation`

---

### Task 2: Block producer selection

**Files:**
- Modify: `commputer-consensus/src/producer.rs`

- [ ] **Step 1: Write failing tests**
  - `test_select_producer_deterministic` — same validators + same round = same producer
  - `test_select_producer_weighted` — over many rounds, higher CRS validator is selected more often
  - `test_select_producer_single_validator` — with one validator, always selected
  - `test_select_producer_empty` — no validators returns error

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement select_producer()**
  - Input: list of (PublicKey, CRS), round number
  - Hash(round_number) selects index weighted by CRS
  - Deterministic: same inputs = same output

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): CRS-weighted block producer selection`

---

### Task 3: Block assembly

**Files:**
- Modify: `commputer-consensus/src/engine.rs`

- [ ] **Step 1: Write failing tests**
  - `test_assemble_empty_block` — no pending txs, no proofs, produces valid block with correct height and prev_hash
  - `test_assemble_block_with_txs` — pending txs are included in block
  - `test_assemble_block_state_root` — assembled block has correct state_root after applying its own transactions
  - `test_assemble_block_increments_height` — each block is one higher than tip

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement assemble_block()**
  - Get chain tip from StateManager
  - Get prev block hash
  - Pull pending txs from pool
  - Create BlockHeader with computed state_root
  - Sign with producer's key (placeholder for now)
  - Return complete Block

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): block assembly from pending transactions`

---

### Task 4: Block validation

**Files:**
- Modify: `commputer-consensus/src/validation.rs`

- [ ] **Step 1: Write failing tests**
  - `test_validate_valid_block` — properly assembled block passes validation
  - `test_validate_wrong_height` — block with wrong height fails
  - `test_validate_wrong_prev_hash` — block with wrong prev_hash fails
  - `test_validate_invalid_transaction` — block with insufficient-balance transfer fails
  - `test_validate_bad_state_root` — block with incorrect state_root fails
  - `test_validate_valid_proof_rewards` — block with proof rewards passes

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement validate_block()**
  - Check height = tip + 1
  - Check prev_hash matches tip block
  - Apply all transactions to a cloned/snapshot state
  - Verify proof results via verifier dispatcher
  - Compute expected state root, compare to block header
  - Return Ok(()) or descriptive error

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): block validation with state root verification`

---

### Task 5: Epoch management

**Files:**
- Modify: `commputer-consensus/src/epoch.rs`

- [ ] **Step 1: Write failing tests**
  - `test_epoch_boundary` — at block N (epoch length), epoch increments
  - `test_emission_recalculation` — new epoch recalculates per-node rate based on validator count
  - `test_channel_allocation` — emission split respects floor percentages
  - `test_no_epoch_change_mid_epoch` — blocks before boundary don't change epoch

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement epoch logic**
  - `EPOCH_LENGTH` constant (e.g., 1000 blocks)
  - `check_epoch_boundary(block_height) -> bool`
  - `recalculate_emission(validator_count) -> EpochEmission` — calls existing `hybrid_emission_rate()` + allocates across channels using floors + demand weighting

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): epoch boundary detection and emission recalculation`

---

### Task 6: ConsensusEngine integration

**Files:**
- Modify: `commputer-consensus/src/engine.rs`
- Modify: `commputer-consensus/src/lib.rs`

- [ ] **Step 1: Write failing tests**
  - `test_engine_produce_and_validate` — engine produces a block, then validates it, both succeed
  - `test_engine_multi_block_chain` — produce 5 blocks in sequence, all valid, state consistent
  - `test_engine_with_proof_rewards` — produce block with CPU proof results, validator gets rewarded
  - `test_engine_epoch_transition` — produce blocks until epoch boundary, emission rate updates

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement ConsensusEngine**
  - Holds reference to StateManager
  - `produce_block()` — select producer, assemble block, return it
  - `validate_and_apply_block(block)` — validate, then apply to state
  - `ConsensusConfig` — epoch length, max txs per block, etc.

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(consensus): ConsensusEngine with produce/validate cycle`

---

### Task 7: End-to-end chain test

**Files:**
- Modify: `commputer-consensus/src/engine.rs` (add integration test)

- [ ] **Step 1: Write integration test**
  - `test_full_chain_lifecycle` — genesis → register validator → produce block with CPU proof → validate → transfer → burn → verify all state across 10 blocks

- [ ] **Step 2: Run full workspace test suite**

Run: `cargo test` — all 69 existing + ~25 new consensus tests pass

- [ ] **Step 3: Commit** — `test(consensus): full chain lifecycle integration test`

---

## Verification

1. `cargo test` — all 69 existing + ~25 new consensus tests pass
2. `cargo check` — clean build across full workspace
3. ConsensusEngine can produce and validate blocks in sequence
4. State is consistent after multi-block chains
5. Epoch transitions recalculate emission correctly
6. CRS weighting influences producer selection
7. Block validation catches all invalid blocks (wrong height, prev_hash, state_root, bad txs)
