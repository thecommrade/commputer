# commputer-da

Data-availability sampling layer for the PoUW verification game.
Spec: `src/staging/docs/2026-06-12-data-availability-design.md`

No trusted setup. Merkle-over-Reed-Solomon + per-verifier random sampling + sha256 re-bind.
A verifier that cannot obtain its sampled chunks within a bounded retry window **abstains**
(posts no reveal) rather than guessing, feeding the existing `NoQuorum` → escalation path
unchanged. No KZG, no NMT, no fraud-proof gossip in v1.

---

## How to run

```
cd src/
cargo test -p commputer-da
```

All 29 tests should pass in < 1 second.

---

## Sampling confidence

Rate-1/2 systematic RS: the original N data chunks are recoverable from **any N of the 2N**
chunks. To hide even one original byte an adversary must withhold > 50% of the chunks. Each
independent random sample then hits a withheld chunk with probability ≥ 1/2:

| s (samples per verifier) | per-verifier false-accept |
|---|---|
| 8  | 1/256 ≈ 4e-3 |
| 16 (v1 default) | ≈ 1.5e-5 |
| 20 | ≈ 1e-6 |
| 30 | ≈ 1e-9 |

With M honest verifiers sampling independently the aggregate false-accept is ≈ (1/2)^(s·M).

---

## Four consensus-touching surfaces

Exactly four code surfaces feed the `da_root` or the sampling seed and must be
bit-reproducible across every node. Any change bumps `params_version` and is a coordinated
protocol change. Everything else (fetch order, retries, wall-clock) is local and never hashed.

### 1. Chunking (`src/chunk.rs`)
- `chunk_size = 65536` (64 KiB); last data chunk right-zero-padded to `chunk_size`.
- `data_len` carried in `DaAttestation` for exact truncation on reconstruction.
- Shard order: data `[0..N)` then parity `[N..2N)`; all indices/lengths little-endian.
- Edge cases pinned by vectors: `data_len == 0` → one zero data chunk; exact multiple adds
  no spurious padding chunk.

**Golden vector** (`tests/vectors.rs::chunking_and_coding_vector`):
`"hello world!"` (12 bytes, chunk_size=4) → 3 data chunks → parity bytes pinned in-test.

### 2. Erasure encoding (`src/code.rs`)
- Pure-Rust GF(2⁸) Vandermonde via `reed-solomon-erasure = "=6.0.0"` (version-pinned).
- Systematic rate-1/2: N data + N parity. Reconstruct from any N of 2N.
- Encoder output is NOT the consensus artifact (`da_root` + the sha256 re-bind are); a
  deterministic encoder is still required so independent encoders agree on `da_root`.

**Golden vector**: parity bytes for the `"hello world!"` fixture pinned in `vectors.rs`.

### 3. Merkle commitment (`src/merkle.rs`)
- Vendored binary sha256 tree (~110 lines, zero new deps).
- Leaf: `sha256(0x00 || index_le(4) || chunk_bytes)` — index-in-leaf blocks leaf-swap.
- Internal: `sha256(0x01 || left || right)` — 0x00/0x01 tags block leaf/internal
  second-preimage (RFC-6962-style domain separation).
- Odd-node rule **pinned**: promote the lone right node unchanged (no
  CVE-2012-2459-style duplicate-leaf forgery).

**Golden vector** (`tests/vectors.rs::attestation_da_root_vector`):
`"hello world!"` → `da_root` hex pinned in-test.

### 4. Sampling challenge derivation (`src/sampling.rs`)
- `seed = sha256(DOMAIN_SAMPLING || da_root || job_id || committee_epoch_le || verifier_id)`
- Counter-hash PRNG: `sha256(seed || ctr_le)` stream (zero extra deps).
- Seeded Fisher-Yates over `[0, n_total)`, picking `s = min(SAMPLES_PER_VERIFIER, n_total)`
  **distinct** indices.
- Binding the seed to `verifier_id` makes the obligation non-grindable and per-verifier
  auditable.
- Degenerate case pinned: a 1–2-chunk program has `n_total` of 2–4; the verifier samples
  the **entire** chunk set (full coverage, false-accept = 0 — no panic, no infinite loop).

**Golden vector** (`tests/vectors.rs::sampling_golden_vector`):
Fixed `(da_root, job_id, epoch, verifier_id, n_total=64)` → fixed 16-index set pinned in-test.

---

## GF(2⁸) ceiling

`ReedSolomon::new(data, parity)` requires `data + parity ≤ 256`. At rate 1/2 that is
**N ≤ 128 data chunks, 256 coded chunks total**. At the default 64 KiB chunk size:

- Raw data ceiling: 128 × 64 KiB = **8 MiB**
- Coded ceiling: 256 × 64 KiB = **16 MiB**

Note: the spec §1.2 "~16 MiB / ≤255 shards" conflated raw and coded and was off-by-one;
the operative figures are N ≤ 128 (data) and 256 (coded). `n_total` is a `u16`.

GF(2¹⁶) novelpoly for objects above this ceiling is feature-gated (`da-gf16`), deferred.

---

## Two implementation judgment-calls (already design-reviewed)

1. **Vendored Merkle tree** — ~110 lines of in-tree sha256 code rather than `rs_merkle`.
   Reason: explicit domain separation (the 0x00/0x01 tags + index-in-leaf are non-default
   for off-the-shelf Merkle crates), minimal dep footprint, no version-pinning risk on a
   consensus-critical path.

2. **Synchronous `DaTransport`** — the spec's §6.5 described an `async-trait`; v1 uses a
   sync trait instead. All v1 impls (`InMemoryTransport`, `LocalDiskTransport`) are
   synchronous; the sim is single-threaded and deterministic; zero `async-trait` dep is
   pulled. A future libp2p adapter wraps its own async behind a blocking facade using the
   same method shapes — zero change to the consensus-touching core.

---

## Founder open questions (v1 defaults chosen, flagged for review)

1. **Attestation anchoring** (consequential): `da_root` must be consensus-anchored — carried
   in `JobSpec`/the commit payload — so all verifiers in a committee sample the same root.
   Without it, two verifiers could sample different roots and manufacture spurious abstentions.
   This touches the **protected `JobSpec`** → founder's edit. The DA crate is built agnostic
   to anchoring; it takes the attestation as input.

2. **engine-side abstain wiring per §7.1**: pre-commit DA resolution must be wired into
   `engine.rs`'s committee-selection loop. The DA crate's `adapter::resolve_and_populate`
   delivers `Available(bytes)|Abstain` and documents the contract; the actual escrow-timing
   change in `engine.rs` is founder-owned (it is a verification-game file protected from
   agent edits). See the full §7.1 wiring contract in `src/adapter.rs`.

3. **`SAMPLES_PER_VERIFIER` default**: v1 uses 16 (per-verifier false-accept ≈ 1.5e-5).
   Confirm or raise to 20 (≈ 1e-6) / 30 (≈ 1e-9) for mainnet.

4. **Who encodes + publishes chunks**: executor at commit, submitter at submit, or both?
   Determines where the deterministic encoder runs and the initial pinning providers.

5. **Incorrect-coding posture for mainnet**: v1 ships no fraud proofs (committee re-execution
   + reconstruct-and-re-bind). Sound for a re-executing committee; insufficient if
   non-re-executing light clients are ever added. Confirm acceptable for mainnet or schedule
   the 2D-RS / KZG path as a pre-mainnet item.

---

## Regression matrix (run 2026-06-12, commit b3ead4a Layer 5)

### cargo test -p commputer-da — 29 tests, all green

```
running 19 tests
test chunk::tests::empty_input_is_single_zero_chunk ... ok
test chunk::tests::last_chunk_zero_padded_and_count_is_ceil ... ok
test chunk::tests::roundtrip_empty ... ok
test chunk::tests::roundtrip_exact_multiple ... ok
test chunk::tests::roundtrip_partial ... ok
test code::tests::too_large_is_rejected ... ok
test code::tests::parity_then_reconstruct_from_any_n ... ok
test merkle::tests::second_preimage_leaf_vs_internal ... ok
test providers::tests::record_ttl_and_republish ... ok
test merkle::tests::root_is_deterministic ... ok
test transport::tests::clock_advances_manually ... ok
test transport::tests::advertise_find_fetch_roundtrip ... ok
test sampling::tests::tiny_set_samples_whole_domain ... ok
test transport::tests::local_disk_roundtrip ... ok
test providers::tests::responsible_set_is_k_xor_closest_and_deterministic ... ok
test merkle::tests::tamper_is_detected ... ok
test sampling::tests::distinct_indices_in_range_and_deterministic ... ok
test sampling::tests::seed_binds_to_verifier_id ... ok
test merkle::tests::root_inclusion_proof_verifies ... ok
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 5 tests
test abstain_timing_is_clock_driven_and_reproducible ... ok
test rebind_rejects_wrong_bytes_under_valid_root ... ok
test full_availability_returns_available_bytes ... ok
test fake_committee_harness_proves_sec71_abstain_composition ... ok
test majority_withheld_abstains ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 5 tests
test attestation_is_constructible_and_plain_data ... ok
test params_defaults_are_pinned ... ok
test chunking_and_coding_vector ... ok
test sampling_golden_vector ... ok
test attestation_da_root_vector ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### cargo test -p commputer-pouw — 53 tests, all green (baseline unchanged)

```
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  (unit)
test result: ok. 7 passed;  0 failed; 0 ignored; 0 measured; 0 filtered out  (sim)
test result: ok. 1 passed;  0 failed; 0 ignored; 0 measured; 0 filtered out  (conservation)
```

### cargo build (workspace) — clean

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.98s
(1 pre-existing dead_code warning in node/chain_health_monitor.rs; not new)
```

### Game-files-untouched proof

```
git diff 5f576b4 HEAD -- \
  src/staging/pouw/src/engine.rs \
  src/staging/pouw/src/settlement.rs \
  src/staging/pouw/src/verdict.rs \
  src/staging/pouw/src/oracle.rs \
  src/staging/pouw/src/wasm/
```

**Output: EMPTY.** The entire DA cycle (Layers 0-5) touched none of the verification-game
files.
