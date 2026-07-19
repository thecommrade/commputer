# EscalationRound On-Chain Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the NoQuorum zero-comp refund stand-in with a real, founder-approved second verification panel (EscalationRound) wired end-to-end: consensus state machine, persistence, tx routing, verifier-loop driving, and full test coverage.

**Architecture:** A second per-job state machine (`ChainState.escalation_rounds: HashMap<[u8;32], EscalationRound>`) parallel to `job_lifecycles`. On `Terminal::Escalate`, the settling block's tail draws a panel (seed `hash(block_hash‖job_id‖"escalate")`) and applies the founder's F2 viability gate: panel ≥ `quorum(k_escalate)` → open the round (pot stays held); smaller → the existing `resolve_escalation_fallback`. The round is driven by Commit/Reveal txs routed by job-id, advanced/settled in `settle_due_jobs`, persisted via a borsh DTO in a new `CF_ESCALATION`, folded into the state root as a 6th Policy-B section, and surfaced to live verifiers through `build_verifier_views`.

**Tech Stack:** Rust workspace at `/home/operator/Coin/src` (cargo). Crates touched: `commputer-pouw-onchain` (staging, non-frozen), `commputer-storage`, `commputer` (node). FROZEN `src/staging/pouw/` is consumed, never modified.

## Global Constraints

- Branch `agent-testnet-20260707`. All commits local; NEVER push. NEVER stage `CLAUDE.md` or `.claude/`.
- FROZEN `src/staging/pouw/` must stay byte-identical (`git diff --stat src/staging/pouw/` empty after every task).
- PROTECTED files (`src/node/src/event_loop.rs`, `main.rs`, `config.rs`, …) are touched ONLY in Task 10, after presenting the exact hunks to the founder and receiving approval. Tasks 1–9 are entirely non-protected.
- Every task ends with: the named tests passing AND `cargo test -p <touched crates>` green, then a commit. Full `cargo test --workspace` (NOT just lib/bins) gates Tasks 5, 9, 10.
- Commit messages: conventional prefix + body, ending with
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Cf4vYxGwvCnUn7E2hux2qw`.
- Founder decisions in force (design doc `src/staging/docs/2026-07-19-escalation-round-design.md`): F1 full scope, F2 viability gate `panel.len() >= quorum(k_escalate)`, F3 reuse round-1 `phase_windows`, F4 `k_escalate=7` default, F5 exactly one round, F6 existing 10% `escalation_reward_bps`.
- Determinism discipline: sorted iteration before anything consensus-visible; no wall-clock/RNG in state.rs paths; borsh DTOs hold only Vec/Option/primitive/array/tuple fields.

---

### Task 1: S1 — Generalise EscalationRound's ledger parameter

**Files:**
- Modify: `src/staging/pouw-onchain/src/escalation_round.rs` (lines ~144 `record_commit`, ~198 `settle`)

**Interfaces:**
- Consumes: `pub trait Ledger: ChainHooks { fn for_job(&mut self, job_id: [u8; 32]); }` from `crate::escrow_ledger` (already exists; `EscrowLedger: Ledger`).
- Produces: `pub fn record_commit(&mut self, l: &mut impl Ledger, c: Commitment, height: u64) -> EventResult` and `pub fn settle(&mut self, l: &mut impl Ledger, eq: &dyn EquivalenceOracle) -> EscalationOutcome`. Later tasks pass `ChainLedger` (which implements `Ledger`) here.

- [ ] **Step 1: Change the two signatures**

In `escalation_round.rs`, add to the imports: `use crate::escrow_ledger::Ledger;` and change:

```rust
    pub fn record_commit(&mut self, l: &mut impl Ledger, c: Commitment, height: u64) -> EventResult {
```
```rust
    pub fn settle(&mut self, l: &mut impl Ledger, eq: &dyn EquivalenceOracle) -> EscalationOutcome {
```

Bodies are unchanged: `l.for_job(...)` resolves via the trait (the trait method was deliberately named identically to `EscrowLedger`'s inherent method), and `l` coerces to the `&mut dyn ChainHooks` the frozen `settle_noquorum_*` take because `Ledger: ChainHooks`. Keep the `use crate::escrow_ledger::EscrowLedger;` import — the `#[cfg(test)]` module still uses the concrete type.

- [ ] **Step 2: Verify the existing suite still passes**

Run: `cd /home/operator/Coin/src && cargo test -p commputer-pouw-onchain escalation`
Expected: `test result: ok. 11 passed` (all 9 escalation_round tests + 2 others matching the filter — the same set that passed before the change; the concrete `EscrowLedger` in tests satisfies `impl Ledger`).

- [ ] **Step 3: Verify frozen crate untouched**

Run: `git -C /home/operator/Coin diff --stat src/staging/pouw/`
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git -C /home/operator/Coin add src/staging/pouw-onchain/src/escalation_round.rs
git commit -m "refactor(pouw): S1 — EscalationRound over &mut impl Ledger (ChainLedger-ready) (local)" # + trailers
```

---

### Task 2: S2 — Job identity fields, accessors, and the persistence DTO

**Files:**
- Modify: `src/staging/pouw-onchain/src/escalation_round.rs` (struct + `open` + new accessors + DTO + tests)
- Modify: `src/staging/pouw-onchain/src/lifecycle.rs` (make 6 private converters `pub(crate)`; NO schema change)

**Interfaces:**
- Consumes: `CommitmentRec`, `RevealRec`, `SettlementOutcomeRec` (pub types in `crate::lifecycle`) and their converters `commit_to_rec`, `commit_from_rec`, `reveal_to_rec`, `reveal_from_rec`, `outcome_to_rec`, `outcome_from_rec` — change these 6 fns in lifecycle.rs from private to `pub(crate)` (bodies untouched).
- Produces (all consumed by Tasks 3–9):
  - `pub struct JobIdentity { pub program_hash: [u8; 32], pub input_hash: [u8; 32], pub da_root: [u8; 32] }` (Clone, Copy, Debug, PartialEq, Eq)
  - `EscalationRound::open(handoff, job_id, identity: JobIdentity, candidates, seed, params, deadlines, stake_of) -> Self` (identity param added 3rd)
  - Accessors: `job_id() -> [u8;32]`, `identity() -> JobIdentity`, `deadlines() -> PanelDeadlines`, `verifier_bond() -> u64`, `commitments() -> &[Commitment]`, `reveals() -> &[Reveal]`, `is_settled() -> bool`, `should_settle(height: u64) -> bool`, `expected_escrow() -> u64`
  - `pub struct EscalationRoundRecord` (borsh) + `to_record(&self) -> EscalationRoundRecord` + `from_record(rec, params: GameParams) -> Self`
  - `pub struct PanelDeadlinesRec { pub commit_by: u64, pub reveal_by: u64 }`, `pub enum PanelPhaseRec { Committing, Revealing, Settled }`, `pub enum EscalationOutcomeRec { Confirmed(SettlementOutcomeRec), Disputed(SettlementOutcomeRec), NoQuorum(SettlementOutcomeRec) }` (all borsh)

- [ ] **Step 1: lifecycle.rs — visibility only.** Change `fn commit_to_rec` → `pub(crate) fn commit_to_rec` (same for `commit_from_rec`, `reveal_to_rec`, `reveal_from_rec`, `outcome_to_rec`, `outcome_from_rec`). No other edits.

- [ ] **Step 2: Write the failing tests** (append to the `#[cfg(test)]` module in `escalation_round.rs`):

```rust
    #[test]
    fn record_round_trips_through_dto_and_settles_identically() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [9u8; 32];
        let committers = panel7();
        let (mut l1, _t0) = held_ledger(job, budget, e_bond, v_bond);
        let (mut l2, _t0b) = held_ledger(job, budget, e_bond, v_bond);
        let mut r = opened(job, budget, e_bond, v_bond);
        for (i, c) in committers.iter().enumerate() {
            assert_eq!(r.record_commit(&mut l1, commit_of(*c, [7u8; 32], [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        // Round-trip mid-flight (Committing, with commitments) — the hard case.
        let rec = r.to_record();
        let mut r2 = EscalationRound::from_record(rec.clone(), GameParams::default());
        assert_eq!(r2.to_record(), rec, "DTO round-trip is lossless");
        // Both settle identically after identical reveals.
        assert_eq!(r.advance(21), PanelPhase::Revealing);
        assert_eq!(r2.advance(21), PanelPhase::Revealing);
        for (i, c) in committers.iter().enumerate() {
            assert_eq!(r.record_reveal(reveal_of(*c, [7u8; 32], [i as u8; 32]), 25), EventResult::Accepted);
            assert_eq!(r2.record_reveal(reveal_of(*c, [7u8; 32], [i as u8; 32]), 25), EventResult::Accepted);
        }
        let o1 = r.settle(&mut l1, &ByteEq);
        let o2 = r2.settle(&mut l2, &ByteEq);
        assert_eq!(o1, o2, "reloaded round settles byte-identically");
    }

    #[test]
    fn accessors_expose_identity_deadlines_and_expected_escrow() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [10u8; 32];
        let mut l = EscrowLedger::new();
        let r = opened(job, budget, e_bond, v_bond);
        assert_eq!(r.job_id(), job);
        assert_eq!(r.identity().program_hash, [0xAAu8; 32]); // whatever `opened` passes
        assert!(r.should_settle(r.deadlines().reveal_by + 1));
        assert!(!r.should_settle(r.deadlines().reveal_by));
        // expected_escrow at open == handoff-held sum (no panel commitments yet).
        let held = budget + e_bond /* + committee bonds per `opened`'s handoff */;
        assert_eq!(r.expected_escrow(), held + r.committee_bonds_total());
        let _ = &mut l;
    }
```

Adapt the two tests to the existing test-helper signatures (`min_funding`, `panel7`, `held_ledger`, `opened`, `commit_of`, `reveal_of`, `ByteEq` — all already in the module); `opened` must be updated in this task to pass a `JobIdentity { program_hash: [0xAA;32], input_hash: [0xBB;32], da_root: [0xCC;32] }`. Add a tiny `pub(crate) fn committee_bonds_total(&self) -> u64` if the test needs it, or inline the literal from `opened`'s handoff.

- [ ] **Step 3: Run to verify they fail** — `cargo test -p commputer-pouw-onchain escalation` → compile errors (`from_record`/`to_record`/`JobIdentity` undefined).

- [ ] **Step 4: Implement.** In `escalation_round.rs`:

(a) Identity type + struct fields (after the `job_id` field):

```rust
/// The program identity the panel needs to DA-fetch + re-execute the job. Carried from the
/// settling lifecycle's record at open (lifecycle.rs P9/D8 put it there for exactly this handoff).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobIdentity {
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    pub da_root: [u8; 32],
}
```

Add `identity: JobIdentity,` to `EscalationRound` (after `job_id`), add `identity: JobIdentity` as the 3rd param of `open`, and `identity,` to the `Self { ... }` init. Update every `open(...)` call in the test module (the `opened` helper) to pass the literal identity above.

(b) Accessors (next to the existing `phase()`/`panel()`):

```rust
    pub fn job_id(&self) -> [u8; 32] { self.job_id }
    pub fn identity(&self) -> JobIdentity { self.identity }
    pub fn deadlines(&self) -> PanelDeadlines { self.deadlines }
    pub fn verifier_bond(&self) -> u64 { self.verifier_bond }
    pub fn commitments(&self) -> &[Commitment] { &self.commitments }
    pub fn reveals(&self) -> &[Reveal] { &self.reveals }
    pub fn is_settled(&self) -> bool { self.settled.is_some() }
    /// Due once the reveal window closes (mirror of JobLifecycle::should_settle).
    pub fn should_settle(&self, height: u64) -> bool {
        !self.is_settled() && height > self.deadlines.reveal_by
    }
    pub(crate) fn committee_bonds_total(&self) -> u64 {
        self.committee_bonds.iter().sum()
    }
    /// The exact pot this round owns right now: the handoff-held sum (budget + Be +
    /// round-1 revealers' bonds) plus every bond escrowed by a panel commit. The settle
    /// preflight guard (state.rs) asserts `escrowed_for_job == expected_escrow()`.
    pub fn expected_escrow(&self) -> u64 {
        self.budget
            + self.executor_bond
            + self.committee_bonds_total()
            + (self.commitments.len() as u64) * self.verifier_bond
    }
```

(c) DTO (mirror the `JobLifecycleRecord` pattern verbatim — borsh types only):

```rust
use borsh::{BorshDeserialize, BorshSerialize};
use crate::lifecycle::{
    commit_from_rec, commit_to_rec, outcome_from_rec, outcome_to_rec, reveal_from_rec,
    reveal_to_rec, CommitmentRec, RevealRec, SettlementOutcomeRec,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum PanelPhaseRec { Committing, Revealing, Settled }

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PanelDeadlinesRec { pub commit_by: u64, pub reveal_by: u64 }

#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub enum EscalationOutcomeRec {
    Confirmed(SettlementOutcomeRec),
    Disputed(SettlementOutcomeRec),
    NoQuorum(SettlementOutcomeRec),
}

/// Persistable, borsh-canonical mirror of `EscalationRound`. Omits `params` (genesis-anchored,
/// re-injected in `from_record` — the C1 discipline). STABLE on-disk schema once the alpha reset
/// ships — version it if the fields ever grow. Only Vec/Option/primitive/array fields ⇒ borsh is
/// canonical ⇒ deterministic for the state-root fold.
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct EscalationRoundRecord {
    pub job_id: [u8; 32],
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    pub da_root: [u8; 32],
    pub budget: u64,
    pub submitter: [u8; 32],
    pub executor: [u8; 32],
    pub executor_hash: [u8; 32],
    pub executor_bond: u64,
    pub verifier_bond: u64,
    pub committee_reveals: Vec<RevealRec>,
    pub committee_bonds: Vec<u64>,
    pub deadlines: PanelDeadlinesRec,
    pub panel: Vec<[u8; 32]>,
    pub phase: PanelPhaseRec,
    pub commitments: Vec<CommitmentRec>,
    pub reveals: Vec<RevealRec>,
    pub settled: Option<EscalationOutcomeRec>,
}
```

`to_record`/`from_record` methods on `EscalationRound` (same module → private-field access), with two private phase/outcome converters:

```rust
fn panel_phase_to_rec(p: PanelPhase) -> PanelPhaseRec { match p { PanelPhase::Committing => PanelPhaseRec::Committing, PanelPhase::Revealing => PanelPhaseRec::Revealing, PanelPhase::Settled => PanelPhaseRec::Settled } }
fn panel_phase_from_rec(p: PanelPhaseRec) -> PanelPhase { match p { PanelPhaseRec::Committing => PanelPhase::Committing, PanelPhaseRec::Revealing => PanelPhase::Revealing, PanelPhaseRec::Settled => PanelPhase::Settled } }
fn esc_outcome_to_rec(o: &EscalationOutcome) -> EscalationOutcomeRec { match o { EscalationOutcome::Confirmed(x) => EscalationOutcomeRec::Confirmed(outcome_to_rec(x)), EscalationOutcome::Disputed(x) => EscalationOutcomeRec::Disputed(outcome_to_rec(x)), EscalationOutcome::NoQuorum(x) => EscalationOutcomeRec::NoQuorum(outcome_to_rec(x)) } }
fn esc_outcome_from_rec(o: &EscalationOutcomeRec) -> EscalationOutcome { match o { EscalationOutcomeRec::Confirmed(x) => EscalationOutcome::Confirmed(outcome_from_rec(x)), EscalationOutcomeRec::Disputed(x) => EscalationOutcome::Disputed(outcome_from_rec(x)), EscalationOutcomeRec::NoQuorum(x) => EscalationOutcome::NoQuorum(outcome_from_rec(x)) } }
```

```rust
    pub fn to_record(&self) -> EscalationRoundRecord {
        EscalationRoundRecord {
            job_id: self.job_id,
            program_hash: self.identity.program_hash,
            input_hash: self.identity.input_hash,
            da_root: self.identity.da_root,
            budget: self.budget,
            submitter: self.submitter.0,
            executor: self.executor.0,
            executor_hash: self.executor_hash,
            executor_bond: self.executor_bond,
            verifier_bond: self.verifier_bond,
            committee_reveals: self.committee_reveals.iter().map(reveal_to_rec).collect(),
            committee_bonds: self.committee_bonds.clone(),
            deadlines: PanelDeadlinesRec { commit_by: self.deadlines.commit_by, reveal_by: self.deadlines.reveal_by },
            panel: self.panel.iter().map(|p| p.0).collect(),
            phase: panel_phase_to_rec(self.phase),
            commitments: self.commitments.iter().map(commit_to_rec).collect(),
            reveals: self.reveals.iter().map(reveal_to_rec).collect(),
            settled: self.settled.as_ref().map(esc_outcome_to_rec),
        }
    }

    /// Rebuild from the DTO, RE-INJECTING the genesis-anchored `GameParams` (C1 discipline —
    /// params are never persisted; settling a reloaded round with wrong params would fork).
    pub fn from_record(rec: EscalationRoundRecord, params: GameParams) -> Self {
        Self {
            job_id: rec.job_id,
            identity: JobIdentity { program_hash: rec.program_hash, input_hash: rec.input_hash, da_root: rec.da_root },
            budget: rec.budget,
            submitter: ParticipantId(rec.submitter),
            executor: ParticipantId(rec.executor),
            executor_hash: rec.executor_hash,
            executor_bond: rec.executor_bond,
            verifier_bond: rec.verifier_bond,
            committee_reveals: rec.committee_reveals.iter().map(reveal_from_rec).collect(),
            committee_bonds: rec.committee_bonds.clone(),
            params,
            deadlines: PanelDeadlines { commit_by: rec.deadlines.commit_by, reveal_by: rec.deadlines.reveal_by },
            panel: rec.panel.iter().map(|b| ParticipantId(*b)).collect(),
            phase: panel_phase_from_rec(rec.phase),
            commitments: rec.commitments.iter().map(commit_from_rec).collect(),
            reveals: rec.reveals.iter().map(reveal_from_rec).collect(),
            settled: rec.settled.as_ref().map(esc_outcome_from_rec),
        }
    }
```

- [ ] **Step 5: Run** `cargo test -p commputer-pouw-onchain` — Expected: all pass (old 9 escalation tests with the updated `opened` helper + the 2 new ones + the rest of the crate).

- [ ] **Step 6: Commit** — `feat(pouw): S2 — EscalationRound identity fields + accessors + borsh DTO (local)`

---

### Task 3: S3 — CF_ESCALATION in rocks.rs

**Files:**
- Modify: `src/storage/src/rocks.rs`

**Interfaces:**
- Consumes: `EscalationRoundRecord` (Task 2) — add `use commputer_pouw_onchain::escalation_round::EscalationRoundRecord;`
- Produces: `const CF_ESCALATION: &str = "escalation_rounds";` and the quintet `put_escalation`, `delete_escalation`, `all_escalation() -> Result<HashMap<[u8;32], EscalationRoundRecord>, String>`, `batch_put_escalation`, `batch_delete_escalation` — exact mirrors of the `*_lifecycle` five (rocks.rs:532–579).

- [ ] **Step 1: Failing test** (append near `m1_all_lifecycle_fails_hard_on_undecodable_row`, copying its shape):

```rust
    #[test]
    fn m1_all_escalation_fails_hard_on_undecodable_row() {
        let dir = tempdir().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        // Plant a syntactically-invalid row directly in the CF.
        let cf = store.db.cf_handle("escalation_rounds").unwrap();
        store.db.put_cf(&cf, [1u8; 32], b"garbage").unwrap();
        let err = store.all_escalation().unwrap_err();
        assert!(err.contains("CF_ESCALATION"), "actionable error names the CF: {err}");
    }
```

(Match the existing test's exact access pattern — if `m1_all_lifecycle_...` uses a helper to plant the row, reuse it.)

- [ ] **Step 2: Run to fail** — `cargo test -p commputer-storage m1_all_escalation` → compile error.

- [ ] **Step 3: Implement.** (a) Constant + doc after `CF_PENDING`:

```rust
// EscalationRound (2026-07-19): dedicated CF for the second-panel escalation rounds (borsh
// EscalationRoundRecord DTO value per raw 32-byte job_id key). The 6th consensus map. Auto-created
// on existing DBs (create_missing_column_families).
const CF_ESCALATION: &str = "escalation_rounds";
```

(b) Add `CF_ESCALATION` to ALL FOUR CF-list sites: `open` descriptor list (rocks.rs:~77-80), `clear_all` (~380-383), `batch_clear_consensus_maps` (~653 — also update its "five" doc comments at ~630/~648 to "six"), `estimate_db_size` (~673-676).

(c) The quintet — copy `put_lifecycle`/`delete_lifecycle`/`all_lifecycle`/`batch_put_lifecycle`/`batch_delete_lifecycle` verbatim, substituting `escalation`/`CF_ESCALATION`/`EscalationRoundRecord`, with the same fail-hard three-arm error handling in `all_escalation` (error strings say `CF_ESCALATION`).

- [ ] **Step 4: Run** `cargo test -p commputer-storage` — Expected: all pass (235 + 1 new).

- [ ] **Step 5: Commit** — `feat(storage): S3 — CF_ESCALATION + persistence quintet (local)`

---

### Task 4: S4 — ChainState plumbing: map, root fold, rollback, persistence, params

**Files:**
- Modify: `src/storage/src/state.rs`

**Interfaces:**
- Consumes: `EscalationRound`, `EscalationRoundRecord` (Task 2), rocks quintet (Task 3). Import: `use commputer_pouw_onchain::escalation_round::EscalationRound;`
- Produces: `pub escalation_rounds: HashMap<[u8; 32], EscalationRound>` on `ChainState`, fully wired into: `new()`, `open()` (+ `persisted_escalation_keys` mirror), `compute_state_root` (6th Policy-B section + the all-empty early return), `capture_pre_block`/`rollback_to_pre_block`/`BlockSnapshot`, `batch_map_deltas`/`commit_map_mirrors`, `reset_to_genesis`, `try_reorg` (clear + saved-mirror tuple), `revert_block` guard, `snapshot()` debug JSON, the `Debug` impl, `set_consensus_params` (C1 rebuild loop). Later tasks rely on the field name `escalation_rounds`.

Mechanical checklist (every site named in recon; each is a copy of the `job_lifecycles`/`pending_jobs` line beside it):

- [ ] **Step 1: Failing test** (state.rs test module):

```rust
    #[test]
    fn escalation_rounds_fold_persist_and_reload() {
        // A mid-flight round folds into the root, survives capture/rollback, persists to
        // CF_ESCALATION, and reloads identically with params re-injected.
        let dir = tempdir().unwrap();
        let mut s = ChainState::open(dir.path()).unwrap();
        s.apply_block(&genesis_block()).unwrap();
        let root_before = s.compute_state_root();
        let r = test_escalation_round([5u8; 32]); // helper: EscalationRound::open with fixed inputs
        s.escalation_rounds.insert([5u8; 32], r);
        let root_with = s.compute_state_root();
        assert_ne!(root_before, root_with, "6th section folds in");
        // Rollback restores byte-identically.
        let snap = s.capture_pre_block_for_test(); // or drive via a failing block if no test hook
        s.escalation_rounds.clear();
        s.rollback_to_pre_block_for_test(snap);
        assert_eq!(s.compute_state_root(), root_with);
        // Persist + reload.
        s.persist_for_test(); // whatever the existing lifecycle persistence tests use
        drop(s);
        let s2 = ChainState::open(dir.path()).unwrap();
        assert_eq!(s2.escalation_rounds.len(), 1);
        assert_eq!(s2.compute_state_root(), root_with, "reload reproduces the root");
    }
```

IMPORTANT: adapt to the EXISTING test idioms — find how the B1a/B1b persistence tests in state.rs drive capture/rollback and persist (they exist for `job_lifecycles`; copy their shape rather than inventing `_for_test` hooks). `test_escalation_round` is a new local helper building a round via `EscalationRound::open` with a 2-candidate pool, `GameParams::default()`, `PanelDeadlines { commit_by: 20, reveal_by: 30 }`, identity `[0xAA;32]/[0xBB;32]/[0xCC;32]`.

- [ ] **Step 2: Run to fail**, then **Step 3: Implement** — every site:

1. Struct field (after `pending_jobs`): `pub escalation_rounds: HashMap<[u8; 32], EscalationRound>,` + mirror `persisted_escalation_keys: HashSet<[u8; 32]>,` (after `persisted_pending_keys`).
2. `new()`: both init to empty.
3. `open()`: after the lifecycle load block — `let escalation_rounds: HashMap<[u8;32], EscalationRound> = rocks.all_escalation().map_err(StateError::StorageError)?.into_iter().map(|(id, rec)| (id, EscalationRound::from_record(rec, game_params.clone()))).collect();` + `let persisted_escalation_keys: HashSet<[u8;32]> = escalation_rounds.keys().copied().collect();` + both into the `Self { ... }`.
4. `compute_state_root`: add `&& self.escalation_rounds.is_empty()` to the Policy-B early return, and append the 6th section AFTER pending_jobs (order is consensus — document it):

```rust
        // escalation_rounds (EscalationRound 2026-07-19): sorted by job_id, length-prefixed;
        // value = borsh(EscalationRoundRecord). The SIXTH Policy-B section — appended after
        // pending_jobs; the section order is consensus.
        let mut escalations: Vec<(&[u8; 32], &EscalationRound)> = self.escalation_rounds.iter().collect();
        escalations.sort_by(|a, b| a.0.cmp(b.0));
        h.update((escalations.len() as u64).to_le_bytes());
        for (job_id, er) in escalations {
            h.update(job_id);
            let blob = borsh::to_vec(&er.to_record())
                .expect("escalation record borsh serialization should not fail");
            h.update((blob.len() as u64).to_le_bytes());
            h.update(&blob);
        }
```

5. `BlockSnapshot` + `capture_pre_block` + `rollback_to_pre_block`: add `escalation_rounds` (clone/restore), exactly like `job_lifecycles`.
6. `batch_map_deltas`: append the stale-delete + full-value re-put pair using `batch_delete_escalation`/`batch_put_escalation` and `er.to_record()`. `commit_map_mirrors`: `self.persisted_escalation_keys = self.escalation_rounds.keys().copied().collect();`. Update both fns' "five" doc comments to "six".
7. `reset_to_genesis`: `self.escalation_rounds.clear();` + `self.persisted_escalation_keys.clear();`.
8. `try_reorg`: add `self.escalation_rounds.clear();` beside the lifecycle clear (~2650) and extend the saved-mirrors save/restore tuple with `persisted_escalation_keys`.
9. `revert_block` guard: add `|| !self.escalation_rounds.is_empty()` to the refusal condition.
10. `Debug` impl: `.field("escalation_rounds", &self.escalation_rounds.len())`. `snapshot()` debug JSON: emit a sorted `escalation_rounds` array via `er.to_record()` (mirror the lifecycle entry shape), keyed `"escalation_rounds"`.
11. `set_consensus_params`: after the lifecycle rebuild loop —

```rust
        // C1: same re-injection for escalation rounds (params are never persisted).
        for er in self.escalation_rounds.values_mut() {
            let rec = er.to_record();
            *er = EscalationRound::from_record(rec, self.game_params.clone());
        }
```

- [ ] **Step 4: Run** `cargo test -p commputer-storage` — all pass. **Step 5: Commit** — `feat(storage): S4 — escalation_rounds consensus map: root fold, rollback, persistence, C1 (local)`

---

### Task 5: S5+S6 — THE FLIP: open-draw-gate on Escalate + settle sweep

**Files:**
- Modify: `src/storage/src/state.rs` (`lifecycle_settle_and_drain`, `settle_due_jobs`, `apply_txs_with_rollback` call, new `escalation_settle_and_drain`)

**Interfaces:**
- Consumes: `EscalationRound::{open, advance, settle, should_settle, is_settled, expected_escrow, panel}`, `JobIdentity` (Task 2); `ChainLedger` (existing; implements `Ledger` — verify it has an `impl Ledger for ChainLedger` with `for_job`; it does, per the §3 integration).
- Produces:
  - `settle_due_jobs(&mut self, height: u64, block_hash: BlockHash)` (signature change; sole caller `apply_txs_with_rollback` passes `block.hash()`)
  - `pub fn lifecycle_settle_and_drain(&mut self, job_id, eq, block_hash: BlockHash) -> Result<Option<(Terminal, Option<SettlementOutcome>)>, StateError>` — on `Terminal::Escalate`: gate-draw-open or fallback (below). Update EVERY existing caller/test (grep `lifecycle_settle_and_drain(`) to pass a block hash (tests: `BlockHash([0u8;32])` or the driving block's hash).
  - `pub fn escalation_settle_and_drain(&mut self, job_id: [u8; 32], eq: &dyn EquivalenceOracle) -> Result<Option<EscalationOutcome>, StateError>`

- [ ] **Step 1: Failing tests** (state.rs test module; reuse the `onchain_claimed`-family helpers and the B10 fixtures):

```rust
    /// F2 gate PASSES: enough candidates outside the round-1 committee ⇒ a real round opens,
    /// the pot stays held, and the panel is deterministic across two nodes.
    #[test]
    fn escalate_opens_round_when_panel_viable_and_is_deterministic() { /* two states built with
        different bonded-stake insertion order (mirror b5_onchain_two_nodes_...): 12 bonded
        verifiers, k=3 committee, force a round-1 NoQuorum (3 distinct reveal hashes), apply the
        SAME settle-height block to both ⇒ assert: escalation_rounds contains the job on both;
        panel identical on both; panel.len() >= GameParams::default().quorum(7) == 5;
        escrowed_for_job == budget + e_bond + 3*v_bond (pot HELD, not refunded);
        identical state roots. */ }

    /// F2 gate FAILS: candidate pool too small ⇒ byte-identical to today's fallback refund.
    #[test]
    fn escalate_falls_back_when_panel_unviable() { /* 5 bonded verifiers (3 drawn ⇒ 2 candidates
        < quorum 5): force round-1 NoQuorum ⇒ assert: escalation_rounds EMPTY; submitter refunded
        the budget; executor bond returned; revealer bonds returned; total_burned unchanged;
        escrowed_for_job == 0. */ }

    /// The full on-chain escalation: open ⇒ panel commits/reveals via txs (Task 6 routes them;
    /// HERE call state.escalation_record_commit/reveal helpers directly) ⇒ settle_due_jobs at
    /// reveal_by+1 settles Confirmed and drains.
    #[test]
    fn escalation_round_settles_confirmed_and_drains() { /* drive 5 panel members to commit+reveal
        the executor's hash ⇒ advance blocks past reveal_by ⇒ assert EscalationOutcome::Confirmed;
        escrowed_for_job == 0; executor balance includes 85% of budget; panel members got bond
        back + escalation reward; conserved() unchanged. */ }

    /// Rollback safety: a block whose LAST tx fails after the tail would have opened a round
    /// leaves escalation_rounds byte-identical (the whole tail is inside the envelope).
    #[test]
    fn rejected_block_leaves_escalation_rounds_untouched() { /* craft a block that would settle
        the NoQuorum lifecycle in its tail but carries a final invalid tx ⇒ apply_block Err ⇒
        assert escalation_rounds empty + state root unchanged. */ }
```

Write these as REAL tests (the comments above are specifications — expand them against the existing helpers; `b5_onchain_two_nodes_same_block_identical_committee_and_root` at state.rs:8816 and `onchain_claimed` at :8780 are the templates; forcing NoQuorum = three verifiers commit+reveal three DIFFERENT hashes).

- [ ] **Step 2: Run to fail.** **Step 3: Implement:**

(a) In `lifecycle_settle_and_drain`, replace the `Terminal::Escalate` arm body:

```rust
        let fb = if let Terminal::Escalate(h) = &terminal {
            // Pot preflight (unchanged): the held sum the round (or fallback) will own.
            let expected = h
                .budget
                .saturating_add(h.executor_bond)
                .saturating_add(h.committee_bonds.iter().sum::<u64>());
            let actual = self.escrowed_for_job(&job_id);
            if actual != expected {
                return Err(StateError::InvalidBlock(format!(
                    "escalate pot {actual} != expected {expected}; refusing escalation open"
                )));
            }
            // EscalationRound (2026-07-19): draw the panel and apply the F2 viability gate.
            // Candidates = the settling lifecycle's claim-time snapshot MINUS its round-1
            // committee (executor auto-excluded inside select_committee). Seed domain-separated
            // from the round-1 draw by the "escalate" tag. Deadlines anchor at the CURRENT
            // (parent) height with the round-1 windows (F3). All inputs are consensus state.
            let rec = self
                .job_lifecycles
                .get(&job_id)
                .expect("lifecycle re-inserted by lifecycle_settle")
                .to_record();
            let committee: std::collections::HashSet<[u8; 32]> = rec.committee.iter().copied().collect();
            let candidates: Vec<commputer_pouw::ids::ParticipantId> = rec
                .candidates
                .iter()
                .filter(|c| !committee.contains(*c))
                .map(|c| commputer_pouw::ids::ParticipantId(*c))
                .collect();
            let seed = commputer_pouw::ids::hash_parts(&[&block_hash.0, &job_id, b"escalate"]);
            let height = self.blocks.height();
            let deadlines = commputer_pouw_onchain::escalation_round::PanelDeadlines {
                commit_by: height.saturating_add(self.phase_windows.commit_blocks),
                reveal_by: height
                    .saturating_add(self.phase_windows.commit_blocks)
                    .saturating_add(self.phase_windows.reveal_blocks),
            };
            let identity = commputer_pouw_onchain::escalation_round::JobIdentity {
                program_hash: rec.program_hash,
                input_hash: rec.input_hash,
                da_root: rec.da_root,
            };
            let h = h.clone();
            let round = {
                let chain = &*self;
                EscalationRound::open(
                    h.clone(), job_id, identity, candidates, seed,
                    self.game_params.clone(), deadlines,
                    &|p| chain.stake_of(&Address(p.0)),
                )
            };
            if round.panel().len() >= self.game_params.quorum(self.game_params.k_escalate) {
                // F2 gate PASSES: the round owns the held pot from here; no money moves at open.
                self.escalation_rounds.insert(job_id, round);
                None
            } else {
                // F2 gate FAILS (structural shortage, not misbehavior): zero-comp refund,
                // byte-identical to the pre-EscalationRound stand-in.
                let mut view = ChainLedger::new(self);
                Some(resolve_escalation_fallback(&mut view, job_id, &h))
            }
        } else {
            None
        };
```

(Borrow note: `EscalationRound::open` only READS `self` via the stake closure — the `{ let chain = &*self; ... }` block scopes the shared borrow before the `insert`/`ChainLedger::new(self)` mutable uses, same dance as `draw_committees_for_completed_jobs`.)

(b) `settle_due_jobs(&mut self, height: u64, block_hash: BlockHash)` — pass `block_hash` through to `lifecycle_settle_and_drain`, and append the escalation sweep after the lifecycle loop:

```rust
        // EscalationRound sweep: advance then settle-when-due, SORTED job order (same
        // discipline as the lifecycle sweep above; the pinned ByteEq oracle is reused).
        let mut esc: Vec<[u8; 32]> = self.escalation_rounds.keys().copied().collect();
        esc.sort_unstable();
        for job_id in esc {
            if let Some(er) = self.escalation_rounds.get_mut(&job_id) {
                er.advance(height);
            }
            let due = self
                .escalation_rounds
                .get(&job_id)
                .map(|er| er.should_settle(height) || er.is_settled())
                .unwrap_or(false);
            if due {
                self.escalation_settle_and_drain(job_id, &SETTLE_ORACLE)?;
            }
        }
```

(c) `escalation_settle_and_drain` (new, mirroring `lifecycle_settle_and_drain`'s guard/dance):

```rust
    /// Settle + drain one escalation round (all three outcomes drain the pot to 0). Removed on
    /// success ⇒ at-most-once. The pot preflight mirrors the primary's P1 caller contract.
    pub fn escalation_settle_and_drain(
        &mut self,
        job_id: [u8; 32],
        eq: &dyn EquivalenceOracle,
    ) -> Result<Option<EscalationOutcome>, StateError> {
        let mut round = match self.escalation_rounds.remove(&job_id) {
            Some(r) => r,
            None => return Ok(None),
        };
        if round.is_settled() {
            // Cached terminal: pot already drained; just drop the round (drain).
            let out = round.settle(&mut ChainLedger::new(self), eq);
            return Ok(Some(out));
        }
        let expected = round.expected_escrow();
        let actual = self.escrowed_for_job(&job_id);
        if actual != expected {
            self.escalation_rounds.insert(job_id, round);
            return Err(StateError::InvalidBlock(format!(
                "escalation pot {actual} != expected {expected}; refusing to settle"
            )));
        }
        let out = round.settle(&mut ChainLedger::new(self), eq);
        Ok(Some(out))
    }
```

(NOTE for the implementer: `round.settle` on an already-settled round returns the cached outcome and moves NO money — that is why the cached-branch call above is safe. Verify `ChainLedger` implements `Ledger` (grep `impl.*Ledger.*for ChainLedger` / `for_job` in state.rs); it must, since `resolve_escalation_fallback(&mut view, ...)` already takes it as `&mut impl Ledger`.)

(d) `apply_txs_with_rollback`: `self.settle_due_jobs(block.height(), block.hash())`.

- [ ] **Step 4: Update broken callers/tests.** Grep `lifecycle_settle_and_drain(` and `settle_due_jobs(` across state.rs tests — add the block-hash arg. The tests pinning the OLD Escalate-always-fallback behavior (search `resolve_escalation_fallback` / `escalate` in test names) must be re-pointed: with a viable pool they now assert round-open, with a tiny pool they assert the unchanged fallback (most existing tests use 3-candidate pools < quorum(7)=5 ⇒ they keep passing via the gate's fallback path — verify, don't assume).

- [ ] **Step 5: Run** `cargo test -p commputer-storage` then FULL `cargo test --workspace` — all green. **Step 6: Commit** — `feat(pouw): S5+S6 THE FLIP — NoQuorum opens a gated real 2nd panel on-chain (local)`

---

### Task 6: S7 — Commit/Reveal routing + phase-defer classification

**Files:**
- Modify: `src/storage/src/state.rs` (`apply_commit`, `apply_reveal`, `tx_is_phase_deferred`, two new helpers)

**Interfaces:**
- Consumes: `escalation_rounds` map (Task 4), `EscalationRound::{record_commit, record_reveal, advance}` (Tasks 1–2). Note `escalation_round::EventResult`/`RejectReason` are DISTINCT types from `lifecycle`'s.
- Produces: `pub fn escalation_record_commit(&mut self, job_id, c: Commitment, height: u64) -> Result<Option<escalation_round::EventResult>, StateError>` and `pub fn escalation_record_reveal(&mut self, job_id, r: Reveal, height: u64) -> Option<escalation_round::EventResult>` (mirrors of the lifecycle helpers, incl. the balance pre-check before `record_commit`).

- [ ] **Step 1: Failing tests:**

```rust
    #[test]
    fn commit_and_reveal_route_to_an_active_escalation_round() { /* open a viable round (Task 5
        helper), then apply_block with a panel member's Commit tx ⇒ accepted, bond escrowed
        (escrowed_for_job grew by v_bond), nonce bumped; advance past commit_by; Reveal tx ⇒
        accepted. A NON-panel validator's Commit ⇒ whole-block reject (NotPanelMember). */ }

    #[test]
    fn escalation_commit_reveal_are_phase_deferred_not_dropped() { /* a panel Commit trial-applied
        during Revealing (or a Reveal during Committing) errors ⇒ tx_is_phase_deferred returns
        true because escalation_rounds contains the job ⇒ select_applicable_txs requeues it. */ }
```

- [ ] **Step 2: Run to fail.** **Step 3: Implement:**

(a) New helpers (beside `lifecycle_record_commit`/`lifecycle_record_reveal`, same borrow dance + balance pre-check):

```rust
    /// EscalationRound twin of `lifecycle_record_commit` (bond escrow via ChainLedger; balance
    /// pre-checked so the infallible escrow cannot panic). `None` if no round for `job_id`.
    pub fn escalation_record_commit(
        &mut self,
        job_id: [u8; 32],
        c: Commitment,
        height: u64,
    ) -> Result<Option<commputer_pouw_onchain::escalation_round::EventResult>, StateError> {
        let mut round = match self.escalation_rounds.remove(&job_id) {
            Some(r) => r,
            None => return Ok(None),
        };
        let committer = Address(c.verifier.0);
        let bal = self.accounts.get(&committer).map(|a| a.balance.raw()).unwrap_or(0);
        if bal < c.bond {
            self.escalation_rounds.insert(job_id, round);
            return Err(StateError::InsufficientBalance);
        }
        let mut view = ChainLedger::new(self);
        let r = round.record_commit(&mut view, c, height);
        self.escalation_rounds.insert(job_id, round);
        Ok(Some(r))
    }

    /// EscalationRound twin of `lifecycle_record_reveal` (no money move).
    pub fn escalation_record_reveal(
        &mut self,
        job_id: [u8; 32],
        r: Reveal,
        height: u64,
    ) -> Option<commputer_pouw_onchain::escalation_round::EventResult> {
        let round = self.escalation_rounds.get_mut(&job_id)?;
        Some(round.record_reveal(r, height))
    }
```

(b) `apply_commit`: after the existing gates, route by which map owns the job (a job is never live in both — the primary drains before the round opens):

```rust
        let c = Commitment { verifier: ParticipantId(from.0), commit, bond };
        if self.job_lifecycles.contains_key(&job_id) {
            match self.lifecycle_record_commit(job_id, c, height)? {
                Some(EventResult::Accepted) => Ok(()),
                Some(EventResult::Rejected(r)) => Err(StateError::InvalidBlock(format!("commit rejected: {r:?}"))),
                None => Err(StateError::InvalidBlock("commit: unknown job".into())),
            }
        } else {
            use commputer_pouw_onchain::escalation_round::EventResult as PanelEventResult;
            match self.escalation_record_commit(job_id, c, height)? {
                Some(PanelEventResult::Accepted) => Ok(()),
                Some(PanelEventResult::Rejected(r)) => Err(StateError::InvalidBlock(format!("panel commit rejected: {r:?}"))),
                None => Err(StateError::InvalidBlock("commit: unknown job".into())),
            }
        }
```

(c) `apply_reveal`: same two-way routing; the escalation arm self-advances first (mirror of the primary's `lifecycle_advance` line):

```rust
        if self.job_lifecycles.contains_key(&job_id) {
            self.lifecycle_advance(job_id, height);
            let r = Reveal { verifier: ParticipantId(from.0), result_hash, salt };
            match self.lifecycle_record_reveal(job_id, r, height) { /* existing three arms */ }
        } else {
            if let Some(round) = self.escalation_rounds.get_mut(&job_id) {
                round.advance(height); // idempotent height transition on the tx path
            }
            use commputer_pouw_onchain::escalation_round::EventResult as PanelEventResult;
            let r = Reveal { verifier: ParticipantId(from.0), result_hash, salt };
            match self.escalation_record_reveal(job_id, r, height) {
                Some(PanelEventResult::Accepted) => Ok(()),
                Some(PanelEventResult::Rejected(rr)) => Err(StateError::InvalidBlock(format!("panel reveal rejected: {rr:?}"))),
                None => Err(StateError::InvalidBlock("reveal: unknown job".into())),
            }
        }
```

(d) `tx_is_phase_deferred`: split the arm —

```rust
            TxKind::Commit { job_id, .. } | TxKind::Reveal { job_id, .. } => {
                self.job_lifecycles.contains_key(job_id)
                    || self.pending_jobs.contains_key(job_id)
                    || self.escalation_rounds.contains_key(job_id)
            }
            TxKind::CompleteJob { job_id, .. } => {
                self.job_lifecycles.contains_key(job_id) || self.pending_jobs.contains_key(job_id)
            }
```

- [ ] **Step 4: Run** `cargo test -p commputer-storage` — all pass. **Step 5: Commit** — `feat(pouw): S7 — Commit/Reveal route to active escalation rounds; C3 defer (local)`

---

### Task 7: Golden oracle + B10 equivalence leg

**Files:**
- Modify: `src/staging/pouw-onchain/src/escalation_round.rs` (test module — golden oracle) and/or `src/storage/src/state.rs` (test module — B10 leg)

**Interfaces:** Consumes the frozen `escalation::resolve` + `Trigger::NoQuorum` + `Escalation` (`src/staging/pouw/src/escalation.rs:66-76`), the B10 `run_on_both`/`assert_equivalent` harness (state.rs:~4674+).

- [ ] **Step 1: Golden-oracle test** (escalation_round.rs tests — all-participate inputs must match the frozen reference field-for-field):

```rust
    #[test]
    fn golden_full_panel_matches_frozen_escalation_resolve() {
        // ALL k_escalate panelists participate ⇒ EscalationRound::settle must equal the frozen
        // escalation::resolve for the same inputs (same seed/candidates ⇒ same panel; every
        // member commits+reveals ⇒ 'effective panel' == full panel, the reference's assumption).
        let (budget, e_bond, v_bond) = min_funding();
        let job = [11u8; 32];
        let p = GameParams::default();
        // Drive the on-chain machine to Confirmed with a FULL panel:
        let (mut l1, _t) = held_ledger(job, budget, e_bond, v_bond);
        let mut r = opened(job, budget, e_bond, v_bond);
        let panel: Vec<ParticipantId> = r.panel().to_vec();
        for (i, m) in panel.iter().enumerate() {
            assert_eq!(r.record_commit(&mut l1, commit_of(*m, EXEC_HASH, [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        r.advance(21);
        let mut panel_reveals = Vec::new();
        for (i, m) in panel.iter().enumerate() {
            let rv = reveal_of(*m, EXEC_HASH, [i as u8; 32]);
            panel_reveals.push(rv.clone());
            assert_eq!(r.record_reveal(rv, 25), EventResult::Accepted);
        }
        let EscalationOutcome::Confirmed(on_chain) = r.settle(&mut l1, &ByteEq) else { panic!("expected Confirmed") };
        // The frozen reference over identical inputs (fresh ledger funded identically):
        let (mut l2, _t2) = held_ledger(job, budget, e_bond, v_bond);
        for m in &panel { /* fund + escrow each panel bond exactly as record_commit did */ }
        let esc = Escalation { seed: SEED, candidates: &CANDS, budget, executor: EXEC,
            executor_hash: EXEC_HASH, executor_bond: e_bond, panel_reveals: &panel_reveals, panel_bond: v_bond };
        let (verdict, reference) = commputer_pouw::escalation::resolve(
            &mut l2, &p, &esc, Trigger::NoQuorum { submitter: SUBM,
                committee_reveals: &COMMITTEE_REVEALS, committee_bonds: &COMMITTEE_BONDS },
            &ByteEq, &stake_all_equal);
        assert!(matches!(verdict, Verdict::Confirmed { .. }));
        assert_eq!(on_chain, reference, "on-chain path == frozen oracle, field-for-field");
    }
```

(Expand the CAPS placeholders from the `opened` helper's actual constants — seed/candidates/executor/submitter/committee reveals+bonds must be the SAME values `opened` builds, or the panels diverge. The `l2` escrow loop replays each `escrow(member, v_bond)` so both ledgers enter settlement in the same state.)

- [ ] **Step 2: B10 escalation leg** (state.rs tests): extend the `Scenario`/`run_on_both` machinery with an escalation phase, OR write a standalone `equivalence_escalation_confirmed_staging_matches_chainstate` that: builds the primary NoQuorum identically on both backends (staging `JobLifecycle`+`EscrowLedger` vs `ChainState` blocks), opens the round on both (staging: `EscalationRound::open` directly with the chain's drawn panel inputs; chain: via the Task 5 flip), drives identical panel commits/reveals on both, settles both, then asserts terminal equality field-for-field + per-participant end balances + `escrowed_for(job)==0` on both + conservation. Copy `assert_equivalent`'s assertions.

- [ ] **Step 3: Run** `cargo test -p commputer-pouw-onchain golden` + `cargo test -p commputer-storage equivalence_escalation` — pass. **Step 4: Commit** — `test(pouw): golden oracle + B10 equivalence leg for EscalationRound (local)`

---

### Task 8: S8 — Verifier-loop panel arm

**Files:**
- Modify: `src/node/src/verifier_loop.rs` (`build_verifier_views` + unit tests)

**Interfaces:**
- Consumes: `EscalationRound::{panel, phase, deadlines, verifier_bond, commitments, reveals, is_settled, identity}` (Task 2), `PanelPhase`.
- Produces: `pub fn build_verifier_views(now_height: u64, me: Address, my_balance: u64, job_lifecycles: &HashMap<[u8;32], JobLifecycle>, escalation_rounds: &HashMap<[u8;32], EscalationRound>) -> VerifierTick` — NEW 5th param. Update ALL non-protected callers (verifier_loop unit tests + `src/node/tests/pouw_payout_e2e.rs`) to pass `&state.escalation_rounds`; the PROTECTED call site in `event_loop.rs` is Task 10 (until then the node crate will not compile against the protected file — so in THIS task, keep the old 4-arg signature as a thin wrapper? NO: instead, make the protected call compile by giving the new param a default via a second function):
  - Add `pub fn build_verifier_views_with_escalations(now_height, me, my_balance, job_lifecycles, escalation_rounds) -> VerifierTick` (the new full logic).
  - Keep `build_verifier_views(...)` (4 args) delegating with an empty map: `build_verifier_views_with_escalations(now_height, me, my_balance, job_lifecycles, &HashMap::new())`. The PROTECTED event_loop keeps calling the 4-arg fn until Task 10 swaps it — the node stays compiling and behavior-unchanged the whole time.

- [ ] **Step 1: Failing test** (verifier_loop.rs test module, mirroring `build_verifier_views_projects_committees_i_am_on`):

```rust
    #[test]
    fn build_views_projects_escalation_panels_i_am_on() {
        // One escalation round whose panel contains `me` ⇒ exactly one extra view with the
        // panel's deadlines/phase/bond + the job identity carried from the round; a round NOT
        // containing me contributes nothing; a settled round sets `settled` (salt GC).
        let me = addr(1);
        let mut esc = HashMap::new();
        esc.insert([1u8; 32], test_round_with_panel(&[me.0, [2u8; 32], [3u8; 32]], PanelPhase::Committing));
        esc.insert([2u8; 32], test_round_with_panel(&[[2u8; 32], [3u8; 32], [4u8; 32]], PanelPhase::Committing));
        let tick = build_verifier_views_with_escalations(10, me, 1_000, &HashMap::new(), &esc);
        assert_eq!(tick.committees.len(), 1);
        let v = &tick.committees[0];
        assert_eq!(v.job_id, [1u8; 32]);
        assert_eq!(v.phase, VerifierPhase::Committing);
        assert!(!v.already_committed && !v.already_revealed);
        assert_eq!(v.da_root, [0xCCu8; 32]); // identity carried from the round
    }
```

(`test_round_with_panel` = local helper constructing an `EscalationRound` via `open` with a candidate list that makes `select_committee` draw exactly the wanted panel — simplest: candidates == wanted panel, `k_escalate >= len`, equal stakes; identity `[0xAA]/[0xBB]/[0xCC]`.)

- [ ] **Step 2: Run to fail.** **Step 3: Implement** — in `build_verifier_views_with_escalations`, after the lifecycle loop and BEFORE the sort, append:

```rust
    // EscalationRound panels (S8): a panel seat is driven through the SAME planner/emit path as a
    // round-1 committee seat — the tx kinds (Commit/Reveal by job_id) are identical and the chain
    // routes them to the round (state.rs S7). PanelPhase maps onto the same VerifierPhase.
    for (job_id, er) in escalation_rounds {
        if !er.panel().iter().any(|p| p.0 == me.0) {
            continue;
        }
        let identity = er.identity();
        committees.push(VerifierCommitteeView {
            job_id: *job_id,
            phase: match er.phase() {
                PanelPhase::Committing => VerifierPhase::Committing,
                PanelPhase::Revealing => VerifierPhase::Revealing,
                PanelPhase::Settled => VerifierPhase::Other,
            },
            commit_by: er.deadlines().commit_by,
            reveal_by: er.deadlines().reveal_by,
            verifier_bond: er.verifier_bond(),
            already_committed: er.commitments().iter().any(|c| c.verifier.0 == me.0),
            already_revealed: er.reveals().iter().any(|r| r.verifier.0 == me.0),
            program_hash: identity.program_hash,
            input_hash: identity.input_hash,
            da_root: identity.da_root,
            settled: er.is_settled(),
        });
    }
```

Imports: `use commputer_pouw_onchain::escalation_round::{EscalationRound, PanelPhase};`. The existing sort-by-job_id after the loop keeps the tick byte-stable. (Panel and round-1 job_ids never coexist — the primary drained before the round opened — so no dedup is needed; note this in a comment.)

- [ ] **Step 4: Run** `cargo test -p commputer --lib verifier` — pass. **Step 5: Commit** — `feat(node): S8 — verifier loop surfaces escalation panels (4-arg shim keeps event_loop unchanged) (local)`

---

### Task 9: e2e — escalation scenarios through the REAL loops

**Files:**
- Modify: `src/node/tests/pouw_payout_e2e.rs`

**Interfaces:** Consumes the existing harness (`drive_round`, `unsigned`, `next_block`, `conserved`, `drive_verifier`, `build_verifier_views_with_escalations`). The harness must grow a variant with N verifiers (currently 3).

- [ ] **Step 1: Update the NoQuorum negative-control.** `pouw_noquorum_refunds_submitter_pays_no_worker_comp` (3 verifiers; candidates minus committee = 0 < quorum(7)=5): the F2 gate takes the FALLBACK — every existing assertion stays true. Add two lines making the gate explicit:

```rust
    assert!(s.escalation_rounds.is_empty(), "F2 gate: 0 spare candidates < quorum(7) ⇒ fallback, no round");
```

- [ ] **Step 2: New scenario (a) — panel Confirms** (harness with 9 bonded verifiers; 3 drawn round-1 split three ways ⇒ NoQuorum ⇒ 6 spare candidates ≥ 5 ⇒ round opens; panel driven by the REAL verifier loops via `build_verifier_views_with_escalations` per panel member ⇒ they DA-fetch, re-execute, commit+reveal the TRUE hash ⇒ Confirmed):

Assert: `escalation_rounds` empties after settle; executor received 85% of budget (net of what round-1 moved); panel members each ended `> starting balance` (bond back + escalation share); the round-1 verifier whose reveal matched the panel verdict is vindicated (bond back), the other two slashed; `escrowed_for_job == 0`; `conserved()` unchanged every block. Forcing the round-1 split: hand-feed the three round-1 verifiers' Commit/Reveal txs with three DISTINCT hashes (the fraud path at e2e:~586-598 is the template for hand-feeding) while the EXECUTOR runs the real loop.

- [ ] **Step 3: New scenario (b) — panel also-NoQuorum ⇒ bounded terminal.** Same 9-verifier open, but hand-feed the panel's commits/reveals split across ≥3 distinct hashes so no 5-quorum forms. Assert: submitter refunded budget; executor bond BURNED (`total_burned` grew by ≥ e_bond); all three round-1 revealers slashed; panel members kept bond + reward; pot 0; conserved.

- [ ] **Step 4: Run** `cargo test -p commputer --test pouw_payout_e2e` — all (3 old + 2 new) pass. Then FULL `cargo test --workspace`. **Step 5: Commit** — `test(pouw): e2e — escalation panel Confirms / bounded-NoQuorum through real loops (local)`

---

### Task 10: PROTECTED hunks (founder approval) + final gates

**Files:**
- Modify (PROTECTED, founder-approval REQUIRED first): `src/node/src/event_loop.rs` — exactly two hunks.

- [ ] **Step 1: Present both hunks to the founder and WAIT for approval** (AskUserQuestion, like the 2026-07-18 FetchChunk hunk):

P1 — C7 ingress filter (~line 2593): the known-job check becomes

```rust
                if !self.state.job_lifecycles.contains_key(job_id)
                    && !self.state.pending_jobs.contains_key(job_id)
                    && !self.state.escalation_rounds.contains_key(job_id)
                {
                    return Err("pouw tx references unknown job");
                }
```

P2 — `push_verifier_snapshot` (~line 421): the call becomes

```rust
        let tick = commputer::verifier_loop::build_verifier_views_with_escalations(
            self.state.blocks.height(), me, my_balance, &self.state.job_lifecycles,
            &self.state.escalation_rounds,
        );
```

- [ ] **Step 2 (after approval): apply, then delete the 4-arg `build_verifier_views` shim** in verifier_loop.rs (its last caller is gone) and rename `build_verifier_views_with_escalations` → `build_verifier_views` everywhere (tests + e2e + event_loop hunk P2 uses the final name — coordinate the rename INTO the approved hunk text, i.e. present P2 already using `build_verifier_views` with 5 args and do the rename in the same commit).
- [ ] **Step 3: Full gates:** `cargo test --workspace` all green; `git diff --stat src/staging/pouw/` empty; 3-lens adversarial review workflow over the full feature diff (consensus/DA/liveness lenses, as on 2026-07-18); then the standard multi-node payout smoke (`scripts/pouw_payout_smoke.sh`) to prove NO REGRESSION on the happy path (a live escalation smoke needs ≥9 bonded nodes or a genesis-lowered `k_escalate` — founder decision at the reset; documented, not built here).
- [ ] **Step 4: Commit** — `feat(pouw): EscalationRound live — protected C7+snapshot hunks (founder-approved) (local)`. Update memory (session file + MEMORY.md).

---

## Plan self-review notes (already applied)

- Spec coverage: F1→Tasks 1-10; F2 gate→Task 5 (both sides tested); F3 windows→Task 5 deadlines; F4/F5/F6→no code (defaults); persistence/root/C1→Tasks 3-4; golden oracle+B10→Task 7; S8→Tasks 8+10; e2e→Task 9; protected surface→Task 10 only; consensus-reset note→design doc.
- Type consistency: `EventResult` name collision between `lifecycle` and `escalation_round` handled via the `PanelEventResult` alias (Task 6); `build_verifier_views` 4→5-arg migration handled via the shim (Task 8) and final rename (Task 10).
- Known adaptation points (implementer MUST resolve against real code, not skip): exact test-helper signatures in each test module; existing-test updates in Task 5 Step 4; the `snapshot()` JSON shape in Task 4.
