# Phase 1.2 WIRING SPEC — the coordinated PoUW live-flip (design + review + amendments)

**Status:** architecture COMPLETE + 3-lens adversarially reviewed (determinism/consensus-safety,
integration/liveness, protected-minimality — all APPROVE_WITH_CHANGES; 2 blockers [same root] + 6 majors
+ 7 minors folded into the BINDING AMENDMENTS C1–C9 at the end, which OVERRIDE the design body).
Base branch `agent-flip-20260705` @ `2f792ff` (step 1.0 + Phase 1.1 landed).

## STAGING DECISION (orchestrator, 2026-07-06) — the flip is built in two stages on the same branch
The design's §5 sequencing splits naturally by protected-ness. We build:
- **Stage 1.2a = NON-PROTECTED apply-path core (the fork-safety heart; build NOW, fully testable):**
  B8 params substrate (core GenesisConfig schema + `set_consensus_params` WITH C1 lifecycle re-injection +
  `open()` defaults preserved); B5 pouw-onchain methods (`post_result`/`draw_committee`/accessors, frozen
  `select_committee` untouched); B5 state.rs wiring (`lifecycle_post_result` helper, `CompleteJob` arm
  top-level+Batch, `draw_committees_for_completed_jobs` in the shared apply tail); M2 (apply_claim_job
  reject-unknown); M1 (rocks.rs fail-hard on undecodable consensus rows); the 1.2-MEMPOOL SOUNDNESS
  HELPER `select_applicable_txs` (C2/C3, non-protected logic); B9 (delete stale src/genesis.json).
  Every piece is non-protected, deterministic, unit+determinism testable. This is where ALL fork-relevant
  logic lives (per §0: committee-draw determinism is a safety invariant).
- **Stage 1.2b = PROTECTED plumbing (founder-gated, per-change approval; build AFTER 1.2a verifies):**
  1.2-POOL (SubmitJobV2 arm in process_job_tx); 1.2-MEMPOOL event_loop call + kind-aware ingress
  pre-filter (incl. ClaimJob per C7); B7 admit call in block assembly + the new `capacity_params` field
  (bigger than the design's "thin call" — see C8); B6 tick → observe/log only; B8 `main.rs`
  genesis.json LOAD on the node run-path (the design's "1–2 line setter" is under-scoped — the node
  currently never reads genesis.json, C6) + `set_consensus_params` call + `refuse_to_bind` hard gate;
  N2 cfg(unix) guards. Then the §1.4 verification gate + local multinode.
- **DEFERRED (separate networked deliverable, NOT the flip — C5):** the real libp2p Kademlia+req/resp DA
  backend, an executor auto-claim/execute loop, a verifier Commit/Reveal loop. Until these land, the
  production money path is consensus-SAFE and conserving but every pot REFUNDS (Confirmed/Disputed only
  reachable under test/RPC injection). The flip is the on-chain foundation; these three actors drive it.

---

I have everything I need. Confirmed: `submit_result` has zero production callers (B5 genuinely unwired), `block.verify_roots()` checks only tx/proof roots (NOT state_root), `slashed_validators` is node-local in `consensus_manager.rs`, churn is stubbed to 0, genesis params are loaded but never threaded into `ChainState`, and there is no executor auto-claim loop. Here is the full architecture design.

---

# Phase 1.2 Architecture & Implementation Design — the coordinated PoUW live-flip

Branch `agent-flip-20260705` @ `2f792ff`. All line anchors verified against the working tree this session. This design resolves the CENTRAL QUESTION with code evidence, then specifies each piece.

## 0. The governing finding (drives every split decision)

`block.verify_roots()` (`src/core/src/block.rs:203`) checks **only** `tx_root` and `proof_root` — **not** `state_root`. The block producer sets `header.state_root = self.state.compute_state_root()` on the *pre-apply* (parent) state (`event_loop.rs:2763`), and no apply path re-derives/compares a post-state root. So a state-root disagreement does **not** reject a block directly. It bites in two deferred, fatal ways:

1. **Checkpoint divergence** every `CHECKPOINT_HASH_INTERVAL` (`event_loop.rs:2766`) and in `multinode_assert.sh`.
2. **Real consensus fork via the membership gate.** `JobLifecycle::record_commit` rejects a non-member (`lifecycle.rs:496` `committee.contains(&c.verifier)`). If two nodes draw *different* committees, then a later block carrying a `Commit` from validator X applies `Ok` on the node that put X in the committee and `Err` (whole-block reject, `state.rs:1697`) on the node that did not → the two honest nodes now disagree on whether height H is valid → **hard fork**.

**Therefore determinism of the committee draw is a safety invariant, independent of whether the root is checked.** Any per-node input (message arrival order, wall-clock, HashMap iteration, `consensus.slashed_validators`) feeding the draw = fork. This is exactly the P8 precedent, and it forces the answer to the CENTRAL QUESTION.

---

## 1. THE PROTECTED / NON-PROTECTED SPLIT TABLE

| Piece | Lands in (exact file) | PROTECTED? | Justification (with code evidence) |
|---|---|---|---|
| **B5 committee draw** | `src/storage/src/state.rs` — new `draw_committees_for_completed_jobs(block_hash)` in the shared apply tail + `CompleteJob` tx arm; + two methods in `src/staging/pouw-onchain/src/lifecycle.rs` | **NO** | The draw is fork-relevant determinism → by §0 it must be unit-testable and seeded by block-level data. Its every input is consensus state: seed = `block.hash()` (fixed per block, node-independent), candidates snapshotted at ClaimJob and already in the state root (`lifecycle.rs:302,410`), `stake_of` = `self.bonded_stake` (consensus map), `k` = genesis param. `select_committee` is a pure frozen fn (`committee.rs:5`, sorts by `(ticket, id)`). This is byte-for-byte the P8 pattern (`settle_due_jobs(block.height())`, `state.rs:735`). `submit_result` has **zero production callers** today (grep: only tests) so nothing in `event_loop.rs` needs to move — it was never there. **Resolves CENTRAL QUESTION: B5 runs in-apply in state.rs.** |
| **B6 timeout tick** | `src/node/src/event_loop.rs` job-timeout arm (`:876`) | **YES** (observe/log only) | Settlement already runs deterministically in-apply via `settle_due_jobs` inside the rollback envelope (`state.rs:735`). B6 must NOT settle from the wall-clock tick: two nodes tick at different heights → per-height root divergence → fork (R2 in the 1.1 spec, already decided). Re-scope B6 to metrics/log only; PROTECTED footprint shrinks to ~logging. |
| **B7 capacity admission** | `src/node/src/event_loop.rs` `handle_block_tick` (`:2707–2733`); decision logic already in `pouw-onchain/capacity.rs`; glue `pending_job_from_tx` in `state.rs` | **YES** (thin scheduling) + **optional** non-protected apply-side cap | Admission *schedules which mempool txs to include* — an action with no state.rs equivalent (state.rs never sees the mempool). It is **not** an apply-enforced consensus rule, so it cannot fork (a produced block is validated by `apply_block_validated`, which has no capacity check). The 51/49 math is the tested pure `capacity::admit` (`capacity.rs:93`); the event_loop call is thin plumbing. **Resolves CENTRAL QUESTION for B7: the *scheduler* must stay in the producer; only a decision to make the flagship split *hard* (apply-enforced) would move a coarse cap into state.rs — offered as optional below.** |
| **1.2-POOL** (V2→pool arm) | `src/node/src/event_loop.rs` `process_job_tx` (`:2245` catch-all) | **YES** (thin) | `process_job_tx` has a `SubmitJob` (V1) arm but the `_ => {}` swallows `SubmitJobV2` → V2 jobs (the only escrowing kind) never enter `job_pool` → no executor sees them. Node-local pool population, not consensus → cannot fork. Minimal PROTECTED arm mirroring V1. |
| **1.2-MEMPOOL** (DoS fix) | `src/node/src/event_loop.rs` `handle_block_tick`; soundness helper `select_applicable_txs` in `src/storage/src/state.rs` | **YES** (thin) — logic **NON-PROTECTED** | Post-B4 a fee-priced junk `Commit` passes `validate_tx_for_mempool` (sig/nonce/fee only, `:2109`) but is a deterministic apply-`Err`; the producer never trial-applies its own candidate (`handle_block_tick` signs + broadcasts without applying, `:2774`), so the whole finalized block is rejected by everyone at `try_apply_finalized` (`:2982`) → permanent height stall = zero-cost DoS. The **soundness engine lives in state.rs** (`select_applicable_txs`: clone, greedily trial-apply, keep only clean txs — testable, complete); event_loop just calls it. |
| **B8 genesis params** | ROOT `genesis.json` + `src/core/src/genesis.rs` (`GenesisConfig`, **non-protected**) + a non-protected `ChainState` setter; **one** call site in `src/node/src/main.rs` | **genesis.json + main.rs call = YES**; schema + loader + setter = **NO** | Params must be consensus-identical (`state.rs:187,201` "all nodes MUST agree or they diverge"). The `TODO(B8)` load path is `state.rs:360–366` (defaults today). Extend the *non-protected* core `GenesisConfig` + add `ChainState::set_consensus_params(...)`; the only PROTECTED edit is a 1–2 line call in `main.rs` after `ChainState::open` (`:456/:903`). |
| **M1 open() fail-hard** | `src/storage/src/rocks.rs` `all_escrow/all_bonded/all_unbonding/all_lifecycle/all_pending` + `state.rs::open` | **NO** | Startup-only; no consensus effect. Currently these warn-skip undecodable rows (`rocks.rs:418,449,483,548`); post-P8 a skipped consensus row → pot-guard mismatch → every block rolls back forever, surfaced only as a startup warn. |
| **B9 delete stale genesis** | delete `src/genesis.json` | **NO** (not in protected list; founder-approved deletion) | `src/genesis.json` uses an incompatible schema (nested `emission`/`channel_floors_bps`) vs the flat root `genesis.json` that `core::genesis::load_genesis` parses (`genesis.rs:73`); a node whose `data_dir` resolves to `src/` silently `default_genesis()`-fallbacks (`main.rs:373`) → wrong params/hash → peers reject. Root `genesis.json` is canonical & PROTECTED (untouched). |
| **N2 cfg(unix)** | `src/node/src/event_loop.rs:666–668, 681–689` | **YES** (2 lines) | `tokio::signal::unix::signal` is Unix-only; gates Windows builds. Pure `#[cfg(unix)]` guard, no logic change. |
| **M2 expire-then-claim** | `src/storage/src/state.rs` `apply_claim_job` (`:1605`) | **NO** | The unknown/expired-id branch is a silent legacy no-op accept (`:1605–1607`). Fix is a deterministic apply-arm change (reject unknown id, or receipt marker) — non-protected, part of the consensus-format flip. |
| **1.2-DA transport** | `src/staging/pouw-onchain/src/da_transport.rs` (in-process backend exists); **real libp2p backend = separate networked deliverable** | backend wire-in **YES** (event_loop/swarm), but **out of scope this pass** | `BridgeTransport` dispatches `DaCommand` to an async backend that **does not exist** for production (`da_transport.rs:4–12` WIRE-IN note; only `spawn_backend` test/in-thread exists, `:147`). Without it real verifiers Abstain → all jobs NoQuorum. Scope: build the money path on the in-process backend now; defer real libp2p to a networked deliverable (§4). |

**Net PROTECTED footprint of the whole flip:** B6 (log-only), B7 (thin `admit` call), 1.2-POOL (one match arm), 1.2-MEMPOOL (one call to a state.rs helper), N2 (2 `cfg` lines), B8 (1–2 line setter call), + the DA backend (deferred). Everything fork-relevant is in non-protected `state.rs` / `pouw-onchain`. This is the design goal achieved.

---

## 2. B5 COMMITTEE DRAW — exact in-apply design

### 2.1 Why in-apply, and the seed
`block.hash()` is derived from the block header + tx/proof roots (`block.rs:85`), all fixed once the block is produced; every node applying the same finalized block computes the identical hash, with **no dependence on node-local state**. It is available in the shared tail: `apply_txs_with_rollback(block)` already holds `block` and passes `block.height()` to `settle_due_jobs` (`state.rs:735`). So `block.hash()` is a perfect, node-identical seed at exactly the point P8 already proved is deterministic.

**Per-job seed (recommended refinement):** `seed_j = hash_parts(&[&block.hash().0, &job_id])` using the frozen `ids::hash_parts` (`pouw/src/ids.rs:10`). This de-correlates jobs sharing a block and still honors the founder lock ("post-result block hash"). **Grind risk (R-B5, document, do not block):** the block producer influences `block.hash()` and can grind it to bias selection; mixing `job_id` forces simultaneous grinding of all jobs but does not eliminate it. A VRF / delayed-beacon is the post-flip hardening; the founder locked block-hash for v1.

### 2.2 Two new `pouw-onchain` methods (frozen `select_committee` untouched)
`submit_result` (`lifecycle.rs:463`) atomically validates + sets `executor_hash` + draws + advances to `Committing`. We must split it because the tx arm knows the result but not `block.hash()`, and the tail knows `block.hash()` but must not re-anchor the window. Add to `JobLifecycle` (non-frozen `lifecycle.rs`):

```rust
/// B5 step 1 (tx arm, parent height): validate + record the result. Does NOT draw / advance.
pub fn post_result(&mut self, executor: ParticipantId, result_hash: [u8;32], height: u64) -> EventResult {
    if self.phase != Phase::AwaitingResult { return Rejected(WrongPhase); }
    if executor != self.executor      { return Rejected(NotExecutor); }
    if height > self.deadlines.result_by { return Rejected(PastWindow); }   // parent-height anchor (G-F)
    self.executor_hash = Some(result_hash);
    Accepted
}
/// B5 step 2 (block tail, block.hash() seed): draw the committee, advance to Committing.
/// NO window recheck (already validated at post_result) — avoids the applied-height double-anchor bug.
pub fn draw_committee(&mut self, seed: [u8;32], stake_of: &dyn Fn(&ParticipantId)->u64) {
    if self.phase != Phase::AwaitingResult || self.executor_hash.is_none() || !self.committee.is_empty() {
        return; // idempotent / nothing to draw
    }
    self.committee = select_committee(&seed, &self.candidates, &self.executor, self.params.k, stake_of);
    self.phase = Phase::Committing;
}
```

**Why not call `submit_result` in the tail:** it re-checks `height > result_by` (`lifecycle.rs:477`) at the applied height (`parent+1`), one greater than the tx's parent-height check; a result posted exactly at `result_by` would then draw `PastWindow` and wrongly time out. `post_result`/`draw_committee` anchor the window once (parent height, matching every other tx arm) and keep the draw window-free.

### 2.3 `CompleteJob` tx arm (state.rs, replaces the nonce-only stub `:1364`)
```rust
TxKind::CompleteJob { job_id, result_hash } => {
    if tx.from.is_zero() { return Err(InvalidBlock("zero address cannot complete jobs")); } // P3
    let height = self.blocks.height();                                   // G-F parent height
    match self.lifecycle_post_result(*job_id, ParticipantId(tx.from.0), *result_hash, height)? {
        Some(EventResult::Accepted)      => {}
        Some(EventResult::Rejected(r))   => return Err(InvalidBlock(format!("complete rejected: {r:?}"))),
        None                             => return Err(InvalidBlock("complete: unknown job")),
    }
    self.accounts.get_or_create(tx.from).nonce += 1;
}
```
`lifecycle_post_result` is a state.rs borrow-dance helper (mirror of `lifecycle_record_reveal`, `state.rs:3188`): `remove` the lifecycle, call `post_result`, re-insert, return the result. No money moves. Batch arm (`:1529`) routes to the same helper (no nonce). **This flips CompleteJob accept→reject for unknown/wrong-phase/wrong-executor/past-window — a consensus-format change in the coordinated flip.**

### 2.4 The block-level draw step (state.rs, shared tail)
Insert into `apply_txs_with_rollback`'s closure (`state.rs:727–736`), **between the tx loop and `settle_due_jobs`**, inside the rollback envelope:
```rust
for tx in &block.transactions { self.apply_transaction(tx)?; }
self.draw_committees_for_completed_jobs(block.hash());   // B5 — money-free, deterministic
self.settle_due_jobs(block.height())                     // P8
```
```rust
fn draw_committees_for_completed_jobs(&mut self, block_hash: BlockHash) {
    let mut jobs: Vec<[u8;32]> = self.job_lifecycles.iter()
        .filter(|(_, l)| l.phase() == Phase::AwaitingResult
                      && l.executor_hash_is_set()      // small accessor added to JobLifecycle
                      && l.committee().is_empty())
        .map(|(k,_)| *k).collect();
    jobs.sort_unstable();                               // HashMap order must never reach consensus
    for job_id in jobs {
        let seed = commputer_pouw::ids::hash_parts(&[&block_hash.0, &job_id]);
        // borrow dance: remove → draw_committee(seed, |p| self.stake_of(&Address(p.0))) → re-insert
        let mut life = self.job_lifecycles.remove(&job_id).unwrap();
        {
            let chain = &*self;                         // immutable stake reads
            life.draw_committee(seed, &|p| chain.stake_of(&Address(p.0)));
        }
        self.job_lifecycles.insert(job_id, life);
    }
}
```
(The `&*self` immutable borrow for `stake_of` while `life` is owned out of the map avoids the mutable-aliasing conflict — same shape as `lifecycle_record_commit`'s pre-check at `state.rs:3176`.)

### 2.5 Candidate enumeration, filter, exclusion — already correct at ClaimJob
Candidates are **snapshotted at ClaimJob** (`apply_claim_job`, `state.rs:1624–1636`), not at draw: sorted `Vec<ParticipantId>` filtered by `is_validator && compliance==Compliant && addr != executor && !is_zero && bonded_stake >= stake_params.min_bond`, `sort_by(|x,y| x.0.cmp(&y.0))`. This is the exact `is_eligible` filter (`state.rs:3081`) with the executor + zero-address exclusions, over **finalized on-chain state only** — never `consensus.slashed_validators` (`consensus_manager.rs:163`, node-local). It is already in the persisted DTO + state root (`lifecycle.rs:402`, root fold `state.rs:618–627`). `select_committee` additionally excludes the executor again defensively (`committee.rs:14`).

### 2.6 Empty / insufficient candidates (deterministic)
`select_committee` takes `min(count, candidates.len())` (`committee.rs:23`) — fewer than `k` eligible ⇒ a **smaller committee**, not an error. Downstream that is self-consistent and conserved: `record_commit` only admits members (`lifecycle.rs:496`); `settle` computes `quorum(k)` on the fixed `k` (`lifecycle.rs:585`), so an undersized committee that cannot reach quorum ⇒ NoQuorum ⇒ `Terminal::Escalate` ⇒ D2 zero-comp fallback (`settlement_resolution.rs:130`) ⇒ full refund, pot→0. **Empty candidate pool** (pre-bonding) ⇒ empty committee ⇒ immediate NoQuorum path ⇒ conserved refund. No panic, no strand (R7 accepted; the zero-comp fallback removes the empty-committee profit that a comp'd fallback would have created).

### 2.7 How B4 reads it
Unchanged: `record_commit` checks `self.committee.contains(&c.verifier)` (`lifecycle.rs:496`), populated by `draw_committee` in the CompleteJob block's tail. A `Commit` in the *same* block as CompleteJob still sees an empty committee (tx loop runs before the tail) → rejected → deferred to a later block by the mempool speculative-apply (§3.4). Correct: the commit window opens only after the result block finalizes.

---

## 3. B6 / B7 / 1.2-POOL / 1.2-MEMPOOL / B8 / M1 / B9 / N2 / M2 — exact changes

### B6 — tick becomes observe-only (event_loop.rs:876)
Replace `self.job_pool.enforce_timeouts(...)` settlement semantics with metrics only. Keep the node-local V1 `job_pool` timeout logging if desired, but **add no ChainState settlement** here. Determinism: settlement is 100% in `settle_due_jobs` (`state.rs:798`), anchored to applied height, reproduced identically on reorg replay (same shared tail). The tick may read `self.state.job_lifecycles.len()`, count due jobs, emit gauges. **Zero consensus effect.**

### B7 — capacity admission in block assembly (event_loop.rs:2707–2733)
After the nonce-bucket candidate filter and the `(fee desc, nonce asc)` sort (`:2726`), before the `MAX_TRANSACTIONS_PER_BLOCK` split:
```rust
// Split compute-job txs from the rest; admit jobs via the tested 51/49 scheduler.
let churn = commputer_pouw_onchain::capacity::validator_churn_bps(prev_validator_count, joined, left); // v1: 0,0,0 → 0
let pending: Vec<PendingJob> = candidates.iter()
    .filter_map(commputer_storage::state::pending_job_from_tx).collect();     // §6 glue (state.rs)
let adm = commputer_pouw_onchain::capacity::admit(&self.capacity_params, churn, &pending);
let admitted: HashSet<[u8;32]> = adm.admitted.into_iter().collect();
candidates.retain(|tx| match pending_job_from_tx(tx) {
    Some(pj) => admitted.contains(&pj.job_id),   // include only admitted jobs
    None     => true,                            // non-job txs unaffected
});
// deferred jobs return to mempool (they were std::mem::take'n; push back like future_txs at :2722)
```
- `pending_job_from_tx` (state.rs free fn, 1.1 §6 — job_id = `tx.hash().0`, `is_flagship` by l2_id, priority = fee) is the only new glue; `admit`/`available_slots`/`validator_churn_bps` (`capacity.rs:93/78/147`) are tested pure fns. **Do not redefine `validator_churn_bps`** (P5 — it exists, counts-based, `prev_count==0→0`).
- **Determinism argument:** not needed for consensus (producer-side scheduling; not apply-enforced). churn = 0 in v1 (no tracking yet, `event_loop.rs:2481` `churn=0.0 // TODO`) → reserve floor 5% → deterministic-enough; a real per-epoch delta is a fast-follow that only affects scheduling fairness, never fork-safety.
- **Optional hard flagship guarantee (non-protected, if founder wants Core Principle #1 protocol-enforced):** add an apply-side coarse cap in `state.rs` — reject a block whose count of `SubmitJobV2` txs exceeds `available_slots(capacity_params, 0)` (a pure fn of genesis `CapacityParams.total_slots`, no churn needed for a *bound*). This is deterministic and testable in state.rs, and bounds the "stuff-the-block" attack; the nuanced 51/49 *ordering* stays producer-side. **Tradeoff:** it is a new consensus rule (can reject otherwise-valid blocks) and must ship inside the coordinated flip. Recommend v1 = producer-side only + risk note; add the cap only if the founder wants the guarantee hardened now.

### 1.2-POOL — SubmitJobV2 arm in process_job_tx (event_loop.rs:2245)
Add before the `_ => {}`:
```rust
TxKind::SubmitJobV2 { program_hash, resources, max_duration_secs, comme_budget, l2_id, .. } => {
    let tx_hash = tx.hash();
    self.job_pool.submit_job(PoolJob {
        job_id: PoolJobId(tx_hash.0),            // SAME id the escrow map + ClaimJob use (G-A)
        submitter: tx.from, comme_budget: comme_budget.raw(),
        cpu_cores: resources.cpu_cores, gpu_vram_mb: resources.gpu_vram_mb, ram_mb: resources.ram_mb,
        storage_mb: resources.storage_mb, bandwidth_mbps: resources.bandwidth_mbps,
        max_duration_secs: *max_duration_secs,
        job_spec_hash: *program_hash,            // program_hash is the V2 identity
        status: PoolJobStatus::Pending, submitted_height: height, l2_id: l2_id.clone(),
    });
}
```
Node-local pool only (no consensus) → cannot fork. Mirrors the V1 arm (`:2196`). **Note:** there is no executor auto-claim loop (grep confirms `process_job_tx` only *tracks* status; ClaimJob/CompleteJob txs are externally injected). The on-chain money path is fully exercised by RPC/test-injected ClaimJob+CompleteJob+Commit+Reveal; a real executor auto-claim/execute loop is part of the executor-runtime deliverable (§4), not the flip.

### 1.2-MEMPOOL — sound producer-side speculative apply (state.rs helper + event_loop call)
**Root cause:** `validate_tx_for_mempool` (`:2109`) checks only null-sender/sig/dup/memo/timelock/fee/nonce; the producer signs+broadcasts without trial-applying (`:2774`); a finalized block that fails apply is dropped whole (`try_apply_finalized:2982` → `Err` arm `:3033`) → height never advances.

**Fix (sound + complete, logic non-protected):** add to state.rs:
```rust
/// Producer-side: return the largest in-order prefix-closed subset of `candidates` that applies
/// cleanly on top of current state (so the produced block cannot fail apply). Read-only w.r.t. self.
pub fn select_applicable_txs(&self, candidates: Vec<Transaction>) -> Vec<Transaction> {
    let mut scratch = self.clone();          // one clone per block tick (== capture_pre_block cost)
    let mut kept = Vec::with_capacity(candidates.len());
    for tx in candidates {
        // trial in an isolated snapshot: apply_transaction on scratch; keep iff Ok
        let snap = scratch.capture_pre_block();
        match scratch.apply_transaction(&tx) {
            Ok(()) => kept.push(tx),
            Err(_) => scratch.rollback_to_pre_block(snap),
        }
    }
    kept
}
```
Then in `handle_block_tick`, after B7 admission and before building the block, `let txs = self.state.select_applicable_txs(txs);`. This is **sound by construction** (every kept tx applies in sequence, so the block applies) and **complete** (catches *all* deterministic apply-Errs, not an enumerated list): pre-B5 Commit/Reveal (WrongPhase/unknown), double/expired ClaimJob, V2-in-Batch, duplicate job_id, insufficient balance, etc.
- **Cheap kind-aware pre-filter (defense-in-depth, in `validate_tx_for_mempool`, PROTECTED, prevents mempool *pollution*):** reject at ingress the statically-doomed kinds — `SubmitJobV2` inside `Batch` (always `Err`, `state.rs:1517`); a `Commit`/`Reveal`/`CompleteJob` whose `job_id` has no lifecycle **and** no pending record (`self.state.job_lifecycles`/`pending_jobs` read — deterministic, read-only). Each check reads only committed state → safe. This keeps junk out of the 5000-slot mempool; the speculative apply is the correctness backstop.
- **Determinism/safety:** `select_applicable_txs` need not be identical across nodes — it only guarantees *soundness* of one producer's block; every node still re-validates via `apply_block_validated`. No fork surface. The `settle_due_jobs`/`draw_committees` tail is not re-run in the scratch (they are money-free/unreachable-guard for a well-formed prefix), but for full fidelity the helper may end with a trial `draw + settle_due_jobs` on scratch and drop the block-tail-poisoning tx if it ever Errs (belt-and-suspenders; in practice unreachable).

### B8 — genesis consensus params (schema, loader, ChainState populate, disagreement handling)
**(a) ROOT `genesis.json` additions** (canonical, PROTECTED — founder edits) — add a nested section mirroring `ConsensusParams`:
```json
"consensus_params": {
  "game": { "k": 3, "k_escalate": 5, "worker_bps": 8500, "verifier_bps": 1000, "burn_bps": 500,
            "executor_bond": ..., "verifier_bond": ..., "quorum_num": 2, "quorum_den": 3, ... },
  "resolution": { "cancel_burn_bps": 200, "timeout_submitter_comp_bps": 2000 },
  "phase_windows": { "result_blocks": 10, "commit_blocks": 10, "reveal_blocks": 10, "claim_blocks": 10 },
  "stake": { "unbonding_blocks": 100, "min_bond": ... },
  "capacity": { "total_slots": 100, "flagship_reserve_bps": 5100, "reserve_floor_bps": 500,
                "reserve_max_bps": 1500, "reserve_churn_coeff_bps": 1000 },
  "wasm_limits": { "fuel": 100000000, ... }, "min_fuel_cap": 1000000
}
```
**(b) `core::genesis::GenesisConfig`** (`genesis.rs:11`, **non-protected**): add `#[serde(default)] pub consensus_params: ConsensusParamsConfig`, a plain serde struct that mirrors the JSON. The `#[serde(default)]` **must reproduce the exact `pouw-onchain` defaults** (`PhaseWindows::default` `10/10/10/10`, `StakeParams::default` `100/1000`, `GameParams::default`, `CapacityParams::default`) so a genesis omitting the section = today's behavior byte-identically (Policy-B continuity). Add a converter producing the four `ChainState` param structs (`GameParams`, `ResolutionParams`, `PhaseWindows`, `StakeParams`) + a `pouw_onchain::ConsensusParams` for validation.
**(c) `ChainState` populate** — add a non-protected setter:
```rust
pub fn set_consensus_params(&mut self, game: GameParams, res: ResolutionParams,
                            pw: PhaseWindows, stake: StakeParams) {
    self.game_params = game; self.resolution_params = res;
    self.phase_windows = pw; self.stake_params = stake;
}
```
Also thread `game_params`/`resolution_params` into `open()`'s lifecycle reconstruction (`state.rs:365–370` currently `GameParams::default()`), so reloaded lifecycles re-inject the genesis params (the `TODO(B8)` at `:362`). Cleanest: load params first, then `from_record(rec, params.clone(), rparams)`.
**(d) The single PROTECTED `main.rs` edit** — after `ChainState::open` (`:456`, `:903`) and the genesis load (`:367`):
```rust
let cfg = core::genesis::load_genesis(&genesis_path)?; // already loaded as _genesis_config today
let (g, r, pw, st, cp) = cfg.consensus_params.into_chain_params();
cp.refuse_to_bind(&compiled_wasm_limits())            // G5 startup assert
   .map_err(|e| /* fail hard: bad genesis / engine mismatch */)?;
state.set_consensus_params(g, r, pw, st);
```
**(e) Disagreement handling (refuse-to-bind):** `ConsensusParams::refuse_to_bind` (`consensus_params.rs:197`) runs `validate()` (internal consistency, bps bounds, non-zero windows, priceability — `:152`) then compares the node's *compiled* wasmi `config_fingerprint` to the genesis-declared one and errors on mismatch. A node with a malformed or wrong-engine genesis **refuses to start** (fail-hard, actionable). There is **no runtime gossip of the fingerprint**, so cross-node agreement is achieved by **shipping the identical `genesis.json`**; a node that loads *different* params computes a different `fingerprint()` → different committees/deadlines → state-root/checkpoint divergence → detected by checkpoints (`event_loop.rs:2766`) + `multinode_assert.sh`. Recommend logging `fingerprint()` at startup (`:387` area) so operators can eyeball-match.

### M1 — open() fail-hard on undecodable consensus rows (rocks.rs + state.rs)
Convert the five `all_*` readers (`rocks.rs:405,436,469,535,+CF_PENDING`) from `warn!`+skip to `Result<HashMap<..>, StorageError>` that **propagates** on (i) a borsh decode failure of a *present* row and (ii) a malformed key width. `open()` (`state.rs:305`) already returns `Result` and threads these — bubble the error so the node exits with "consensus CF row undecodable — resync from a fresh data dir" instead of silently loading a partial map that makes P8's pot-guard reject every block forever (the documented M1 fail-STOP). **Preserve warn-skip nowhere in consensus CFs** (their schema is fixed/versioned by comment; there is no legitimately-optional consensus row). An **empty CF** stays fine (empty iterator → empty map → no error) — only a *present-but-undecodable* row fails. Genuinely-optional non-consensus data (none here) is unaffected. Non-consensus (accounts/blocks) already `.expect` (`rocks.rs:193,228`), so this only tightens the five money maps to the same standard.

### B9 — delete stale src/genesis.json
`rm src/genesis.json`. Root `genesis.json` (PROTECTED, canonical) is the flat schema `core::genesis::load_genesis` expects; `src/genesis.json` is the incompatible nested schema that, if a node's `data_dir` resolves under `src/`, triggers the silent `default_genesis()` fallback (`main.rs:373`) → wrong genesis hash → peer rejection. Also note the **dead** second loader `main.rs:1878–1902` (`#[allow(dead_code)] load_genesis_config`, `["genesis.json","../genesis.json"]`) — leave it (dead) or, as a PROTECTED cleanup, delete it; it is not on the live path (`create_genesis_for_dir` uses `dir.join("genesis.json")`).

### N2 — cfg(unix) on the two signal registrations (event_loop.rs:666, 681)
Wrap both `tokio::signal::unix::signal(...)` bindings and their `select!` arms (`:901–910` sighup; SIGTERM arm) in `#[cfg(unix)]`, with a `#[cfg(not(unix))]` fallback that parks (`std::future::pending`). No behavior change on Unix; enables Windows compilation. Own commit per the distribution blueprint.

### M2 — expire-then-claim (state.rs apply_claim_job:1605)
Current `else { return Ok(()) }` (unknown/expired id → silent no-op accept). **Recommendation:** reject unknown ids as part of the flip:
```rust
let Some(rec) = self.pending_jobs.get(&job_id).copied() else {
    return Err(StateError::InvalidBlock("claim: unknown or expired job id".into()));
};
```
This is deterministic (reads only `pending_jobs`) and non-protected. It flips ClaimJob-unknown accept→reject, symmetric with the CompleteJob/Commit/Reveal flips already in the bundle. **Impact analysis:** on-chain ClaimJob for a legacy V1 `SubmitJob` was *always* a no-op (V1's pool is node-local, never in `pending_jobs`) — so rejecting it breaks no money flow; it only turns a meaningless accept into a meaningless reject. **Caveat (M2 residual, documented):** because P8 expires a pending job at the first block past `claim_by` and txs run before the tail driver, a ClaimJob in the *same* block as the expiry-crossing still sees the record (guard can't fire) — the "claim past window" guard (`:1609`) stays defense-in-depth. If the founder prefers zero behavior change, keep the legacy accept and treat M2 as cosmetic; the reject is the cleaner end-state and I recommend it inside the coordinated flip.

---

## 4. 1.2-DA SCOPING (realistic assessment)

**Can the production libp2p `BridgeTransport` backend be built + verified this pass? No.** `da_transport.rs:4–12` states the backend is "the founder's tokio task driving the node's libp2p swarm (Kademlia + request-response)" and only the in-thread `spawn_backend(store)` (`:147`) exists. Building the real backend requires: a Kademlia DHT for chunk-provider discovery + a request-response protocol for chunk fetch, wired into the existing `network.swarm` (PROTECTED event_loop/network), and it **cannot be verified without a live multi-node network** — which this environment does not have. Shipping an unverified networked backend into the flip would violate "verify end-to-end."

**Scope decision:** treat the real DA backend as a **separate networked deliverable** (Phase 2 / post-flip), and exercise the money path **now** with the in-process transport:
- **Local single-process / multi-thread test:** use `da_transport::spawn_backend(shared_ChunkStore)` (or `InMemoryTransport`) so committee verifiers see DA-Available and actually `Commit`/`Reveal` — driving Confirmed/Disputed terminals, not only NoQuorum. The 3-node `multinode_smoke.sh` runs three node processes on loopback; for the flip's money-path assertion, back their DA with a shared in-process store (or accept NoQuorum→refund as the conserved default and assert *that* path end-to-end).
- **What the money path needs now (all satisfied without real DA):** Bond → SubmitJobV2 (escrow) → ClaimJob (lifecycle open) → CompleteJob (post_result + tail draw) → Commit/Reveal (against the drawn committee) → `settle_due_jobs` (Confirmed/Disputed/Timeout/NoQuorum→D2). Verifiers' DA-availability is the *only* thing the in-process transport stands in for.
- **What real multi-node PoUW still needs (explicit):** (1) the libp2p Kademlia + request-response DA backend; (2) an **executor runtime loop** that auto-claims pool jobs, runs `pouw_executor::execute_job` (`pouw_executor.rs:40`), and emits ClaimJob/CompleteJob txs (today these are externally injected); (3) a verifier loop that samples DA and emits Commit/Reveal. All three are node-side plumbing (PROTECTED/new node modules), independently testable against the now-live on-chain money path, and do not change consensus.

---

## 5. SEQUENCING (workspace green at each step)

Order chosen so each step compiles + tests independently, and the fork-relevant pieces land before the plumbing that depends on them.

1. **B8 params substrate (non-protected first):** `core::genesis::GenesisConfig.consensus_params` + converter + `ChainState::set_consensus_params` + `open()` param re-injection. Defaults preserve byte-identical behavior → whole suite stays green. *Independently testable* (round-trip serde, default==today).
2. **B5 pouw-onchain methods:** `post_result` / `draw_committee` / `executor_hash_is_set` accessor in `lifecycle.rs` + unit tests (draw is deterministic for a fixed seed; undersized/empty committee). Frozen `select_committee` untouched → frozen crate byte-identical. *Independent.*
3. **B5 state.rs wiring:** `lifecycle_post_result` helper, `CompleteJob` arm (top-level + Batch), `draw_committees_for_completed_jobs` in the tail. Now the draw→membership→settle chain is closed. **Interdependent with B4 (already landed) and B8 (params feed `k`/deadlines).** Test: two independent `ChainState`s apply the identical block → identical committee + identical `compute_state_root`.
4. **M2** (apply_claim_job reject-unknown) + **P6-style test updates** — small, rides B5's consensus-format bundle.
5. **M1** (rocks.rs fail-hard) — orthogonal, non-protected, *independent*.
6. **1.2-MEMPOOL soundness helper** `select_applicable_txs` in state.rs (non-protected, *independently testable*: a junk Commit is dropped, a clean block survives).
7. **PROTECTED plumbing bundle (founder per-change approval), each a thin call:** 1.2-POOL (V2 arm) → 1.2-MEMPOOL call + kind-aware pre-filter → B7 admit call → B6 log-only rescope → B8 `main.rs` setter+refuse_to_bind → N2 cfg guards.
8. **B9** delete `src/genesis.json` (+ optional dead-loader cleanup).
9. **1.4 verification gate** (§6). DA backend + executor/verifier loops = separate post-flip deliverable (§4).

**Interdependency map:** B5-draw ⇄ B4-membership (draw must precede any accepted Commit) ⇄ B8-params (`k`, `min_bond`, phase windows). These three must be coherent in the same flip. Everything else (M1, 1.2-MEMPOOL, 1.2-POOL, N2, B9) is independently landable.

---

## 6. VERIFICATION-GATE PLAN (THE MAP §1.4, extended)

**B10 golden-equivalence (extend the existing `run_on_both` harness, `state.rs:3546+`):**
- New terminal case exercising the **full B5 draw on-chain**: open via real SubmitJobV2→ClaimJob blocks, CompleteJob block (draw in tail), Commit/Reveal blocks, `settle_due_jobs` → assert the ChainLedger terminal field-for-field == staging `EscrowLedger` reference, per-participant end balances, pot==0, conservation on both. Must include: Confirmed, Disputed, Timeout (no CompleteJob), NoQuorum→D2 zero-comp, **and the forfeiture (commit-no-reveal) variant** (P2/P10e).

**New determinism tests (the fork-safety core):**
- **Two-independent-nodes-same-block:** build one finalized block containing a CompleteJob; apply it to two independently-constructed `ChainState`s (different HashMap insertion histories for accounts/bonded_stake) → assert **identical drawn committee** and **identical `compute_state_root()`**. Repeat with the block hash perturbed → committee changes (seed-sensitivity).
- **Committee-in-root:** assert `compute_state_root` changes when the committee changes (it folds `to_record().committee`, `state.rs:623`), and that a node that mis-drew (inject a wrong committee) diverges — proving the root actually commits to the draw.
- **Candidate-order independence:** two states with the same eligible set inserted in different orders → identical sorted candidate snapshot at ClaimJob → identical committee.
- **P8 already-covered but re-assert:** same blocks → same settle heights → same root; reorg replay reproduces.

**Local multinode script exercise (`multinode_smoke.sh` + `multinode_assert.sh`):**
- 3 nodes on loopback: block sequence Bond → ValidatorRegister → SubmitJobV2 (escrow) → ClaimJob (open) → CompleteJob (draw) → Commit×committee → Reveal×committee → settle. Assert **cross-node state-root agreement at every height** and matching checkpoint hashes.
- **Kill/restart** one node mid-lifecycle (between CompleteJob and Reveal) → on restart `open()` reloads pot + pending + lifecycle (incl. drawn committee) from CFs → node rejoins, agrees on root, lifecycle settles identically. Proves per-block persistence (1.0) + committee-in-DTO survive crash.
- DA: back verifiers with the in-process store (§4) so a **Confirmed** terminal is reached network-wide, not only NoQuorum.

**Conservation across apply AND reorg:**
- Block-by-block invariant `Σbalances + total_escrowed() + total_bonded() + total_unbonding() + total_burned` (helpers at `state.rs:2857,3086,3091`) across the whole driven path and at each terminal.
- Reorg: `revert_block` fail-safe-refuses any PoUW-active block (guard 1 maps-non-empty + guard 2 kind-scan, `state.rs:1786`) → force `try_reorg` full replay with a live pending job + a drawn committee → assert the 5th CF reconciles and the replayed committee is identical (draw is a pure fn of replayed blocks).

---

## 7. RISK NOTES

- **R-B5-grind (fork-safe, incentive-soft):** `block.hash()` is producer-influenceable → committee grinding. Mitigated (not eliminated) by per-job `hash(block_hash‖job_id)`. Founder-locked for v1; VRF/delayed-beacon is the hardening. *Not a fork risk* (deterministic); an incentive/fairness risk.
- **Fork surfaces to keep closed:** the draw must read ONLY `block.hash()` + snapshotted candidates + `bonded_stake` + genesis `k` — never `consensus.slashed_validators` (`consensus_manager.rs:163`), never wall-clock, never HashMap order (sort every enumeration: `draw_committees` sorts job_ids, candidates sorted at claim). Any slip = per-node committee = membership-gate fork (§0).
- **B7 soft guarantee (documented):** producer-side admission is not apply-enforced, so a malicious producer can violate the 51/49 flagship floor. The whitepaper's "protocol-enforced" claim is only met if the optional state.rs apply-cap lands. Flag for founder; v1 accepts soft scheduling.
- **1.2-MEMPOOL residual:** `select_applicable_txs` clones ChainState per block tick (same O(accounts+maps) cost as `capture_pre_block`, trivial at testnet scale). A byzantine producer can still *choose* to produce a failing block (self-harm: no reward, view-change hands off) — the fix protects *honest* producers from mempool poison, which is the DoS.
- **Stranded funds:** every pot has a deterministic drain — `expire_pending_job` (unclaimed), `settle_due_jobs` (claimed), D2 zero-comp fallback (NoQuorum/empty committee). Empty-committee jobs refund fully (no comp profit — zero-comp fallback). No reachable non-conserved state (P1 rollback + pot pre-validation guards).
- **Legacy paths untouched (must verify):** Transfer, Bond/RequestUnbond/WithdrawUnbonded, V1 SubmitJob burn (`state.rs:1300` `total_burned +=`), block production/reward (`credit_block_reward:676`) are not modified by any 1.2 piece except the additive B7 *scheduling* (job txs only) and the mempool speculative filter (drops only doomed txs). B10 + full suite must stay green; V1 `is_burn`/`burn_amount` semantics unchanged.
- **Borsh / upgrade (consensus-affecting, coordinated):** B5 flips CompleteJob/ClaimJob/Commit/Reveal unknown-id accept→reject; B8 changes `ConsensusParams::fingerprint` (params now consensus-load-bearing) and the committee enters the state root the moment a lifecycle exists (already 5-section since the first Bond, P10a). Old binaries already cannot decode the N1/V2 TxKinds — same coordinated-upgrade envelope; irrelevant pre-network, mandatory from the first public node. The `JobLifecycleRecord` DTO schema is settled (D8/P9 identity fields) so no disk migration is needed for the EscalationRound fast-follow.
- **B8 disagreement:** no runtime fingerprint gossip → param disagreement surfaces only as state-root/checkpoint divergence. `refuse_to_bind` catches *local* invalidity/engine-mismatch at startup; cross-node agreement relies on distributing the identical `genesis.json`. Log the fingerprint at boot.
- **M1 tightening:** converting warn-skip→fail-hard means a forward-incompatible consensus row now stops the node. Acceptable — consensus CF schemas are fixed/versioned; a fresh-data-dir resync is the deploy assumption (state.rs:300 migration note).

**Key file:line references for the orchestrator:** state.rs tail insert `:735`; CompleteJob arm `:1364`/`:1529`; apply_claim_job `:1585`; lifecycle helpers `:3162–3202`; ChainLedger `:3104`; compute_state_root fold `:564–645`; open() param TODO `:360–366`. lifecycle.rs `submit_result:463`, `record_commit:489/496`, `open:319`, DTO `:167`. capacity.rs `admit:93`, `validator_churn_bps:147`. consensus_params.rs `fingerprint:109`, `validate:152`, `refuse_to_bind:197`. event_loop.rs signals `:666/:681`, mempool `:2109`, process_job_tx `:2245`, block assembly `:2707–2763`, B6 tick `:876`, finalize `:2982`. main.rs genesis load `:362–382`, ChainState open `:456/:903`. block.rs verify_roots `:203` (no state_root). Genesis: root `genesis.json` (canonical), stale `src/genesis.json` (delete), core `GenesisConfig` `genesis.rs:11`.
---

# BINDING AMENDMENTS C1–C9 (from the 3-lens review; OVERRIDE the design body)

**C1 — BLOCKER FIX (both blocker findings, same root): reloaded-lifecycle param re-injection.**
`open()` reconstructs each JobLifecycle via `from_record(rec, GameParams::default(), ResolutionParams::default())`
(state.rs ~:363-370); `settle` reads the per-lifecycle `self.params` (lifecycle.rs ~:585). A node that
restarts mid-lifecycle would settle with DEFAULT params while peers use GENESIS params → different Terminal
→ different SettlementOutcomeRec in the state root → HARD FORK (the design's own example genesis has
k_escalate:5 vs default 7). FIX (non-protected, in 1.2a): `set_consensus_params(game,res,pw,stake)` must,
after setting the scalar fields, REBUILD every in-memory lifecycle: for each entry `to_record()` then
`from_record(rec, new_game, new_res)`. (1.2b's main.rs then calls set_consensus_params with genesis params
right after open(); no settlement runs in that startup window, so the rebuild is sufficient and keeps the
protected footprint to one call.) DETERMINISM TEST (mandatory): persist a post-draw lifecycle, reopen,
set_consensus_params with NON-DEFAULT params, settle → assert Terminal + compute_state_root == a
never-restarted node that had those params from genesis. Do NOT rely on genesis==defaults.

**C2 — MAJOR: the mempool soundness helper must not clone ChainState.** `select_applicable_txs` as designed
does `self.clone()` — ChainState is NOT Clone (holds `rocks: Option<RocksStore>` with non-Clone DB+AtomicU64).
Respecify (non-protected, state.rs): take one `capture_pre_block()` snapshot at entry, greedily trial-apply
each candidate via `apply_transaction` on `&mut self` (per-tx capture/rollback on Err), then
`rollback_to_pre_block(entry_snap)` to fully restore self before the real block is built — with restoration
GUARANTEED on every early-return/`?`/panic (this mutates the live producer ChainState; a missed restore
smears it — the exact class P1 rollback fixed). Test: a junk fee-priced Commit is dropped AND the producer's
post-call state root == its pre-call root (no smear).

**C3 — MAJOR: split doomed vs deferred (no over-filtering).** record_commit/record_reveal reject WrongPhase
(lifecycle.rs ~:490/:513) — deterministic NOW but VALID LATER once the phase advances; a Commit sharing a
block with its own CompleteJob sees an empty committee (draw runs in the tail). `select_applicable_txs` must
return `(kept, requeue)`: PERMANENTLY-doomed (unknown/duplicate job_id, V2-in-Batch, insufficient balance,
zero-from) → drop; PHASE/WINDOW-deferred (WrongPhase where the phase can still advance; committee-not-yet-drawn)
→ requeue so the tx survives to its window (1.2b's handle_block_tick pushes `requeue` back into pending_txs
like the future-nonce path). Standard mempool holds a not-yet-includable tx; never silently discard it.

**C4 — non-zero phase windows are a load-bearing invariant.** B5's same-block draw-then-settle is safe only
if commit_blocks/reveal_blocks > 0 (else a CompleteJob's tail draw is immediately swept by settle_due_jobs →
instant NoQuorum). `ConsensusParams::validate()` forbids zero windows (consensus_params.rs ~:180). 1.2a:
ensure set_consensus_params (or a validate call) rejects any window < 1; determinism/settle tests use
non-zero windows. 1.2b: main.rs `refuse_to_bind` must FAIL HARD (exit) on Err — not optional.

**C5 — STRATEGIC (framing, no code): the post-1.2 money path is production-INERT (all pots refund).** There
is no executor auto-claim loop, no verifier Commit/Reveal loop, and no real DA backend (design §4; grep
confirms ClaimJob/CompleteJob/Commit/Reveal are only test/RPC-injected today). So every production
SubmitJobV2 pot sits until claim_by → expire refund, or (if claim+complete injected) all verifiers Abstain →
NoQuorum → D2 zero-comp refund. Confirmed/Disputed are reachable ONLY under test/in-process-DA injection.
The flip is consensus-SAFE and fully conserving — but "PoUW is live and paying out" additionally requires
the three deferred deliverables. State this in every commit/PR/status; do NOT represent 1.2-POOL as closing
the claim path.

**C6 — B8 is bigger than "a 1–2 line setter" and B9's rationale is wrong.** The node ALWAYS builds genesis
via `create_genesis_for_dir(None)` → `default_genesis()` (main.rs ~:359/:458/:912); the genesis.json file-load
branch is DEAD (only reached when data_dir is Some, which never happens), and the peer genesis hash comes
from the genesis BLOCK (no consensus_params in the header). So B8 (1.2b) must add a REAL genesis.json load on
the node run-path (a genuine load-and-thread block in main.rs + a path decision: `--genesis` flag or copy
into data_dir), not a dead-branch hook. B9's real justification is "schema-incompatible DEAD file, no live
reader" (NOT "wrong genesis hash → peers reject"); the delete is harmless. B8 does NOT change the genesis
block hash.

**C7 — M2 flips legacy V1 ClaimJob accept→reject; cover it at ingress.** V1 SubmitJob pool jobs never enter
`pending_jobs` (only V2 does), so post-M2 any on-chain ClaimJob for a V1/unknown id Errs the whole block.
Deterministic across flipped nodes (no fork), part of the coordinated consensus-format change. 1.2b: add
ClaimJob to the kind-aware mempool ingress pre-filter (reject an id absent from pending_jobs and not an open
lifecycle) so honest producers never build a block M2 will reject. Confirm no V1 tooling emits on-chain
ClaimJob before the flip goes to a network.

**C8 — B7 needs a real params source + must requeue, not drop.** EventLoop has no `capacity_params` field and
no churn tracking; B8's setter omits CapacityParams. 1.2b: add CapacityParams to the B8 bundle (ChainState +
setter) and read it via self.state; account the new field as protected surface. B7 must PUSH BACK
non-admitted job txs into pending_txs (the design's `candidates.retain` drops them — job-liveness bug). v1
churn = 0 (documented); B7 is producer-side scheduling (not apply-enforced) — the whitepaper's "protocol
-enforced 51% flagship" is only met if the OPTIONAL non-protected apply-side coarse cap (state.rs, reject a
block whose SubmitJobV2 count exceeds available_slots) lands — founder call, defaults to soft v1 + risk note.

**C9 — minor fixes bundle.** (a) B6 already ChainState-inert (the tick touches only the node-local V1
job_pool) → "observe/log only" needs ~no protected change; document as such. (b) N2 confirmed genuinely
2-line cfg(unix), zero Unix behavior change. (c) B8 core-side serde defaults MUST derive from / round-trip
against the pouw-onchain default constructors (add a test: core GenesisConfig::default().consensus_params
→ chain params == the pouw-onchain defaults ChainState uses today) so a genesis omitting the section can't
silently shift consensus params. (d) Log the ConsensusParams fingerprint at boot (operators eyeball-match
across nodes; no runtime gossip). (e) B5 per-job seed = `hash_parts(&[&block_hash.0, &job_id])` (frozen
ids::hash_parts) — de-correlates jobs in a block; producer grind is a documented incentive risk, NOT a fork
(deterministic); VRF is the post-flip hardening.

## 1.2a TEST SURFACE (must be green before commit)
B5 determinism (two independently-built ChainStates apply the identical CompleteJob block → identical
committee + identical compute_state_root; candidate-order independence; seed-sensitivity; committee is in the
root); C1 restart-param-determinism (non-default params); empty/undersized committee → conserved NoQuorum
refund; CompleteJob arm guards (unknown/wrong-phase/wrong-executor/past-window/zero-from all reject); M2
reject-unknown; M1 fail-hard on a corrupt row; C2/C3 select_applicable_txs (junk Commit dropped, no
producer smear, WrongPhase Commit requeued not dropped); the whole 206-test storage suite + B10 5 terminals +
core + pouw-onchain stay green; frozen src/staging/pouw byte-identical. Conservation on both ledger backends.
