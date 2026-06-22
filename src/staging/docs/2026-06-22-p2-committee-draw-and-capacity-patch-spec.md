# P2 — Founder Patch-Spec: committee draw + lifecycle + G6 capacity admission

**Status:** founder-applies. The staging references (`lifecycle`, `escalation_round`, `capacity`,
`bonded_stake`, `settlement_resolution`) are built + tested; the non-protected on-chain foundations
(escrow ledger, bonded stake, Commit/Reveal TxKinds) are built on this branch. This doc lists the
remaining edits — most in PROTECTED files — to make the verification game *live*. Agents do NOT apply
the PROTECTED edits.
**Date:** 2026-06-22 · **Branch context:** `agent-overnight-20260622` (off `main`).
**Founder decisions locked:** committee **seed = post-result block hash, v1** (open-Q#5); G4 stake
source = the on-chain bonded stake just built; seed grinding hardening (VRF) deferred to mainnet.
**Depends on:** P0 (execution) + P1 (escrow foundation) + the G4 stake source + the Commit/Reveal
TxKinds (all on this branch). Apply those first.

> **✅ Fact-check corrections folded (2026-06-22, 3-reviewer adversarial pass).** Substantive: (1) the
> committee candidate filter now uses ONLY deterministic finalized on-chain state — do NOT use the
> node-local `consensus.slashed_validators` runtime set (it is intra-epoch, message-order-dependent →
> committee divergence); see §2. (2) §3 now states honestly that the staging `JobLifecycle` APPLIES
> money internally via `EscrowLedger` (record_commit escrows; settle pays/burns), so the integration is
> NOT a clean decide-then-apply — the options are re-framed accordingly. Factual: `EventLoop.consensus`
> (not `consensus_manager`); compliance variant is `ComplianceStatus::Compliant` (no `Active`);
> Address↔ParticipantId are inline `ParticipantId(a.0)`/`Address(p.0)` (no helper exists); deadlines via
> `life.deadlines.reveal_by`; the golden-equivalence test is WRITTEN AFTER the on-chain resolvers exist
> (the staging `resolve_confirmed_matches_run_priced_job_end_state` compares two staging paths, not the
> boundary); resolve which `genesis.json` is authoritative (root vs `src/genesis.json`, the latter is 31
> lines w/o consensus_params) before §7; the `job_lifecycles` field + inline casts are a NON-PROTECTED
> foundation step to add BEFORE the protected edits (see Apply order).

---

## 0. What is already built (non-protected, on this branch — no founder action needed)

| Piece | Where | Status |
|---|---|---|
| Per-job escrow ledger (`escrow_by_job` + `escrow_into_job`/`pay_from_job`/`burn_from_job`) | `storage/src/state.rs` | ✅ `cd0acfe` |
| Bonded stake source (`bond`/`request_unbond`/`withdraw_unbonded`/`slash_stake`/`stake_of`/`is_eligible`) | `storage/src/state.rs` | ✅ `23e7ed6` |
| `Commit`/`Reveal` TxKinds + inert apply arms | `core/src/transaction.rs`, `storage/src/state.rs` | ✅ this branch |
| Verification game logic (lifecycle, escalation, settlement, committee select) | `staging/pouw-onchain/src/*`, `staging/pouw/src/*` | ✅ tested reference |
| G6 capacity accountant (`admit`/`dynamic_reserve_bps`/`available_slots`) | `staging/pouw-onchain/src/capacity.rs` | ✅ tested, pure |

The remaining work is **wiring**: draw the committee, persist the lifecycle, route Commit/Reveal to
it, advance/settle by block height, admit jobs per the 51/49 split, and anchor the params in genesis.

---

## 1. The consensus seed — post-result block hash (v1) *(PROTECTED: event_loop.rs)*

`select_committee` / `JobLifecycle::submit_result` need a `seed: [u8;32]` that is **unpredictable to
the executor at the moment it submits its result** (else the executor grinds the committee). The
founder-chosen v1 source: **the hash of the block at commit-phase entry** — i.e., the block that
*includes or follows* the executor's `CompleteJob`. The executor cannot know that block's hash when it
submits, because the hash depends on the full block contents (including its own tx and others').

```rust
// In process_job_tx, on CompleteJob acceptance: the seed is the hash of THIS block (the one carrying
// the CompleteJob). At apply time the block hash is known to all nodes deterministically.
let seed: [u8; 32] = current_block_hash.0; // BlockHash of the block being applied
```

**Grinding residue (documented, accepted for testnet v1):** a *block producer* can still influence the
hash by choosing which txs to include / reordering. Mitigations for mainnet (deferred): (a) use the
hash of the block at `result_by` height + N (a future block the producer of the result block can't
choose), or (b) a VRF. v1 ships the simple block-hash; the `EscalationRound` (second panel from a
*fresh* seed) bounds the damage of a single grind. **Whatever the source, it MUST be identical on every
node** (a consensus value), never wall-clock or per-node randomness.

---

## 2. Committee draw on CompleteJob *(PROTECTED: node/src/event_loop.rs `process_job_tx`, ~2231)*

Today `CompleteJob` (event_loop.rs:2231) only calls `job_pool.complete_job`. Augment it to open the
job's verification lifecycle and draw the committee:

```rust
TxKind::CompleteJob { job_id, result_hash } => {
    let jid = PoolJobId(*job_id);
    self.job_pool.complete_job(&jid, *result_hash, height); // unchanged (pool view)

    // NEW: open the lifecycle's Committing phase + draw the committee. The lifecycle was created at
    // ClaimJob (see §3 — `job_lifecycles` is a NON-PROTECTED foundation field to add BEFORE this).
    if let Some(life) = self.state.job_lifecycles.get_mut(job_id) {
        let seed = current_block_hash.0;                                // §1
        // Candidate pool: filter ONLY by DETERMINISTIC finalized on-chain state (see warning below).
        // Address and ParticipantId are both [u8;32] newtypes — cast inline (no helper exists).
        let candidates: Vec<ParticipantId> = self.state.accounts.iter()
            .filter(|a| a.is_validator
                && self.state.is_eligible(&a.address)                  // bonded >= min_bond (deterministic)
                && a.compliance == ComplianceStatus::Compliant         // deterministic on-chain status
                && a.address != executor_addr)
            .map(|a| ParticipantId(a.address.0))
            .collect();
        let stake_of = |p: &ParticipantId| self.state.stake_of(&Address(p.0));
        life.submit_result(ParticipantId(executor_addr.0), *result_hash, seed, height, &stake_of);
    }
}
```

The committee is stored in the lifecycle and is identical on every node (same seed, same eligible set,
same `stake_of`).

> **⚠ CONSENSUS DETERMINISM (fact-check blocker, fixed above).** The candidate filter MUST read only
> finalized on-chain state. Do **NOT** use `EventLoop.consensus.slashed_validators` — it is a node-local,
> intra-epoch, message-arrival-order-dependent runtime set, so different nodes would draw different
> committees for the same `CompleteJob` at the same height and fork. A validator slashed enough to drop
> below `min_bond` is already excluded by `is_eligible` (bonded stake is finalized on-chain state). If you
> want an additional "slashed-this-epoch" exclusion, derive it from a **persisted, deterministically-
> updated** slashed set in `ChainState` (updated by block application), never the consensus runtime set.

---

## 3. Per-job lifecycle store + the EscrowLedger integration *(storage/src/state.rs non-protected
   for the store; ⚠ the money-path integration is a DESIGN DECISION — see below)*

`ChainState` gains a per-job lifecycle map alongside `escrow_by_job`:

```rust
pub job_lifecycles: HashMap<[u8;32], commputer_pouw_onchain::lifecycle::JobLifecycle>,
```

(adds a `storage → commputer-pouw-onchain` dependency — additive). It is created when a job is claimed
(`ClaimJob`: open at `AwaitingResult` with `budget + executor_bond` already escrowed per P1), advanced
on `CompleteJob` (§2), fed by Commit/Reveal (§4), and advanced/settled by height (§5).

### ⚠ The one real design decision: lifecycle money-path ↔ `escrow_by_job`

**Honest framing (fact-check correction):** the staging `JobLifecycle` does NOT just *decide* — it
*moves money inside its methods* via the staging `EscrowLedger`: `record_commit` calls
`l.escrow(verifier, verifier_bond)` (lifecycle.rs ~212) and `settle` calls the resolvers which
`l.pay(...)`/`l.burn(...)` (lifecycle.rs ~284-329 → settlement_resolution.rs). On-chain the pot is
`ChainState`'s `escrow_by_job` + `Account.balance`. So there is no "free" decide-then-apply: every
option below either runs the reference money logic against an on-chain-backed ledger, or reimplements
it. Three honest options:

- **(A) Transient-ledger adapter.** Per settlement, build an `EscrowLedger` seeded from the job pot +
  participant balances, run the lifecycle method (it moves money in the transient ledger), then diff the
  result back into `ChainState`. Reuses the tested logic unchanged, but the reconcile-back step
  (`EscrowLedger.credit` is a mint; its `balances` is a separate map) is fiddly and must be proven not
  to mint/lose value.
- **(B, RECOMMENDED) Trait-abstract the ledger.** Give the lifecycle/settlement a small `Ledger` trait
  (`escrow`/`pay`/`burn`/`for_job`) and `impl Ledger for ChainState` (delegating to
  `escrow_into_job`/`pay_from_job`/`burn_from_job`). The lifecycle's *tested* money logic then runs
  UNCHANGED against the real chain ledger — no reimplementation, no reconcile. Cost: a contained edit to
  the staging reference (parameterize `&mut EscrowLedger` → `&mut impl Ledger`); `EscrowLedger` keeps
  satisfying the trait so its existing tests are untouched. This is the soundest path now that we know
  the lifecycle applies money internally.
- **(C) Refactor the lifecycle to decision-only.** Strip the `l.escrow/pay/burn` calls so methods return
  only `Verdict`/`Terminal` data, then apply money on-chain via reimplemented resolvers. More invasive to
  the reference than (B) and loses the reference's money-move tests; not recommended.

**Golden-equivalence test (corrected):** whichever option, the cross-boundary equivalence test
(on-chain end-state == staging reference end-state) is **written AFTER the chosen integration is
implemented** — it does not exist yet. The existing `settlement_resolution::
resolve_confirmed_matches_run_priced_job_end_state` compares two *staging* paths (engine vs resolver),
not the on-chain boundary; it is the *pattern* to mirror, not a pre-built cross-boundary check.

**This decision needs founder sign-off before implementation** — it is the one non-mechanical piece of
P2. Recommendation: **(B) trait-abstract the ledger** (reuses the tested money logic verbatim). Until
signed off, the lifecycle store + its money-path wiring (this §3, the Commit/Reveal escrow routing in
§4, and the settle calls in §5) are ALL deferred; the genuinely mechanical items are §6 (filter), §7
(genesis params), §8 (G6 admission), §9 (substrate).

---

## 4. Route Commit/Reveal to the lifecycle *(storage/src/state.rs — non-protected; the inert arms go live)*

The inert `TxKind::Commit`/`TxKind::Reveal` arms (built this branch) become:

```rust
TxKind::Commit { job_id, commit, bond } => {
    if !sender.is_validator { return Err(/* unchanged */); }
    let height = /* current */;
    // With §3 option B (Ledger trait), record_commit ESCROWS THE BOND ITSELF via the ChainState-backed
    // ledger (staging lifecycle.rs ~212 does `l.escrow(verifier, verifier_bond)` internally) — do NOT
    // also call escrow_into_job here (that would double-escrow). On Rejected, nothing was escrowed.
    let c = Commitment { verifier: ParticipantId(sender_addr.0), commit: *commit, bond: bond.raw() };
    if let EventResult::Rejected(r) = self.record_job_commit(job_id, c, height) {   // see borrow note
        return Err(StateError::InvalidBlock(format!("commit rejected: {r:?}")));
    }
    sender.nonce += 1;
}
TxKind::Reveal { job_id, result_hash, salt } => {
    if !sender.is_validator { return Err(/* unchanged */); }
    let r = Reveal { verifier: ParticipantId(sender_addr.0), result_hash: *result_hash, salt: *salt };
    if let EventResult::Rejected(reason) = self.record_job_reveal(job_id, r, height) {
        return Err(StateError::InvalidBlock(format!("reveal rejected: {reason:?}")));
    }
    sender.nonce += 1;
}
```

**Borrow note (important).** The lifecycle lives INSIDE `ChainState` (`job_lifecycles`) but
`record_commit` also needs the ledger (the rest of `ChainState`). You cannot `&mut`-borrow
`self.job_lifecycles` and `&mut self` (as the ledger) at once. Wrap the call in a `ChainState` helper
(`record_job_commit`/`record_job_reveal`) that either (a) `remove`s the lifecycle from the map, runs
`record_commit` with `&mut self` as the `impl Ledger`, then re-inserts it, or (b) impls `Ledger` on a
disjoint view holding `&mut self.escrow_by_job` + `&mut self.accounts` (NOT all of `self`) so the split
borrow is legal. The bond escrows on `Accepted` only (staging semantics), so a rejected commit strands
nothing.

---

## 5. Phase advancement + settlement *(PROTECTED: node/src/event_loop.rs, the `enforce_timeouts` loop ~876)*

The live `enforce_timeouts(height, 2)` (~event_loop.rs:876, every ~30s) currently re-homes optimistic
jobs. Repurpose it to drive the lifecycle by block height:

```rust
for (job_id, life) in self.state.job_lifecycles.iter_mut() {
    life.advance(height);                          // Committing→Revealing at commit_by; →settle window
    // Settle when the reveal window passed OR the result window passed with no result. NOTE: the
    // lifecycle's `deadlines`/phase-internals are PRIVATE fields — add a public signal to lifecycle.rs
    // (a contained, non-protected change), e.g. `pub fn should_settle(&self, height: u64) -> bool`
    // (true once past reveal_by in Revealing, or past result_by in AwaitingResult). Then:
    if life.should_settle(height) {
        // PRE-VALIDATE the pot == budget + Be + Σ committed bonds (P1 caller-contract) THEN settle.
        let terminal = life.settle(&mut ledger_view, &ByteEq);     // (option C: decide-then-apply on-chain)
        match terminal {
            Terminal::Escalate(handoff) => { /* open an EscalationRound from a fresh seed (follow-on) */ }
            _ => { self.state.job_lifecycles.remove(job_id); }     // drained terminal
        }
    }
}
```

`advance`/`settle` are **idempotent** (the lifecycle caches its terminal) so a re-org / double tick
re-runs no money — but each money-moving *event* (commit-bond escrow) must apply at most once per tx
(dedupe on tx hash, as today). The reveal/commit/result deadlines come from `PhaseDeadlines` (§7).

---

## 6. Candidate filtering (open-Q#10) *(PROTECTED: event_loop.rs, in §2)*

`all_validators()` filters nothing today. The eligible candidate pool for `select_committee` MUST
exclude: the executor (done in §2), **slashed** validators (`consensus_manager.slashed_validators`),
**non-compliant** accounts (`Account.compliance != Active`), and **ineligible** validators
(`is_eligible` == false, i.e. bonded `< min_bond`). `select_committee` then weights the survivors by
`stake_of`. This filter is consensus-load-bearing (all nodes must compute the identical eligible set).

---

## 7. Genesis consensus params *(PROTECTED: genesis.json)*

> **✅ D-2 RESOLVED (2026-06-23): canonical = ROOT `genesis.json`.** `GenesisConfig`
> (`core/src/genesis.rs`) expects the FLAT schema (`emission_base_rate`/`emission_floor_rate`/
> `channel_floors`), with `emission_base_rate` a REQUIRED field. The root `genesis.json` matches it;
> `src/genesis.json` uses a different nested schema (`emission{}`/`channel_floors_bps`/
> `reference_node_specs`/`protocol_version`) that `load_genesis` CANNOT parse → it returns `Err` → the
> node falls back to **default genesis** (wrong genesis hash → peers reject it). The node loads
> `"genesis.json"` from CWD then `../` (`main.rs:1894`). **Founder action: DELETE `src/genesis.json`**
> (stale, footgun) so only the canonical root file remains; add `consensus_params` to the ROOT
> `genesis.json` + matching fields on `GenesisConfig`.

These ALL must be genesis-anchored and identical across nodes (the node should `refuse_to_bind` on
divergence — see the P3 `consensus_params` spec). Add a `consensus_params` section:

| Param bundle | Source struct | Notes |
|---|---|---|
| `GameParams` (k, bonds, bounty bps, fuel regime) | `commputer_pouw::params::GameParams` | G5; fuel regime `5000/2500/2500/5/tight` signed off |
| `ResolutionParams` (cancel 2%, timeout 20%) | `settlement_resolution::ResolutionParams` | P1 |
| `PhaseDeadlines` (result_by/commit_by/reveal_by, in blocks) | `lifecycle::PhaseDeadlines` | NEW — pick window lengths (e.g. 10/10/10 blocks for testnet) |
| `StakeParams` (unbonding_blocks, min_bond) | `state::StakeParams` | G4 — placeholders 100/1000, set real values |
| `CapacityParams` (total_slots + reserve/flagship bps) | `capacity::CapacityParams` | G6 — defaults 100/5100/500/1500/1000 |
| `WasmLimits` | `commputer_pouw::wasm::WasmLimits` | P0/P3 |

---

## 8. G6 capacity admission *(PROTECTED: event_loop.rs block-production / SubmitJob admission path)*

The `capacity::admit` function is pure + tested; wire it where the block is assembled from the mempool:

1. Compute `churn_bps` = `10_000 * |joined ∪ left| / prev_validator_count` from the validator-set
   delta of the previous epoch (track `prev_validator_count` + the delta in `ChainState` per epoch — a
   small non-protected addition: a `validator_churn_bps()` helper).
2. Build `Vec<PendingJob>` from the mempool's `SubmitJob`/`SubmitJobV2` txs: `job_id =
   PoolJobId(tx_hash.0)`, `is_flagship = l2::is_flagship(l2_id)` (`FLAGSHIP_L2_ID =
   "commputer-analytics-l2"`), `priority = tx.fee` (a non-protected `pending_job_from_tx` helper — see
   §10).
3. `let a = capacity::admit(&params.capacity, churn_bps, &pending);` — admit `a.admitted` into the
   block's compute capacity; leave `a.deferred` in the mempool for a later block.

This enforces whitepaper Core Principle #1 (51% flagship floor, work-conserving) at the protocol level.

---

## 9. main.rs substrate *(PROTECTED: node/src/main.rs)*

Construct + thread through: the genesis `consensus_params` bundle (→ `ChainState.stake_params`,
`GameParams`, `PhaseDeadlines`, `CapacityParams`); the `EquivalenceOracle` (`ByteEq` for v1 — the
semantic-equivalence oracle is a later cycle); and (if `refuse_to_bind` is adopted) the startup
divergence check. No new long-lived services — the lifecycle store lives in `ChainState`.

---

## 10. Non-protected glue the agent CAN pre-build (so the PROTECTED edits are thin calls)

- `pending_job_from_tx(tx) -> Option<capacity::PendingJob>` — resolves job_id/is_flagship/priority from
  a SubmitJob/V2 tx (a pure mapping; testable; non-protected). *(Candidate for a follow-on cycle.)*
- `validator_churn_bps(prev_count, joined, left) -> u32` — the churn formula (pure; testable).
- The option-(C) on-chain settlement resolvers + their golden-equivalence test (pending the §3 sign-off).

---

## Caller contracts the wiring MUST honor (carried from P1/P2)

1. Budget + executor bond escrowed before `Committing` (P1 submit/claim handlers).
2. **Pre-validate the pot == `budget + Be + Σ committed bonds` before `settle`** — the resolvers panic
   on under-funding (option C: return Err instead, rejecting the malformed terminal).
3. Committee size bounded by `k`/`k_escalate` (true by `select_committee`'s `take(k)`).
4. The seed is unpredictable at result-submission (§1).
5. Each money-moving event applies at most once per tx (dedupe on tx hash); `advance`/`settle` idempotent.

## Apply order

1. (done on branch) escrow + bonded-stake foundations + Commit/Reveal TxKinds.
2. **Sign off the §3 lifecycle money-path integration (recommend option B: trait-abstract the ledger).**
3. Non-protected foundation (do BEFORE the protected edits): add `job_lifecycles` field to `ChainState`
   (+ init in `new`/`open`, Debug, resets, RocksDB persistence per the escrow WIRE-IN TODO pattern); add
   the chosen §3 integration (option B: a `Ledger` trait + `impl Ledger for ChainState`, parameterizing
   the staging `&mut EscrowLedger` → `&mut impl Ledger`); add `pub fn should_settle(&self, height)` to
   `lifecycle.rs`; then the Commit/Reveal escrow routing (§4).
4. PROTECTED: committee draw (§2, deterministic filter only) + phase advance/settle (§5) + G6 admission
   (§8) + main.rs substrate (§9).
5. PROTECTED: genesis consensus params (§7 — confirm which genesis.json first).
6. Verify (done-when): a committee is drawn deterministically from the post-result block hash on every
   node; verifiers commit then reveal across blocks; a wrong result → `Disputed` + slashed bond; an
   unavailable program shrinks the effective committee → `NoQuorum`→`Escalate` with escrow held; a
   committed non-revealer's bond is forfeited; the 51/49 admission split holds; conservation holds
   across a block apply AND its re-org.

## NOT in this patch-spec (follow-ons, references already built)
- Escalation second round live trigger (`EscalationRound`, `escalation_round.rs` built) — an `Escalate`
  path off `Terminal::Escalate` from a fresh seed.
- Real libp2p DA transport (P4) — the §7.1 gate here is "who committed"; P4 wires the sampling that
  determines who *can* commit (`da_transport.rs` bridge built).
- Semantic-equivalence oracle (v1 uses `ByteEq`).
- VRF seed hardening for mainnet (§1).
