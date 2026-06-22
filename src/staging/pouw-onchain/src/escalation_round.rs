//! Module 6 — the multi-block escalation panel round (follows P2a; completes the game money-path).
//!
//! Consumes a `lifecycle::EscalationHandoff` (emitted when the primary round splits to NoQuorum,
//! escrow held = budget + Be + original-revealers·Bv) and runs the larger `k_escalate` panel as a
//! second commit-reveal round across blocks. Realistic panel: DA-abstaining panelists never commit,
//! commit-no-reveal panelists forfeit their bond. Settles via the FROZEN lower-level
//! `settle_noquorum_confirmed`/`settle_noquorum_disputed` directly (NOT `escalation::resolve`, which
//! assumes a full revealing panel and would pay un-escrowed bonds on a partial panel). Reuses every
//! frozen game piece; modifies none. `settle` is idempotent; conserves on every terminal.
//!
//! WIRE-IN (P2 founder patch-spec): an `Escalate` TxKind / the `event_loop` opens an EscalationRound
//! when a `JobLifecycle` settles to `Terminal::Escalate`; the consensus seed is a post-escalation
//! block-hash/VRF. The panel re-executes the job (DA-fetched program) off-chain and submits
//! commit/reveal txs that map to `record_commit`/`record_reveal`.

use commputer_pouw::commit_reveal::reveal_matches;
use commputer_pouw::committee::select_committee;
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{Commitment, Reveal, SettlementOutcome, Verdict};
use commputer_pouw::oracle::{ChainHooks, EquivalenceOracle};
use commputer_pouw::params::GameParams;
use commputer_pouw::settlement::{settle_noquorum_confirmed, settle_noquorum_disputed};
use commputer_pouw::verdict::compute_verdict;
use crate::escrow_ledger::EscrowLedger;
use crate::lifecycle::EscalationHandoff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelPhase {
    Committing,
    Revealing,
    Settled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelDeadlines {
    pub commit_by: u64,
    pub reveal_by: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    WrongPhase,
    PastWindow,
    NotPanelMember,
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

/// Terminal of the escalation round — all settled, no further escalation (bounded).
#[derive(Clone, Debug, PartialEq)]
pub enum EscalationOutcome {
    Confirmed(SettlementOutcome), // panel vindicated the executor
    Disputed(SettlementOutcome),  // panel rejected the executor
    NoQuorum(SettlementOutcome),  // bounded terminal: panel split / too few available
}

/// The `k_escalate` panel re-execution round for one escalated job, as a deterministic multi-block
/// state machine. Holds only plain data; `stake_of`/`eq`/`&mut EscrowLedger` are passed to methods.
pub struct EscalationRound {
    job_id: [u8; 32],
    // from the handoff
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_hash: [u8; 32],
    executor_bond: u64,
    verifier_bond: u64,
    committee_reveals: Vec<Reveal>, // original committee revealers (to partition)
    committee_bonds: Vec<u64>,      // their held bonds
    // panel
    params: GameParams,
    deadlines: PanelDeadlines,
    panel: Vec<ParticipantId>, // drawn at open
    // collected panel data
    phase: PanelPhase,
    commitments: Vec<Commitment>,
    reveals: Vec<Reveal>,
    settled: Option<EscalationOutcome>,
}

impl EscalationRound {
    /// Open the round and draw the `k_escalate` panel from the consensus `seed`. PRECONDITION: the
    /// held escrow (budget + Be + original-revealers·Bv) is already in the job's pot (P2a terminal).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        handoff: EscalationHandoff,
        job_id: [u8; 32],
        candidates: Vec<ParticipantId>,
        seed: [u8; 32],
        params: GameParams,
        deadlines: PanelDeadlines,
        stake_of: &dyn Fn(&ParticipantId) -> u64,
    ) -> Self {
        let EscalationHandoff {
            budget,
            submitter,
            executor,
            executor_hash,
            executor_bond,
            committee_reveals,
            committee_bonds,
            verifier_bond,
        } = handoff;
        let panel = select_committee(&seed, &candidates, &executor, params.k_escalate, stake_of);
        Self {
            job_id,
            budget,
            submitter,
            executor,
            executor_hash,
            executor_bond,
            verifier_bond,
            committee_reveals,
            committee_bonds,
            params,
            deadlines,
            panel,
            phase: PanelPhase::Committing,
            commitments: Vec::new(),
            reveals: Vec::new(),
            settled: None,
        }
    }

    pub fn phase(&self) -> PanelPhase {
        self.phase
    }

    pub fn panel(&self) -> &[ParticipantId] {
        &self.panel
    }

    /// A panel member commits (DA-Available ⇒ they call this; Abstain ⇒ they don't). Validates
    /// phase/window/panel-membership/bond/no-double-commit, then escrows the verifier bond.
    pub fn record_commit(&mut self, l: &mut EscrowLedger, c: Commitment, height: u64) -> EventResult {
        if self.phase != PanelPhase::Committing {
            return EventResult::Rejected(RejectReason::WrongPhase);
        }
        if height > self.deadlines.commit_by {
            return EventResult::Rejected(RejectReason::PastWindow);
        }
        if !self.panel.contains(&c.verifier) {
            return EventResult::Rejected(RejectReason::NotPanelMember);
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
        if self.phase != PanelPhase::Revealing {
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

    /// Height-driven Committing→Revealing at commit_by. Idempotent.
    pub fn advance(&mut self, height: u64) -> PanelPhase {
        if self.phase == PanelPhase::Committing && height > self.deadlines.commit_by {
            self.phase = PanelPhase::Revealing;
        }
        self.phase
    }

    /// Finalize the escalation round (call at reveal_by). Idempotent (caches the outcome). Drains
    /// the held escrow + the panel bonds via the frozen settle_noquorum_* functions.
    pub fn settle(&mut self, l: &mut EscrowLedger, eq: &dyn EquivalenceOracle) -> EscalationOutcome {
        if let Some(o) = &self.settled {
            return o.clone();
        }
        self.phase = PanelPhase::Settled;

        // Uniform commit-no-reveal forfeiture (before the verdict branch): burn each committed panel
        // member who never revealed. After this the pot holds budget + Be + orig_revealers·Bv +
        // revealing_panel·Bv. (`self.reveals` is exactly the revealers — record_reveal only stores
        // reveal_matches-validated reveals — so it equals the settlement panel below.)
        let revealed_ids: Vec<ParticipantId> = self.reveals.iter().map(|r| r.verifier).collect();
        let non_revealers: Vec<ParticipantId> = self
            .commitments
            .iter()
            .map(|c| c.verifier)
            .filter(|v| !revealed_ids.contains(v))
            .collect();
        l.for_job(self.job_id);
        for _ in &non_revealers {
            l.burn(self.verifier_bond);
        }
        let forfeit_burned = non_revealers.len() as u64 * self.verifier_bond;
        let forfeit_slashed: Vec<(ParticipantId, u64)> =
            non_revealers.iter().map(|v| (*v, self.verifier_bond)).collect();

        let panel: Vec<ParticipantId> = revealed_ids;
        let panel_bonds: Vec<u64> = vec![self.verifier_bond; panel.len()];

        let quorum = self.params.quorum(self.params.k_escalate);
        let verdict = compute_verdict(&self.reveals, &self.executor_hash, quorum, eq);

        let mut outcome = match verdict {
            Verdict::Confirmed { result_hash } => {
                let (vind, vind_bonds, rej, rej_bonds) =
                    partition(&self.committee_reveals, &self.committee_bonds, &result_hash, eq);
                let out = settle_noquorum_confirmed(
                    l, &self.params, self.budget, self.executor, self.executor_bond,
                    &vind, &vind_bonds, &rej, &rej_bonds, &panel, &panel_bonds,
                );
                EscalationOutcome::Confirmed(out)
            }
            Verdict::Disputed { correct_hash } => {
                let (honest, honest_bonds, rej, rej_bonds) =
                    partition(&self.committee_reveals, &self.committee_bonds, &correct_hash, eq);
                let out = settle_noquorum_disputed(
                    l, &self.params, self.budget, self.submitter, self.executor, self.executor_bond,
                    &honest, &honest_bonds, &rej, &rej_bonds, &panel, &panel_bonds,
                );
                EscalationOutcome::Disputed(out)
            }
            Verdict::NoQuorum => {
                // Bounded terminal: refund submitter, burn executor bond (panel keeps its escalation
                // reward + bonds), slash the WHOLE original committee. Matches escalation.rs:163-182.
                let all: Vec<ParticipantId> =
                    self.committee_reveals.iter().map(|r| r.verifier).collect();
                let out = settle_noquorum_disputed(
                    l, &self.params, self.budget, self.submitter, self.executor, self.executor_bond,
                    &[], &[], &all, &self.committee_bonds, &panel, &panel_bonds,
                );
                EscalationOutcome::NoQuorum(out)
            }
        };

        // Merge the forfeiture into the returned outcome's log.
        let o = match &mut outcome {
            EscalationOutcome::Confirmed(o)
            | EscalationOutcome::Disputed(o)
            | EscalationOutcome::NoQuorum(o) => o,
        };
        o.burned += forfeit_burned;
        o.slashed.extend(forfeit_slashed);

        self.settled = Some(outcome.clone());
        outcome
    }
}

/// Partition `reveals` (with matching `bonds`) by whether each value is equivalent to `decided`.
/// Returns (matching, matching_bonds, rejected, rejected_bonds). Mirrors the frozen private
/// `escalation::partition_committee`.
fn partition(
    reveals: &[Reveal],
    bonds: &[u64],
    decided: &[u8; 32],
    eq: &dyn EquivalenceOracle,
) -> (Vec<ParticipantId>, Vec<u64>, Vec<ParticipantId>, Vec<u64>) {
    let mut matching = Vec::new();
    let mut matching_bonds = Vec::new();
    let mut rejected = Vec::new();
    let mut rejected_bonds = Vec::new();
    for (rev, &b) in reveals.iter().zip(bonds.iter()) {
        if eq.equiv(&rev.result_hash, decided) {
            matching.push(rev.verifier);
            matching_bonds.push(b);
        } else {
            rejected.push(rev.verifier);
            rejected_bonds.push(b);
        }
    }
    (matching, matching_bonds, rejected, rejected_bonds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    fn deadlines() -> PanelDeadlines {
        PanelDeadlines { commit_by: 20, reveal_by: 30 }
    }

    /// 7 panel candidates (ids 20..27) — exactly k_escalate ⇒ the whole pool is the panel.
    fn panel7() -> Vec<ParticipantId> {
        (20u8..27).map(pid).collect()
    }

    fn handoff_3way(budget: u64, e_bond: u64, v_bond: u64) -> EscalationHandoff {
        // original committee (ids 10,11,12) split 3 ways: executor claimed [7]; they revealed 7/5/9.
        EscalationHandoff {
            budget,
            submitter: pid(0),
            executor: pid(9),
            executor_hash: [7u8; 32],
            executor_bond: e_bond,
            committee_reveals: vec![
                Reveal { verifier: pid(10), result_hash: [7u8; 32], salt: [0u8; 32] },
                Reveal { verifier: pid(11), result_hash: [5u8; 32], salt: [0u8; 32] },
                Reveal { verifier: pid(12), result_hash: [9u8; 32], salt: [0u8; 32] },
            ],
            committee_bonds: vec![v_bond; 3],
            verifier_bond: v_bond,
        }
    }

    #[test]
    fn opens_in_committing_with_full_panel() {
        let h = handoff_3way(3960, 3960, 1650);
        let stake = |_: &ParticipantId| 1u64;
        let r = EscalationRound::open(h, [1u8; 32], panel7(), [42u8; 32], GameParams::default(), deadlines(), &stake);
        assert_eq!(r.phase(), PanelPhase::Committing);
        assert_eq!(r.panel().len(), 7); // pool of exactly k_escalate=7 ⇒ whole pool
    }

    use commputer_pouw::economics::{budget_min, executor_bond_min, verifier_bond_min};
    use commputer_pouw::commit_reveal::make_commitment;

    fn min_funding() -> (u64, u64, u64) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let b = budget_min(f, &p).unwrap();
        (b, executor_bond_min(f, b, &p).unwrap(), verifier_bond_min(f, &p).unwrap())
    }

    /// Ledger mirroring the held P2a escrow: budget (submitter) + Be (executor) + the 3 original
    /// committee bonds escrowed into the job pot; panel candidates credited so they can escrow on
    /// commit. Returns (ledger, total_supply-after-all-credits).
    fn held_ledger(job: [u8; 32], budget: u64, e_bond: u64, v_bond: u64) -> (EscrowLedger, u64) {
        let mut l = EscrowLedger::new();
        l.credit(pid(0), budget);
        l.credit(pid(9), e_bond);
        for o in [pid(10), pid(11), pid(12)] { l.credit(o, v_bond); }
        for p in panel7() { l.credit(p, v_bond); }
        let total0 = l.total_supply();
        l.for_job(job);
        l.escrow(pid(0), budget);
        l.escrow(pid(9), e_bond);
        for o in [pid(10), pid(11), pid(12)] { l.escrow(o, v_bond); }
        (l, total0)
    }

    fn commit_of(v: ParticipantId, hash: [u8; 32], salt: [u8; 32], bond: u64) -> Commitment {
        make_commitment(&v, &hash, &salt, bond)
    }
    fn reveal_of(v: ParticipantId, hash: [u8; 32], salt: [u8; 32]) -> Reveal {
        Reveal { verifier: v, result_hash: hash, salt }
    }

    fn opened(job: [u8; 32], budget: u64, e_bond: u64, v_bond: u64) -> EscalationRound {
        let stake = |_: &ParticipantId| 1u64;
        EscalationRound::open(handoff_3way(budget, e_bond, v_bond), job, panel7(), [42u8; 32], GameParams::default(), deadlines(), &stake)
    }

    #[test]
    fn commit_reveal_and_advance_validate() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [1u8; 32];
        let (mut l, _t0) = held_ledger(job, budget, e_bond, v_bond);
        let mut r = opened(job, budget, e_bond, v_bond);
        let p20 = pid(20);

        // a valid panel commit escrows the bond
        assert_eq!(r.record_commit(&mut l, commit_of(p20, [7u8; 32], [0u8; 32], v_bond), 15), EventResult::Accepted);
        assert!(l.escrowed_for(&job) >= budget + e_bond + 3 * v_bond + v_bond);
        // non-panel member, wrong bond, double-commit, past window
        assert_eq!(r.record_commit(&mut l, commit_of(pid(99), [7u8; 32], [0u8; 32], v_bond), 15), EventResult::Rejected(RejectReason::NotPanelMember));
        assert_eq!(r.record_commit(&mut l, commit_of(pid(21), [7u8; 32], [0u8; 32], v_bond + 1), 15), EventResult::Rejected(RejectReason::WrongBond));
        assert_eq!(r.record_commit(&mut l, commit_of(p20, [7u8; 32], [1u8; 32], v_bond), 15), EventResult::Rejected(RejectReason::DoubleCommit));
        assert_eq!(r.record_commit(&mut l, commit_of(pid(21), [7u8; 32], [0u8; 32], v_bond), 21), EventResult::Rejected(RejectReason::PastWindow));

        // reveal before Revealing is rejected
        assert_eq!(r.record_reveal(reveal_of(p20, [7u8; 32], [0u8; 32]), 16), EventResult::Rejected(RejectReason::WrongPhase));
        assert_eq!(r.advance(21), PanelPhase::Revealing);
        // matching reveal accepted; replay / unknown / mismatch / past-window rejected
        assert_eq!(r.record_reveal(reveal_of(p20, [7u8; 32], [0u8; 32]), 25), EventResult::Accepted);
        assert_eq!(r.record_reveal(reveal_of(p20, [7u8; 32], [0u8; 32]), 25), EventResult::Rejected(RejectReason::AlreadyRevealed));
        assert_eq!(r.record_reveal(reveal_of(pid(21), [7u8; 32], [0u8; 32]), 25), EventResult::Rejected(RejectReason::UnknownCommitter));
        // mismatch needs a prior commit by pid(21)
        let mut r2 = opened(job, budget, e_bond, v_bond);
        let (mut l2, _t) = held_ledger(job, budget, e_bond, v_bond);
        assert_eq!(r2.record_commit(&mut l2, commit_of(pid(21), [7u8; 32], [0u8; 32], v_bond), 15), EventResult::Accepted);
        assert_eq!(r2.advance(21), PanelPhase::Revealing);
        assert_eq!(r2.record_reveal(reveal_of(pid(21), [7u8; 32], [9u8; 32]), 25), EventResult::Rejected(RejectReason::RevealMismatch));
        assert_eq!(r2.record_reveal(reveal_of(pid(21), [7u8; 32], [0u8; 32]), 31), EventResult::Rejected(RejectReason::PastWindow));
    }

    use commputer_pouw::oracle::ByteEq;

    /// Drive the panel: each id in `committers` commits, then each `(id, hash)` in `revealed` reveals;
    /// advance past both windows; settle. Returns (outcome, ledger, total0).
    fn run_panel(
        job: [u8; 32], budget: u64, e_bond: u64, v_bond: u64,
        committers: &[ParticipantId],
        revealed: &[(ParticipantId, [u8; 32])],
    ) -> (EscalationOutcome, EscrowLedger, u64) {
        let (mut l, total0) = held_ledger(job, budget, e_bond, v_bond);
        let mut r = opened(job, budget, e_bond, v_bond);
        for (i, c) in committers.iter().enumerate() {
            let salt = [i as u8; 32];
            let hash = revealed.iter().find(|(v, _)| v == c).map(|(_, h)| *h).unwrap_or([7u8; 32]);
            assert_eq!(r.record_commit(&mut l, commit_of(*c, hash, salt, v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(r.advance(21), PanelPhase::Revealing);
        for (i, c) in committers.iter().enumerate() {
            if let Some((_, h)) = revealed.iter().find(|(v, _)| v == c) {
                assert_eq!(r.record_reveal(reveal_of(*c, *h, [i as u8; 32]), 25), EventResult::Accepted);
            }
        }
        let out = r.settle(&mut l, &ByteEq);
        (out, l, total0)
    }

    #[test]
    fn panel_confirms_executor_vindicates_matching_original_verifier() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [2u8; 32];
        // all 7 panelists re-execute and agree with the executor ([7]) ⇒ Confirmed.
        let committers = panel7();
        let revealed: Vec<(ParticipantId, [u8; 32])> = committers.iter().map(|c| (*c, [7u8; 32])).collect();
        let (out, l, total0) = run_panel(job, budget, e_bond, v_bond, &committers, &revealed);
        match out {
            EscalationOutcome::Confirmed(o) => {
                // executor (worker) paid 85% of budget; original verifier pid(10) revealed [7] ⇒ vindicated.
                assert_eq!(o.worker_paid, 3_366);
                assert!(l.balance_of(&pid(9)) >= 3_366, "executor paid worker share");
                assert!(l.balance_of(&pid(10)) > 0, "vindicated original verifier paid + bond back");
                assert_eq!(l.balance_of(&pid(11)), 0, "rejected original verifier slashed");
                assert_eq!(l.balance_of(&pid(12)), 0, "rejected original verifier slashed");
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert_eq!(l.escrowed_for(&job), 0, "held escrow fully drained");
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn panel_disputes_executor_refunds_submitter() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [3u8; 32];
        // all 7 panelists agree on [5] (≠ executor's [7]) ⇒ Disputed on [5].
        let committers = panel7();
        let revealed: Vec<(ParticipantId, [u8; 32])> = committers.iter().map(|c| (*c, [5u8; 32])).collect();
        let (out, l, total0) = run_panel(job, budget, e_bond, v_bond, &committers, &revealed);
        match out {
            EscalationOutcome::Disputed(o) => {
                assert_eq!(o.submitter_refunded, budget);
                assert!(o.slashed.iter().any(|(v, _)| *v == pid(9)), "executor bond slashed");
                assert!(l.balance_of(&pid(11)) > 0, "honest original verifier (revealed [5]) paid + bond back");
                assert_eq!(l.balance_of(&pid(10)), 0, "rejected original verifier slashed");
            }
            other => panic!("expected Disputed, got {other:?}"),
        }
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn panel_noquorum_is_bounded_terminal() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [4u8; 32];
        // 7 panelists split so no value reaches quorum(7)=5: 4×[1], 3×[2].
        let committers = panel7();
        let revealed: Vec<(ParticipantId, [u8; 32])> = committers.iter().enumerate()
            .map(|(i, c)| (*c, if i < 4 { [1u8; 32] } else { [2u8; 32] })).collect();
        let (out, l, total0) = run_panel(job, budget, e_bond, v_bond, &committers, &revealed);
        match out {
            EscalationOutcome::NoQuorum(o) => {
                assert_eq!(o.submitter_refunded, budget, "submitter refunded on bounded terminal");
                // whole original committee slashed
                for o_id in [pid(10), pid(11), pid(12)] {
                    assert_eq!(l.balance_of(&o_id), 0, "original committee member slashed");
                }
            }
            other => panic!("expected NoQuorum, got {other:?}"),
        }
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn panel_commit_no_reveal_is_forfeited() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [5u8; 32];
        // all 7 commit, but only 6 reveal [7] (still ≥ quorum 5 ⇒ Confirmed); pid(26) forfeits.
        let committers = panel7();
        let revealed: Vec<(ParticipantId, [u8; 32])> =
            committers.iter().take(6).map(|c| (*c, [7u8; 32])).collect();
        let (out, l, total0) = run_panel(job, budget, e_bond, v_bond, &committers, &revealed);
        match out {
            EscalationOutcome::Confirmed(o) => {
                assert!(o.slashed.iter().any(|(v, b)| *v == pid(26) && *b == v_bond), "non-revealer forfeited");
            }
            other => panic!("expected Confirmed, got {other:?}"),
        }
        assert_eq!(l.escrowed_for(&job), 0, "forfeited bond burned, pot drained");
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn too_few_panelists_available_is_bounded_noquorum() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [6u8; 32];
        // only 4 of 7 panelists are available/commit (4 < quorum(7)=5) ⇒ bounded NoQuorum.
        // (DA-abstaining panelists simply never commit, so escrow no bond.)
        let committers: Vec<ParticipantId> = panel7().into_iter().take(4).collect();
        let revealed: Vec<(ParticipantId, [u8; 32])> = committers.iter().map(|c| (*c, [7u8; 32])).collect();
        let (out, l, total0) = run_panel(job, budget, e_bond, v_bond, &committers, &revealed);
        assert!(matches!(out, EscalationOutcome::NoQuorum(_)), "too few available ⇒ bounded NoQuorum");
        // the 3 non-committing panelists escrowed nothing
        for np in panel7().into_iter().skip(4) {
            assert_eq!(l.balance_of(&np), v_bond, "abstaining panelist kept its credited bond, escrowed nothing");
        }
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
    }

    #[test]
    fn settle_is_idempotent() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [7u8; 32];
        let committers = panel7();
        // drive a full Confirmed round inline (not via run_panel), keeping the round to re-settle:
        let (mut l, total0) = held_ledger(job, budget, e_bond, v_bond);
        let mut r = opened(job, budget, e_bond, v_bond);
        for (i, c) in committers.iter().enumerate() {
            assert_eq!(r.record_commit(&mut l, commit_of(*c, [7u8; 32], [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(r.advance(21), PanelPhase::Revealing);
        for (i, c) in committers.iter().enumerate() {
            assert_eq!(r.record_reveal(reveal_of(*c, [7u8; 32], [i as u8; 32]), 25), EventResult::Accepted);
        }
        let first = r.settle(&mut l, &ByteEq);
        assert_eq!(l.escrowed_for(&job), 0);
        assert_eq!(l.total_supply(), total0);
        let exec_bal = l.balance_of(&pid(9));
        let second = r.settle(&mut l, &ByteEq);
        assert_eq!(first, second, "settle idempotent");
        assert_eq!(l.balance_of(&pid(9)), exec_bal, "no value moved on re-settle");
        assert_eq!(l.total_supply(), total0);
    }

    use crate::lifecycle::{JobLifecycle, Phase, PhaseDeadlines, Terminal};
    use crate::settlement_resolution::ResolutionParams;

    #[test]
    fn end_to_end_primary_escalate_then_panel_settles_and_conserves() {
        let (budget, e_bond, v_bond) = min_funding();
        let job = [200u8; 32];
        let primary_committee = vec![pid(10), pid(11), pid(12)]; // primary round's 3 verifiers
        let panel = panel7();

        // ONE ledger funding everyone up front (primary actors + panel candidates); total0 captured
        // before any escrow, asserted invariant across BOTH rounds.
        let mut l = EscrowLedger::new();
        l.credit(pid(0), budget);
        l.credit(pid(9), e_bond);
        for c in &primary_committee { l.credit(*c, v_bond); }
        for p in &panel { l.credit(*p, v_bond); }
        let total0 = l.total_supply();
        // submit+claim escrow (P1 precondition for the primary round)
        l.for_job(job);
        l.escrow(pid(0), budget);
        l.escrow(pid(9), e_bond);

        // --- Primary round (P2a): 3 verifiers reveal a 3-way split ⇒ Escalate. ---
        let stake = |_: &ParticipantId| 1u64;
        let mut lc = JobLifecycle::open(
            job, pid(0), pid(9), e_bond, budget, v_bond,
            GameParams::default(), ResolutionParams::default(), primary_committee.clone(),
            PhaseDeadlines { result_by: 10, commit_by: 20, reveal_by: 30 },
        );
        // NB: JobLifecycle methods return `lifecycle::EventResult`, a DISTINCT type from this
        // module's `EventResult`; qualify the primary-round assertions to its own enum.
        assert_eq!(lc.submit_result(pid(9), [7u8; 32], [42u8; 32], 5, &stake), crate::lifecycle::EventResult::Accepted);
        let split = [[1u8; 32], [2u8; 32], [3u8; 32]];
        for (i, c) in primary_committee.iter().enumerate() {
            assert_eq!(lc.record_commit(&mut l, commit_of(*c, split[i], [i as u8; 32], v_bond), 15), crate::lifecycle::EventResult::Accepted);
        }
        assert_eq!(lc.advance(21), Phase::Revealing);
        for (i, c) in primary_committee.iter().enumerate() {
            assert_eq!(lc.record_reveal(reveal_of(*c, split[i], [i as u8; 32]), 25), crate::lifecycle::EventResult::Accepted);
        }
        lc.advance(31);
        let handoff = match lc.settle(&mut l, &ByteEq) {
            Terminal::Escalate(h) => h,
            other => panic!("expected primary Escalate, got {other:?}"),
        };
        // escrow held after the primary round: budget + Be + 3 committee bonds
        assert_eq!(l.escrowed_for(&job), budget + e_bond + 3 * v_bond);
        assert_eq!(l.total_supply(), total0);

        // --- Escalation round: the k_escalate=7 panel re-executes and confirms the executor ([7]). ---
        let mut er = EscalationRound::open(handoff, job, panel.clone(), [99u8; 32], GameParams::default(), deadlines(), &stake);
        for (i, p) in panel.iter().enumerate() {
            assert_eq!(er.record_commit(&mut l, commit_of(*p, [7u8; 32], [i as u8; 32], v_bond), 15), EventResult::Accepted);
        }
        assert_eq!(er.advance(21), PanelPhase::Revealing);
        for (i, p) in panel.iter().enumerate() {
            assert_eq!(er.record_reveal(reveal_of(*p, [7u8; 32], [i as u8; 32]), 25), EventResult::Accepted);
        }
        assert!(matches!(er.settle(&mut l, &ByteEq), EscalationOutcome::Confirmed(_)));

        // The originally-held escrow now drains to 0 across the whole two-round lifecycle; conserved.
        assert_eq!(l.escrowed_for(&job), 0, "held escrow fully resolved by the escalation round");
        assert_eq!(l.total_supply(), total0, "supply invariant across both rounds");
    }
}
