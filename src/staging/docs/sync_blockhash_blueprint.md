# A7-sync-blockhash — SyncMachine block-hash plumbing, fork-choice & rollback

Blueprint for the founder. **Read-only agent output.** Nothing here was applied to the
real tree. Companion staged code: `src/staging/sync_machine_v2.rs`.

---

## 0. TL;DR

`src/node/src/sync_machine.rs` records peer tips as **bare `u64` heights**
(`record_height(u64)`, line 106) and selects a sync target by **median of heights**
(`begin_downloading` line 127, `complete_verification` line 219). Because no block
**hash** is ever recorded against a height:

1. **fork-at-depth-N** (two peers report the same height with *different* tip hashes)
   is invisible — the median is computed over heights that secretly disagree.
2. **lying-peer** (valid-looking block, wrong hash) cannot be distinguished from an
   honest peer at the same height.
3. `complete_verification` (line 219, condition `our_height >= new_target` line 231)
   **collapses two distinct situations into "done"**:
   - we are genuinely caught up on the canonical chain, **and**
   - we are at/above the target height but on an **orphaned / minority** chain.
   There is **no rollback path** for the second case — the machine declares
   `Complete` and the node goes `Active` on a dead fork.

This is the W5.10 staging gap: the 5 scenarios (fork-at-depth, rapid-reorg,
partial-block, out-of-order, lying-peer) are **PSEUDO** in
`src/staging/sync_machine_comprehensive_tests.rs` today because the type system
cannot even express "two peers, same height, different hash."

The fix: **augment** (do not delete) the height plumbing with tip *hashes* keyed by
peer, add a pluggable **Snowball-weight** fork-choice hook, and add a **rollback
signal** out of `complete_verification` that the protected `event_loop.rs` caller
drives into the *already-existing* `ChainState::revert_to(...)`.

---

## 1. Verified ground truth (every claim has a file:line)

### 1.1 The machine (NON-PROTECTED — may be patched as a full file)
`src/node/src/sync_machine.rs`:
- `pub const SYNC_BATCH_SIZE: u64 = 10;` — line 19
- `enum SyncState { Idle, QueryHeight, Downloading, Verifying, Complete }` — lines 31-43
- `struct SyncMachine { state, target_height, height_responses: Vec<u64>, … }` — lines 46-61.
  **`height_responses` is `Vec<u64>` — the type with no room for a hash.** (line 52)
- `pub fn record_height(&mut self, height: u64)` — **line 106** (push onto `Vec<u64>`)
- `pub fn begin_downloading(&mut self, our_height) -> u64` — line 127 (median of heights)
- `pub fn next_batch(&mut self, our_height) -> Option<(u64,u64)>` — line 155
- `pub fn complete_verification(&mut self, our_height) -> bool` — **line 219**;
  the collapse is the `if our_height >= new_target { … Complete; true }` at **line 231**.
- `pub fn select_peer(&self, available: &[PeerId]) -> Option<PeerId>` — line 252
- `pub fn record_batch_failure(&mut self, peer) -> bool` — line 189

### 1.2 The sole production caller (PROTECTED — founder-only, blueprint not edit)
`src/node/src/event_loop.rs`:
- **`self.sync_machine.record_height(h);` — line 1541.** This is inside
  `SwarmEvent::Behaviour(CommpBehaviourEvent::Sync(event))` →
  `RrEvent::Message { peer, message }` (**`peer` bound at line 1479**) →
  `RrMessage::Response { response, .. }` (line 1511) →
  `SyncResponse::Height(h)` (line 1536). **`peer` is in scope at line 1541** —
  confirmed — so a `record_tip(height, hash, peer)` signature is satisfiable at the
  call site **without** restructuring the match.
- The state-machine **driver** lives at **lines 783-869** (the `match self.sync_machine.state().clone()`
  inside the sync tick). `complete_verification` is driven at **line 848**; on `true`
  it sets `sync_complete`, calls `reset()`, and `node_state.force_active()` (lines 849-852).
  **This is exactly where the rollback branch must be handled.**
- `record_height` / `record_tip` is the only sync-machine method whose **signature**
  change ripples into a protected file. All other v2 additions are additive.

### 1.3 BlockHash (core crate — stable, reusable)
`src/core/src/block.rs`:
- `pub struct BlockHash(pub [u8; 32]);` — line 15, derives
  `Debug,Clone,Copy,PartialEq,Eq,Hash,PartialOrd,Ord,Serialize,Deserialize,Borsh*` (lines 13-14)
- `BlockHash::GENESIS = Self([0u8;32])` — line 18
- `impl Display` (hex of first 8 bytes) — line 21
- Re-exported as `commputer_core::block::BlockHash` — `core/src/lib.rs:31`
- `Block::hash()` line 144, `BlockHeader::hash()` line 69, `block.header.parent_hash` line 39.

### 1.4 Consensus = Snowball-weight, NOT total-difficulty
`src/consensus/src/snowball.rs`:
- `SnowballVoter` votes over `BlockHash` (line 33), `confidence: HashMap<BlockHash,u32>`
  (line 37), `record_round(&mut self, responses: &HashMap<BlockHash, usize>) -> bool`
  (line 108). Fork choice = **the hash that reaches quorum (α, default 14/20), held for
  β consecutive rounds** (lines 113-145).
- **There is no `total_difficulty`, no `work`, no PoW field anywhere** in `block.rs`
  (`BlockHeader` lines 31-62) or the consensus crate. Grep for `difficulty`/`work`
  returns nothing structural. **Therefore fork-choice MUST be Snowball-weight based:**
  the canonical tip at a height is the **hash the most peers (by quorum weight) report.**
  The v2 hook mirrors `SnowballVoter::record_round`'s "pick the quorum choice" logic
  but operates over *peer tip reports* during sync (a lighter-weight, pre-finality
  analog used only to choose a sync target — final acceptance still flows through the
  real `SnowballVoter` in the consensus path).

### 1.5 Rollback machinery ALREADY EXISTS (do not reinvent)
`src/storage/src/state.rs` (`ChainState`, the type of `self.state` per `event_loop.rs:111`):
- `pub fn revert_block(&mut self, height: u64) -> Result<(), StateError>` — **line 1029**;
  can only revert the tip (line 1030), refuses genesis (line 1035), restores account
  balances/nonces from `state_diffs` (lines 1040-1047), reverses burn tracking
  (1049-1057), then `self.blocks.remove_at_height(height)` (line 1060).
- `pub fn revert_to(&mut self, target_height: u64) -> Result<u64, StateError>` — **line 1067**;
  reverts tip-down to `target_height` (block AT target stays), **bounded by
  `FINALITY_DEPTH`** (lines 1072-1077).
- `pub const FINALITY_DEPTH: u64 = 10;` — `state.rs:107`. (Note the consensus crate has
  a *separate* `DEFAULT_FINALITY_DEPTH = 100` at `consensus/src/finality.rs:10`; the
  one that bounds `revert_to` is the storage one, **10**.)
`src/storage/src/blockstore.rs`:
- `pub fn remove_at_height(&mut self, height: u64)` — line 96 (drops the height index
  entry + block, decrements `latest_height`).
- `get_by_height` (35), `contains` (66), `latest` (42), `height` (51) — the read surface
  the fork-choice hook needs.

**Consequence for rollback design:** a reorg deeper than `FINALITY_DEPTH = 10` is
*intentionally* impossible via `revert_to` (it returns `Err`). The v2 machine must
therefore distinguish **shallow reorg (≤10, revert + re-download)** from **deep
divergence (>10, cannot safely revert — must full-wipe-and-resync)**, which is what
the existing `ForkDetector` (`src/node/src/fork_detector.rs`, `should_resync()`
line 40) already signals. v2 returns a typed outcome so the caller picks the right tool.

### 1.6 Protocol gap — the wire can't carry a hash yet
`src/network/src/sync_protocol.rs` (NON-PROTECTED network crate):
- `enum SyncResponse { Block(Option<Vec<u8>>), Blocks(Vec<Vec<u8>>), Height(u64) }`
  — lines 26-33. **`Height(u64)` carries no hash.** The peer answering `GetHeight`
  (`event_loop.rs:1504-1508`) sends only `self.state.blocks.height()`.
- To plumb a *real* tip hash end-to-end, this variant must be **augmented**
  (add `tip_hash: [u8;32]`) or a new `Tip { height, hash }` variant added, and the
  responder at `event_loop.rs:1504` must fill it from `self.state.blocks.latest()` (line 42 of blockstore).
  This is a **separate, network-crate change** the founder makes when wiring v2;
  the blueprint flags it so the founder doesn't ship a v2 machine that's hash-aware
  but fed zero hashes. See §5 step 3.

---

## 2. Design — augment, don't replace

The v2 module **keeps the existing height-median path bit-for-bit** (so the current
W5.10 tests in `sync_machine_comprehensive_tests.rs` keep passing) and **adds** a
parallel hash-aware layer:

```
record_height(h)                 -> kept, now an alias that calls
                                    record_tip(h, BlockHash::GENESIS-sentinel, synthetic-peer)
                                    so legacy callers compile unchanged during migration.
record_tip(height, hash, peer)   -> NEW. stores PeerTip{height,hash,peer} in tip_reports
                                    AND pushes height into height_responses (legacy median
                                    still works).
```

### 2.1 New data
```rust
struct PeerTip { height: u64, hash: BlockHash, peer: PeerId }
// per-machine, cleared on the same boundaries height_responses is cleared:
tip_reports: Vec<PeerTip>,
```

### 2.2 Pluggable fork-choice hook (Snowball-weight)
```rust
pub trait ForkChoice {
    /// Given all peer tip reports at the current verify round and our own tip,
    /// return the (height, hash) the network has the most Snowball weight behind,
    /// or None if no quorum is observable.
    fn choose(&self, reports: &[PeerTip], our_tip: Option<(u64, BlockHash)>) -> Option<(u64, BlockHash)>;
}
```
Default impl `SnowballWeightForkChoice { quorum: usize }`:
- Group reports by `(height, hash)`, count distinct peers (a peer's repeated reports
  count once — mirrors that Snowball samples *peers*, `snowball.rs:93-103`).
- The winning tip is the **highest height whose hash is backed by ≥ quorum peers**;
  among ties at a height, the hash with the most peer weight (the quorum choice,
  `snowball.rs:113-118`).
- `quorum` defaults to `SnowballParams::default().quorum` (14) but is clamped to the
  observed peer count for small testnets (so a 3-peer testnet isn't frozen),
  matching the stepped-curve note in MEMORY (`SnowballParams::production` at peer≥21).

This is *pluggable*: a test injects a `FixedForkChoice` to force a specific decision;
production injects `SnowballWeightForkChoice`. **No total-difficulty path is offered
because the protocol has no difficulty** (§1.4).

### 2.3 Rollback-aware verification outcome
`complete_verification` is **superseded** by `complete_verification_v2`, which returns a
typed verdict instead of a bare `bool`:
```rust
pub enum VerifyOutcome {
    /// Caught up on the canonical chain. Go Active. (== old `true`)
    Complete,
    /// Behind. Keep downloading toward `new_target`. (== old `false`)
    KeepDownloading { new_target: u64 },
    /// We are at/above target height but our tip hash != the network-chosen tip hash
    /// at a common height. We are on an orphan. Caller must revert to `fork_point`
    /// then re-download. fork_point is chosen so depth <= FINALITY_DEPTH when possible.
    Rollback { fork_point: u64, canonical_tip: BlockHash, depth: u64 },
    /// Divergence deeper than FINALITY_DEPTH — revert_to() would refuse. Caller must
    /// wipe and full-resync (ForkDetector::should_resync path).
    WipeAndResync { canonical_tip: BlockHash },
}
```
The critical new branch: even when `our_height >= target_height` (old line 231 "done"),
v2 first asks the fork-choice hook for the canonical `(height, hash)`. If our recorded
tip hash at that height disagrees, we emit `Rollback`/`WipeAndResync` instead of
`Complete`. **This is the orphaned-chain case the old code silently mis-classified.**

`fork_point` selection: the deepest height H where our hash and the canonical hash
still agree (or, when unknown because the wire didn't carry historical hashes, the
conservative `target_height - 1` capped by `FINALITY_DEPTH`). `depth = our_height - fork_point`.
If `depth > FINALITY_DEPTH (10)` → `WipeAndResync` (because `revert_to` would `Err`).

---

## 3. How the 5 W5.10 scenarios become expressible (and tested)

All five are PSEUDO today purely because `Vec<u64>` can't hold a hash. With
`record_tip` + `tip_reports` + a `ForkChoice`, each becomes a real assertion. The
staged tests in `sync_machine_v2.rs` (module `w5_10_scenarios`) implement them:

| # | Scenario | What was impossible before | What the v2 test asserts |
|---|----------|----------------------------|--------------------------|
| 1 | **fork-at-depth-N** | Two peers at height 50 with different hashes were indistinguishable | Record tips `(50, hashA, peerA)` and `(50, hashB, peerB)`; assert fork-choice surfaces the quorum hash and `complete_verification_v2` does NOT blindly Complete when our tip == the minority hash |
| 2 | **rapid-reorg** | Median jumped between rounds but hash churn was invisible | Two verify rounds: round 1 canonical = hashA@100, round 2 = hashB@100 from a fresh quorum; assert outcome flips to `Rollback`/`KeepDownloading` and the machine does not declare Complete on the stale hash |
| 3 | **partial-block** | A peer reporting height N but unable to serve block N looked identical to an honest tip | Record a tip at height N, then `record_batch_failure(peer)` to MAX; assert that peer is excluded by `select_peer` and its tip is dropped from fork-choice weight |
| 4 | **out-of-order** | Heights arriving non-monotonically only affected a sort | Record tips out of order `(30,_),(10,_),(20,_)`; assert fork-choice still picks the highest quorum-backed tip, not insertion order |
| 5 | **lying-peer** | A valid-looking block with the wrong hash at a real height couldn't be flagged | Record honest quorum `(50, good)` from k peers and one `(50, evil)`; assert fork-choice rejects `evil` (below quorum) and, if our local tip == `evil`, outcome is `Rollback{ canonical_tip: good }` not `Complete` |

Each test injects a deterministic `ForkChoice` and uses real `BlockHash` values
(`BlockHash([n;32])`) and real `PeerId::random()` peers — no tautologies.

---

## 4. Exact anchors for the founder (sync_machine.rs)

When porting `sync_machine_v2.rs` content into the real `src/node/src/sync_machine.rs`:

| Insert / change | Anchor in current sync_machine.rs |
|-----------------|-----------------------------------|
| Add `use commputer_core::block::BlockHash;` | after the `use libp2p::PeerId;` at **line 15** |
| Add `PeerTip` struct + `tip_reports: Vec<PeerTip>` field | struct def **lines 46-61**; init in `new()` **lines 66-74**; clear it everywhere `height_responses.clear()` appears: lines 95, 143, 161, 229, and in `reset()` 267 |
| Add `record_tip(height, hash, peer)`; convert `record_height` to delegate | replace/augment **line 106** |
| Add `ForkChoice` trait + `SnowballWeightForkChoice` + a `fork_choice: Box<dyn ForkChoice>` field (or generic param) | new items; field initialized in `new()` (**lines 65-75**) with the Snowball default |
| Add `complete_verification_v2(&mut self, our_height, our_tip) -> VerifyOutcome` alongside the existing `complete_verification` | next to **line 219** (keep the old one for back-compat, or have it call the v2 and map `Complete`→`true`, everything else→`false`) |
| Keep `VerifyOutcome` enum | new public type |

**Migration-safe approach (recommended):** keep `record_height` and
`complete_verification` as thin shims so the project still compiles the instant the
v2 types are added, then flip the protected caller (§5) in a second, isolated commit.

---

## 5. Founder wire-in (includes the PROTECTED event_loop.rs steps)

**Step 1 — land v2 types (non-protected).** Port `sync_machine_v2.rs` additions into
`src/node/src/sync_machine.rs`. Compiles standalone; old tests stay green.

**Step 2 — augment the wire so a real hash exists (non-protected, network crate).**
In `src/network/src/sync_protocol.rs:32`, change
`Height(u64)` → `Height { height: u64, tip_hash: [u8; 32] }` (or add `Tip { height, hash }`).
This is a **breaking wire change** — bump the protocol string `SYNC_PROTOCOL`
(`sync_protocol.rs:11`, currently `"/commputer/sync/1"` → `"/commputer/sync/2"`) so old
and new nodes don't silently mis-parse. (Pre-testnet, no deployed peers — safe.)

**Step 3 — fill the hash on the responder side (PROTECTED event_loop.rs).**
At **event_loop.rs:1504-1508** (`SyncRequest::GetHeight`), replace
`SyncResponse::Height(self.state.blocks.height())` with the new variant carrying
`self.state.blocks.latest().map(|b| b.hash().0).unwrap_or([0;32])`.

**Step 4 — feed the hash into the machine (PROTECTED event_loop.rs).**
At **event_loop.rs:1536-1541**, the `SyncResponse::Height(h)` arm becomes the new
variant `{ height, tip_hash }`; replace
`self.sync_machine.record_height(h);` with
`self.sync_machine.record_tip(height, BlockHash(tip_hash), peer);`
(`peer` is already in scope from line 1479 — confirmed). **This single line is the
entire signature ripple the task warns about.**

**Step 5 — handle the rollback verdict (PROTECTED event_loop.rs, the important one).**
At the `SyncState::Verifying` branch, **event_loop.rs:847-854**, replace the
`if self.sync_machine.complete_verification(our_height) { … force_active }` with a
match on `complete_verification_v2(our_height, our_tip)`:
```text
VerifyOutcome::Complete           => sync_complete=true; reset(); node_state.force_active();
VerifyOutcome::KeepDownloading{..} => (fall through; next tick re-downloads — old `false` path)
VerifyOutcome::Rollback{ fork_point, .. } => {
        match self.state.revert_to(fork_point) {      // storage/state.rs:1067
            Ok(n)  => { info!("reorg: reverted {n} blocks to {fork_point}"); /* stay in Downloading; re-query */ }
            Err(e) => { warn!("revert refused: {e}"); /* escalate to wipe path */ }
        }
}
VerifyOutcome::WipeAndResync{..} => { self.fork_detector.record_mismatch(); /* trigger existing wipe-and-resync */ }
```
`our_tip` = `self.state.blocks.latest().map(|b| (b.height(), b.hash()))`.

**Step 6 — port the W5.10 tests** from `sync_machine_v2.rs` module `w5_10_scenarios`
into `sync_machine.rs`'s `#[cfg(test)]` block (or a `tests/` integration file), and
retire the PSEUDO placeholders in `sync_machine_comprehensive_tests.rs`.

**Step 7 — `cargo test -p commputer sync_machine` and
`cargo test -p commputer-storage revert` to confirm both halves.**

---

## 6. Risk / scope notes

- The **only** protected-file edits are 3 lines in `event_loop.rs` (steps 3, 4, 5) — a
  responder fill, a `record_height`→`record_tip` swap, and the Verifying-branch match.
  Everything else (sync_machine.rs, sync_protocol.rs) is non-protected.
- `revert_to` is **already tested** in storage (`state.rs:2623-2721`:
  `revert_block_restores_balance`, `revert_to_multi_block`,
  `revert_beyond_finality_depth_fails`, `revert_wrong_height_fails`) — the rollback
  primitive is trustworthy; v2 only *decides when to call it*.
- `FINALITY_DEPTH = 10` is the hard ceiling on automatic reorg. Anything deeper is
  `WipeAndResync` by construction — this is a feature (matches the existing
  `ForkDetector` threshold semantics), not a limitation to engineer around.
- v2 does **not** change finalized-block acceptance — that still flows through the real
  `SnowballVoter` in the consensus path (`event_loop.rs:1582-1585`). The sync-time
  fork-choice is only for picking a *download target*, which is why a lightweight
  peer-tip-weight analog (not the full β-round voter) is appropriate.
