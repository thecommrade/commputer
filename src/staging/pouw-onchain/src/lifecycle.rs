//! Module 5 — the multi-block lifecycle state machine (blueprint phase P2a; open-Q#4).
//!
//! Replaces the synchronous `engine::run_job` for one compute job's PRIMARY verification
//! round. A real chain collects the executor's result, the committee's commits, and their
//! reveals across DIFFERENT blocks; this holds the phase state across blocks, takes each
//! step as on-chain DATA (validated with the frozen `reveal_matches`), draws the committee
//! from a consensus seed (`select_committee`), and fires the P1 resolvers at the terminal.
//! Reuses every frozen game piece; modifies none.
//!
//! ## §7.1 DA gate
//! Expressed at the consensus layer as "effective committee = who committed": a verifier
//! whose off-chain DA fetch abstained simply never submits a Commit, so never escrows a
//! bond. Quorum is anchored to the intended committee `k` (founder decision).
//!
//! ## Conservation
//! Every terminal conserves: budget + executor_bond + Σ(committed verifier bonds) is fully
//! paid/burned/returned (drained to 0) for Confirmed/Disputed/Timeout, or HELD as
//! `budget + Be + revealers·Bv` for Escalate (the deferred escalation round settles it).
//! Committed-but-not-revealed bonds are burned (commit-no-reveal forfeiture) on every
//! terminal — provable griefing, and conservation-required since the resolvers only resolve
//! the slice passed to them.
//!
//! WIRE-IN (P2 founder patch-spec, later): new `TxKind` Commit/Reveal variants feed
//! record_commit/record_reveal; `event_loop.rs` (PROTECTED) advances phases by block height
//! and supplies the consensus seed (block-hash/VRF). NoQuorum's `Escalate` handoff feeds a
//! follow-on k_escalate round (a clean recursion of this machine).

use commputer_pouw::commit_reveal::reveal_matches;
use commputer_pouw::committee::select_committee;
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{Commitment, Reveal, SettlementOutcome, Verdict};
use commputer_pouw::oracle::EquivalenceOracle;
use commputer_pouw::params::GameParams;
use commputer_pouw::verdict::compute_verdict;
use crate::escrow_ledger::Ledger;
use crate::settlement_resolution::{resolve_confirmed, resolve_disputed, resolve_timeout, ResolutionParams};
use borsh::{BorshSerialize, BorshDeserialize};

/// Lifecycle phase. Advances by block height; `submit_result` moves AwaitingResult→Committing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    AwaitingResult,
    Committing,
    Revealing,
    Settled,
}

/// Block-height deadlines for each phase (consensus heights).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseDeadlines {
    pub result_by: u64,
    pub commit_by: u64,
    pub reveal_by: u64,
}

/// Why an event was rejected (the on-chain tx would be invalid).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    WrongPhase,
    PastWindow,
    NotExecutor,
    NotCommitteeMember,
    WrongBond,
    DoubleCommit,
    UnknownCommitter,
    RevealMismatch,
    AlreadyRevealed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventResult {
    Accepted,
    Rejected(RejectReason),
}

/// What a follow-on (k_escalate) round needs to settle a NoQuorum via the frozen
/// `escalation::resolve(Trigger::NoQuorum)`. The escalation round draws its own panel from a
/// fresh seed + candidates; the seed/candidates are NOT carried here (second-round-owned).
#[derive(Clone, Debug, PartialEq)]
pub struct EscalationHandoff {
    pub budget: u64,
    pub submitter: ParticipantId,
    pub executor: ParticipantId,
    pub executor_hash: [u8; 32],
    pub executor_bond: u64,
    pub committee_reveals: Vec<Reveal>,
    pub committee_bonds: Vec<u64>,
    pub verifier_bond: u64,
}

/// The terminal outcome of the primary round.
#[derive(Clone, Debug, PartialEq)]
pub enum Terminal {
    Confirmed(SettlementOutcome),
    Disputed(SettlementOutcome),
    TimedOut(SettlementOutcome),
    Escalate(EscalationHandoff),
}

// ── PoUW P2 (B1b): persistable DTO for RocksDB persistence + state-root folding ──────────────
// JobLifecycle embeds FROZEN game types (GameParams/Commitment/Reveal/SettlementOutcome/
// ParticipantId) that derive no Borsh; the frozen `pouw` crate must stay byte-identical. So we
// mirror every field to primitives/local Borsh types here (same module ⇒ to_record/from_record read
// JobLifecycle's private fields directly). GameParams/ResolutionParams are genesis-anchored consensus
// params identical for every job (G5) — NOT persisted per-job; re-injected in `from_record`.
// The DTO holds only Vec/Option/primitive/array/tuple fields (no HashMap/HashSet) ⇒ borsh is canonical
// ⇒ deterministic across nodes for state-root folding.

/// Mirror of the local `Phase` enum (4 unit variants; declaration order == borsh discriminant).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum PhaseRec { AwaitingResult, Committing, Revealing, Settled }

/// Mirror of the local `PhaseDeadlines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PhaseDeadlinesRec { pub result_by: u64, pub commit_by: u64, pub reveal_by: u64 }

/// Mirror of the frozen `Commitment` (ParticipantId → [u8;32]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CommitmentRec { pub verifier: [u8; 32], pub commit: [u8; 32], pub bond: u64 }

/// Mirror of the frozen `Reveal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RevealRec { pub verifier: [u8; 32], pub result_hash: [u8; 32], pub salt: [u8; 32] }

/// Mirror of the frozen `SettlementOutcome` (ALL 7 u64 fields + the slashed log).
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct SettlementOutcomeRec {
    pub worker_paid: u64,
    pub verifiers_paid: u64,
    pub burned: u64,
    pub submitter_refunded: u64,
    pub challenger_paid: u64,
    pub panel_paid: u64,
    pub bonds_returned: u64,
    pub slashed: Vec<([u8; 32], u64)>,
}

/// Mirror of the local `EscalationHandoff`.
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct EscalationHandoffRec {
    pub budget: u64,
    pub submitter: [u8; 32],
    pub executor: [u8; 32],
    pub executor_hash: [u8; 32],
    pub executor_bond: u64,
    pub committee_reveals: Vec<RevealRec>,
    pub committee_bonds: Vec<u64>,
    pub verifier_bond: u64,
}

/// Mirror of the local `Terminal` (declaration order == borsh discriminant).
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub enum TerminalRec {
    Confirmed(SettlementOutcomeRec),
    Disputed(SettlementOutcomeRec),
    TimedOut(SettlementOutcomeRec),
    Escalate(EscalationHandoffRec),
}

/// Persistable, borsh-canonical mirror of `JobLifecycle`. Omits `params`/`rparams` (genesis-anchored
/// consensus params re-injected in `from_record`). Treat the field layout as a STABLE on-disk schema:
/// a reorder/add changes the on-disk bytes AND the state root — version it if the fields ever grow.
/// (Schema settled 2026-07-06, P9/D8: the record carries the program identity —
/// program_hash/input_hash/da_root — so the EscalationRound fast-follow and CompleteJob
/// result-binding need no disk/state-root migration. Nothing was deployed before this revision.)
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct JobLifecycleRecord {
    pub job_id: [u8; 32],
    /// P9/D8 identity: `sha256(wasm)` — the linchpin program identity.
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    /// DA-sampling anchor (Merkle-over-Reed-Solomon attestation root).
    pub da_root: [u8; 32],
    pub submitter: [u8; 32],
    pub executor: [u8; 32],
    pub executor_bond: u64,
    pub budget: u64,
    pub verifier_bond: u64,
    pub candidates: Vec<[u8; 32]>,
    pub deadlines: PhaseDeadlinesRec,
    pub phase: PhaseRec,
    pub executor_hash: Option<[u8; 32]>,
    pub committee: Vec<[u8; 32]>,
    pub commitments: Vec<CommitmentRec>,
    pub reveals: Vec<RevealRec>,
    pub settled: Option<TerminalRec>,
}

// ---- field-level conversions (private module helpers) ----
fn phase_to_rec(p: Phase) -> PhaseRec {
    match p {
        Phase::AwaitingResult => PhaseRec::AwaitingResult,
        Phase::Committing => PhaseRec::Committing,
        Phase::Revealing => PhaseRec::Revealing,
        Phase::Settled => PhaseRec::Settled,
    }
}
fn phase_from_rec(p: PhaseRec) -> Phase {
    match p {
        PhaseRec::AwaitingResult => Phase::AwaitingResult,
        PhaseRec::Committing => Phase::Committing,
        PhaseRec::Revealing => Phase::Revealing,
        PhaseRec::Settled => Phase::Settled,
    }
}
fn commit_to_rec(c: &Commitment) -> CommitmentRec {
    CommitmentRec { verifier: c.verifier.0, commit: c.commit, bond: c.bond }
}
fn commit_from_rec(c: &CommitmentRec) -> Commitment {
    Commitment { verifier: ParticipantId(c.verifier), commit: c.commit, bond: c.bond }
}
fn reveal_to_rec(r: &Reveal) -> RevealRec {
    RevealRec { verifier: r.verifier.0, result_hash: r.result_hash, salt: r.salt }
}
fn reveal_from_rec(r: &RevealRec) -> Reveal {
    Reveal { verifier: ParticipantId(r.verifier), result_hash: r.result_hash, salt: r.salt }
}
fn outcome_to_rec(o: &SettlementOutcome) -> SettlementOutcomeRec {
    SettlementOutcomeRec {
        worker_paid: o.worker_paid,
        verifiers_paid: o.verifiers_paid,
        burned: o.burned,
        submitter_refunded: o.submitter_refunded,
        challenger_paid: o.challenger_paid,
        panel_paid: o.panel_paid,
        bonds_returned: o.bonds_returned,
        slashed: o.slashed.iter().map(|(p, a)| (p.0, *a)).collect(),
    }
}
fn outcome_from_rec(o: &SettlementOutcomeRec) -> SettlementOutcome {
    SettlementOutcome {
        worker_paid: o.worker_paid,
        verifiers_paid: o.verifiers_paid,
        burned: o.burned,
        submitter_refunded: o.submitter_refunded,
        challenger_paid: o.challenger_paid,
        panel_paid: o.panel_paid,
        bonds_returned: o.bonds_returned,
        slashed: o.slashed.iter().map(|(p, a)| (ParticipantId(*p), *a)).collect(),
    }
}
fn handoff_to_rec(h: &EscalationHandoff) -> EscalationHandoffRec {
    EscalationHandoffRec {
        budget: h.budget,
        submitter: h.submitter.0,
        executor: h.executor.0,
        executor_hash: h.executor_hash,
        executor_bond: h.executor_bond,
        committee_reveals: h.committee_reveals.iter().map(reveal_to_rec).collect(),
        committee_bonds: h.committee_bonds.clone(),
        verifier_bond: h.verifier_bond,
    }
}
fn handoff_from_rec(h: &EscalationHandoffRec) -> EscalationHandoff {
    EscalationHandoff {
        budget: h.budget,
        submitter: ParticipantId(h.submitter),
        executor: ParticipantId(h.executor),
        executor_hash: h.executor_hash,
        executor_bond: h.executor_bond,
        committee_reveals: h.committee_reveals.iter().map(reveal_from_rec).collect(),
        committee_bonds: h.committee_bonds.clone(),
        verifier_bond: h.verifier_bond,
    }
}
fn terminal_to_rec(t: &Terminal) -> TerminalRec {
    match t {
        Terminal::Confirmed(o) => TerminalRec::Confirmed(outcome_to_rec(o)),
        Terminal::Disputed(o) => TerminalRec::Disputed(outcome_to_rec(o)),
        Terminal::TimedOut(o) => TerminalRec::TimedOut(outcome_to_rec(o)),
        Terminal::Escalate(h) => TerminalRec::Escalate(handoff_to_rec(h)),
    }
}
fn terminal_from_rec(t: &TerminalRec) -> Terminal {
    match t {
        TerminalRec::Confirmed(o) => Terminal::Confirmed(outcome_from_rec(o)),
        TerminalRec::Disputed(o) => Terminal::Disputed(outcome_from_rec(o)),
        TerminalRec::TimedOut(o) => Terminal::TimedOut(outcome_from_rec(o)),
        TerminalRec::Escalate(h) => Terminal::Escalate(handoff_from_rec(h)),
    }
}

/// One compute job's primary verification round, as a deterministic multi-block state machine.
/// Holds only plain data; `stake_of`/`eq`/`&mut impl Ledger` are passed to the methods (the ledger is
/// the `EscrowLedger` reference in tests, `ChainState` on the live node — P2 §3 option B).
/// `Clone` exists for the chain's P1 pre-block snapshot (rollback-on-Err); cloning never runs game
/// logic.
#[derive(Clone)]
pub struct JobLifecycle {
    job_id: [u8; 32],
    // P9/D8 program identity, fixed at `open` (from the SubmitJobV2 pending record).
    program_hash: [u8; 32],
    input_hash: [u8; 32],
    da_root: [u8; 32],
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
    budget: u64,
    verifier_bond: u64,
    params: GameParams,
    rparams: ResolutionParams,
    candidates: Vec<ParticipantId>,
    deadlines: PhaseDeadlines,
    // mutable lifecycle state
    phase: Phase,
    executor_hash: Option<[u8; 32]>,
    committee: Vec<ParticipantId>,
    commitments: Vec<Commitment>,
    reveals: Vec<Reveal>,
    /// The computed terminal, cached so `settle` is idempotent (a re-org / double tick at
    /// the wire-in re-runs no money). `None` until the first `settle`.
    settled: Option<Terminal>,
}

impl JobLifecycle {
    /// Open at AwaitingResult. PRECONDITION: budget + executor_bond already escrowed into
    /// the job's pot (the chain's submit+claim handlers, per the P1 patch-spec).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        job_id: [u8; 32],
        program_hash: [u8; 32],
        input_hash: [u8; 32],
        da_root: [u8; 32],
        submitter: ParticipantId,
        executor: ParticipantId,
        executor_bond: u64,
        budget: u64,
        verifier_bond: u64,
        params: GameParams,
        rparams: ResolutionParams,
        candidates: Vec<ParticipantId>,
        deadlines: PhaseDeadlines,
    ) -> Self {
        Self {
            job_id,
            program_hash,
            input_hash,
            da_root,
            submitter,
            executor,
            executor_bond,
            budget,
            verifier_bond,
            params,
            rparams,
            candidates,
            deadlines,
            phase: Phase::AwaitingResult,
            executor_hash: None,
            committee: Vec::new(),
            commitments: Vec::new(),
            reveals: Vec::new(),
            settled: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn committee(&self) -> &[ParticipantId] {
        &self.committee
    }

    /// Whether `settle` has already run (the terminal is cached). The chain MUST short-circuit
    /// on this BEFORE its pot pre-validation: after the first settle the pot no longer equals
    /// `expected_escrow()` (forfeited bonds were burned), so re-validating would wedge every
    /// re-entry (P2 amendment, 2026-07-06).
    pub fn is_settled(&self) -> bool {
        self.settled.is_some()
    }

    /// The cached terminal, if `settle` already ran. Moves no money — the safe re-entry surface.
    pub fn settled_terminal(&self) -> Option<Terminal> {
        self.settled.clone()
    }

    /// The exact pot the job MUST hold at settlement: budget + executor bond + every committed
    /// verifier bond (revealed or not — a committed-but-unrevealed bond is still escrowed and is burnt
    /// as forfeiture at settle). The chain pre-validates `escrowed_for(job) == expected_escrow()` before
    /// driving `settle`, so a malformed pot is REJECTED rather than panicking the ledger mid-settle.
    pub fn expected_escrow(&self) -> u64 {
        let bonds: u64 = self.commitments.iter().map(|c| c.bond).sum();
        self.budget
            .saturating_add(self.executor_bond)
            .saturating_add(bonds)
    }

    /// PoUW P2 (B1b): project to the persistable, borsh-canonical DTO. Reads private fields directly
    /// (same module). `params`/`rparams` are NOT included — they are genesis-anchored consensus params.
    pub fn to_record(&self) -> JobLifecycleRecord {
        JobLifecycleRecord {
            job_id: self.job_id,
            program_hash: self.program_hash,
            input_hash: self.input_hash,
            da_root: self.da_root,
            submitter: self.submitter.0,
            executor: self.executor.0,
            executor_bond: self.executor_bond,
            budget: self.budget,
            verifier_bond: self.verifier_bond,
            candidates: self.candidates.iter().map(|p| p.0).collect(),
            deadlines: PhaseDeadlinesRec {
                result_by: self.deadlines.result_by,
                commit_by: self.deadlines.commit_by,
                reveal_by: self.deadlines.reveal_by,
            },
            phase: phase_to_rec(self.phase),
            executor_hash: self.executor_hash,
            committee: self.committee.iter().map(|p| p.0).collect(),
            commitments: self.commitments.iter().map(commit_to_rec).collect(),
            reveals: self.reveals.iter().map(reveal_to_rec).collect(),
            settled: self.settled.as_ref().map(terminal_to_rec),
        }
    }

    /// PoUW P2 (B1b): rebuild a full `JobLifecycle` from its DTO, RE-INJECTING the genesis-anchored
    /// consensus params (identical for every job, so this reproduces the original params exactly —
    /// true both now with defaults and post-B8 with genesis values).
    pub fn from_record(rec: JobLifecycleRecord, params: GameParams, rparams: ResolutionParams) -> Self {
        Self {
            job_id: rec.job_id,
            program_hash: rec.program_hash,
            input_hash: rec.input_hash,
            da_root: rec.da_root,
            submitter: ParticipantId(rec.submitter),
            executor: ParticipantId(rec.executor),
            executor_bond: rec.executor_bond,
            budget: rec.budget,
            verifier_bond: rec.verifier_bond,
            params,
            rparams,
            candidates: rec.candidates.iter().map(|b| ParticipantId(*b)).collect(),
            deadlines: PhaseDeadlines {
                result_by: rec.deadlines.result_by,
                commit_by: rec.deadlines.commit_by,
                reveal_by: rec.deadlines.reveal_by,
            },
            phase: phase_from_rec(rec.phase),
            executor_hash: rec.executor_hash,
            committee: rec.committee.iter().map(|b| ParticipantId(*b)).collect(),
            commitments: rec.commitments.iter().map(commit_from_rec).collect(),
            reveals: rec.reveals.iter().map(reveal_from_rec).collect(),
            settled: rec.settled.as_ref().map(terminal_from_rec),
        }
    }

    /// Whether this job's window has elapsed and the node should call `settle` at `height`. The
    /// deadlines are private, so the node (event_loop `enforce_timeouts`, P2) drives termination
    /// through this: past `result_by` with no result (timeout), or past `reveal_by` once reached
    /// Committing/Revealing. Already-`Settled` jobs return false (idempotent — nothing more to do).
    pub fn should_settle(&self, height: u64) -> bool {
        match self.phase {
            Phase::AwaitingResult => height > self.deadlines.result_by, // executor never delivered
            Phase::Committing | Phase::Revealing => height > self.deadlines.reveal_by,
            Phase::Settled => false,
        }
    }

    /// Executor delivers CompleteJob{result_hash}. Records the claim, draws the committee
    /// from the consensus `seed` (known only AFTER the result, per engine.rs:107), and
    /// transitions to Committing. The seed/stake_of are inputs the chain supplies.
    pub fn submit_result(
        &mut self,
        executor: ParticipantId,
        result_hash: [u8; 32],
        seed: [u8; 32],
        height: u64,
        stake_of: &dyn Fn(&ParticipantId) -> u64,
    ) -> EventResult {
        if self.phase != Phase::AwaitingResult {
            return EventResult::Rejected(RejectReason::WrongPhase);
        }
        if executor != self.executor {
            return EventResult::Rejected(RejectReason::NotExecutor);
        }
        if height > self.deadlines.result_by {
            return EventResult::Rejected(RejectReason::PastWindow);
        }
        self.executor_hash = Some(result_hash);
        self.committee =
            select_committee(&seed, &self.candidates, &self.executor, self.params.k, stake_of);
        self.phase = Phase::Committing;
        EventResult::Accepted
    }

    /// B5 step 1 (CompleteJob tx arm, PARENT height): the executor delivers its result hash —
    /// validate phase/executor/window and record it. Does NOT draw the committee or advance the
    /// phase (that is `draw_committee`, run in the block tail with the block-hash seed). Splitting
    /// `submit_result` this way lets the tx arm anchor the result-window at parent height (matching
    /// every other tx arm) while the tail draws from the finalized `block.hash()`; re-checking the
    /// window in `draw_committee` at the APPLIED height (parent+1) would wrongly time out a result
    /// posted exactly at `result_by`, so the draw is deliberately window-free.
    pub fn post_result(
        &mut self,
        executor: ParticipantId,
        result_hash: [u8; 32],
        height: u64,
    ) -> EventResult {
        if self.phase != Phase::AwaitingResult {
            return EventResult::Rejected(RejectReason::WrongPhase);
        }
        if executor != self.executor {
            return EventResult::Rejected(RejectReason::NotExecutor);
        }
        if height > self.deadlines.result_by {
            return EventResult::Rejected(RejectReason::PastWindow);
        }
        self.executor_hash = Some(result_hash);
        EventResult::Accepted
    }

    /// B5 step 2 (block tail, `block.hash()` seed): draw the committee from the frozen
    /// `select_committee` and advance AwaitingResult→Committing. Idempotent and window-free — it
    /// only fires for a job that has posted a result but not yet drawn (guarded on
    /// phase/executor_hash/empty-committee), so a re-run (a re-org replay, or a second tail pass)
    /// is a no-op. `seed`/`stake_of` are consensus inputs the chain supplies (seed =
    /// `hash(block_hash‖job_id)`, `stake_of` = bonded stake); moves no money.
    pub fn draw_committee(&mut self, seed: [u8; 32], stake_of: &dyn Fn(&ParticipantId) -> u64) {
        if self.phase != Phase::AwaitingResult
            || self.executor_hash.is_none()
            || !self.committee.is_empty()
        {
            return; // nothing to draw (wrong phase / no result / already drawn)
        }
        self.committee =
            select_committee(&seed, &self.candidates, &self.executor, self.params.k, stake_of);
        self.phase = Phase::Committing;
    }

    /// Whether the executor has posted its result (B5 draw precondition). Read-only accessor for
    /// the chain's block-tail draw filter (the field is private).
    pub fn executor_hash_is_set(&self) -> bool {
        self.executor_hash.is_some()
    }

    /// A committee verifier commits (DA-Available ⇒ they call this; Abstain ⇒ they don't).
    /// Validates phase/window/membership/bond/no-double-commit, then escrows the bond.
    pub fn record_commit(&mut self, l: &mut impl Ledger, c: Commitment, height: u64) -> EventResult {
        if self.phase != Phase::Committing {
            return EventResult::Rejected(RejectReason::WrongPhase);
        }
        if height > self.deadlines.commit_by {
            return EventResult::Rejected(RejectReason::PastWindow);
        }
        if !self.committee.contains(&c.verifier) {
            return EventResult::Rejected(RejectReason::NotCommitteeMember);
        }
        if c.bond != self.verifier_bond {
            return EventResult::Rejected(RejectReason::WrongBond);
        }
        if self.commitments.iter().any(|x| x.verifier == c.verifier) {
            return EventResult::Rejected(RejectReason::DoubleCommit);
        }
        l.for_job(self.job_id);
        l.escrow(c.verifier, self.verifier_bond);
        self.commitments.push(c);
        EventResult::Accepted
    }

    /// A committer opens its commitment. Validates phase/window/matching-commitment/no-replay.
    pub fn record_reveal(&mut self, r: Reveal, height: u64) -> EventResult {
        if self.phase != Phase::Revealing {
            return EventResult::Rejected(RejectReason::WrongPhase);
        }
        if height > self.deadlines.reveal_by {
            return EventResult::Rejected(RejectReason::PastWindow);
        }
        let commitment = match self.commitments.iter().find(|c| c.verifier == r.verifier) {
            Some(c) => c,
            None => return EventResult::Rejected(RejectReason::UnknownCommitter),
        };
        if !reveal_matches(commitment, &r) {
            return EventResult::Rejected(RejectReason::RevealMismatch);
        }
        if self.reveals.iter().any(|x| x.verifier == r.verifier) {
            return EventResult::Rejected(RejectReason::AlreadyRevealed);
        }
        self.reveals.push(r);
        EventResult::Accepted
    }

    /// Height-driven phase advance. Only the Committing→Revealing transition is height-driven
    /// and money-free; the timeout (AwaitingResult past result_by) and the reveal-window close
    /// are detected by `settle`, which needs the ledger. Idempotent.
    pub fn advance(&mut self, height: u64) -> Phase {
        if self.phase == Phase::Committing && height > self.deadlines.commit_by {
            self.phase = Phase::Revealing;
        }
        self.phase
    }

    /// Finalize the primary round (call at reveal_by, or at result_by if no result arrived).
    /// Applies the uniform commit-no-reveal forfeiture, then routes the verdict to a P1
    /// resolver (Confirmed/Disputed/Timeout, pot drained) or returns an Escalate handoff
    /// (NoQuorum, escrow held). **Idempotent:** the terminal is computed once and cached; a
    /// second call (a re-org or double block-tick at the wire-in) returns the same outcome
    /// and moves no value — symmetric with `advance`.
    pub fn settle(&mut self, l: &mut impl Ledger, eq: &dyn EquivalenceOracle) -> Terminal {
        if let Some(t) = &self.settled {
            return t.clone();
        }
        self.phase = Phase::Settled;

        let terminal = match self.executor_hash {
            // Timeout: the executor never delivered a result (no committee was ever drawn).
            None => {
                let out = resolve_timeout(
                    l, &self.rparams, self.job_id, self.budget, self.submitter,
                    self.executor, self.executor_bond,
                );
                Terminal::TimedOut(out)
            }
            Some(executor_hash) => {
                // Partition committers: revealers (effective set) vs non-revealers.
                let revealed_ids: Vec<ParticipantId> =
                    self.reveals.iter().map(|r| r.verifier).collect();
                let non_revealers: Vec<ParticipantId> = self
                    .commitments
                    .iter()
                    .map(|c| c.verifier)
                    .filter(|v| !revealed_ids.contains(v))
                    .collect();

                // Uniform commit-no-reveal forfeiture: burn each non-revealer's bond BEFORE the
                // verdict branch, so the pot holds exactly budget + Be + revealers·Bv on every path.
                l.for_job(self.job_id);
                for _ in &non_revealers {
                    l.burn(self.verifier_bond);
                }
                let forfeit_burned = non_revealers.len() as u64 * self.verifier_bond;
                let forfeit_slashed: Vec<(ParticipantId, u64)> =
                    non_revealers.iter().map(|v| (*v, self.verifier_bond)).collect();

                let quorum = self.params.quorum(self.params.k);
                match compute_verdict(&self.reveals, &executor_hash, quorum, eq) {
                    Verdict::Confirmed { .. } => {
                        let mut out = resolve_confirmed(
                            l, &self.params, self.job_id, self.budget, self.executor,
                            self.executor_bond, &revealed_ids, self.verifier_bond,
                        );
                        out.burned += forfeit_burned;
                        out.slashed.extend(forfeit_slashed);
                        Terminal::Confirmed(out)
                    }
                    Verdict::Disputed { correct_hash } => {
                        let honest: Vec<ParticipantId> = self
                            .reveals
                            .iter()
                            .filter(|r| eq.equiv(&r.result_hash, &correct_hash))
                            .map(|r| r.verifier)
                            .collect();
                        let mut out = resolve_disputed(
                            l, &self.params, self.job_id, self.budget, self.submitter,
                            self.executor, self.executor_bond, &revealed_ids, &honest,
                            self.verifier_bond,
                        );
                        out.burned += forfeit_burned;
                        out.slashed.extend(forfeit_slashed);
                        Terminal::Disputed(out)
                    }
                    Verdict::NoQuorum => Terminal::Escalate(EscalationHandoff {
                        budget: self.budget,
                        submitter: self.submitter,
                        executor: self.executor,
                        executor_hash,
                        executor_bond: self.executor_bond,
                        committee_reveals: self.reveals.clone(),
                        committee_bonds: vec![self.verifier_bond; self.reveals.len()],
                        verifier_bond: self.verifier_bond,
                    }),
                }
            }
        };

        self.settled = Some(terminal.clone());
        terminal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escrow_ledger::EscrowLedger; // tests drive the concrete reference ledger
    use commputer_pouw::oracle::ChainHooks; // for .escrow/.pay/.burn on the concrete EscrowLedger

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    fn deadlines() -> PhaseDeadlines {
        PhaseDeadlines { result_by: 10, commit_by: 20, reveal_by: 30 }
    }

    /// P9 identity placeholder for test opens (identity is opaque to the game logic).
    const IDENT: [u8; 32] = [0xAB; 32];

    use commputer_pouw::economics::{budget_min, executor_bond_min, verifier_bond_min};
    use commputer_pouw::commit_reveal::make_commitment;
    use commputer_pouw::oracle::ByteEq;

    /// Default-param fuel minimums: budget 3_960, e_bond 3_960, v_bond 1_650.
    fn min_funding() -> (u64, u64, u64) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let b = budget_min(f, &p).unwrap();
        (b, executor_bond_min(f, b, &p).unwrap(), verifier_bond_min(f, &p).unwrap())
    }

    /// Ledger with budget (submitter) + executor bond (executor) escrowed into the job pot
    /// (the submit+claim precondition); candidates credited so they can escrow on commit.
    /// Returns (ledger, total_supply-after-funding).
    fn funded(job: [u8; 32], budget: u64, e_bond: u64, v_bond: u64, cands: &[ParticipantId]) -> (EscrowLedger, u64) {
        let mut l = EscrowLedger::new();
        l.credit(pid(0), budget);
        l.credit(pid(9), e_bond);
        for c in cands { l.credit(*c, v_bond); }
        let total0 = l.total_supply();
        l.for_job(job);
        l.escrow(pid(0), budget);
        l.escrow(pid(9), e_bond);
        (l, total0)
    }

    fn cands3() -> Vec<ParticipantId> { vec![pid(10), pid(11), pid(12)] }

    /// A valid commitment by `v` to `hash` under `salt`, posting `bond`.
    fn commit_of(v: ParticipantId, hash: [u8; 32], salt: [u8; 32], bond: u64) -> Commitment {
        make_commitment(&v, &hash, &salt, bond)
    }

    fn reveal_of(v: ParticipantId, hash: [u8; 32], salt: [u8; 32]) -> Reveal {
        Reveal { verifier: v, result_hash: hash, salt }
    }

    /// Drive a freshly-opened lifecycle to Committing with the committee drawn.
    fn opened_committing(job: [u8; 32], result: [u8; 32], budget: u64, e_bond: u64, v_bond: u64) -> JobLifecycle {
        let mut lc = JobLifecycle::open(
            job, IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cands3(), deadlines(),
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(lc.submit_result(pid(9), result, [42u8; 32], 5, &stake), EventResult::Accepted);
        lc
    }

    #[test]
    fn opens_in_awaiting_result_with_no_committee() {
        let lc = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), 3960, 3960, 1650,
            GameParams::default(), ResolutionParams::default(),
            vec![pid(10), pid(11), pid(12)], deadlines(),
        );
        assert_eq!(lc.phase(), Phase::AwaitingResult);
        assert!(lc.committee().is_empty());
    }

    #[test]
    fn should_settle_tracks_phase_windows() {
        // deadlines: result_by 10, commit_by 20, reveal_by 30.
        let mut lc = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), 3960, 3960, 1650,
            GameParams::default(), ResolutionParams::default(),
            vec![pid(10), pid(11), pid(12)], deadlines(),
        );
        // AwaitingResult: settle once past result_by (executor never delivered).
        assert!(!lc.should_settle(10));
        assert!(lc.should_settle(11));
        // Committing (committee drawn): settle once past reveal_by.
        let stake = |_: &ParticipantId| 1u64;
        lc.submit_result(pid(9), [7u8; 32], [42u8; 32], 5, &stake);
        assert!(!lc.should_settle(30));
        assert!(lc.should_settle(31));
        // After settling, nothing more to do.
        let mut l = EscrowLedger::new();
        l.credit(pid(0), 3960);
        l.credit(pid(9), 3960);
        l.for_job([1u8; 32]);
        l.escrow(pid(0), 3960);
        l.escrow(pid(9), 3960);
        lc.advance(31);
        let _ = lc.settle(&mut l, &ByteEq);
        assert!(!lc.should_settle(999), "Settled is terminal");
    }

    #[test]
    fn submit_result_draws_committee_and_advances() {
        let mut lc = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), 3960, 3960, 1650,
            GameParams::default(), ResolutionParams::default(),
            vec![pid(10), pid(11), pid(12)], deadlines(),
        );
        let stake = |_: &ParticipantId| 1u64;
        assert_eq!(
            lc.submit_result(pid(9), [7u8; 32], [42u8; 32], 5, &stake),
            EventResult::Accepted
        );
        assert_eq!(lc.phase(), Phase::Committing);
        assert_eq!(lc.committee().len(), 3); // pool of exactly k=3 ⇒ whole pool

        // rejections
        let mut lc2 = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), 3960, 3960, 1650,
            GameParams::default(), ResolutionParams::default(),
            vec![pid(10), pid(11), pid(12)], deadlines(),
        );
        // not the executor
        assert_eq!(
            lc2.submit_result(pid(8), [7u8; 32], [42u8; 32], 5, &stake),
            EventResult::Rejected(RejectReason::NotExecutor)
        );
        // past the result window
        assert_eq!(
            lc2.submit_result(pid(9), [7u8; 32], [42u8; 32], 11, &stake),
            EventResult::Rejected(RejectReason::PastWindow)
        );
        // wrong phase: after a successful submit, a second submit is rejected
        assert_eq!(lc2.submit_result(pid(9), [7u8; 32], [42u8; 32], 5, &stake), EventResult::Accepted);
        assert_eq!(
            lc2.submit_result(pid(9), [7u8; 32], [42u8; 32], 6, &stake),
            EventResult::Rejected(RejectReason::WrongPhase)
        );
    }

    #[test]
    fn record_commit_escrows_bond_and_validates() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [1u8; 32];
        let (mut l, _total0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, [7u8; 32], budget, e_bond, v_bond);

        // a valid commit escrows the verifier bond into the job pot
        let c10 = commit_of(pid(10), [7u8; 32], [0u8; 32], v_bond);
        assert_eq!(lc.record_commit(&mut l, c10, 15), EventResult::Accepted);
        assert_eq!(l.escrowed_for(&job), budget + e_bond + v_bond);
        assert_eq!(l.balance_of(&pid(10)), 0); // bond moved out of balance

        // double-commit rejected (no extra escrow)
        let c10b = commit_of(pid(10), [7u8; 32], [1u8; 32], v_bond);
        assert_eq!(lc.record_commit(&mut l, c10b, 15), EventResult::Rejected(RejectReason::DoubleCommit));
        // non-committee member rejected
        let c99 = commit_of(pid(99), [7u8; 32], [0u8; 32], v_bond);
        assert_eq!(lc.record_commit(&mut l, c99, 15), EventResult::Rejected(RejectReason::NotCommitteeMember));
        // wrong bond rejected
        let cbad = commit_of(pid(11), [7u8; 32], [0u8; 32], v_bond + 1);
        assert_eq!(lc.record_commit(&mut l, cbad, 15), EventResult::Rejected(RejectReason::WrongBond));
        // past commit window rejected
        let c11 = commit_of(pid(11), [7u8; 32], [0u8; 32], v_bond);
        assert_eq!(lc.record_commit(&mut l, c11, 21), EventResult::Rejected(RejectReason::PastWindow));
        // escrow only moved for the one accepted commit
        assert_eq!(l.escrowed_for(&job), budget + e_bond + v_bond);
    }

    #[test]
    fn record_reveal_validates_and_advance_transitions() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [1u8; 32];
        let (mut l, _t0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, [7u8; 32], budget, e_bond, v_bond);
        // commit pid(10) under salt [0;32]
        assert_eq!(lc.record_commit(&mut l, commit_of(pid(10), [7u8; 32], [0u8; 32], v_bond), 15), EventResult::Accepted);

        // reveal before Revealing phase is rejected (WrongPhase)
        assert_eq!(lc.record_reveal(reveal_of(pid(10), [7u8; 32], [0u8; 32]), 16), EventResult::Rejected(RejectReason::WrongPhase));

        // advance past commit_by → Revealing
        assert_eq!(lc.advance(21), Phase::Revealing);

        // a matching reveal is accepted
        assert_eq!(lc.record_reveal(reveal_of(pid(10), [7u8; 32], [0u8; 32]), 25), EventResult::Accepted);
        // re-reveal rejected
        assert_eq!(lc.record_reveal(reveal_of(pid(10), [7u8; 32], [0u8; 32]), 25), EventResult::Rejected(RejectReason::AlreadyRevealed));
        // a reveal from a non-committer is rejected
        assert_eq!(lc.record_reveal(reveal_of(pid(11), [7u8; 32], [0u8; 32]), 25), EventResult::Rejected(RejectReason::UnknownCommitter));
        // a reveal that does not open the commitment (wrong salt) is rejected
        let mut lc2 = opened_committing(job, [7u8; 32], budget, e_bond, v_bond);
        let (mut l2, _t) = funded(job, budget, e_bond, v_bond, &cands3());
        assert_eq!(lc2.record_commit(&mut l2, commit_of(pid(10), [7u8; 32], [0u8; 32], v_bond), 15), EventResult::Accepted);
        assert_eq!(lc2.advance(21), Phase::Revealing);
        assert_eq!(lc2.record_reveal(reveal_of(pid(10), [7u8; 32], [9u8; 32]), 25), EventResult::Rejected(RejectReason::RevealMismatch));
        // past reveal window rejected
        assert_eq!(lc2.record_reveal(reveal_of(pid(10), [7u8; 32], [0u8; 32]), 31), EventResult::Rejected(RejectReason::PastWindow));
    }

    /// Helper: open → submit_result → all of `committers` commit to `commit_hash` → advance →
    /// each of `revealers` reveals `reveal_hash` → advance past reveal_by → settle.
    /// `commit_hash`/`reveal_hash` per verifier are supplied by the closures.
    #[allow(clippy::type_complexity)]
    fn run_round(
        job: [u8; 32],
        result: [u8; 32],
        budget: u64, e_bond: u64, v_bond: u64,
        committers: &[ParticipantId],
        revealed: &[(ParticipantId, [u8; 32])], // (verifier, revealed hash); absent ⇒ no reveal
    ) -> (Terminal, EscrowLedger, u64) {
        let (mut l, total0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, result, budget, e_bond, v_bond);
        for (i, c) in committers.iter().enumerate() {
            let salt = [i as u8; 32];
            // commit to whatever this verifier will reveal (so the commitment opens), defaulting
            // to `result` for committers with no reveal entry.
            let hash = revealed.iter().find(|(v, _)| v == c).map(|(_, h)| *h).unwrap_or(result);
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, hash, salt, v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in committers.iter().enumerate() {
            if let Some((_, h)) = revealed.iter().find(|(v, _)| v == c) {
                let salt = [i as u8; 32];
                assert_eq!(lc.record_reveal(reveal_of(*c, *h, salt), 25), EventResult::Accepted);
            }
        }
        lc.advance(31);
        let term = lc.settle(&mut l, &ByteEq);
        (term, l, total0)
    }

    #[test]
    fn settle_confirmed_happy_path_85_10_5_conserves() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [1u8; 32];
        let result = [7u8; 32];
        // all 3 commit to and reveal the true result ⇒ Confirmed
        let revealed: Vec<(ParticipantId, [u8; 32])> =
            cands3().into_iter().map(|c| (c, result)).collect();
        let (term, l, total0) = run_round(job, result, budget, e_bond, v_bond, &cands3(), &revealed);
        match term {
            Terminal::Confirmed(out) => {
                assert_eq!(out.worker_paid, 3_366);   // 85% of 3_960
                assert_eq!(out.verifiers_paid, 396);  // 10% split across 3 (132 each)
                assert_eq!(out.burned, 198);          // 5%
                assert_eq!(out.bonds_returned, e_bond + 3 * v_bond);
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn settle_timeout_when_executor_never_delivers() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [2u8; 32];
        let (mut l, total0) = funded(job, budget, e_bond, v_bond, &cands3());
        // open but NO submit_result; advance past result_by, then settle
        let mut lc = JobLifecycle::open(
            job, IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cands3(), deadlines(),
        );
        lc.advance(11); // past result_by; stays AwaitingResult (no committee drawn)
        let term = lc.settle(&mut l, &ByteEq);
        match term {
            Terminal::TimedOut(out) => {
                // resolve_timeout: full budget + 20% of e_bond to submitter, 80% of e_bond burned
                assert_eq!(out.submitter_refunded, budget + e_bond / 5);
                assert_eq!(out.burned, e_bond - e_bond / 5);
                assert_eq!(out.slashed, vec![(pid(9), e_bond)]);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn settle_disputed_refunds_submitter_slashes_executor() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [3u8; 32];
        let result = [7u8; 32];      // executor claims 7
        let correct = [5u8; 32];     // committee says 5
        // all 3 commit to and reveal 5 (against the executor) ⇒ Disputed on 5
        let revealed: Vec<(ParticipantId, [u8; 32])> =
            cands3().into_iter().map(|c| (c, correct)).collect();
        let (term, l, total0) = run_round(job, result, budget, e_bond, v_bond, &cands3(), &revealed);
        match term {
            Terminal::Disputed(out) => {
                assert_eq!(out.submitter_refunded, budget);
                assert_eq!(out.verifiers_paid, 792);          // 20% of e_bond to the 3 honest (264 each)
                assert_eq!(out.burned, e_bond - 792);
                assert_eq!(out.bonds_returned, 3 * v_bond);
                assert_eq!(out.slashed, vec![(pid(9), e_bond)]);
            }
            other => panic!("expected Disputed, got {other:?}"),
        }
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn settle_noquorum_escalates_and_holds_escrow() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [4u8; 32];
        let result = [7u8; 32];
        // 3-way split: each reveals a distinct value ⇒ no class reaches quorum(3)=2 ⇒ NoQuorum
        let revealed = vec![
            (pid(10), [1u8; 32]),
            (pid(11), [2u8; 32]),
            (pid(12), [3u8; 32]),
        ];
        let (term, l, total0) = run_round(job, result, budget, e_bond, v_bond, &cands3(), &revealed);
        match term {
            Terminal::Escalate(h) => {
                assert_eq!(h.committee_reveals.len(), 3);
                assert_eq!(h.committee_bonds, vec![v_bond; 3]);
                assert_eq!(h.budget, budget);
                assert_eq!(h.executor_bond, e_bond);
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        // escrow HELD (not drained, not stranded): budget + Be + 3 committed bonds
        assert_eq!(l.escrowed_for(&job), budget + e_bond + 3 * v_bond);
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn settle_confirmed_burns_non_revealer_bond() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [5u8; 32];
        let result = [7u8; 32];
        // 3 commit; only pid(10) and pid(11) reveal the true result (quorum(3)=2 ⇒ Confirmed);
        // pid(12) commits but never reveals ⇒ its bond is forfeited (burned).
        let revealed = vec![(pid(10), result), (pid(11), result)];
        let (term, l, total0) = run_round(job, result, budget, e_bond, v_bond, &cands3(), &revealed);
        match term {
            Terminal::Confirmed(out) => {
                assert_eq!(out.worker_paid, 3_366);
                // verifier pool 396 split across the 2 revealers (198 each)
                assert_eq!(out.verifiers_paid, 396);
                // burn = 5% protocol (198) + forfeited non-revealer bond (1_650)
                assert_eq!(out.burned, 198 + v_bond);
                assert!(out.slashed.contains(&(pid(12), v_bond)), "non-revealer forfeited");
                assert_eq!(out.bonds_returned, e_bond + 2 * v_bond); // only the 2 revealers' bonds
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn settle_escalate_burns_non_revealer_and_holds_only_revealer_bonds() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [6u8; 32];
        let result = [7u8; 32];
        // 3 commit; pid(10),pid(11) reveal DIFFERENT values (split, max class 1 < quorum 2 ⇒
        // NoQuorum); pid(12) commits but never reveals ⇒ forfeited.
        let revealed = vec![(pid(10), [1u8; 32]), (pid(11), [2u8; 32])];
        let (term, l, total0) = run_round(job, result, budget, e_bond, v_bond, &cands3(), &revealed);
        match term {
            Terminal::Escalate(h) => {
                assert_eq!(h.committee_reveals.len(), 2, "only the 2 revealers handed off");
                assert_eq!(h.committee_bonds, vec![v_bond; 2]);
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        // pid(12)'s bond burned; held = budget + Be + 2 revealer bonds (a true invariant)
        assert_eq!(l.escrowed_for(&job), budget + e_bond + 2 * v_bond);
        assert_eq!(l.total_supply(), total0);
    }

    use commputer_da::commit::{build_attestation, chunk_proof};
    use commputer_da::facade::{chunk_hash, AvailabilityOutcome, DataAvailability};
    use commputer_da::params::{ChunkingParams, DaAttestation, ProviderId};
    use commputer_da::transport::{InMemoryTransport, ManualClock};

    /// Publish every coded chunk of `program` to a fresh transport. Returns (transport, attestation).
    fn publish(program: &[u8]) -> (InMemoryTransport, DaAttestation) {
        let transport = InMemoryTransport::new();
        let (att, coded) = build_attestation(program, &ChunkingParams::default()).expect("attestation");
        let provider = ProviderId([200; 32]);
        for i in 0..att.n_total {
            transport.put(chunk_hash(&att, i), provider, coded[i as usize].clone(), chunk_proof(&coded, i));
        }
        (transport, att)
    }

    /// Which committee members get Available from the DA facade (the §7.1 gate).
    fn da_available(
        transport: &InMemoryTransport, clock: &ManualClock, att: &DaAttestation,
        committee: &[ParticipantId],
    ) -> Vec<ParticipantId> {
        let da = DataAvailability { transport, clock, retry_window_ticks: 1_000, max_attempts_per_chunk: 8 };
        // job_id = sha256(program_id || input_hash); here input_hash is fixed for the test.
        let mut h = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut h, att.program_id);
        sha2::Digest::update(&mut h, [9u8; 32]);
        let job_id: [u8; 32] = sha2::Digest::finalize(h).into();
        committee
            .iter()
            .filter(|c| matches!(da.verify_available(att, job_id, 1, c.0), AvailabilityOutcome::Available(_)))
            .copied()
            .collect()
    }

    #[test]
    fn da_gate_full_availability_confirms_withholding_escalates() {
        let (budget, e_bond, v_bond) = min_funding();
        let result = [7u8; 32];
        let program = b"\x00asm\x01\x00\x00\x00deterministic-program-bytes".to_vec();

        // --- Full availability: every committee member commits ⇒ Confirmed. ---
        let job = [10u8; 32];
        let (transport, att) = publish(&program);
        let clock = ManualClock::new();
        let (mut l, total0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, result, budget, e_bond, v_bond);
        let available = da_available(&transport, &clock, &att, lc.committee());
        assert_eq!(available.len(), 3, "all committee members available");
        for (i, c) in available.iter().enumerate() {
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, result, [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in available.iter().enumerate() {
            assert_eq!(lc.record_reveal(reveal_of(*c, result, [i as u8; 32]), 25), EventResult::Accepted);
        }
        lc.advance(31);
        assert!(matches!(lc.settle(&mut l, &ByteEq), Terminal::Confirmed(_)));
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);

        // --- Withhold ALL chunks: every member abstains ⇒ 0 commits ⇒ NoQuorum ⇒ Escalate. ---
        let job2 = [11u8; 32];
        let (transport2, att2) = publish(&program);
        for i in 0..att2.n_total {
            transport2.withhold(chunk_hash(&att2, i));
        }
        let clock2 = ManualClock::new();
        let (mut l2, total0b) = funded(job2, budget, e_bond, v_bond, &cands3());
        let mut lc2 = opened_committing(job2, result, budget, e_bond, v_bond);
        let available2 = da_available(&transport2, &clock2, &att2, lc2.committee());
        assert!(available2.is_empty(), "unavailable program shrinks effective committee to 0");
        assert_eq!(lc2.advance(21), Phase::Revealing);
        lc2.advance(31);
        assert!(matches!(lc2.settle(&mut l2, &ByteEq), Terminal::Escalate(_)));
        // no verifier escrowed a bond (nobody committed); only budget + Be held
        assert_eq!(l2.escrowed_for(&job2), budget + e_bond);
        assert_eq!(l2.total_supply(), total0b);
    }

    #[test]
    fn settle_is_idempotent_second_call_moves_no_value() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [7u8; 32];
        let result = [7u8; 32];
        let (mut l, total0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, result, budget, e_bond, v_bond);
        for (i, c) in cands3().iter().enumerate() {
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, result, [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in cands3().iter().enumerate() {
            assert_eq!(lc.record_reveal(reveal_of(*c, result, [i as u8; 32]), 25), EventResult::Accepted);
        }
        lc.advance(31);

        let first = lc.settle(&mut l, &ByteEq);
        assert!(matches!(first, Terminal::Confirmed(_)));
        // pot already drained + supply conserved after the first settle
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
        let exec_bal = l.balance_of(&pid(9));

        // a second settle returns the SAME terminal and moves no value (no panic, no double-spend)
        let second = lc.settle(&mut l, &ByteEq);
        assert_eq!(first, second, "settle is idempotent");
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.balance_of(&pid(9)), exec_bal, "no balance moved on re-settle");
    }

    #[test]
    fn same_seed_and_events_reach_identical_committee_terminal_and_ledger() {
        let (budget, e_bond, v_bond) = min_funding();
        let result = [7u8; 32];
        let revealed: Vec<(ParticipantId, [u8; 32])> = cands3().into_iter().map(|c| (c, result)).collect();
        let (t1, l1, _) = run_round([20u8; 32], result, budget, e_bond, v_bond, &cands3(), &revealed);
        let (t2, l2, _) = run_round([20u8; 32], result, budget, e_bond, v_bond, &cands3(), &revealed);
        // identical terminal outcome
        assert_eq!(t1, t2);
        // identical ledger end-state for every actor
        for who in [pid(0), pid(9), pid(10), pid(11), pid(12)] {
            assert_eq!(l1.balance_of(&who), l2.balance_of(&who));
        }
        assert_eq!(l1.total_supply(), l2.total_supply());
    }

    #[test]
    fn job_lifecycle_record_round_trips_through_borsh() {
        // Non-trivial: committee drawn, 3 commit, only 2 reveal (a non-revealer), settled Confirmed.
        let (budget, e_bond, v_bond) = min_funding();
        let job = [1u8; 32];
        let result = [7u8; 32];
        let (mut l, _t0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, result, budget, e_bond, v_bond);
        for (i, c) in cands3().iter().enumerate() {
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, result, [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in cands3().iter().take(2).enumerate() {
            assert_eq!(lc.record_reveal(reveal_of(*c, result, [i as u8; 32]), 25), EventResult::Accepted);
        }
        lc.advance(31);
        assert!(matches!(lc.settle(&mut l, &ByteEq), Terminal::Confirmed(_)));

        let rec = lc.to_record();
        // P9/D8: the DTO carries the program identity fixed at open.
        assert_eq!(rec.program_hash, IDENT, "program_hash persisted");
        assert_eq!(rec.input_hash, IDENT, "input_hash persisted");
        assert_eq!(rec.da_root, IDENT, "da_root persisted");
        let bytes = borsh::to_vec(&rec).expect("serialize");
        let rec2: JobLifecycleRecord = borsh::from_slice(&bytes).expect("deserialize");
        assert_eq!(rec, rec2, "DTO borsh round-trips");

        let lc2 = JobLifecycle::from_record(rec2, GameParams::default(), ResolutionParams::default());
        assert_eq!(lc2.to_record(), rec, "from_record ∘ to_record is identity under the same params");
        assert_eq!(lc2.phase(), lc.phase());
        assert_eq!(lc2.committee(), lc.committee());
        assert_eq!(lc2.expected_escrow(), lc.expected_escrow());
    }

    #[test]
    fn post_result_then_draw_committee_matches_submit_result() {
        // B5: post_result (record only) + draw_committee (block-tail draw) must reproduce exactly
        // what the monolithic submit_result produces, given the same seed.
        let (budget, e_bond, v_bond) = min_funding();
        let seed = [42u8; 32];
        let stake = |_: &ParticipantId| 1u64;

        let mut a = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cands3(), deadlines(),
        );
        assert_eq!(a.submit_result(pid(9), [7u8; 32], seed, 5, &stake), EventResult::Accepted);

        let mut b = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cands3(), deadlines(),
        );
        // Draw before a result is a no-op (nothing posted yet).
        b.draw_committee(seed, &stake);
        assert_eq!(b.phase(), Phase::AwaitingResult);
        assert!(b.committee().is_empty());
        assert_eq!(b.post_result(pid(9), [7u8; 32], 5), EventResult::Accepted);
        assert!(b.committee().is_empty(), "post_result records only — no draw");
        assert_eq!(b.phase(), Phase::AwaitingResult);
        b.draw_committee(seed, &stake);

        assert_eq!(a.phase(), b.phase());
        assert_eq!(a.committee(), b.committee(), "split path == monolithic submit_result");
        assert_eq!(b.phase(), Phase::Committing);
    }

    #[test]
    fn draw_committee_is_deterministic_and_idempotent() {
        let (budget, e_bond, v_bond) = min_funding();
        let stake = |_: &ParticipantId| 1u64;
        let mk = || {
            let mut lc = JobLifecycle::open(
                [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
                GameParams::default(), ResolutionParams::default(),
                vec![pid(10), pid(11), pid(12), pid(13), pid(14)], deadlines(),
            );
            assert_eq!(lc.post_result(pid(9), [7u8; 32], 5), EventResult::Accepted);
            lc
        };
        let mut a = mk();
        let mut b = mk();
        a.draw_committee([99u8; 32], &stake);
        b.draw_committee([99u8; 32], &stake);
        assert_eq!(a.committee(), b.committee(), "same seed ⇒ identical committee");
        assert_eq!(a.committee().len(), 3, "k=3 drawn from 5 candidates");

        // Idempotent: a second draw at a DIFFERENT seed cannot re-draw (committee already set).
        let first = a.committee().to_vec();
        a.draw_committee([1u8; 32], &stake);
        assert_eq!(a.committee(), &first[..], "already-drawn committee is frozen");

        // Seed-sensitivity: a different seed on a fresh lifecycle changes the committee.
        let mut c = mk();
        c.draw_committee([1u8; 32], &stake);
        assert_ne!(c.committee(), a.committee(), "different seed ⇒ different committee");
    }

    #[test]
    fn draw_committee_undersized_and_empty_pools_are_smaller_not_errors() {
        let (budget, e_bond, v_bond) = min_funding();
        let stake = |_: &ParticipantId| 1u64;
        // Two candidates, k=3 ⇒ a committee of 2 (min(k, pool)), NOT an error.
        let mut two = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(),
            vec![pid(10), pid(11)], deadlines(),
        );
        assert_eq!(two.post_result(pid(9), [7u8; 32], 5), EventResult::Accepted);
        two.draw_committee([5u8; 32], &stake);
        assert_eq!(two.committee().len(), 2, "undersized pool ⇒ smaller committee");
        assert_eq!(two.phase(), Phase::Committing);

        // Empty candidate pool ⇒ empty committee, still Committing (settles as NoQuorum later).
        let mut empty = JobLifecycle::open(
            [2u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), vec![], deadlines(),
        );
        assert_eq!(empty.post_result(pid(9), [7u8; 32], 5), EventResult::Accepted);
        empty.draw_committee([5u8; 32], &stake);
        assert!(empty.committee().is_empty(), "empty pool ⇒ empty committee (no panic)");
        assert_eq!(empty.phase(), Phase::Committing);
    }

    #[test]
    fn post_result_rejects_wrong_executor_wrong_phase_past_window() {
        let (budget, e_bond, v_bond) = min_funding();
        let mut lc = JobLifecycle::open(
            [1u8; 32], IDENT, IDENT, IDENT, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), cands3(), deadlines(),
        );
        // wrong executor
        assert_eq!(lc.post_result(pid(8), [7u8; 32], 5), EventResult::Rejected(RejectReason::NotExecutor));
        // past the result window (deadlines.result_by == 10)
        assert_eq!(lc.post_result(pid(9), [7u8; 32], 11), EventResult::Rejected(RejectReason::PastWindow));
        // accepted, then a second post is WrongPhase (still AwaitingResult until draw, but the
        // executor_hash is set — post_result only fires in AwaitingResult and a re-post is rejected
        // once the committee is drawn/phase advances)
        assert_eq!(lc.post_result(pid(9), [7u8; 32], 5), EventResult::Accepted);
        let stake = |_: &ParticipantId| 1u64;
        lc.draw_committee([9u8; 32], &stake);
        assert_eq!(lc.post_result(pid(9), [7u8; 32], 6), EventResult::Rejected(RejectReason::WrongPhase));
    }

    #[test]
    fn job_lifecycle_record_round_trips_escalate_handoff() {
        // Exercises TerminalRec::Escalate + EscalationHandoffRec: 3-way split ⇒ NoQuorum ⇒ Escalate.
        let (budget, e_bond, v_bond) = min_funding();
        let job = [4u8; 32];
        let result = [7u8; 32];
        let (mut l, _t0) = funded(job, budget, e_bond, v_bond, &cands3());
        let mut lc = opened_committing(job, result, budget, e_bond, v_bond);
        let revealed = [(pid(10), [1u8; 32]), (pid(11), [2u8; 32]), (pid(12), [3u8; 32])];
        for (i, c) in cands3().iter().enumerate() {
            let h = revealed.iter().find(|(v, _)| v == c).map(|(_, h)| *h).unwrap();
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, h, [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in cands3().iter().enumerate() {
            let h = revealed.iter().find(|(v, _)| v == c).map(|(_, h)| *h).unwrap();
            assert_eq!(lc.record_reveal(reveal_of(*c, h, [i as u8; 32]), 25), EventResult::Accepted);
        }
        lc.advance(31);
        assert!(matches!(lc.settle(&mut l, &ByteEq), Terminal::Escalate(_)));

        let rec = lc.to_record();
        let rec2: JobLifecycleRecord = borsh::from_slice(&borsh::to_vec(&rec).unwrap()).unwrap();
        assert_eq!(rec, rec2);
        let lc2 = JobLifecycle::from_record(rec2, GameParams::default(), ResolutionParams::default());
        assert_eq!(lc2.to_record(), rec);
    }
}
