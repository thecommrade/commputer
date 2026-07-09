# Commputer PROTECTED Enforcement + Security Batch — Final Application Plan

**Date:** 2026-07-08
**Branch context:** `agent-testnet-20260707` @ `f0fdac4` (working tree base for every anchor below).
**Purpose:** the single apply-ready plan the founder executes at the alpha genesis reset to (a) turn producer-signature enforcement LIVE, (b) delete the Sybil-open legacy gossip vote path and route votes through the peer-keyed `VoteAggregator`, and (c) close the 14 confirmed protected security findings ([0][1][2][4][7][12][13][16][18][20][24][27][28][30]) plus the §0 `network_height`-poisoning chain-halt — all in one coordinated, reset-gated build unit.
**Provenance:** region mapping pass `wf_4cd45a46-a86` (R1–R5 in `pb_maps.json`) → 3-lens review (`pb_reviews.json`: consensus-safety, deconfliction/compile-order, minimality/liveness) → the two newly-mapped non-protected pre-stages (node_state decay partner; consensus_manager vote-arm rewire) → **this finalize pass**, which folds every review blocker and major into the binding corrections of §0 and re-sequences the raw hunks accordingly.
**Cross-references:** `src/staging/docs/2026-07-07-enforcement-batch-spec.md` (E1–E16) and `src/staging/docs/2026-07-07-security-addendum-protected.md` (findings [0]–[34]).

**STATUS: reviewed, awaiting founder approval; nothing applied yet.**

## How to execute this plan
1. **Read §0 first.** The binding corrections (P1–P8) OVERRIDE any conflicting text in the raw region hunks. Where a raw hunk and a P-correction disagree, the P-correction wins.
2. **Phase 0 (§1) is mine to implement + test now**, before founder approval of the protected phases. Every Phase-0 hunk is non-protected and additive; the tree must **build clean and pass all baselines with Phase 0 alone** (everything Phase-0 adds is either inert/dead-code or a same-value no-op until the protected unit wires it).
3. **Phases 1–5 (§2) are PROTECTED and founder-gated.** They touch only `event_loop.rs`, `main.rs`, `config.rs`, `genesis.json` (the four files on the CLAUDE.md protected list), plus the single atomic `ENFORCE_PRODUCER_SIGNATURES` value flip in the non-protected `core/block.rs` applied in the same commit. Apply **top-to-bottom per file, re-anchoring on the quoted `current_code`, never on line numbers** (numbers shift as earlier hunks land).
4. Apply order and compile-coupling are in §3; the verification gate is §4; the data-dir wipe / reset procedure is §5; the region-by-region approval checklist is §6.
5. This document does not modify source. It is the map the founder approves against.

---

## §0 — BINDING CORRECTIONS (override the raw hunks)

Every review **blocker** and **major** is folded below as a numbered correction. Each states the defect, the review lens it came from, and the resolution that supersedes the raw region hunks.

### P1 — The consensus_manager rewire IS the compile-safe path, AND `try_finalize_round` MUST consume `aggregator.tally()`
*(resolves consensus-safety **BLOCKER 1**; realizes E7.)*

The R1 protected hunks delete both legacy gossip feeders (`SnowballResponse` :1779, `VoteResponse` :1867) and convert the two surviving feeders (rr `Vote` :1666, solo self-vote :3010) to `record_peer_response(..)`. After R1 there is **no live `record_response` caller**. Verified defect: `try_finalize_round` (consensus_manager.rs:**390–391**) finalizes ONLY from `state.round_responses` via `std::mem::take(&mut state.round_responses)`. If the pre-stage only adds `record_peer_response` feeding a *separate* `VoteAggregator` while `try_finalize_round` still reads `round_responses`, **the tree compiles but no round ever finalizes — the chain never advances past the reset on every node.** Compile-coupling does NOT catch this (`record_peer_response` exists → it links).

**Binding resolution — the Phase-0 `consensus_manager.rs` rewire (§1.2) is mandatory and must, in ONE commit:**
- **REPLACE** the `round_responses: HashMap<BlockHash, usize>` field with `aggregator: VoteAggregator<PeerId>` (the ONLY counting path — the Sybil-countable raw increment ceases to exist).
- Add `record_peer_response(height, BlockHash, PeerId) -> bool` routing through `aggregator.record_vote(..)` (dedup by authenticated `PeerId`).
- **REWIRE `try_finalize_round`** to `let tally = state.aggregator.tally(height, &mut rand::thread_rng());` and, on any non-empty tally, reset `state.aggregator = VoteAggregator::new(self.params.sample_size);` **before** `state.voter.record_round(&tally)` — this preserves Snowball's β-consecutive-round consume semantics that the old `mem::take` provided.
- Keep `record_response(height, BlockHash)` as a **LIVE (non-`cfg`) delegating shim** that attributes each call to `PeerId::random()` (distinct voter per call ⇒ exact pre-dedup count semantics ⇒ all existing consensus_manager tests pass unchanged). The founder adds `#[cfg(test)]` to this shim **in the same protected stage-1a commit** that flips the four event_loop feed sites — that is why the shim stays LIVE through Phase 0.
- Companion (§1.3): add `VoteAggregator::set_sample_size(&mut self, usize)` to `vote_aggregator.rs` (verified **absent** at f0fdac4) — required by the `update_params_for_network_size` propagation hunk, else a stale `k=1` caps every tally below quorum and finalization deadlocks when the (1→20) curve rescales.

**Acceptance is a hard gate, not an assumption:** do not apply the protected batch until a **local `scripts/multinode_assert.sh` at `SMOKE_NODES=2` AND `SMOKE_NODES=3`** shows finalizations *advancing* and **per-node applied-height convergence** (§4), because `try_finalize_round` rewiring is not compile-checked.

### P2 — `apply_synced_block` is ONE merged hunk, keeping the gap-loop clamp AND the orphan cap AND the far-ahead reject
*(resolves deconfliction/compile-order **BLOCKER 1**.)*

R2 hunk 7 and R4 hunk 3 both edit the same out-of-order branch of `apply_synced_block` (event_loop.rs:**3192–3208**). R4 hunk 3's `current_code` strictly **encloses** R2 hunk 7's `current_code` (the `if self.orphan_pool.len() < 100 { …push(block)… }` at 3197–3201). Applied in region order R2→R4, R2 hunk 7 rewrites 3197–3201 to `bounded_orphan_insert` first, then R4 hunk 3 fails to match and the mechanical apply **silently drops the CRITICAL finding-[1] gap-loop clamp** `let gap_end = height.min(expected.saturating_add(MAX_SYNC_GAP))`. Without it a peer answering our `GetBlock` with header `height = u64::MAX` drives ~1.8e19 synchronous `request_block` iterations on the single-threaded loop = permanent freeze.

**Binding resolution — delete R2 hunk 7 and R4 hunk 3; apply the single combined hunk in §2.4** (`current_code` = the full 3192–3208 block), whose `new_code`, in order, is: (1) `let expected = …height() + 1;` (2) the far-ahead reject guard using `MAX_SYNC_TARGET_GAP` (100_000); (3) the out-of-order branch with `bounded_orphan_insert` (moves `block`) **then** the clamped `for h in expected..gap_end` loop (uses only `expected`/`height`, so move/borrow order is sound). **The twin `try_apply_finalized` gap loop at :3055 is a SEPARATE function and stays as its own protected hunk (§2.4b) — finding [1] requires BOTH be clamped; E6 alone clamps only the first.**

### P3 — The `network_height` decay partner is MANDATORY, not optional; the clamp alone does NOT close [0]/[2]/[7]
*(resolves minimality/liveness **BLOCKER 1** and consensus-safety **MAJOR 2**.)*

`advance_network_height` only ever **raises** `self.network_height` (monotonic). At f0fdac4 `node_state.rs` has **no `SANE_MAX_GAP` and no decay** — `set_network_height` (node_state.rs:76–82) is strictly monotonic. Fully traced halt: a registered/rogue validator (leader election is warn-only, event_loop.rs:1941 "accepting anyway") signs one EMPTY block at `tip+MAX_SYNC_WINDOW`; it passes `validate_block_from_peer` (valid sig, trivial roots), `advance_network_height` pins `network_height = tip+2000`, the per-tick feed makes the gap `2000 > STALE_THRESHOLD(10)`, `node_state` goes Active→Stale→Syncing, block production (:2677) and the consensus tick (:2943) both early-return on `!is_active()`, and — with no decay and both resync triggers living *inside* those already-returned functions — the node never returns to Active. **Single-message, unrecoverable, fleet-wide halt.** The clamp reduces blast radius but does not restore liveness.

**Binding resolution — land the full node_state decay partner (§1.1) AND wire it in the protected event_loop unit (§2.2):**
- **Phase 0 `node_state.rs` (§1.1)** adds `SANE_MAX_GAP = 2000`, a bounded `peer_heights: Vec<(u64,u64)>` (cap `MAX_PEER_HEIGHT_SAMPLES = 64`), `record_peer_height` / `forget_peer_height` / `recompute_network_height` (the ONLY path that may LOWER `network_height` — median of authenticated samples, floored at `our_height`, ceilinged at `our_height + SANE_MAX_GAP`), plus an inert clamp inside `set_network_height`. Inert/standalone: `set_network_height` keeps its signature and monotonic "never lowers" contract; the new API is dead code until §2.2 wires it. 7 existing tests pass; 5 new tests added (incl. the exact self-heal-to-Active scenario).
- **Protected event_loop wiring (§2.2) is the switch that makes decay EFFECTIVE** and is load-bearing for liveness:
  - **(critical)** at the sync tick (event_loop.rs:**781**) replace `self.node_state.set_network_height(self.network_height)` with `self.node_state.recompute_network_height();` (keep the `set_our_height(self.state.blocks.height())` at :780). Re-feeding a monotonic value every tick would re-pin the target after every recompute.
  - feed the tracker from **authenticated evidence only**: after a block passes `validate_block_from_peer`, `self.node_state.record_peer_height(peer_hash, block.header.height)`; on a `SyncResponse::Height(h)` reply to our own `GetHeight` probe, `record_peer_height(peer_hash, h)` (folded into §2.1 hunk 3).
  - on every peer-removal site (event_loop.rs:**576**, **1062**, and the connection-closed path), `self.node_state.forget_peer_height(peer_hash)`.
  - `peer_hash` MUST be the full-PeerId hash `commputer::peer_hash::peer_bucket(&peer)` (P7), **not** the truncated `[..8]` fold (findings [20]/[30]) — colliding peers would overwrite each other's sample and let a few identities dominate the median.
- **The recompute call MUST sit on a path that runs even while `!is_active()`** (the :781 tick, which runs unconditionally) — never behind an `is_active()`/`sync_complete` gate — or the node cannot climb back out of Syncing. Founder must confirm the :781 call site is not itself gated when finalizing §2.2.
- **Regression-gate it (§4):** inject a validly-signed empty block at `tip+2000` into a 2-node net; assert both nodes resume producing within a bounded time.

### P4 — `ENFORCE_PRODUCER_SIGNATURES` flip is EXPLICIT, compile-coupled in effect, and boot-asserted
*(resolves consensus-safety **MAJOR 1**.)*

The flip is a `pub const bool` value change (block.rs:**127**); Stage-1c calls `block.verify_producer_signature()` which already exists (block.rs:198) and returns `true` for any unsigned block while the const is `false` (block.rs:199–206). So the Stage-1c gate is a **no-op until the flip** — the batch would compile and run with the forgery hole ([4]) open and no loud failure. Compile-coupling does NOT catch this.

**Binding resolution:**
- The flip `false → true` (block.rs:127) is applied **atomically in the same protected commit** as the Stage-1c verify hunk (§2.1 hunk 15) and the signing-side E5 (§1.4). block.rs is non-protected, but this value change **rides the protected unit / reset**, never lands early.
- Add a **runtime boot assertion** in the protected `main.rs run_node` path (§2.5): `assert!(commputer_core::block::ENFORCE_PRODUCER_SIGNATURES, "alpha reset requires producer-signature enforcement");` so a forgotten flip fails loudly at boot.
- Add a **launch-gate grep** to §4: `grep 'ENFORCE_PRODUCER_SIGNATURES: bool = true' src/core/src/block.rs` must succeed before the multinode gate (matches enforcement-spec §3 canary).
- E12 (§1.5) rewrites the one break-on-flip test `strict_rejects_unsigned_nongenesis` to the const-aware `assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES)` so `cargo test -p commputer_core` stays green in both const states.

### P5 — Chain-id bump must edit `core::genesis::TESTNET_CHAIN_ID` (the CONSENSUS value), not just `config.rs`
*(resolves consensus-safety **MAJOR 3**.)*

Verified: the on-chain chain_id is `commputer_core::genesis::TESTNET_CHAIN_ID` (genesis.rs:**6**) — written into every produced header (event_loop.rs:2869), **signed** via `signable_bytes`, embedded in the genesis header, and **enforced on apply** (state.rs:**1078**, which accepts only empty/`TESTNET_CHAIN_ID`/`MAINNET_CHAIN_ID`). `config.rs::DEFAULT_TESTNET_CHAIN_ID` (config.rs:12) is **display-only** (main.rs:426/1070/1288, rpc.rs). Bumping only config/genesis.json leaves the consensus identity and genesis hash unchanged (no real re-namespace; old-chain signatures stay replay-valid). Worse, wiring a new alpha string into production (2869) while state.rs:1078 still accepts only the old value would reject the producer's own blocks → fleet halt.

**Binding resolution:**
- Bump `core::genesis::TESTNET_CHAIN_ID` **in place** (non-protected, §1.11) to `commputer-testnet-2`, so producer (event_loop:2869) and validator (state.rs:1078) move together.
- Bump the paired `genesis_timestamp` in core/genesis.rs (§1.11) — this is the actual genesis-**hash** severance; a chain-id change alone re-derives the hash via the header, but the spec pairs both, so keep them together.
- Update `config.rs::DEFAULT_TESTNET_CHAIN_ID` (§2.6, protected) and root `genesis.json` (§2.7, protected) to the **byte-identical** string.
- Add an equality test (§1.12): `assert_eq!(config::DEFAULT_TESTNET_CHAIN_ID, commputer_core::genesis::TESTNET_CHAIN_ID)` so display and consensus cannot silently diverge again.

### P6 — `MAX_SYNC_WINDOW` is pinned **==** `node_state::SANE_MAX_GAP`, enforced by a compile-time assert
*(resolves deconfliction/compile-order **MAJOR 1**.)*

`advance_network_height` clamps to `tip + MAX_SYNC_WINDOW`; the :781 feed then hands that to node_state, which applies its own `SANE_MAX_GAP`. If `SANE_MAX_GAP < MAX_SYNC_WINDOW`, a value already legitimately admitted is re-shrunk by node_state and the "am-I-behind?" gate disagrees with the sync target — a late-join liveness hazard. R1 shipped `MAX_SYNC_WINDOW=2000` as a placeholder against a then-nonexistent `SANE_MAX_GAP`.

**Binding resolution:**
- Both constants are **2000** and both live in **non-protected** files (`MAX_SYNC_WINDOW` in the new `peer_hash.rs` per P7; `SANE_MAX_GAP` in `node_state.rs`). Because the :781 feed sets `our_height == tip` each tick, `our_height + SANE_MAX_GAP == tip + MAX_SYNC_WINDOW` — the two clamps never fight when EQUAL.
- Add a build-fails-on-divergence guard in `peer_hash.rs` (§1.6): `const _: () = assert!(MAX_SYNC_WINDOW == crate::node_state::SANE_MAX_GAP);`
- This is a **hard pre-apply gate**, not an open question: `SANE_MAX_GAP` lands in Phase 0 with the known value; `MAX_SYNC_WINDOW` is set equal in the same Phase 0. **Second-tier bound (documented, deliberate):** `network_height` caps at `tip+2000` (the STALE gate); orphan-accept caps at `tip+MAX_SYNC_TARGET_GAP(100_000)` (the far-ahead reject, §2.4). Two different quantities, both chosen, not placeholders.

### P7 — `peer_bucket` + `MAX_SYNC_WINDOW` move to a non-protected helper module; the protected diff shrinks
*(resolves minimality/liveness **MAJOR 1**; also eliminates the R1/R4 `peer_bucket` name collision the deconfliction lens flagged.)*

The raw R1 module-level hunk added `fn peer_bucket(&PeerId)->u64` and `const MAX_SYNC_WINDOW` **directly into PROTECTED event_loop.rs** (~15 lines), and R4's serve hunk **re-implemented the same full-PeerId DefaultHasher** as a separate inline closure — duplicated logic across two protected sites, and two same-named `peer_bucket` bindings in one file.

**Binding resolution — create the non-protected module `src/node/src/peer_hash.rs` (§1.6), registered in `lib.rs`, exporting:**
```rust
pub fn peer_bucket(peer: &libp2p::PeerId) -> u64          // full-bytes DefaultHasher (E9/[20]/[30])
pub fn peer_bucket_tagged(peer: &libp2p::PeerId, tag: u8) -> u64  // GetBlock=0 / GetBlocks=1 (E6)
pub const MAX_SYNC_WINDOW: u64 = 2000;                    // == node_state::SANE_MAX_GAP (P6)
```
- **DELETE the R1 module-level hunk** (raw R1 hunk 1). R1's three rekey sites call `commputer::peer_hash::peer_bucket(&peer)`; R4's serve handler calls `commputer::peer_hash::peer_bucket_tagged(&peer, 0|1)` (no inline closure). `advance_network_height` references `commputer::peer_hash::MAX_SYNC_WINDOW`.
- Only `advance_network_height` (a `&mut self` method) genuinely stays in the protected file.

### P8 — Faucet identity is single-source; a genesis/seed split-brain fails LOUDLY at boot, not silently at runtime
*(resolves minimality/liveness **MAJOR 2**; reconciles E3.)*

The quota + fee-floor exemption (§2.3 hunk B) and the genesis funding (`alpha_genesis_accounts`) both key off compile-time `ALPHA_FAUCET_ADDRESS_HEX`, while the live faucet **signs** with the wallet derived from `COMMPUTER_FAUCET_SEED` (§1.14 provision helper). Nothing asserts `address(seed) == ALPHA_FAUCET_ADDRESS_HEX`. On mismatch the live faucet is neither funded nor exempted; once `> MAX_MEMPOOL_TXS_PER_ACCOUNT` dispenses are pending (nonce already consumed on `try_send`), admission rejects and the nonce desyncs → faucet dead until restart. The E3 in-flight snapshot also counts by `faucet_wallet.address()` while the exemption uses `ALPHA_FAUCET_ADDRESS_HEX` — two sources for one identity.

**Binding resolution:**
- In the non-protected `rpc::provision_faucet_from_env` helper (§1.14), when the faucet is enabled (`COMMPUTER_FAUCET_SEED` set): **fail-closed** — return `Err` (which `?` aborts boot in `main.rs`) unless `ALPHA_FAUCET_ADDRESS_HEX == Some(hex::encode(wallet.address().0))`. Upgrade the raw helper's `warn!` on mismatch to a hard error. Both must be `Some` and equal, or the node refuses to bind.
- The event_loop exemption (§2.3 hunk B) exempts the faucet address from **both** the F-3 quota and the fee-floor (belt-and-suspenders; the funded faucet passes the fee-floor anyway). The exemption resolves `ALPHA_FAUCET_ADDRESS_HEX`; because the boot check guarantees `address(seed) == ALPHA_FAUCET_ADDRESS_HEX`, the exemption and the signing wallet are the same identity.
- Faucet funding is **100,000 COMME** (`ALPHA_FAUCET_ALLOCATION = 100_000 * UNITS_PER_COMME = 1e13 raw`), Slice-4 mechanism, `Option<&str>` const in `testnet_genesis.rs`, credited **BEFORE `apply_block`** (E1, §2.5).

**Remaining founder micro-decisions carried forward (not blockers):** final `MAX_SYNC_WINDOW`/`SANE_MAX_GAP` numeric (both 2000 proposed); the offline faucet-wallet address generation procedure and the matching `genesis.json accounts` entry; whether to also stop *emitting* the now-ignored outbound `VoteResponse`/`SnowballQuery` gossip (E2 residual, safe to defer). See §6.

---

## §1 — PHASE 0: NON-PROTECTED PRE-STAGES (land first, tree builds + passes clean)

I (the agent) implement and test **all of §1 before founder approval of the protected phases**. Every hunk is additive to a non-protected file (or a new file) and is inert/dead-code or a same-value no-op until the protected unit wires it. **Gate for the whole of Phase 0: `cargo build --workspace` clean + all baselines pass (§4) with zero protected files touched.** Apply top-to-bottom per file; re-anchor on `current_code`.

### §1.1 — `src/node/src/node_state.rs` — the decay partner (P3). 5 hunks.
Adds `SANE_MAX_GAP = 2000` (`>= MAX_SYNC_WINDOW`, kept EQUAL) + `MAX_PEER_HEIGHT_SAMPLES = 64`; the private `peer_heights: Vec<(u64,u64)>` field + its `new()` init; the inert clamp inside `set_network_height` (`height.min(our_height + SANE_MAX_GAP)`, preserving the monotonic "never lowers" contract for callers); and `record_peer_height` / `forget_peer_height` / `recompute_network_height` (median of samples, floored at `our_height`, ceilinged at `our_height + SANE_MAX_GAP`, the ONLY path that may LOWER `network_height`); plus 5 new tests. **Full hunk bodies: `pb` node_state pre-stage JSON (`compiles_standalone: true`, 7 old tests unchanged).** Inert because `set_network_height` keeps its signature and only clamps absurd (`> our_height+2000`) jumps that never occur in +1 operation, and the decreasing API is unreferenced until §2.2.

### §1.2 — `src/node/src/consensus_manager.rs` — E7 peer-keyed aggregator + live shim (P1). 7 hunks.
Anchors at f0fdac4 (spec line numbers stale — match on text):
- **imports** (after `use commputer_consensus::snowball::…` ~:9): add `use commputer_consensus::VoteAggregator;` + `use libp2p::PeerId;` (both already deps; `VoteAggregator` re-exported at consensus/lib.rs:22).
- **`struct HeightState`** (:163–170): **REPLACE** `round_responses: HashMap<BlockHash, usize>` with `aggregator: VoteAggregator<PeerId>` (the sole counting path).
- **lifecycle doc** (:177): point step 3 at `record_peer_response()`.
- **`update_params_for_network_size`** per-height loop (~:246): add `state.aggregator.set_sample_size(sample);` beside `state.voter.set_params(...)` — REQUIRES §1.3.
- **`add_candidate_inner`** HeightState literal (~:329): `aggregator: VoteAggregator::new(self.params.sample_size),`.
- **`record_response`** (:372–378): add `pub fn record_peer_response(&mut self, height, BlockHash, PeerId) -> bool` (routes `aggregator.record_vote`, dedup bool, `debug!` on dup), and keep `record_response` as a **LIVE** shim `self.record_peer_response(height, preference, PeerId::random())`.
- **`try_finalize_round`** accumulated-votes block (:389–401): **REWIRE** to `let tally = state.aggregator.tally(height, &mut rand::thread_rng());` then `if !tally.is_empty() { state.aggregator = VoteAggregator::new(self.params.sample_size); let finalized = state.voter.record_round(&tally); … }`.

Verified: every existing consensus_manager test calls `record_response` N≤sample_size times/round, so the k-sampled tally equals the exact vote multiset → `record_round` input is identical → all tests pass. Gate: `cargo test -p commputer consensus_manager`.

### §1.3 — `src/consensus/src/vote_aggregator.rs` — `set_sample_size` (P1 companion). 1 hunk.
Verified **absent** at f0fdac4. Add to the `impl<P: Eq + Hash + Clone> VoteAggregator<P>` block (after `new`):
```rust
/// Update k when the network rescales (mirrors SnowballParams::sample_size).
/// Recorded votes are kept — only the per-tally sampling bound changes.
pub fn set_sample_size(&mut self, sample_size: usize) { self.sample_size = sample_size; }
```
Gate: `cargo test -p commputer-consensus vote_aggregator`.

### §1.4 — `src/core/src/block.rs` — E5 sign `checkpoint_hash` (R2 hunk 1). 1 hunk.
`signable_bytes` tail (block.rs:**78**, currently ends chain_id serialize then `bytes`): append `borsh::BorshSerialize::serialize(&self.checkpoint_hash, &mut bytes).unwrap();` before `bytes`. Consensus-affecting (signed byte domain) but harmless while `ENFORCE=false`; sign (signing.rs:40) and verify (block.rs:117) both route through `signable_bytes`, and `checkpoint_hash` is set (event_loop.rs:2885) before `sign_block` (2892). No golden-vector pins the layout.

### §1.5 — `src/core/src/block.rs` — E12 const-aware test (R2 hunk 3, P4). 1 hunk.
Rewrite `strict_rejects_unsigned_nongenesis` (block.rs:~343–345): replace `assert!(b.verify_producer_signature())` with `assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES);`. Passes in both const states (pre: `assert_eq!(true,true)`; post: `assert_eq!(false,false)`). **NB: the `ENFORCE=false→true` flip itself is NOT in Phase 0 — it is the atomic flip in §2 (P4).**

### §1.6 — `src/node/src/peer_hash.rs` — NEW non-protected helper (P7 + P6). 1 hunk.
```rust
// peer_hash.rs — full-PeerId bucket keys (E9/[20]/[30]) + the sync-window clamp bound.
// WIRING: event_loop.rs (PROTECTED) calls peer_bucket at the 3 consensus-limiter/health
// rekey sites and peer_bucket_tagged in the sync serve handler; advance_network_height
// references MAX_SYNC_WINDOW. Registered via `pub mod peer_hash;` in lib.rs.
use std::hash::{Hash, Hasher};

/// SECURITY(net-height §0): max blocks ahead of our tip a single advance may raise the
/// target. Pinned EQUAL to node_state::SANE_MAX_GAP so the two clamps never fight (P6).
pub const MAX_SYNC_WINDOW: u64 = 2000;
const _: () = assert!(MAX_SYNC_WINDOW == crate::node_state::SANE_MAX_GAP);

/// Non-grindable bucket key from the FULL PeerId bytes (was [..8] ⇒ ~2 key bytes).
pub fn peer_bucket(peer: &libp2p::PeerId) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    peer.to_bytes().hash(&mut h);
    h.finish()
}
/// Separate GetBlock (tag 0) / GetBlocks (tag 1) buckets so batch sync is not starved.
pub fn peer_bucket_tagged(peer: &libp2p::PeerId, tag: u8) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    peer.to_bytes().hash(&mut h);
    tag.hash(&mut h);
    h.finish()
}
```
Standalone: pure functions + a const, referenced only by the protected phase. The `const _` assert makes a future `MAX_SYNC_WINDOW`/`SANE_MAX_GAP` divergence a **build error**.

### §1.7 — `src/node/src/block_maps.rs` — NEW non-protected caps (R2 hunk 5, findings [13]/[24]). 1 hunk.
Full body in `pb_maps.json` R2 hunk 5: `bounded_orphan_insert` (per-parent ≤20 + total ≤200, refuse-on-full), `prune_producer_blocks` (drop ≤tip then clear over 10k), `prune_block_seen_times` (clear over 10k) + 5 unit tests. Imports verified: `commputer_core::block::{Block, BlockHash, BlockHeader, CURRENT_PROTOCOL_VERSION}`, `commputer_core::identity::Address`. Gate: `cargo test -p commputer block_maps`.

### §1.8 — `src/node/src/mempool_quota.rs` — NEW non-protected quota (Slice 3.1, findings [12]/F-3). 1 hunk.
Required by §2.3 hunk B; verified absent at f0fdac4. Exports:
```rust
pub const MAX_MEMPOOL_TXS_PER_ACCOUNT: usize = /* founder-tuned, e.g. 64 */;
/// Reject (never evict) when a sender already has >= cap pending. Returns the same
/// Result<(), &'static str> shape validate_tx_for_mempool composes with `?`.
pub fn account_quota_ok(pending_from_sender: usize, cap: usize) -> Result<(), &'static str> {
    if pending_from_sender >= cap { Err("account mempool quota exceeded") } else { Ok(()) }
}
```
Gate: `cargo test -p commputer mempool_quota`.

### §1.9 — `src/node/src/lib.rs` — module registrations. 1 hunk.
After `pub mod kademlia_bootstrap_fix;` (:11) append (distinct lines, non-conflicting):
```rust
pub mod block_maps;
pub mod mempool_quota;
pub mod peer_hash;
```

### §1.10 — `src/node/src/testnet_genesis.rs` — faucet allocation (R5 hunk 1, Slice-4/E1). 1 hunk.
Before `#[cfg(test)]` (~:76): add `pub const ALPHA_FAUCET_ADDRESS_HEX: Option<&str> = None;` (founder pastes the offline address at reset — `None` ⇒ byte-identical genesis), `pub const ALPHA_FAUCET_ALLOCATION: u64 = 100_000 * UNITS_PER_COMME;`, and `pub fn alpha_genesis_accounts() -> Vec<(String, u64)>` (single faucet entry or empty). `UNITS_PER_COMME` already imported (:5); file has `#![allow(dead_code)]`.

### §1.11 — `src/core/src/genesis.rs` — CONSENSUS chain-id + timestamp bump (P5/M3). 1 hunk.
- `pub const TESTNET_CHAIN_ID: &str = "commputer-testnet-1";` (genesis.rs:6) → `"commputer-testnet-2";`
- bump the fixed `genesis_timestamp` (spec Slice-5.3: `1774656000` → `1783468800`) — the genesis-hash severance.
Non-protected, consensus-affecting, reset-only. This is the value state.rs:1078 enforces and event_loop.rs:2869 writes, so producer and validator move together. **Do NOT compile any faucet allocation into core/genesis.rs (E1 — the single allocation source is testnet_genesis.rs).**

### §1.12 — chain-id equality test (P5). 1 hunk.
Add a `#[test]` (in `config.rs`'s test module or a node integration test): `assert_eq!(commputer::config::DEFAULT_TESTNET_CHAIN_ID, commputer_core::genesis::TESTNET_CHAIN_ID);` so display (config) and consensus (core) cannot silently diverge.

### §1.13 — `src/node/Cargo.toml` — `zeroize` dep (R5 hunk 2, E11). 1 hunk.
After `hex = "0.4"` (:28) add `zeroize = "1"` (already in Cargo.lock via core; required by §1.14). node/Cargo.toml is non-protected.

### §1.14 — `src/node/src/rpc.rs` — RpcState fields + provision helper (R5 hunks; E10/E11 + P8). 2 hunks.
- **RpcState struct fields** (Slice-4 hunk 4.2): add `pub faucet_wallet: Option<commputer_core::wallet::Wallet>` and `pub faucet_next_nonce: tokio::sync::Mutex<u64>` (the `mempool: Mutex<Vec<MempoolTxInfo>>` field already exists). Hard compile-couple with §2.5's RpcState literal.
- **`provision_faucet_from_env`** (R5 hunk 3, between `build_faucet_transfer` and `/// POST /faucet`): derive the wallet from `COMMPUTER_FAUCET_SEED`, `zeroize` + `std::env::remove_var` the phrase, seed the nonce from `state.accounts.get(addr)`, never log the phrase. **P8 override:** on a set-but-mismatched address, return `Err` (fail-closed, aborts boot) instead of the raw `warn!` — require `ALPHA_FAUCET_ADDRESS_HEX == Some(hex::encode(wallet.address().0))`. `None` seed ⇒ `Ok((None, 0))`. Verified callable APIs (`Wallet::from_seed_phrase` core wallet.rs:56, `state.accounts.get`, `hex::encode`).

### §1.15 — `src/storage/src/state.rs` — F10/F21 zero-from reconcile (baked decision). Verify + reconcile only.
Non-protected, owned by the storage auto-fix stream. F33 zero-address guard already lives in `apply_genesis_accounts`; **confirm** the MilestoneBurn/CharitableDonation forgery (F10) and Transfer zero-from (F21) guards are present at the reset. Not a protected-batch code hunk — a pre-reset verification item; if a guard is missing, it lands here (non-protected) before the reset. Note in §6.

**Phase-0 total: 26 mapped hunks** across `node_state.rs`(5), `consensus_manager.rs`(7), `vote_aggregator.rs`(1), `block.rs`(2), `peer_hash.rs`(1), `block_maps.rs`(1), `mempool_quota.rs`(1), `lib.rs`(1), `testnet_genesis.rs`(1), `core/genesis.rs`(1), chain-id test(1), `Cargo.toml`(1), `rpc.rs`(2) — plus the §1.15 storage reconcile (verification item).

---

## §2 — PHASES 1–5: PROTECTED HUNKS (founder-gated, ONE build unit)

Protected files: `event_loop.rs`, `main.rs`, `config.rs`, `genesis.json`. **Apply top-to-bottom per file; re-anchor on the quoted `current_code` (numbers shift).** Grouped by region so the founder approves region-by-region (§6). Every hunk body is in `pb_maps.json` unless a P-correction restates it; the corrections in §0 win on conflict.

### Phase 1 — R1: consensus/gossip ingress + `network_height` + block-ingestion auth (`event_loop.rs`)

**Apply the raw R1 hunks 2–16, WITH these §0 overrides:**
- **DELETE raw R1 hunk 1** (module-level `MAX_SYNC_WINDOW`+`peer_bucket`) — moved to `peer_hash.rs` (P7). All references become `commputer::peer_hash::peer_bucket(&peer)` / `commputer::peer_hash::MAX_SYNC_WINDOW`.
- Hunk 2: `TOPIC_PEER_ADDRS` loop → `.iter().take(MAX_PEERS_PER_EXCHANGE)` (finding [16]).
- Hunk 3 (`SyncResponse::Block/Blocks/Height`, ~:1534–1562): drop the pre-validation `network_height` raises from Block/Blocks; keep `Height` as the trusted advance via `self.advance_network_height(h)`; **P3 add:** in the `Height` arm also `self.node_state.record_peer_height(commputer::peer_hash::peer_bucket(&peer), h);`.
- Hunk 4 (rr `BlockProposal`, ~:1587–1606): swap the weak fold for `peer_bucket(&peer)` (E9/[20]); delete the pre-validation `self.network_height = height` AND the direct `self.node_state.set_network_height(height)` at :1600; advance ONLY inside the validated branch via `self.advance_network_height(height)`; **P3 add:** after validation, `self.node_state.record_peer_height(peer_bucket(&peer), height);`.
- Hunk 5 (`VoteRequest` rate-limit): `peer_bucket(&peer)` (E9, 2nd site).
- Hunk 6 (rr `ConsensusResponse::Vote`, ~:1663–1670): `self.consensus.record_peer_response(height, BlockHash(preference), peer)` (finding [4]) + `self.health_monitor.record_vote(peer_bucket(&peer))` (E9/[30], 3rd site).
- Hunk 7 (`BlockCandidate`): move the raise AFTER `validate_block_from_peer` as `self.advance_network_height(height)`; **P3 add** `record_peer_height(peer_bucket(&source), height)`.
- Hunk 8 (`SnowballQuery`): **remove** the raise entirely (no-block gossip query — the cheapest poison vector).
- Hunk 9 (`SnowballResponse`): **E2 DELETE** — `ConsensusMessage::SnowballResponse { .. } => {}` (inert no-op for exhaustiveness; removes the Sybil-countable `record_response`).
- Hunk 10 (`BlockResponse{Some}`): add `validate_block_from_peer` before `add_candidate` (Slice-1.6 bypass close) + `advance_network_height(height)` after; **P3 add** `record_peer_height`.
- Hunk 11 (gossip `BlockProposal`): raise → post-validation `advance_network_height`; **P3 add** `record_peer_height`.
- Hunk 12 (`BlockQuery`): **remove** the raise (no-block gossip query).
- Hunk 13 (`VoteResponse`): **E2 DELETE** — `ConsensusMessage::VoteResponse { .. } => {}`. Together with hunk 9 this leaves **no live `self.consensus.record_response(` caller**, which is what lets the founder mark the shim `#[cfg(test)]` (P1/E7).
- Hunk 14: add the `advance_network_height(&mut self, candidate: u64)` method (clamps to `self.state.blocks.height() + commputer::peer_hash::MAX_SYNC_WINDOW`, raises only).
- Hunk 15 (`validate_block_from_peer`, before Stage 2 merkle): **Stage 1c** — `if block.height() == 0 { self.adjust_peer_score(source, -20); return false; }` (E4 height-0 reject) then `if !block.verify_producer_signature() { self.ban_peer(source, "sent block with missing/invalid producer signature"); return false; }` (Slice-1.5; ban string quoted verbatim for the §3 rogue-binary drill).
- Hunk 16 (solo self-vote, ~:3010): `self.consensus.record_peer_response(next_height, pref, self.network.local_peer_id)`.

### Phase 2 — P3 decay wiring (`event_loop.rs`) — the switch that makes node_state self-heal
Beyond the per-hunk `record_peer_height` feeds folded into Phase 1 above:
- **§2.2a (critical):** sync tick, event_loop.rs:**780–781** — keep `self.node_state.set_our_height(self.state.blocks.height());`; **replace** `self.node_state.set_network_height(self.network_height);` with `self.node_state.recompute_network_height();`. Confirm this call site runs unconditionally (not behind `is_active()`/`sync_complete`).
- **§2.2b:** every peer-removal site — event_loop.rs:**576** (`ban_peer`), **1062** (peer rotation), and the connection-closed handler — add `self.node_state.forget_peer_height(commputer::peer_hash::peer_bucket(&peer_id));`.

### Phase 3 — R2 + R4 sync/ingest safety (`event_loop.rs`)
- **§2.3a — R2 hunk 6:** `handle_received_block` restructure — move `if !self.validate_block_from_peer(&block, source) { return; }` to the TOP (before `block_seen_times`/`producer_blocks`/orphan inserts), capture `let applied_tip = self.state.blocks.height();`, then `commputer::block_maps::prune_block_seen_times(&mut self.block_seen_times, applied_tip);`, `prune_producer_blocks(&mut self.producer_blocks, applied_tip);`, and `bounded_orphan_insert(&mut self.orphan_pool, block.header.parent_hash, block)`. Closes candidate-entry bypass #2 (process_orphans no longer re-injects unvalidated blocks) + findings [13]/[24].
- **§2.4 — MERGED `apply_synced_block` hunk (P2):** delete raw R2 hunk 7 + R4 hunk 3; apply the single combined hunk (`current_code` = full 3192–3208): `let expected`; far-ahead reject `if height > expected.saturating_add(commputer::sync_machine::MAX_SYNC_TARGET_GAP) { warn!(…); return; }`; then `if height != expected { if height > expected { debug!(…); commputer::block_maps::bounded_orphan_insert(&mut self.orphan_pool, block.header.parent_hash, block); let gap_end = height.min(expected.saturating_add(commputer::sync_machine::MAX_SYNC_GAP)); for h in expected..gap_end { self.request_block(h); } } return; }`.
- **§2.4b — R4 hunk 4 (twin, separate function):** `try_apply_finalized` behind-branch (~:3050–3060) — clamp `let gap_end = height.min(expected.saturating_add(commputer::sync_machine::MAX_SYNC_GAP)); for h in expected..gap_end { self.request_block(h); }` (finding [1] twin; no far-ahead guard — height is consensus-finalized).
- **§2.4c — R4 hunk 2 (serve handler, ~:1501–1530, Request arm ONLY):** gate `GetBlock`/`GetBlocks` on `self.sync_rate_limiter.check(commputer::peer_hash::peer_bucket_tagged(&peer, 0|1))` returning an empty response on over-limit (no early `return`); replace `start + 100` with `start.saturating_add(100)` (finding [28]); **do NOT touch the sibling `Response` arm** (its `network_height` writes belong to Phase 1). P7: use the shared `peer_bucket_tagged`, no inline closure.
- **§2.4d/e — R4 hunks 5/6:** add the `pub sync_rate_limiter: commputer_network::sync_rate_limiter::SyncRateLimiter` field (after `consensus_rate_limiter`, ~:209) and its `::new()` init (~:287). Apply ONCE (co-owned with the EventLoop-struct region — do not double-apply).

### Phase 4 — R3 mempool / tx admission (`event_loop.rs`)
- **Hunk A** (`validate_tx_for_mempool` top, ~:2129): `tx.validate_shape()?;` before `tx.verify()` (findings [3]/[5]/[6]/[11]/[22] ingress partner).
- **Hunk B** (nonce block, ~:2176–2189): compute `faucet_exempt` once from `commputer::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX` (P8 — the boot check guarantees it matches the signing wallet); if `!faucet_exempt`, `commputer::mempool_quota::account_quota_ok(pending_from_sender, MAX_MEMPOOL_TXS_PER_ACCOUNT)?` (F-3) and the finding-[18] fee floor (`balance < committed_fees + tx.fee` ⇒ `Err`); `pending_from_sender` becomes `usize`, `as u64` at the nonce sum.
- **Hunk C** (`handle_rpc_transaction` end, ~:2124): insert `self.enforce_mempool_limit();` between `push(tx)` and `update_rpc_status()` (finding [12]).
- **Hunk D** (`enforce_mempool_limit`, ~:2321): eviction key `(bal >= tx.fee, tx.fee)` — unaffordable-first, then lowest-fee (finding [18] eviction half).

### Phase 5 — R5 bootstrap + config + genesis + the atomic flip
- **§2.5 — `main.rs` (protected):** (1) `run_node` fresh-chain branch (~:911–915) — `let alloc = testnet_genesis::alpha_genesis_accounts(); state.apply_genesis_accounts(&alloc)?;` **BEFORE** `apply_block(&genesis)` (E1 crash-safe ordering). (2) `open_chain_state` fresh-chain branch (~:457–460) — identical parity call. (3) after `initial_status` (~:1136–1138) — `let (faucet_wallet, faucet_next_nonce) = rpc::provision_faucet_from_env(&state)?;` (E10; `?` aborts boot on the P8 fail-closed check). (4) `RpcState` literal (~:1163–1164) — add `faucet_wallet,` + `faucet_next_nonce: tokio::sync::Mutex::new(faucet_next_nonce),`. (5) **P4 boot assert** (early in `run_node`) — `assert!(commputer_core::block::ENFORCE_PRODUCER_SIGNATURES, "alpha reset requires producer-signature enforcement");`.
- **§2.6 — `config.rs` (protected):** `DEFAULT_TESTNET_CHAIN_ID` (:12) → `"commputer-testnet-2"` (byte-identical to §1.11).
- **§2.7 — `genesis.json` (protected):** `"chain_id"` (:2) → `"commputer-testnet-2"` (reference-only; if the founder fills the faucet address, also add `"accounts": [["<addr_hex>", 10000000000000]]`; E1 permits dropping the file entirely, in which case discard this hunk rather than leaving `-1`).
- **§2.8 — THE ATOMIC FLIP:** `core/block.rs:127` `ENFORCE_PRODUCER_SIGNATURES: false → true` (P4). File is non-protected but this value change is applied **in the same commit** as the Phase 1–5 protected hunks — it is the go-live switch, never lands early.

**Protected total: 34 hunks** — Phase 1 (15, raw R1 hunk 1 removed) + Phase 2 (2: :781 switch, disconnect forget) + Phase 3 (5: handle_received_block, merged apply_synced_block, try_apply_finalized twin, serve, +field/init counted as 2) + Phase 4 (4) + Phase 5 (7: 5 main.rs + config + genesis) — **plus the single atomic non-protected `ENFORCE` flip (§2.8)** applied in the same commit. (`record_peer_height` decay feeds are folded into the Phase-1 advance hunks, not counted separately.)

---

## §3 — COMPILE-COUPLING & APPLY ORDER

**Sequence: Phase 0 (all of §1) → [tree builds + passes §4 baselines] → Phases 1–5 protected as ONE commit + §2.8 flip → [tree builds + passes §4 + multinode gate].**

**What will NOT compile until its counterpart lands:**
- Phase-1/3/4 event_loop hunks reference `commputer::peer_hash::{peer_bucket, peer_bucket_tagged, MAX_SYNC_WINDOW}` (§1.6), `commputer::block_maps::{bounded_orphan_insert, prune_*}` (§1.7), `commputer::mempool_quota::{account_quota_ok, MAX_MEMPOOL_TXS_PER_ACCOUNT}` (§1.8), `commputer::sync_machine::MAX_SYNC_GAP` (§1.9/sync_machine — note `MAX_SYNC_GAP` is added in sync_machine.rs; it is not yet in §1's list, add it there = §1 also carries the `MAX_SYNC_GAP = SYNC_BATCH_SIZE` const), `commputer::testnet_genesis::{ALPHA_FAUCET_ADDRESS_HEX, alpha_genesis_accounts}` (§1.10), and `record_peer_response`/`record_peer_height`/`recompute_network_height`/`forget_peer_height` (§1.1/§1.2). **All are non-protected Phase-0 symbols; a missing one is an intended loud compile failure.**
- The `main.rs` `RpcState` literal (§2.5) will not compile until the `RpcState` **struct** gains `faucet_wallet`/`faucet_next_nonce` (§1.14) — the atomic stage-1b pair; the tree is `E0063`-red between the two files if split.
- **NOT compile-coupled (the review's key correction — do not treat the compiler as the safety net):** the `ENFORCE` flip (§2.8), the node_state decay *effectiveness* (the :781 switch, §2.2a), and the chain-id consensus bump (§1.11). Each is a value change or a semantic rewire that links regardless. They are guarded instead by: the P4 boot assert + §4 grep; the P3 poisoning-recovery regression test; and the P5 equality test + state.rs:1078 enforcement.

**`MAX_SYNC_GAP` addendum to §1:** add `pub const MAX_SYNC_GAP: u64 = SYNC_BATCH_SIZE;` after `SYNC_BATCH_SIZE` (sync_machine.rs:19) as a Phase-0 non-protected hunk (R4 hunk 1). It reconciles finding-[1]'s loose 256 with binding E6's one-batch bound; `MAX_SYNC_TARGET_GAP = 100_000` (sync_machine.rs:41) already exists and is reused by the far-ahead reject.

**Ordering within Phase 0:** `node_state.rs` (§1.1) and `peer_hash.rs` (§1.6) must both be present before the `const _: () = assert!(MAX_SYNC_WINDOW == node_state::SANE_MAX_GAP)` compiles; `vote_aggregator.rs::set_sample_size` (§1.3) before `consensus_manager.rs` (§1.2); `Cargo.toml zeroize` (§1.13) before the `rpc.rs` helper (§1.14); `lib.rs` (§1.9) before any `commputer::block_maps|mempool_quota|peer_hash` reference. All of Phase 0 lands together as the pre-stage commit(s), then `cargo build --workspace` must be clean.

---

## §4 — VERIFICATION GATE

**Per-crate baselines (must remain green after Phase 0, and again after the protected unit):**
- `cargo test -p commputer_core` → **204** (E12 keeps it green across the flip; §1.5).
- `cargo test -p commputer-storage` → **234**.
- `cargo test -p commputer-network` → **86**.
- `cargo test -p commputer --lib` (node lib) → **56**.
- `cargo test -p commputer --bins` (node bins) → **182**.
- New Phase-0 unit gates: `cargo test -p commputer-consensus vote_aggregator`; `cargo test -p commputer consensus_manager` (all existing pass unchanged, P1); `cargo test -p commputer block_maps`; `cargo test -p commputer mempool_quota`; `cargo test -p commputer node_state` (7 old + 5 new decay tests).

**NEW targeted checks (add before launch):**
1. **Unsigned-block rejection (2-node):** with `ENFORCE=true`, a peer-relayed block with empty/invalid `(sig, key)` at height > 0 is REJECTED by `validate_block_from_peer` and the sender banned (grep the verbatim ban string `"sent block with missing/invalid producer signature"`).
2. **`network_height`-poisoning recovery (P3 regression — HARD):** inject a gossip `SnowballQuery{height:u64::MAX}` (must NOT advance the target at all — §2.1 hunk 8) AND a validly-signed EMPTY block at `tip+2000` into a 2-node net; assert `network_height` self-heals below `tip+STALE_THRESHOLD` and **both nodes resume producing within a bounded time** (proves the decay + :781 recompute switch actually un-pins Syncing).
3. **Enforcement canary:** `grep 'ENFORCE_PRODUCER_SIGNATURES: bool = true' src/core/src/block.rs` must succeed (P4).
4. **`scripts/multinode_assert.sh` at `SMOKE_NODES=2` AND `SMOKE_NODES=3` are HARD preconditions (E8):** assert **per-node applied-height CONVERGENCE**, not just an aggregate finalization count (a producer-only-finalizing regression must fail this). Per E8, if the strict finalization-count gate flakes at a contested mid-rung, switch `try_finalize_round`'s reset to fire only when `record_round` saw a quorum (documented fallback; keep the primary consume-on-any-non-empty unless the gate forces it).
5. **Extended late-join smoke (E6 point 3):** a node joining with a backlog > 10 batches must still converge (the `MAX_SYNC_GAP=10` clamp bounds gap-fill to one batch; bulk catch-up rides `sync_machine` `GetBlocks`).
6. **Strict `verify-chain`** on the freshly-produced reset chain (main.rs:1308 diagnostic now checks sigs; all reset blocks are signed).

**Do not apply the protected batch, and do not launch, until 1–6 pass.**

---

## §5 — GENESIS RESET PROCEDURE

The reset severs the stale chain via the new genesis **hash** (chain-id `commputer-testnet-2` + bumped `genesis_timestamp`, §1.11), so the old data dir must be wiped.

1. **Build** the full batch tree (Phase 0 + protected + §2.8 flip); confirm §4 all-green + both multinode gates.
2. **Data-dir wipe (manual `rm -rf`, no code change):** the testnet data dir is `config.rs::data_dir(true)` = `~/.commputer/testnet` (verified unchanged; the wipe is operational). Wipe it on the seed AND every other node. Targets are the RocksDB CFs under that dir (blocks, accounts, the 4 pouw maps, lifecycle CFs). `create_genesis()` = `create_genesis_for_dir(None)` builds from the compiled `default_genesis()`; the run path never reads root `genesis.json` unless a copy sits *inside* the data dir — ensure none does.
3. **Chain-id/genesis-hash implication:** because `TESTNET_CHAIN_ID` is in the signed header and enforced at state.rs:1078, every node MUST run the new binary; a mixed old/new fleet forks immediately (intended — that is the severance). Old-chain signatures are not replay-valid under the new genesis hash.
4. **Faucet:** founder generates the faucet wallet OFFLINE (E11), pastes its address into `ALPHA_FAUCET_ADDRESS_HEX` (§1.10) and, if `genesis.json` is kept, the matching `accounts` entry (§2.7); sets `COMMPUTER_FAUCET_SEED` on **exactly one** node (the single-provisioner invariant; two provisioned nodes collide nonce counters). The P8 boot check refuses to bind on a seed/address mismatch.
5. **Order:** build → wipe seed → **start seed first** (it mints block 0 with the compiled genesis + faucet allocation credited BEFORE apply_block) → wipe + start the other nodes (they sync from the seed) → confirm cross-node applied-height convergence + a successful faucet dispense.

---

## §6 — FOUNDER APPROVAL CHECKLIST

**Phase 0 (non-protected) — I implement + test before you approve the protected phases.** Approve that Phase 0 landed clean (build + all §4 baselines) as the precondition. Then approve, region-by-region:

- [ ] **Approval item 1 — Phase 1 (R1, `event_loop.rs`):** producer-sig Stage 1c + E4 height-0 reject; `network_height` §0 (advance-only-after-validate, no-block queries removed); E2 delete of both legacy gossip vote arms; VoteAggregator feed sites; E9 rekeys via `peer_hash::peer_bucket`; [16] PeerExchange cap. *(15 hunks)*
- [ ] **Approval item 2 — Phase 2 (P3 decay wiring, `event_loop.rs`):** the `:781` `set_network_height → recompute_network_height` switch + `forget_peer_height` at peer-removal sites + `record_peer_height` feeds. *(2 hunks + folded feeds)*
- [ ] **Approval item 3 — Phase 3 (R2+R4 sync/ingest, `event_loop.rs`):** `handle_received_block` validate-first restructure + map prunes; the MERGED `apply_synced_block` hunk (P2); the `try_apply_finalized` twin clamp; the sync-serve rate limiter + `start+100` fix; sync_rate_limiter field/init. *(5 hunks)*
- [ ] **Approval item 4 — Phase 4 (R3 mempool, `event_loop.rs`):** `validate_shape` ingress; F-3 quota + faucet exemption + fee floor; RPC-path `enforce_mempool_limit`; unaffordable-first eviction. *(4 hunks)*
- [ ] **Approval item 5 — Phase 5 (R5 bootstrap):** `main.rs` genesis-accounts-before-apply (both paths) + provision call + RpcState fields + P4 boot assert; `config.rs` chain-id; `genesis.json` chain-id. *(7 hunks)*
- [ ] **Approval item 6 — §2.8 THE ATOMIC FLIP:** `ENFORCE_PRODUCER_SIGNATURES = true` in the same commit.
- [ ] **Approval item 7 — Reset execution (§5):** data-dir wipe + seed-first restart + faucet provisioning on one node.

**Still-open micro-decisions to settle at approval:**
1. **`MAX_SYNC_WINDOW` / `SANE_MAX_GAP` value** — both proposed **2000** (must be EQUAL, P6). Confirm or set a new shared value.
2. **Faucet address generation** — offline wallet (E11); paste into `ALPHA_FAUCET_ADDRESS_HEX` + (if kept) `genesis.json accounts [["<addr>", 10000000000000]]`. Confirm the single-provisioner node.
3. **Chain-id string** — `commputer-testnet-2` across core/genesis.rs + config.rs + genesis.json (must be byte-identical).
4. **`MAX_MEMPOOL_TXS_PER_ACCOUNT`** and the block_maps caps (20/200/10k/10k) — proposed defaults; tune before Phase 0.
5. **Keep vs drop root `genesis.json`** (E1 permits dropping; if dropped, discard §2.7).
6. **E2 residual** — also stop *emitting* the now-ignored outbound `VoteResponse`/`SnowballQuery` gossip? Safe to defer.
7. **F10/F21 zero-from guards** (§1.15) — confirm present in storage/state.rs at the reset.
