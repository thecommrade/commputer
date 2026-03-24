# State Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the state management layer — the foundation that tracks wallet balances, validator registry, emission accounting, compliance state, and block storage for the Commputer L1.

**Architecture:** A new `state` module within `commputer-core` containing a `StateManager<S: StateStore>` generic over a storage backend. Initial implementation uses `InMemoryStore` (HashMap + RwLock). The `StateStore` trait allows swapping to persistent storage (sled/rocksdb) later without changing any consumer code. All state mutations go through `StateManager`, which enforces protocol invariants.

**Tech Stack:** Rust, blake3 (state root hashing), borsh (deterministic serialization), std::sync::RwLock, std::collections::HashMap. No new Cargo dependencies — everything needed is already in commputer-core.

---

## Context

The `commputer-core` crate is fully implemented with all protocol types, constants, and 7 passing tests. All other crates (consensus, network, proofs, validator, node) are scaffolded but empty. Every subsystem needs to read/write state — consensus applies blocks, proofs record rewards, the node queries balances. State management is the foundation everything else depends on.

## File Structure

```
commputer-core/src/
├── state/
│   ├── mod.rs          -- StateManager struct, re-exports, apply_block, apply_transaction
│   ├── store.rs        -- StateStore trait + InMemoryStore implementation
│   ├── accounts.rs     -- AccountState struct, balance/nonce/tier tracking
│   ├── validators.rs   -- Validator registry operations (register/deregister/update)
│   ├── emission.rs     -- EmissionState: total emitted/burned, remaining supply, epoch tracking
│   ├── blocks.rs       -- Block storage: by height/hash, chain tip, genesis
│   ├── txpool.rs       -- Pending transaction pool (add/get/remove)
│   ├── grace.rs        -- Grace balance tracking for "Earn It" contributors
│   └── tests.rs        -- Integration tests for full state transitions
├── lib.rs              -- Add `pub mod state;`
└── (existing files unchanged)
```

## Critical Files (existing, to reference/modify)

- `commputer-core/src/types.rs` — PublicKey, CommeAmount, Hash, all protocol constants
- `commputer-core/src/transaction.rs` — Transaction, TransactionPayload (8 variants), ProofChannel
- `commputer-core/src/identity.rs` — ValidatorIdentity, ComplianceStatus, ChannelScores, BehaviorProfile
- `commputer-core/src/block.rs` — Block, BlockHeader (state_root field), GenesisConfig
- `commputer-core/src/tokenomics.rs` — HolderTier::from_balance(), hybrid_emission_rate(), burst_compute_price()
- `commputer-core/src/compliance.rs` — NetworkComplianceState, compute_nerf_percent()
- `commputer-core/src/error.rs` — CommpError enum (already has InsufficientBalance, ValidatorNotFound, etc.)
- `commputer-core/src/lib.rs` — must add `pub mod state;`

## Prerequisite: Add Borsh derives to existing types

Several existing types need `BorshSerialize, BorshDeserialize` added for deterministic state root hashing. This is additive — no logic changes, no broken tests:

- `identity.rs`: ValidatorIdentity, ComplianceStatus, ChannelScores, BehaviorProfile, ComplianceViolation, SybilEvidence, ComplianceEvent
- `compliance.rs`: NetworkComplianceState, NerfThresholds, NerfAdjustment, ComplianceBounty
- `tokenomics.rs`: EpochEmission, ChannelAllocation, EmissionCurvePoint, HolderTier

---

### Task 1: StateStore trait + InMemoryStore

**Files:**
- Create: `commputer-core/src/state/store.rs`
- Create: `commputer-core/src/state/mod.rs` (initial, just re-exports store)
- Modify: `commputer-core/src/lib.rs` (add `pub mod state;`)

- [ ] **Step 1: Write failing tests**
  - `test_account_roundtrip` — set account, get it back, values match
  - `test_account_missing` — get nonexistent returns None
  - `test_validator_roundtrip` — set, get, values match
  - `test_validator_remove` — set, remove, get returns None
  - `test_validator_count` — add 3, count returns 3
  - `test_block_roundtrip` — put block, get by height, get by hash
  - `test_chain_tip` — set tip, get tip

- [ ] **Step 2: Run tests, verify they fail** (types don't exist yet)

- [ ] **Step 3: Implement StateStore trait**
  - Define trait with methods: get/set_account, get/set/remove_validator, validator_count, all_validators, get/put_block, get/set_chain_tip
  - All return `Result<T, CommpError>`

- [ ] **Step 4: Implement InMemoryStore**
  - HashMap behind RwLock for each collection
  - Implement all StateStore methods

- [ ] **Step 5: Run tests, verify they pass**

- [ ] **Step 6: Commit** — `feat(state): add StateStore trait and InMemoryStore`

---

### Task 2: AccountState struct

**Files:**
- Create: `commputer-core/src/state/accounts.rs`

- [ ] **Step 1: Write failing tests**
  - `test_new_account_defaults` — zero balance, nonce 0, tier None
  - `test_recalculate_tier` — various balances map to correct tiers
  - `test_tier_boundaries` — exact boundary values (0, 1, 10, 20, 33 COMME)

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement AccountState**
  - Fields: balance, nonce, tier, grace_balance_secs, last_active
  - `new()` — zero defaults
  - `recalculate_tier()` — calls HolderTier::from_balance

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): add AccountState with tier tracking`

---

### Task 3: EmissionState

**Files:**
- Create: `commputer-core/src/state/emission.rs`

- [ ] **Step 1: Write failing tests**
  - `test_initial_emission_state` — remaining = TOTAL_SUPPLY, zero emitted/burned
  - `test_emit_reduces_remaining` — emit N, remaining decreases by N
  - `test_burn_tracking` — burn N, total_burned increases, remaining unaffected (burns reduce circulating, not remaining-to-emit)

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement EmissionState**
  - Fields: total_emitted, total_burned, remaining_supply, current_epoch
  - `new()` — remaining = TOTAL_SUPPLY
  - `record_emission(amount)` — add to emitted, subtract from remaining
  - `record_burn(amount)` — add to burned

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): add EmissionState tracking`

---

### Task 4: StateManager + Genesis

**Files:**
- Modify: `commputer-core/src/state/mod.rs` (add StateManager)

- [ ] **Step 1: Write failing tests**
  - `test_create_state_manager` — construct with InMemoryStore, no panic
  - `test_init_genesis` — init, chain tip is 0, genesis block exists
  - `test_double_genesis_fails` — init twice, second returns error
  - `test_genesis_emission_state` — after genesis, remaining = TOTAL_SUPPLY

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement StateManager**
  - Generic `StateManager<S: StateStore>`
  - Fields: store, emission (RwLock), compliance (RwLock), tx_pool (RwLock)
  - `new(store)` — initialize with defaults
  - `init_genesis(config)` — create genesis block, init emission/compliance

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): add StateManager with genesis initialization`

---

### Task 5: Transfer transactions

**Files:**
- Modify: `commputer-core/src/state/mod.rs` (add apply_transaction, apply_transfer)

- [ ] **Step 1: Write failing tests**
  - `test_transfer_basic` — fund A, transfer to B, verify balances
  - `test_transfer_insufficient_balance` — send more than have, error
  - `test_transfer_creates_recipient` — send to new account, auto-created
  - `test_transfer_updates_tiers` — cross tier boundary, tier changes
  - `test_transfer_increments_nonce` — nonce goes up by 1
  - `test_transfer_wrong_nonce` — wrong nonce, InvalidTransaction error

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement apply_transaction + apply_transfer**
  - `apply_transaction` dispatches on TransactionPayload variant
  - `apply_transfer` — check balance, debit sender, credit recipient, recalculate tiers, increment nonce

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): implement transfer transactions`

---

### Task 6: Validator registry

**Files:**
- Create: `commputer-core/src/state/validators.rs`

- [ ] **Step 1: Write failing tests**
  - `test_register_validator` — register, verify in registry
  - `test_register_duplicate_fails` — double register, error
  - `test_deregister_validator` — register then deregister, removed
  - `test_deregister_nonexistent_fails` — deregister without register, error
  - `test_validator_update_contribution` — 50% → 80%, stored

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement register/deregister/update**

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): implement validator registry`

---

### Task 7: Burn transactions

**Files:**
- Modify: `commputer-core/src/state/mod.rs` (add apply_burn)

- [ ] **Step 1: Write failing tests**
  - `test_burst_burn_deducts_balance` — burn 5, balance -5
  - `test_burst_burn_updates_emission` — burned amount tracked
  - `test_burst_burn_insufficient` — burn more than have, error

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement apply_burn**

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): implement burst compute burns`

---

### Task 8: Transaction pool

**Files:**
- Create: `commputer-core/src/state/txpool.rs`

- [ ] **Step 1: Write failing tests**
  - `test_add_pending_tx` — add tx, retrieve it
  - `test_get_pending_limit` — add 10, request 5, get 5
  - `test_remove_pending` — add 3, remove 2, 1 remains
  - `test_pending_ordering` — returned by timestamp order

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement TransactionPool**

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): add transaction pool`

---

### Task 9: Grace balance tracking

**Files:**
- Create: `commputer-core/src/state/grace.rs`

- [ ] **Step 1: Write failing tests**
  - `test_grace_accrual` — N seconds online = N seconds grace
  - `test_grace_drain` — N seconds offline = N seconds less
  - `test_grace_refill_ratio` — 5 days online = 10 days grace (2:1)
  - `test_grace_max_cap` — cannot exceed MAX_GRACE_BALANCE
  - `test_grace_floor_zero` — cannot go below 0

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement update_grace_balance**

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): add grace balance tracking`

---

### Task 10: State root hash

**Files:**
- Modify: `commputer-core/src/state/mod.rs` (add compute_state_root)
- Modify: `commputer-core/src/identity.rs` (add BorshSerialize derives)
- Modify: `commputer-core/src/compliance.rs` (add BorshSerialize derives)

- [ ] **Step 1: Add Borsh derives to existing types** (prerequisite)

- [ ] **Step 2: Write failing tests**
  - `test_empty_state_root` — genesis state produces deterministic hash
  - `test_state_root_deterministic` — same state = same hash every time
  - `test_state_root_changes_on_mutation` — transfer changes root
  - `test_state_root_order_independent` — add A then B vs B then A = same root

- [ ] **Step 3: Run tests, verify fail**

- [ ] **Step 4: Implement compute_state_root**
  - Sorted account/validator iteration by PublicKey bytes
  - Blake3 streaming hash with domain separators
  - Borsh serialization for deterministic encoding

- [ ] **Step 5: Run tests, verify pass**

- [ ] **Step 6: Commit** — `feat(state): deterministic state root hashing`

---

### Task 11: apply_block — full block application

**Files:**
- Modify: `commputer-core/src/state/mod.rs` (add apply_block)
- Create: `commputer-core/src/state/tests.rs` (integration tests)

- [ ] **Step 1: Write failing tests**
  - `test_apply_genesis_block` — apply genesis, tip at 0
  - `test_apply_block_with_transfers` — 3 transfers, all balances updated
  - `test_apply_block_wrong_height` — skip height, error
  - `test_apply_block_wrong_prev_hash` — wrong prev hash, error
  - `test_apply_block_with_proof_rewards` — verified proofs credit validators
  - `test_apply_block_updates_chain_tip` — tip advances

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement apply_block**
  - Verify height = tip + 1
  - Verify prev_hash matches stored tip
  - Apply all transactions in order
  - Credit rewards from verified proofs
  - Store block, update tip, update emission

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(state): implement full block application`

---

### Task 12: Final integration test

**Files:**
- Modify: `commputer-core/src/state/tests.rs`

- [ ] **Step 1: Write end-to-end test**
  - `test_multi_block_lifecycle` — genesis → register validator → mine (proof rewards) → transfer → burn → verify all state is consistent

- [ ] **Step 2: Run full test suite** — `cargo test` in workspace root, all tests pass

- [ ] **Step 3: Commit** — `test(state): add multi-block lifecycle integration test`

---

## Verification

1. `cargo test` — all existing 7 tests + ~40 new state tests pass
2. `cargo check` — clean build, no warnings
3. State root hash is deterministic — run same operations, get same hash
4. InMemoryStore is swappable — StateManager works with any StateStore impl
5. No new Cargo dependencies added
6. Existing tests unaffected by Borsh derive additions
