# Phase 1.1 BUILD SPEC — B2/B3/B4 + D2 zero-comp fallback (+P1 rollback, +P8 settle driver)

**Status:** design COMPLETE + 3-lens adversarially reviewed (conservation/economics, determinism/
consensus-safety, integration/compat — all APPROVE_WITH_CHANGES; 1 blocker + 9 majors folded into the
BINDING AMENDMENTS at the end, which OVERRIDE the design body wherever they conflict). Founder decisions
locked 2026-07-06: D2-FINAL = zero comp always; D8 = JobLifecycleRecord carries program identity now.
**NOT YET BUILT.** Next session: run the build+verify cycle against this spec (see the session handoff
memory for the exact workflow recipe used for step 1.0 — same shape: single builder → conformance
review + adversarial bug hunt + independent test run → orchestrator fixes minors → commit).

Base branch `agent-flip-20260705` (step 1.0 per-block persistence = dff2804 is the foundation this
builds on). Editable: src/storage/**, src/core/src/transaction.rs (NOT token.rs), src/staging/pouw-onchain/**.
PROTECTED untouched; src/staging/pouw/** frozen byte-identical.

---

# Phase 1.1 Implementation Design — B2/B3/B4 + D2 fallback + §10 glue

**Base:** branch `agent-flip-20260705`, HEAD `dff2804` (step 1.0 per-block atomic persistence landed).
**Editable:** `src/storage/src/{state.rs,rocks.rs,account.rs}`, `src/core/src/transaction.rs`, `src/staging/pouw-onchain/**`. Frozen: `src/staging/pouw/**` (byte-identical). PROTECTED untouched.
**Verified against code:** state.rs@dff2804 (arms :1101–1221, batch :1266–1337, escrow :2305–2385, ChainLedger :2591–2639, lifecycle helpers :2649–2721, persist :2050–2140, root :523–588, revert :1361–1417, B10 harness :3546–3856), core/transaction.rs (:16–205, :258–332), pouw-onchain lifecycle.rs / settlement_resolution.rs / capacity.rs / consensus_params.rs / escrow_ledger.rs, frozen pouw committee.rs/ids.rs/params.rs/economics.rs, node/event_loop.rs :2192–2247 (read-only, for identity convention).

---

## 0. Ground decisions (made here, everything below depends on them)

| # | Decision | Rationale |
|---|----------|-----------|
| G-A | **On-chain job identity = `tx.hash().0` of the SubmitJobV2 tx.** | Matches the node pool convention (`PoolJobId(tx_hash.0)`, event_loop.rs:2206) that `ClaimJob{job_id}` already references, and the patch-spec §8/§10 (`job_id = PoolJobId(tx_hash.0)`). Nonce is inside the hash ⇒ per-tx unique. The frozen `JobId::derive` and the DA `sha256(program_id‖input_hash)` job_id are *staging/off-chain sampling* keys, not the consensus escrow key. |
| G-B | **A 5th consensus map `pending_jobs: HashMap<[u8;32], PendingJobRecord>`** carries (submitter, budget, program identity, claim deadline) from SubmitJobV2 to ClaimJob. | `ClaimJob{job_id}` carries only the id (TxKind layout untouched — hard constraint); the lifecycle needs submitter+budget at open. Tx-history lookup is not crash-safe (receipts are not persisted; blocks prune from memory). A placeholder zero-executor lifecycle was considered and **rejected** (overloads the audited machine; zero-`from` txs skip signature verification — a forged `CompleteJob` could hit a zero-executor lifecycle at B5). The 5th map rides step 1.0's existing mirror machinery mechanically. |
| G-C | **`SubmitJobV2` inside `Batch` is rejected** (`InvalidBlock`). | No unique per-op id exists inside a batch (one tx hash, nonce bumped once). V1-in-batch keeps legacy burn. ClaimJob/Commit/Reveal in batch route through the same helpers as top-level (no semantic divergence). |
| G-D | **Bond amounts (deterministic, pre-B8):** `executor_bond = max(budget, game_params.executor_bond)`, `verifier_bond = game_params.verifier_bond`. | The fuel-derived formulas (`executor_bond_min` etc.) need a `fuel_cap` that is not an on-chain tx field. `economics.rs:102` itself applies the parent-spec `Be ≥ B` floor (`formula.max(budget)`); the flat `GameParams` bonds are the genesis-anchored knobs B8 will set. Defaults (100/20 raw) are placeholders — see Risk R5. |
| G-E | **New ChainState field `phase_windows: PhaseWindows`** (from `pouw-onchain::consensus_params`), defaulted, `TODO(B8)`; `PhaseWindows` gains `claim_blocks: u64` (default 10). | Deadlines at ClaimJob are `claim_height + windows` (the existing `deadlines_for` arithmetic, anchored at claim not submit). `claim_blocks` bounds how long a pending job may sit unclaimed (`claim_by = submit_height + claim_blocks`). Requires updating `ConsensusParams::fingerprint()` + `validate()` (both in editable consensus_params.rs). |
| G-F | **Height convention:** every height passed into lifecycle guards is `self.blocks.height()` = the *parent* height during apply — identical to the existing `RequestUnbond`/`WithdrawUnbonded` `now` convention (state.rs:1212/1218). Deterministic on every node. | |
| G-G | **Candidates are snapshotted at ClaimJob**, sorted ascending by address bytes. | The patch-spec §2 pseudocode computes candidates at CompleteJob but then calls `submit_result(...)`, which has **no candidates parameter** — it reads `self.candidates` set at `open` (lifecycle.rs:441). Spec/code conflict → adapt: snapshot at open (B3). `select_committee` output is order-independent (sorts by `(ticket, id)`), but the candidates `Vec` is in the persisted DTO and the state root, and `AccountStore::iter()` is HashMap-ordered — **sorting is consensus-load-bearing**. Recorded as a patch-spec deviation. |
| G-H | **Err = reject the whole block.** Every guard failure in the new arms returns `Err(StateError::…)` before the nonce bump (the Bond-arm pattern, state.rs:1201–1208). Deterministic: every guard reads only consensus state (maps, accounts, parent height). Honest producers must mempool-pre-validate (pre-existing pattern for InsufficientBalance etc.). | |

---

## 1. Job identity & lookup

- **Key for `escrow_by_job`, `pending_jobs`, `job_lifecycles`:** `job_id = sha256(borsh(SubmitJobV2 tx)) = tx.hash().0` computed inside the `SubmitJobV2` apply arm.
- **ClaimJob / Commit / Reveal / CompleteJob / DisputeJob** all carry `job_id: [u8;32]` fields already — the submitter publishes the tx, everyone derives the same id; the pool already uses this id (event_loop.rs:2206/2224). No TxKind change.
- **Duplicate submit:** literally re-broadcasting the same tx fails the nonce check (`InvalidNonce`, state.rs:876). A *distinct* tx colliding on hash is a SHA-256 collision (ignore). Defense-in-depth guard anyway (deterministic): reject `SubmitJobV2` if `pending_jobs`, `escrow_by_job`, **or** `job_lifecycles` already contains `job_id` → `Err(InvalidBlock("duplicate job id"))`. Same logical job (same program/input) submitted twice by anyone = two distinct jobs with two distinct pots — allowed by design (each pot conserves independently).
- **`PendingJobRecord`** (new, in state.rs; borsh; all fixed-size fields ⇒ canonical encoding):
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
  pub struct PendingJobRecord {
      pub submitter: [u8; 32],     // Address bytes
      pub budget: u64,             // raw units, == escrow pot at submit
      pub program_hash: [u8; 32],  // sha256(wasm) — the linchpin identity
      pub input_hash: [u8; 32],
      pub da_root: [u8; 32],
      pub submitted_height: u64,
      pub claim_by: u64,           // submitted_height + phase_windows.claim_blocks (anchored at submit)
  }
  ```
  `l2_id`/fee/resources deliberately excluded: B7 admission runs mempool-side pre-block (§6); execution metadata lives in tx history. Treat the field layout as a stable on-disk schema (same warning as `UnbondingChunk`).

## 2. B2 — SubmitJobV2 burn→escrow

**state.rs, split the shared arm at :1103–1121:**

```rust
// V1 stays byte-for-byte legacy burn:
TxKind::SubmitJob { comme_budget, .. } => { /* existing body unchanged, incl. total_burned += */ }

// V2 escrows (PoUW P1/B2). NOTE: outer `sender` borrow not used (NLL — the Bond-arm pattern).
TxKind::SubmitJobV2 { program_hash, input_hash, da_root, comme_budget, .. } => {
    if comme_budget.raw() < commputer_core::compute::MIN_JOB_BUDGET {
        return Err(StateError::InvalidBlock(format!("compute job budget {} below minimum {}", ...)));
    }
    let job_id = tx.hash().0;                                     // G-A
    if self.pending_jobs.contains_key(&job_id)
        || self.escrow_by_job.contains_key(&job_id)
        || self.job_lifecycles.contains_key(&job_id) {
        return Err(StateError::InvalidBlock("duplicate job id".into()));
    }
    // balance check + move balance→pot in one audited primitive (no partial state on Err):
    self.escrow_into_job(&tx.from, job_id, comme_budget.raw())?; // InsufficientBalance rejects block
    let h = self.blocks.height();
    self.pending_jobs.insert(job_id, PendingJobRecord {
        submitter: tx.from.0, budget: comme_budget.raw(),
        program_hash: *program_hash, input_hash: *input_hash, da_root: *da_root,
        submitted_height: h, claim_by: h.saturating_add(self.phase_windows.claim_blocks),
    });
    self.accounts.get_or_create(tx.from).nonce += 1;              // AFTER all fallible ops
    // total_burned NOT touched — escrow is held, not burned.
}
```

**Batch arm (:1266–1283):** split identically; `SubmitJob` keeps the existing burn body; `SubmitJobV2` → `return Err(StateError::InvalidBlock("SubmitJobV2 not allowed in Batch".into()))` (G-C).

**Failure semantics:** every `Err` fires before any mutation of that arm except `escrow_into_job`, which itself validates pot-overflow *then* balance before mutating (state.rs:2314–2324 — no partial state). Consistent with every other arm: Err ⇒ whole block rejected, nonce not bumped.

**core/transaction.rs edits:**
- `is_burn()` (:258): delete `TxKind::SubmitJobV2 { .. }` from the top pattern **and** from the Batch `matches!` scan (:266).
- `burn_amount()` (:305): delete the `SubmitJobV2` arm (:311) and remove it from the Batch loop pattern (:321–324) (leave `SubmitJob` in both).
- Doc comment on the `SubmitJobV2` variant (:114–119): update "burns comme_budget at submit" → escrows into the per-job pot.

**Every caller of `is_burn`/`burn_amount` (workspace grep, verified):**
1. `state.rs:1406` (`revert_block` burn reversal) — correct after the edit: V2 no longer contributes to `total_burned`, and any V2-carrying block is refused by revert anyway (guard 2 extension below).
2. `core/tests/tx_validation_proptests.rs:141–162` — only asserts Transfer (non-burn) + BurstCompute (burn); unaffected. **Add** V2 assertions (§8).
3. `transaction.rs` internal — the edit itself. No rpc/node/explorer callers exist.

**state.rs `tx_touches_consensus_maps` (:2268)** — extend per its own comment: add `SubmitJobV2 { .. } | ClaimJob { .. } | Commit { .. } | Reveal { .. } | CompleteJob { .. }` to `kind_touches` (ClaimJob only conditionally touches maps; over-approximation is the fail-safe direction). Also update the stale comment block :2263–2267 and the "Empty until the flip" comments at :165 and :2286–2289.

## 3. B3 — ClaimJob opens the lifecycle

Extract into a helper so top-level and Batch arms share one body:

```rust
/// B3: full ClaimJob semantics. Returns Ok(()) on both the V2-open path and the legacy path.
fn apply_claim_job(&mut self, from: Address, job_id: [u8; 32]) -> Result<(), StateError> {
    // Guard order matters: lifecycle-exists FIRST so a double-claim can never fall through
    // to the legacy no-op accept.
    if self.job_lifecycles.contains_key(&job_id) {
        return Err(StateError::InvalidBlock("job already claimed".into()));
    }
    // Validator gate KEPT (legacy Feature-53 semantics + patch-spec §4 keeps tx-level gates).
    let is_validator = self.accounts.get(&from).map(|a| a.is_validator).unwrap_or(false);
    if !is_validator {
        return Err(StateError::InvalidBlock("only validators can claim compute jobs".into()));
    }
    let Some(rec) = self.pending_jobs.get(&job_id).copied() else {
        return Ok(()); // legacy path: V1 pool job / unknown id — accept, no money (unchanged behavior)
    };
    let height = self.blocks.height();                       // G-F
    if height > rec.claim_by {
        return Err(StateError::InvalidBlock("claim window expired".into()));
    }
    // Executor bond: deterministic v1 rule (G-D).
    let e_bond = rec.budget.max(self.game_params.executor_bond);
    // Pot sanity (defense-in-depth; B2 guarantees this): pot must hold exactly the budget.
    if self.escrowed_for_job(&job_id) != rec.budget {
        return Err(StateError::InvalidBlock("job pot != pending budget".into()));
    }
    // Candidate snapshot (G-G): deterministic filter over finalized on-chain state ONLY,
    // sorted by address bytes (HashMap iteration order must never reach consensus state).
    let mut candidates: Vec<ParticipantId> = self.accounts.iter()
        .filter(|a| a.is_validator
            && a.compliance == ComplianceStatus::Compliant
            && a.address != from
            && self.bonded_stake.get(&a.address).copied().unwrap_or(0) >= self.stake_params.min_bond)
        .map(|a| ParticipantId(a.address.0))
        .collect();
    candidates.sort_by(|x, y| x.0.cmp(&y.0));
    // Escrow the executor bond (balance→pot; InsufficientBalance rejects the block, no partial state).
    self.escrow_into_job(&from, job_id, e_bond)?;
    let deadlines = PhaseDeadlines {                          // anchored at CLAIM height
        result_by: height.saturating_add(self.phase_windows.result_blocks),
        commit_by: /* result_by */ + self.phase_windows.commit_blocks,
        reveal_by: /* commit_by */ + self.phase_windows.reveal_blocks,
    };
    let lc = JobLifecycle::open(
        job_id, ParticipantId(rec.submitter), ParticipantId(from.0),
        e_bond, rec.budget, self.game_params.verifier_bond,
        self.game_params.clone(), self.resolution_params, candidates, deadlines,
    );
    self.job_lifecycles.insert(job_id, lc);
    self.pending_jobs.remove(&job_id);
    Ok(())
}
```

Top-level arm (:1123) becomes `self.apply_claim_job(tx.from, *job_id)?; self.accounts.get_or_create(tx.from).nonce += 1;`. Batch arm (:1284) becomes `self.apply_claim_job(from, *job_id)?;` (outer Batch bumps nonce once — existing convention).

Notes:
- Pot after open = `budget + e_bond` = `expected_escrow()` with zero commitments — the P1 precondition documented at `JobLifecycle::open` (lifecycle.rs:301) holds by construction.
- `is_eligible` is inlined via `bonded_stake` (not `self.is_eligible(&addr)`) purely to avoid a borrow conflict with `self.accounts.iter()`; semantics identical (state.rs:2568). Either formulation is fine if borrows are ordered.
- Empty candidate pool is allowed (pre-bonding): B5 would draw an empty committee → NoQuorum → D2 fallback; conserved (Risk R7).
- Self-claim (submitter == executor) allowed; self-dealing is strictly value-losing (≥5% burn + fee).
- **Pending expiry (companion helper, same commit):** `pub fn expire_pending_job(&mut self, job_id: [u8;32], height: u64) -> Result<Option<SettlementOutcome>, StateError>` — if a pending record exists and `height > claim_by`: pre-validate `pot == budget`, then `pay_from_job(job_id, submitter, budget)` (full refund — no-fault: nobody claimed; the submitter already paid the tx fee, which is the anti-spam cost; the *voluntary* 2%-burn `resolve_cancel` needs a CancelJob TxKind and stays a follow-on), remove the record. `Ok(None)` if not pending / not yet due. This is what B6's tick (or §7's recommended in-apply driver) calls so unclaimed pots cannot strand *after* the flip. Nothing calls it on this branch except tests — accepted stranding pre-flip per the gate.

## 4. B4 — Commit/Reveal route to the lifecycle helpers

Shared helpers (used by top-level and Batch arms):

```rust
fn apply_commit(&mut self, from: Address, job_id: [u8;32], commit: [u8;32], bond: u64) -> Result<(), StateError> {
    let is_validator = self.accounts.get(&from).map(|a| a.is_validator).unwrap_or(false);
    if !is_validator { return Err(StateError::InvalidBlock("only validators can commit to compute jobs".into())); }
    let height = self.blocks.height();
    // Verifier is ALWAYS the tx sender — no spoofing surface.
    let c = Commitment { verifier: ParticipantId(from.0), commit, bond };
    match self.lifecycle_record_commit(job_id, c, height)? {   // Err(InsufficientBalance) propagates
        Some(EventResult::Accepted) => Ok(()),
        Some(EventResult::Rejected(r)) =>
            Err(StateError::InvalidBlock(format!("commit rejected: {r:?}"))),
        None => Err(StateError::InvalidBlock("commit: unknown job".into())),
    }
}

fn apply_reveal(&mut self, from: Address, job_id: [u8;32], result_hash: [u8;32], salt: [u8;32]) -> Result<(), StateError> {
    let is_validator = ...same gate...;
    let height = self.blocks.height();
    // Deliberate addition vs patch-spec §4: drive the height-based Committing→Revealing
    // transition on the tx path (advance is idempotent + money-free, lifecycle.rs:496), so a
    // reveal after commit_by does not depend on the B6 tick having run. Deterministic: pure
    // function of consensus state + parent height.
    self.lifecycle_advance(job_id, height);
    let r = Reveal { verifier: ParticipantId(from.0), result_hash, salt };
    match self.lifecycle_record_reveal(job_id, r, height) {
        Some(EventResult::Accepted) => Ok(()),
        Some(EventResult::Rejected(rr)) => Err(StateError::InvalidBlock(format!("reveal rejected: {rr:?}"))),
        None => Err(StateError::InvalidBlock("reveal: unknown job".into())),
    }
}
```

Top-level arms (:1148–1171) become gate-free bodies calling the helper then `nonce += 1`; Batch arms (:1305–1322) call the helpers only. **No `escrow_into_job` anywhere in these arms** — `record_commit` escrows the bond itself through `ChainLedger` (lifecycle.rs:465–466 → state.rs:2659–2669); adding one would double-escrow (the explicitly banned mistake).

**The B4-before-B5 crux, pinned:**
- Commit against an **unknown job** (no lifecycle): `lifecycle_record_commit` → `Ok(None)` → `Err` → block rejected. This *closes the inert-Commit spam window* (today's arm accepts arbitrary declared bonds at fee-only cost).
- Commit against a **claimed job pre-B5**: `submit_result` has never run ⇒ phase is `AwaitingResult` ⇒ `Rejected(WrongPhase)` ⇒ `Err`. So **on this branch no Commit/Reveal tx can appear in any valid block at all** — strictly inert-but-strict, deterministic (all inputs are consensus state).
- Commit against a lifecycle in `Committing` with an **empty committee** (only reachable post-B5 with an empty candidate pool): `committee.contains(...)` fails ⇒ `Rejected(NotCommitteeMember)` ⇒ `Err`. Deterministic.
- **Validators-only gate: KEPT** (the patch-spec §4 pseudocode keeps it verbatim — "the patch-spec's answer wins"). It is *not* the security boundary — committee membership inside `record_commit` is — but it is deterministic, cheap, and matches the spec. The real membership check needs no B5: an empty/wrong committee already rejects as above. Note: a committee member who deregisters validator status between draw and commit is gated out (deterministic either way; recorded).
- Determinism of Err-vs-accept: every branch is a function of (consensus maps, account flags, parent height) — byte-identical on every node applying the same block on the same parent. `RejectReason` strings appear only in the error (block-rejection) path, never in accepted state.

## 5. D2 — NoQuorum→Escalate fallback terminal

**Placement (both, deliberately):**
1. **The resolver lives in pouw-onchain `settlement_resolution.rs`** — `Ledger`-generic like its five siblings — because the B10 equivalence extension must run the *same* code against both backends (staging `EscrowLedger` + `ChainLedger`). A state.rs-only resolver could not be applied to the staging side. (Module cycle lifecycle↔settlement_resolution is intra-crate and legal; lifecycle.rs already imports settlement_resolution.)
2. **The interception is a state.rs wrapper** around `lifecycle_settle` — it owns the borrow dance, the pot pre-validation, and terminal draining.

```rust
// pouw-onchain/src/settlement_resolution.rs (new; frozen crate untouched)
/// D2 v1: NoQuorum fallback (founder-decided 2026-07-05). No verdict was reached, so NO party
/// is slashed: the executor is partially compensated for unproven work
/// (`escalate_fallback_executor_comp_bps` of the budget), the submitter refunded the remainder,
/// the executor bond returned intact, and every REVEALER's bond returned (non-revealer bonds
/// were already burned by the primary round before the verdict branch — lifecycle.rs:535-543).
/// Drains the pot to exactly 0; burns nothing. The on-chain EscalationRound replaces this
/// post-flip (it cannot run on-chain today: concrete `&mut EscrowLedger` methods, no DTO).
pub fn resolve_escalation_fallback(
    l: &mut impl Ledger, rp: &ResolutionParams, job_id: [u8;32],
    h: &crate::lifecycle::EscalationHandoff,
) -> SettlementOutcome {
    l.for_job(job_id);
    let comp = bps(h.budget, rp.escalate_fallback_executor_comp_bps);  // floor div ⇒ comp+refund == budget
    let refund = h.budget - comp;
    l.pay(h.executor, comp);
    l.pay(h.submitter, refund);
    l.pay(h.executor, h.executor_bond);                                 // no-fault: bond intact
    for (r, b) in h.committee_reveals.iter().zip(&h.committee_bonds) { l.pay(r.verifier, *b); }
    SettlementOutcome {
        worker_paid: comp, submitter_refunded: refund,
        bonds_returned: h.executor_bond + h.committee_bonds.iter().sum::<u64>(),
        ..Default::default()                                            // burned: 0
    }
}
```

- **New `ResolutionParams` field:** `escalate_fallback_executor_comp_bps: u32`, default `2_000` (20% — mirrors `timeout_submitter_comp_bps`, the analogous harmed-party share; "timeout-style" per D2). Update `ConsensusParams::fingerprint()` and `validate()` (bps ≤ 10_000) in consensus_params.rs. `ResolutionParams` is genesis-anchored, never persisted per-job — no DTO/schema impact.
- **Conservation proof sketch:** at `Terminal::Escalate` the pot holds exactly `budget + Be + revealers·Bv` (settle pre-validated `pot == expected_escrow()`, then burned each non-revealer bond — lifecycle.rs:344–353, 535–543; readiness-assessment §5 re-verified this). The fallback pays `comp + (budget−comp) + Be + Σ committee_bonds` where `committee_bonds = vec![Bv; reveals.len()]` — the identical sum. Only `pay` ops (pot→balances, supply-invariant per the P1 primitive contract); pot → 0; `total_burned` unchanged.

```rust
// storage/src/state.rs (new wrapper)
/// Settle + drain: the on-branch (and B6) entry point. Confirmed/Disputed/TimedOut drain
/// via settle itself; Escalate settles via the D2 fallback. The lifecycle is REMOVED on
/// success ⇒ at-most-once by construction (second call ⇒ Ok(None), no money).
pub fn lifecycle_settle_and_drain(&mut self, job_id: [u8;32], eq: &dyn EquivalenceOracle)
    -> Result<Option<(Terminal, Option<SettlementOutcome>)>, StateError>
{
    let Some(terminal) = self.lifecycle_settle(job_id, eq)? else { return Ok(None) };
    let fb = if let Terminal::Escalate(h) = &terminal {
        // Pre-validate the pot == the exact sum the fallback will move (defensive: provably
        // equal after settle, but the ChainLedger .expect()s must stay unreachable).
        let expected = h.budget
            .saturating_add(h.executor_bond)
            .saturating_add(h.committee_bonds.iter().sum());
        if self.escrowed_for_job(&job_id) != expected {
            return Err(StateError::InvalidBlock(format!("escalate pot {} != expected {}", ...)));
            // lifecycle already re-inserted by lifecycle_settle; terminal cached ⇒ retry is idempotent
        }
        let h = h.clone();
        let rp = self.resolution_params;                       // Copy BEFORE the &mut view
        let mut view = ChainLedger::new(self);
        Some(resolve_escalation_fallback(&mut view, &rp, job_id, &h))
    } else { None };
    self.job_lifecycles.remove(&job_id);                       // drain (pot is 0 on every path here)
    Ok(Some((terminal, fb)))
}
```

- **Idempotency:** `settle` caches its terminal (lifecycle.rs:510) so re-entry moves no primary-round money; the fallback's money moves once because (a) success removes the lifecycle (second call short-circuits at `None`), (b) failure leaves the pot untouched and the terminal cached, so a retry re-runs the same deterministic check. `ChainLedger::new` requires `pub(crate)`/private access — the wrapper lives in state.rs next to the existing helpers, so no visibility change.
- **Existing `lifecycle_settle` is left unchanged** (B10's five tests keep passing verbatim); the wrapper is additive.
- **B10 fallback-equivalence case:** drive the existing NoQuorum scenario through `run_on_both` (both terminals `Escalate`, pots held equal — the current test at :3806 proves that part), then apply **the same** `resolve_escalation_fallback` to both sides: staging via `&mut both.staging` (EscrowLedger already impls `Ledger`), chain via `lifecycle_settle_and_drain` (which re-settles to the cached Escalate then runs the fallback — exercising the production entry point). Assert: fallback `SettlementOutcome`s equal field-for-field; then the full `assert_equivalent`-style checks (per-actor end balances, both pots == 0, both conserve their baselines, `total_burned` unchanged by the fallback on both sides — staging burn only the primary-round forfeits).

## 6. §10 glue helpers (pre-wiring for PROTECTED B7)

**`pending_job_from_tx` — lives in storage `state.rs`** (free function): pouw-onchain deliberately has no `commputer-core` dependency (it uses mirror structs — jobspec_map.rs pattern), and only `storage/src/{state,rocks,account}.rs` are editable in that crate (a new module would need lib.rs). `node` depends on `storage` → B7's call site reaches it.

```rust
/// §10 glue for PROTECTED B7 (block-assembly capacity admission). Pure mapping per the
/// patch-spec §8: job_id = tx hash (G-A — the SAME id the escrow map uses), flagship by l2_id,
/// priority = fee. Batch returns None: batched V2 is rejected at apply (G-C) and batched V1
/// jobs are not pool-visible (event_loop::process_job_tx does not unpack Batch).
pub fn pending_job_from_tx(tx: &Transaction) -> Option<commputer_pouw_onchain::capacity::PendingJob> {
    match &tx.kind {
        TxKind::SubmitJob { l2_id, .. } | TxKind::SubmitJobV2 { l2_id, .. } =>
            Some(commputer_pouw_onchain::capacity::PendingJob {
                job_id: tx.hash().0,
                is_flagship: l2_id.as_deref().map(commputer_core::l2::is_flagship).unwrap_or(false),
                priority: tx.fee,
            }),
        _ => None,
    }
}
```

**`validator_churn_bps` — lives in pouw-onchain `capacity.rs`** (pure, next to its consumer `dynamic_reserve_bps`):

```rust
/// §10: churn_bps = 10_000·|joined ∪ left| / prev_count, clamped to 10_000.
/// prev_count == 0 ⇒ 10_000 (bootstrap: maximum reserve — the safe direction, matching
/// available_slots' div_ceil bias). Sets (not counts) so an address that joined AND left
/// within the epoch counts once, per the spec's |joined ∪ left|.
pub fn validator_churn_bps(
    prev_count: u64,
    joined: &std::collections::BTreeSet<[u8; 32]>,
    left: &std::collections::BTreeSet<[u8; 32]>,
) -> u32 {
    if prev_count == 0 { return 10_000; }
    let changed = joined.union(left).count() as u128;
    ((changed * 10_000) / prev_count as u128).min(10_000) as u32
}
```

Adaptation recorded: the patch-spec §10 signature `(prev_count, joined, left)` is honored; the epoch-delta *tracking* (who joined/left) is B7's PROTECTED job — the helper stays pure. Unit tests: zero churn → 0; join-and-leave same address counts once; churn > prev_count clamps to 10_000; prev_count 0 → 10_000; composed check `dynamic_reserve_bps(defaults, validator_churn_bps(...))` hits the 500/1_500 clamps at the boundaries. `pending_job_from_tx` tests: V1 and V2 map; `job_id == tx.hash().0`; flagship id ↔ `FLAGSHIP_L2_ID` exactly; `None` l2 → not flagship; priority == fee; Transfer/Batch → None.

## 7. Persistence interplay with step 1.0

**`pending_jobs` becomes the 5th reconciled map.** Mechanical extension of every 4-map site (each has an existing template three lines above it):

| Site | Edit |
|---|---|
| rocks.rs | `const CF_PENDING: &str = "pending_jobs";` + add to **both** CF lists (:74, :377); `batch_put_pending` / `batch_delete_pending` / `all_pending` (borsh value, warn-skip malformed rows — clone the CF_ESCROW trio :394–420 and the CF_LIFECYCLE value pattern) |
| state.rs field + `new()` | `pub pending_jobs: HashMap<[u8;32], PendingJobRecord>` + `persisted_pending_keys: HashSet<[u8;32]>` (:174/:211 blocks, :264/:271) |
| `open()` | load `all_pending`, seed the mirror (:335/:356) |
| `Debug` + `snapshot()` | add the count (:236) / sorted JSON section (:473–499) |
| `compute_state_root()` | 5th sorted, length-prefixed section (job_id ‖ len ‖ borsh blob — the CF_LIFECYCLE fold pattern :576–585); **extend the Policy-B all-empty early-return** (:525–531) so pre-flip roots stay byte-identical |
| `batch_map_deltas` / `commit_map_mirrors` | 5th delete-then-put pass (:2105–2130) / 5th mirror rebuild (:2135–2140) — this single choke point covers per-block persist, `flush_consensus_maps`, and try_reorg's post-replay reconcile |
| `revert_block` guard 1 | add `!self.pending_jobs.is_empty()` (:1377–1383); guard 2 covered by the `tx_touches_consensus_maps` extension (§2) |
| `reset_to_genesis` | `pending_jobs.clear()` + mirror clear (:2218/:2243) |

**Proof that every new mutation rides `persist_applied_block`'s single WriteBatch** (exhaustive mutation inventory of the new code):

| Mutation | Where it lands | Carried by |
|---|---|---|
| submitter/executor/committer balance debits, payouts, refunds | `Account.balance` via `escrow_into_job`/`pay_from_job` → `accounts.get_mut/get_or_create` | accounts dirty journal → batch puts (:2078–2082) |
| nonce bumps | accounts | dirty journal |
| pot create/grow/drain/delete | `escrow_by_job` | 5-map delta pass (delete-reconciled via mirror) |
| pending insert (B2) / remove (B3, expiry) | `pending_jobs` | new 5th delta pass |
| lifecycle insert (B3), internal mutation by commit/reveal/advance/settle (B4/D2 — key-stable value changes) | `job_lifecycles` | **full-value re-put every block** (:2127–2129 — exactly why 1.0 chose value re-puts over key tracking); removal at drain → mirror delete |
| `total_burned` (fees, V1 burns, forfeitures, settle burns; **not** V2 escrow) | meta counter | `META_TOTAL_BURNED` rides every batch (:2070) |
| `game_params` / `resolution_params` / `phase_windows` / `stake_params` | never mutated at apply (genesis-anchored, B8) | n/a by construction |

No other state is touched — each arm above is written solely in terms of the escrow primitives, the lifecycle helpers (which bottom out in `ChainLedger` → the same primitives), map insert/remove, and account nonce. **One caveat that must be preserved:** `lifecycle_settle_and_drain` (and `expire_pending_job`) are *not tx arms* — pre-B6 only tests call them; whenever they run outside block application (B6 tick), they are out-of-band mutations swept by the *next* block's batch (the `out_of_band_mutation_swept_by_next_block` regression at :5379 covers the mechanism), and they create a **per-height cross-node root divergence hazard** — see R2 for the recommended in-apply driver.

## 8. Test plan

**Per-arm apply tests (state.rs `#[cfg(test)]`, real signed txs through `apply_block_validated`):**
- B2: V2 escrows (pot == budget, balance debited, `total_burned` moves **only by the fee**, pending record exact incl. `claim_by`); V2 below MIN_JOB_BUDGET rejects; V2 insufficient balance rejects block (state unchanged); duplicate job_id rejects; V2-in-Batch rejects; V1 top-level and V1-in-Batch still burn (byte-identical legacy assertions); state root changes only via accounts+new sections (Policy-B early-return still byte-identical for a pre-flip state).
- B3: happy claim (lifecycle open, pot == budget+Be, pending removed, `expected_escrow` matches, candidates == sorted eligible set — construct 3 bonded validators + 1 unbonded + 1 non-compliant and assert exact membership+order); non-validator claim rejects; double claim rejects (second tx, and a second ClaimJob **within one Batch**); claim after `claim_by` rejects; claim of unknown/V1 id = legacy accept (nonce only, no money); insufficient bond balance rejects (pot still == budget, no lifecycle); `e_bond = max(budget, game_params.executor_bond)` boundary (set `game_params.executor_bond > budget` in-test).
- B4: pre-B5 inertness pinned — Commit against claimed job rejects block (WrongPhase), Commit unknown job rejects, Reveal unknown/wrong-phase rejects; then out-of-band `submit_result` (test hook via pub `job_lifecycles`) and drive tx-level Commit accept (bond escrowed exactly once — pot delta == Bv), wrong-bond reject, double-commit reject (tx and in-Batch), non-member reject, committer balance < bond rejects with lifecycle intact; Reveal accept after commit_by **without any advance call** (proves the arm's built-in `lifecycle_advance`), mismatch/replay rejects; non-validator gate on both.
- `expire_pending_job`: refunds exactly budget, removes record, pot 0, burn unchanged; not-due → `Ok(None)`; second call → `Ok(None)`.
- D2: `lifecycle_settle_and_drain` — Confirmed/TimedOut paths drain the map entry; Escalate path pays comp/refund/Be/revealer-bonds exactly (assert per-account), pot 0, `total_burned` unchanged, lifecycle removed; **settle-twice** → second `Ok(None)`, all balances byte-identical; malformed pot (test-tamper the pot) → `Err`, nothing moved, retry-able.
- **Conservation across the full driven path** for each terminal (Confirmed / Disputed / Timeout / NoQuorum→fallback / non-revealer-forfeiture): blocks carry Bond + ValidatorRegister + SubmitJobV2 + ClaimJob (+ Commit/Reveal after the out-of-band `submit_result`), then `lifecycle_settle_and_drain` in lieu of B6; assert `Σbalances + total_escrowed + total_bonded + total_unbonding + total_burned` invariant block-by-block and at terminal, pot drained, at-most-once.
- **B10 extension:** the §5 fallback-equivalence case (same resolver on both backends + `assert_equivalent` + outcome equality + burn-unchanged). Existing 5 equivalence tests must pass unmodified.
- **Crash-persistence (leverages the 1.0 harness pattern, :5318):** block1 funds+registers+bonds, block2 SubmitJobV2, block3 ClaimJob, out-of-band `submit_result` (swept by block4), block4 Commit txs → capture `compute_state_root` → **drop without flush** → `open()` → assert pot, pending absence, lifecycle DTO equality, mirrors, root equality → continue post-reopen: Reveal block + `lifecycle_settle_and_drain` → conservation holds end-to-end across the restart. Second variant: crash between block2 and any claim → reopen → pending record + pot intact → `expire_pending_job` refunds.
- Replay/duplicate suite: re-submit same V2 tx (InvalidNonce), same-shape new-nonce V2 (new job_id, independent pot), double-claim, double-commit, reveal-replay, settle-twice — each asserting zero money delta on the rejected op.
- `tx_touches_consensus_maps`/revert: a block containing any of the five kinds is refused by `revert_block` even with empty maps (extend :5610's pattern); pure-transfer revert still works.
- core: `is_burn`/`burn_amount` — V1 true/budget, **V2 false/ZERO**, Batch-with-V2 false/ZERO, BurstCompute unchanged; existing proptests (:141–162) untouched.
- pouw-onchain: `resolve_escalation_fallback` conservation + pot-drain + zero-burn on `EscrowLedger` (sibling of the 5 resolver tests); `ResolutionParams` default asserts the new bps; fingerprint changes when the new field/`claim_blocks` change and validate rejects >10_000 bps / zero windows; capacity `validator_churn_bps` suite (§6).
- **Must keep passing:** full storage suite (158 at `dff2804` — includes 1.0's crash/mirror/revert suite and N1's bond suite :3217–3350/:5244–5461), B10's 5 terminals, pouw-onchain 82, core 188+, frozen pouw untouched (byte-identical check), whole-workspace build.

## 9. Risk notes (what a reviewer should attack)

- **R1 — Reachable states pre-B5/B6 on this branch:** (accepted per the gate, but enumerate) V2 pots strand until claim/expiry has a driver; claimed jobs strand at `AwaitingResult`; an executor who sends `CompleteJob` still times out and **loses the bond** (submit_result unwired) — nobody should run real value on this branch; the flip is atomic for exactly this reason. No *non-conserved* state is reachable: every reachable mutation is escrow-in or a guarded reject; nothing drains a pot except the tested settle/expire helpers.
- **R2 — B6 tick vs state root (the biggest live-flip hazard, decision needed):** settlement driven from `enforce_timeouts` (wall-clock tick) mutates consensus maps out-of-band ⇒ two nodes settle at different heights ⇒ **per-height state roots diverge** even though terminals agree, breaking `multinode_assert` root comparison and any root-in-header check. Recommended (non-protected!) alternative: a `settle_due_jobs(height)` pass invoked from `apply_block`/`apply_block_validated` after the tx loop and **before** `persist_applied_block` (rides the same WriteBatch): iterate `job_lifecycles` + `pending_jobs` in **sorted key order**, `advance` → `should_settle` → `lifecycle_settle_and_drain` / `expire_pending_job`. Deterministic, crash-atomic, shrinks PROTECTED B6 to nothing for settlement. Deviates from THE MAP's B6 assignment ⇒ founder sign-off required; if declined, B6's design must anchor settlement to a deterministic height rule, never the tick moment.
- **R3 — Zero-address txs skip signature verification** (state.rs:777–786): a forged `from=zero` ValidatorRegister (fee 0) could mint a "validator" zero-account; the money arms are shielded (zero has no balance ⇒ escrow fails; committee membership gates commits) but a zero "validator" could enter the **candidate snapshot** if someone bonds to it (bond requires a signed tx from zero — impossible — so unreachable; still: consider `!from.is_zero()` in the candidate filter, one line). Pre-existing hole, Phase-2 item 6.
- **R4 — Mid-block Err poisons in-memory state** (pre-existing): apply loops mutate in place; a block rejected at tx *i* leaves txs `<i` + the fee of tx *i* applied in memory (never persisted — persist runs only on success). B2–B4 raise the stakes (escrow mutations). Same class as existing InsufficientBalance arms; producers pre-validate; a byzantine block can desync a node's *memory* until restart. Flag for the 1.4 verification gate.
- **R5 — Placeholder economics until B8:** `GameParams::default()` bonds (100/20 raw) make verifier bonds dust and `quorum(k=3)=2` trivially attackable-by-collusion; `StakeParams` 100/1_000 likewise. Conservation holds regardless; incentive security does not. B8 must land in the same coordinated flip (already gated) — the design intentionally reads all knobs from the `game_params`/`resolution_params`/`phase_windows`/`stake_params` fields so B8 is a pure genesis-population change.
- **R6 — Candidate snapshot staleness:** committee candidacy fixed at claim; members can unbond/deregister before committing (deregistered ⇒ blocked by the kept validator gate; merely-unbonded ⇒ can still commit — bond escrows from spendable balance, slash surface preserved). Deterministic; fairness/freshness deferred to B5 follow-up (or an added candidates-refresh method if the founder prefers draw-time snapshot — requires a small pouw-onchain lifecycle addition, recorded as the alternative).
- **R7 — Empty-committee NoQuorum loop:** with no bonded validators, every claimed+completed job ends Escalate→fallback: executor nets +20% of budget per job without verification. Post-B7 capacity caps volume and the submitter chooses to submit; still, consider gating the flip on `bonded_stake` non-trivial, or founder may prefer fallback comp = 0 when `reveals.is_empty()` — **flag for founder** (one-line variant of the resolver).
- **R8 — NoQuorum collusion incentive:** an executor colluding with revealers to force a 3-way split converts a would-be Disputed into fallback (bond returned + 20% comp). Bounded by verifier opportunity cost (they forfeit the 10% verifier pool) and fixed post-flip by the real on-chain EscalationRound (fresh panel). Accepted for v1 by D2; document in the flip notes.
- **R9 — Reorg/fork with live maps:** `revert_block` fail-safe-refuses (extended kinds list) ⇒ any fork past a PoUW-active block forces `try_reorg` full-replay or `reset_to_genesis` + resync. Operationally acceptable at testnet; verify try_reorg's replay path reconciles the 5th CF (it flows through `batch_map_deltas` — the single choke point — but add an explicit reorg test with a pending job).
- **R10 — Consensus-format changes bundled here** (all pre-network, but list for the flip notes): state-root gains a 5th section once `pending_jobs` is non-empty; V2-in-Batch and unknown-job Commit/Reveal flip from accept→reject; `ConsensusParams::fingerprint` changes (new `claim_blocks` + fallback bps). Old binaries already can't decode the N1 TxKinds — same coordinated-upgrade envelope.
- **R11 — `bps()` rounding:** `comp = floor(budget·bps/10_000)`; `refund = budget − comp` — conserva­tion exact by construction; assert in the resolver test with a budget not divisible by 5.
- **R12 — Borrow discipline:** every new arm must follow the Bond-arm NLL pattern (no use of the outer `sender` after `&mut self` calls; nonce via fresh `get_or_create`). The candidate snapshot must complete (collect) before `escrow_into_job`. Compile-enforced, but reviewers should check no arm silently re-orders a fallible op after the nonce bump.

**Patch-spec deviations recorded:** candidates snapshotted at ClaimJob, sorted (spec §2 pseudocode is inconsistent with `submit_result`'s actual signature); Reveal arm self-advances phase (spec §5 relied on the tick); V2 banned in Batch; `pending_jobs` 5th map + `claim_blocks` window + `expire_pending_job` (spec had no pending-job persistence or expiry); `ResolutionParams` gains the fallback bps (D2 is post-spec); `validator_churn_bps` takes sets, not counts.

**Suggested commit slicing (each builds + tests green):** (1) pouw-onchain: `resolve_escalation_fallback` + `ResolutionParams`/`PhaseWindows.claim_blocks` + fingerprint/validate + `validator_churn_bps`; (2) storage: `pending_jobs` map + CF + root/mirror/revert/reset plumbing + `PendingJobRecord` (inert — nothing writes it yet); (3) core: is_burn/burn_amount + variant doc + tests; (4) storage: B2+B3+B4 arms + helpers + `phase_windows` field + `tx_touches_consensus_maps` + `pending_job_from_tx` + per-arm tests; (5) storage: `lifecycle_settle_and_drain` + `expire_pending_job` + conservation/crash/replay suites + B10 fallback case. Note (3)+(4) are gate-bound to each other (V2 must not be both non-burn and non-escrow in any commit — put the is_burn edit and the arm flip in adjacent commits on the branch, merged only together, or fold (3) into (4)).
---

# BINDING AMENDMENTS (from the 3-lens review + founder decisions — OVERRIDE the design body above)

**P1 — BLOCKER FIX, block-level rollback-on-Err (new hard scope).** The design's §2/R4 claim that
mid-block Err smear is "never persisted" is FALSE under step 1.0: smeared accounts stay in the dirty
journal, smeared maps stay live, and the NEXT successful block's persist_applied_block writes them all
(state.rs:2078-2130) into CFs and the state root. A malicious block fed to a subset of nodes then forks
honest nodes persistently (sync path applies peer blocks via apply_block_validated). FIX in this build,
all non-protected: on ANY apply_transaction Err inside apply_block/apply_block_validated/apply_block_atomic,
restore pre-block state before returning Err — rocks-backed: reload accounts+5 maps+meta from CFs (disk
== post-last-good-block by step 1.0's guarantee) and re-run open()'s hygiene (clear dirty/removed,
mirrors from loaded maps); memory-only (tests/try_reorg replay): snapshot the 5 maps + meta + touched-
account before-images at block entry and restore. Regression test: block(tx1 escrows, tx2 fails) →
assert root AND next-persisted CF bytes identical to a node that never saw the invalid block.

**P2 — settle re-entry wedge (3/3 lenses).** lifecycle_settle pre-validates pot == expected_escrow()
BEFORE the cached-terminal short-circuit (state.rs:2708-2715), but expected_escrow sums ALL bonds incl.
non-revealers (lifecycle.rs:348-353) while the first settle already burned forfeited bonds
(lifecycle.rs:535-543) → any re-entry on Escalate-with-forfeiture Errs forever; should_settle is false
once Settled → pot strands permanently. FIX: short-circuit on the cached terminal BEFORE the pot
pre-validation (cached path moves no money; return Ok(Some(cached))); expose is_settled(); the drain
wrapper must never re-enter the pre-check. TEST: forfeiture NoQuorum (2 reveal / 1 silent) → settle →
drain succeeds (pot 0) → second settle returns Ok(cached), not Err.

**P3 — zero-address guards MANDATORY (3/3 lenses).** Zero-from txs skip signature verification entirely
(state.rs:777-780; mempool-only null-sender check is bypassable by a byzantine producer), so a funded
zero address + forged ValidatorRegister + forged Bond = a keyless bonded "validator" that enters the B3
candidate snapshot and, post-B5, a committee seat anyone can puppet. FIX: `tx.from.is_zero() → Err` in
the SubmitJobV2/ClaimJob/Commit/Reveal arms AND the Bond/RequestUnbond/WithdrawUnbonded arms (per-arm
guard — MiningReward and other legitimate zero-from system paths are untouched), plus `!is_zero` in the
B3 candidate filter. The design's R3 "impossible" claim is wrong — reachable today. General zero-address
audit stays a Phase-2 item.

**P4 — D2-FINAL (founder, 2026-07-06): ZERO COMP ALWAYS.** resolve_escalation_fallback = pure
refund/return of the handoff pot: full budget → submitter, Be → executor, revealer bonds → committers
(non-revealer forfeitures already burned by the audited settle — unchanged). NO comp parameter exists
(drop the drafted escalate_fallback_executor_comp_bps entirely — no new ResolutionParams field, less B8
surface, conservation is exact by construction). Rationale: any comp makes forced-NoQuorum profitable
(2-of-3 collusion swing > 1.2×budget; DA-starvation); matches resolve_unavailable's
no-verification-no-pay precedent. Document the residual (now profit-NEUTRAL) griefing until the real
EscalationRound lands.

**P5 — §10 glue correction (2/3 lenses).** `validator_churn_bps` ALREADY EXISTS
(pouw-onchain/capacity.rs:147-154, counts-based, prev_count==0→0 bootstrap semantics, pinned by two
tests) — do NOT redefine or shadow it; the design's §6 BTreeSet version is dropped. Build ONLY the thin
`pending_job_from_tx` adapter delegating to the existing `pending_job_from_fields` (capacity.rs:161-173).

**P6 — B4 test inversions.** `commit_from_validator_is_inert_accepted` (state.rs:3158) and
`reveal_from_validator_is_inert_accepted` (:3178) pin the OLD inert behavior — rewrite them as
reject-pinning tests (unknown-job Commit/Reveal → block rejected, zero money delta, P1 rollback leaves
root unchanged). Audit that section for other pre-flip pins; the non-validator reject test (:3190)
survives.

**P7 — PROTECTED-inventory additions (recorded in THE MAP §1.2; NOT buildable here).**
(a) 1.2-MEMPOOL: kind-aware mempool admission / producer speculative-apply — post-B4 a junk fee-priced
Commit tx makes every containing block unapplicable = zero-cost block-production DoS; the design's
"honest producers pre-validate" claim is false (no such code exists). (b) 1.2-POOL: process_job_tx has
no SubmitJobV2 arm → V2 jobs never enter executor pools → every pot dead-ends at expiry. Both land with
B5/B6 in the founder's PROTECTED pass.

**P8 — deterministic in-apply settlement driver (new scope, non-protected, inert while maps empty).**
Build `settle_due_jobs(height)` into the SHARED post-tx tail used by ALL THREE apply paths (adjacent to
persist_applied_block so money moves inside the block's WriteBatch): iterate due jobs in SORTED job-key
order; drive lifecycle_advance → should_settle → settle_and_drain (P2 semantics) and expire_pending_job
(claim_by passed → refund pot to submitter, remove pending record). EquivalenceOracle = ByteEq pinned as
a named const at the single call site (a future oracle change must enter ConsensusParams::fingerprint
before becoming configurable). B6's PROTECTED tick becomes observe/log-only. This supersedes patch-spec
§5's tick-driven sketch — settlement must NEVER run from wall-clock ticks (per-height root divergence =
fork). try_reorg's replay reproduces identical settle heights by construction (same shared tail).

**P9 — D8 (founder, 2026-07-06): JobLifecycleRecord carries program identity.** Add
program_hash/input_hash/da_root ([u8;32] each) to JobLifecycle at open() and to JobLifecycleRecord
(to_record/from_record) NOW, while nothing is deployed — the schema is settled once; the EscalationRound
fast-follow and CompleteJob result-binding then need no disk/state-root migration. Update the DTO
stable-schema doc comment; roundtrip tests updated accordingly.

**P10 — minor fixes bundle.** (a) R10 correction: the state root gains the 5th (pending_jobs) section
the moment ANY consensus map is non-empty — the first Bond tx suffices (Policy-B early-return is
all-or-nothing, state.rs:525-531); pin with a test that a bonded-only state's fold includes the empty
pending section. (b) G-B rationale reword: pruned blocks ARE readable from RocksDB; the true reason for
the 5th map is no job_id→tx index + receipts not persisted (ReceiptStore::new() at open). (c) job_id
malleability (memo/timelock outside the signed payload → third party can shift the job_id pre-inclusion):
DOCUMENT as a known fund-safe griefing vector (expiry refund is the recovery); the signed-payload id +
PROTECTED PoolJobId change is a flip-notes follow-up. (d) apply_block_atomic gets the same shared tail
as the other two paths (P8) — no third divergent entry point. (e) The B10 fallback-equivalence case must
include the FORFEITURE variant (P2's wedge is invisible in the all-reveal scenario).

**P11 — prose/spec hygiene.** The design's §2 "never persisted" and R4 claims are corrected by P1; R3 by
P3; §5 idempotency claim by P2; §6 by P5. THE MAP §1.1/§1.2 and the D2 row were updated 2026-07-06.
Patch-spec §5 is superseded by P8 for settlement driving (annotation deferred to avoid churn — this spec
is the authoritative source for the 1.1 build).

**P12 — expected test surface.** New tests per design §8 PLUS: P1 rollback regression, P2 forfeiture
settle/drain/re-settle, P3 zero-from rejections (per arm), P4 zero-comp conservation on both ledger
backends (B10 extension, incl. forfeiture variant), P8 driver determinism (same blocks → same
settle heights → same root; reorg replay reproduces), P9 DTO roundtrip with identity fields, P10a
5-section-root pin, expiry-refund path. The two P6 inversions replace their old pins. Everything else in
the 158-test storage suite, B10's 5 terminals, N1 bond suite, core tx tests (minus is_burn changes,
which get updated pins), pouw-onchain suites must stay green; frozen pouw crate byte-identical.

---

# POST-BUILD VERIFY OUTCOME (2026-07-06)

Build landed; 3-agent verify (conformance APPROVE, bughunt FIX_REQUIRED, independent tests all green +
frozen crate byte-identical). Storage 158→206, core 205, pouw-onchain 84, pouw(frozen) 53.

**MAJOR (fixed):** P1 rocks-backed rollback reloaded pre-block state from the CFs, silently rewinding
out-of-band mutations applied in memory since the last block (the epoch tick's `current_epoch` bump +
per-account uptime pokes, peer-disconnect grace drains) — those ride the NEXT block's persist, so a disk
reload on a rejected block would rewind them while the event loop's companion epoch state stayed
advanced (a mixed state no crash produces → account-root divergence from honest peers). FIX:
`capture_pre_block` now ALWAYS takes the memory snapshot (dropped the `PreBlock::Rocks` variant +
`reload_consensus_state_from_rocks`); restores exact pre-block memory incl. the dirty/removed journals.
Cost = O(accounts+maps) clone per block, trivial at testnet scale. New test
`p1_rollback_preserves_out_of_band_mutations_on_a_rocks_node` (would fail under the old reload path).

**MINOR (documented, deferred — no code change):**
- (M1) Undecodable consensus-map CF rows are warn-skipped at `open()` (pre-existing pattern) but now a
  pot/pending/lifecycle mismatch makes the P8 driver's pot guard reject EVERY subsequent block forever
  (clean rollback each time, no smear — a fail-STOP, not corruption, but surfaced only as a startup warn
  line). Disk corruption already means the node must resync; step 1.0's fresh-data-dir deployment
  assumption (A8) covers it. FOLLOW-UP for the flip's hardening pass: make `open()` fail hard (or
  cross-check key consistency and refuse to start) on any undecodable consensus-map row, converting a
  mystifying runtime halt into an actionable startup error.
- (M2) Expire-then-claim races degrade to the legacy no-op accept (the P8 driver expires a pending job at
  the first block past `claim_by`, and txs run before the driver, so a late ClaimJob either still sees the
  record — guard can't fire — or falls through to the silent legacy accept). No money at risk
  (deterministic; the refund already ran), but a post-expiry claim is an indistinguishable no-op to
  wallets. The `claim past window` guard is therefore defense-in-depth-only (its test injects state to
  reach it). FOLLOW-UP (founder call, belongs with the PROTECTED consensus-format bundle since Commit/
  Reveal unknown-id already flipped accept→reject): reject ClaimJob for ids absent from `pending_jobs`
  once V1 pool jobs drain, or emit a distinct no-op receipt.

Both follow-ups are recorded in THE MAP for the Phase 1.2 PROTECTED pass. Everything money-bearing is
CONFIRMED conserved on both ledger backends across all five terminals incl. the zero-comp fallback and
forfeiture variants.
