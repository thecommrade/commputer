# CPU Proof Channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the CPU proof channel — challenge generation, proof execution, and verification — establishing the pattern all other proof channels will follow.

**Architecture:** The CPU proof uses iterated Blake3 hashing: given a seed and iteration count, the prover computes `hash(hash(hash(...seed...)))` N times and returns the final result plus intermediate checkpoints. The verifier spot-checks by recomputing a random subset of checkpoints. This is deterministic, CPU-bound, and verifiable without redoing all the work. A `ProofEngine` trait defines the interface all five channels share.

**Tech Stack:** Rust, blake3 (hash function), rand (challenge generation), existing types from commputer-core (CpuChallenge, ProofChallenge, ProofResult).

---

## Context

The `commputer-proofs` crate is fully scaffolded but empty. The core types (`CpuChallenge`, `ProofChallenge`, `ProofResult`) are defined in `commputer-core/src/proof.rs`. This plan implements the first proof channel and establishes the `ProofEngine` trait that GPU, storage, RAM, and bandwidth channels will also implement.

## File Structure

```
commputer-proofs/src/
├── lib.rs          -- Add ProofEngine trait, re-exports
├── cpu.rs          -- CpuProofEngine: generate, execute, verify
├── verifier.rs     -- Generic verification dispatcher (routes by channel)
└── (gpu.rs, storage.rs, ram.rs, bandwidth.rs remain empty for now)
```

## Critical Files (existing, to reference)

- `commputer-core/src/proof.rs` — CpuChallenge (seed, iterations, difficulty), ProofChallenge, ProofResult
- `commputer-core/src/types.rs` — Hash, PublicKey, Timestamp
- `commputer-core/src/transaction.rs` — ProofChannel enum
- `commputer-proofs/Cargo.toml` — already has blake3, rand, sha2, commputer-core

---

### Task 1: ProofEngine trait

**Files:**
- Modify: `commputer-proofs/src/lib.rs`

- [ ] **Step 1: Define the ProofEngine trait**
  - `generate_challenge(target, difficulty, rng) -> ProofChallenge` — creates a challenge for a validator
  - `execute(challenge) -> ProofResult` — the validator computes the proof
  - `verify(challenge, result) -> bool` — verifiers confirm the work

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p commputer-proofs`

- [ ] **Step 3: Commit** — `feat(proofs): add ProofEngine trait`

---

### Task 2: CPU challenge generation

**Files:**
- Modify: `commputer-proofs/src/cpu.rs`

- [ ] **Step 1: Write failing tests**
  - `test_generate_challenge` — generates a valid CpuChallenge with correct channel, non-zero seed, specified iterations
  - `test_challenge_unique_seeds` — two challenges have different seeds (randomness works)
  - `test_challenge_difficulty_scaling` — higher difficulty = more iterations

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement CpuProofEngine::generate_challenge**
  - Fill seed from rng
  - Scale iterations based on difficulty (e.g., difficulty * 10_000 base iterations)
  - Serialize CpuChallenge into ProofChallenge.challenge_data
  - Set deadline proportional to difficulty

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(proofs): CPU challenge generation`

---

### Task 3: CPU proof execution

**Files:**
- Modify: `commputer-proofs/src/cpu.rs`

- [ ] **Step 1: Write failing tests**
  - `test_execute_produces_result` — execute a challenge, get a non-empty result_data
  - `test_execute_deterministic` — same challenge = same result every time
  - `test_execute_different_seeds` — different seeds = different results
  - `test_execute_includes_checkpoints` — result_data contains intermediate checkpoints

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement CpuProofEngine::execute**
  - Start with seed, iterate: `state = blake3(state)` N times
  - Every `iterations / checkpoint_count` steps, save the intermediate state as a checkpoint
  - Return final hash + checkpoints in result_data (serialized)

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(proofs): CPU proof execution with checkpoints`

---

### Task 4: CPU proof verification

**Files:**
- Modify: `commputer-proofs/src/cpu.rs`

- [ ] **Step 1: Write failing tests**
  - `test_verify_valid_proof` — generate → execute → verify = true
  - `test_verify_tampered_result` — modify result_data → verify = false
  - `test_verify_wrong_challenge` — result from different challenge → verify = false
  - `test_verify_spot_checks` — verifier recomputes subset of checkpoints, all match

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement CpuProofEngine::verify**
  - Deserialize challenge_data into CpuChallenge
  - Deserialize result_data into CpuProofOutput (final_hash + checkpoints)
  - Pick random checkpoint indices (deterministic from challenge ID as seed)
  - Recompute hash chain from previous checkpoint to next checkpoint
  - If all spot-checked segments match, return true
  - Also verify final_hash is correct by checking from last checkpoint to end

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(proofs): CPU proof verification with spot-checking`

---

### Task 5: Generic verifier dispatcher

**Files:**
- Modify: `commputer-proofs/src/verifier.rs`

- [ ] **Step 1: Write failing tests**
  - `test_dispatch_cpu_channel` — verify dispatches to CpuProofEngine for ProofChannel::Processing
  - `test_dispatch_unknown_channel` — other channels return unimplemented error

- [ ] **Step 2: Run tests, verify fail**

- [ ] **Step 3: Implement verify_proof dispatcher**
  - Match on ProofChannel, delegate to channel-specific engine
  - CPU → CpuProofEngine::verify
  - Others → return error (not yet implemented)

- [ ] **Step 4: Run tests, verify pass**

- [ ] **Step 5: Commit** — `feat(proofs): generic proof verification dispatcher`

---

### Task 6: End-to-end integration test

**Files:**
- Modify: `commputer-proofs/src/cpu.rs` (add integration test)

- [ ] **Step 1: Write integration test**
  - `test_full_cpu_proof_lifecycle` — generate challenge → execute proof → verify → passes
  - `test_cpu_proof_at_various_difficulties` — difficulty 1, 5, 10 all work correctly
  - `test_cpu_proof_timing` — execution time scales roughly with difficulty

- [ ] **Step 2: Run full test suite**

Run: `cargo test` in workspace root, all tests pass (55 existing + ~15 new)

- [ ] **Step 3: Commit** — `test(proofs): CPU proof channel end-to-end tests`

---

## Verification

1. `cargo test` — all 55 existing tests + ~15 new proof tests pass
2. `cargo check` — clean build across full workspace
3. Full lifecycle works: generate → execute → verify = true
4. Tampered proofs fail verification
5. Verification is faster than execution (spot-checking, not full recomputation)
6. ProofEngine trait is generic enough for other channels to implement
