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

    // NEW: open the lifecycle's Committing phase + draw the committee.
    if let Some(life) = self.state.job_lifecycles.get_mut(job_id) {     // created at SubmitJob+ClaimJob (see §3)
        let seed = current_block_hash.0;                                // §1
        // candidate pool: eligible bonded validators, minus the executor (open-Q#10 filter, §6).
        let candidates: Vec<ParticipantId> = self.state.accounts.iter()
            .filter(|a| a.is_validator)
            .map(|a| a.address)
            .filter(|addr| self.state.is_eligible(addr)                 // bonded >= min_bond
                && !self.consensus_manager.slashed_validators.contains(addr)
                && /* compliant */ true
                && *addr != executor_addr)
            .map(addr_to_participant)                                   // [u8;32] passthrough
            .collect();
        let stake_of = |p: &ParticipantId| self.state.stake_of(&participant_to_addr(p));
        life.submit_result(executor_pid, *result_hash, seed, height, &stake_of);
    }
}
```

`ParticipantId`/`Address` are both `[u8;32]` newtypes — `addr_to_participant`/`participant_to_addr`
are field passthroughs (`ParticipantId(addr.0)` / `Address(pid.0)`). The committee is now stored in the
lifecycle and is identical on every node (same seed, same eligible set, same `stake_of`).

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

`JobLifecycle::record_commit(l: &mut EscrowLedger, ...)` and `settle(l: &mut EscrowLedger, eq)` move
money through the **staging `EscrowLedger`** type, but on-chain the pot lives in `ChainState`'s
`escrow_by_job` + `Account.balance`. These must be reconciled. Three options:

- **(A) Transient-ledger adapter.** Before each lifecycle call, build an `EscrowLedger` seeded from the
  job's pot + participant balances; call the method; diff balances back into `ChainState`. *Rejected:*
  `EscrowLedger.credit` is a mint and its `balances` map is a separate universe — reconciling which
  accounts changed is fragile for consensus money.
- **(B) Trait-abstract the ledger.** Refactor the staging settlement/lifecycle to take a `Ledger` trait
  both `EscrowLedger` and `ChainState` implement. *Cleanest long-term* but edits the frozen reference;
  defer.
- **(C, RECOMMENDED for v1) Decide-then-apply.** Use the lifecycle/`compute_verdict` logic to DECIDE the
  terminal (`Verdict` + committee + per-bond data — all already on-chain data), then apply the money via
  the **on-chain** `pay_from_job`/`burn_from_job` by reimplementing the 5 resolvers' splits against
  `ChainState` (they are short: 85/10/5, dispute bounty, forfeiture). Pin them with a **golden-
  equivalence test** asserting the on-chain end-state equals the staging `settlement_resolution`
  reference for the same inputs (the staging crate already has `resolve_confirmed_matches_run_priced_job`
  — mirror that across the boundary). This reuses the *decision* logic (the hard part) and keeps the
  *money moves* in the audited on-chain primitives.

**This decision needs founder sign-off before implementation** — it is the only non-mechanical piece of
P2. Recommendation: (C). Until signed off, the lifecycle store is the only deferred sub-item; everything
else below is mechanical.

---

## 4. Route Commit/Reveal to the lifecycle *(storage/src/state.rs — non-protected; the inert arms go live)*

The inert `TxKind::Commit`/`TxKind::Reveal` arms (built this branch) become:

```rust
TxKind::Commit { job_id, commit, bond } => {
    if !sender.is_validator { return Err(/* unchanged */); }
    let height = /* current */;
    if let Some(life) = self.job_lifecycles.get_mut(job_id) {
        // escrow the bond into the job pot (P1), then record. record_commit validates phase/window/
        // membership/no-double-commit and (option C) the chain escrows on Accepted only.
        let c = Commitment { verifier: ParticipantId(sender_addr.0), commit: *commit, bond: bond.raw() };
        match life.record_commit(&mut ledger_view, c, height) {        // or the option-C decide-then-apply
            EventResult::Accepted => { self.escrow_into_job(&sender_addr, *job_id, bond.raw())?; }
            EventResult::Rejected(r) => return Err(StateError::InvalidBlock(format!("commit rejected: {r:?}"))),
        }
    }
    sender.nonce += 1;
}
TxKind::Reveal { job_id, result_hash, salt } => {
    if !sender.is_validator { return Err(/* unchanged */); }
    if let Some(life) = self.job_lifecycles.get_mut(job_id) {
        let r = Reveal { verifier: ParticipantId(sender_addr.0), result_hash: *result_hash, salt: *salt };
        if let EventResult::Rejected(reason) = life.record_reveal(r, height) {
            return Err(StateError::InvalidBlock(format!("reveal rejected: {reason:?}")));
        }
    }
    sender.nonce += 1;
}
```

Escrow the bond **only on `Accepted`** so a rejected commit strands nothing. (Borrow note: compute the
`record_*` result, then call `self.escrow_into_job` after the `life` borrow ends.)

---

## 5. Phase advancement + settlement *(PROTECTED: node/src/event_loop.rs, the `enforce_timeouts` loop ~876)*

The live `enforce_timeouts(height, 2)` (~event_loop.rs:876, every ~30s) currently re-homes optimistic
jobs. Repurpose it to drive the lifecycle by block height:

```rust
for (job_id, life) in self.state.job_lifecycles.iter_mut() {
    life.advance(height);                          // Committing→Revealing at commit_by; →settle window
    if life.phase() == Phase::Revealing && height > reveal_deadline(life)
        || /* result_by passed with no result */ {
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

## 7. Genesis consensus params *(PROTECTED: genesis.json — confirm which of root vs src/genesis.json
   is authoritative first)*

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
2. **Sign off the §3 lifecycle money-path integration (recommend option C).**
3. Non-protected: `job_lifecycles` store + the option-(C) resolvers + Commit/Reveal routing (§3,§4,§10).
4. PROTECTED: committee draw (§2) + candidate filter (§6) + phase advance/settle (§5) + G6 admission
   (§8) + main.rs substrate (§9).
5. PROTECTED: genesis consensus params (§7).
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
