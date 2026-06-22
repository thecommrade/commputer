//! Module 4 — the five terminal money-path resolution functions (blueprint phase P1;
//! G1 = adopt-escrow). What `storage/state.rs` calls when a job reaches a terminal state.
//!
//! Each resolver takes the terminal outcome as DATA (the chain holds the verdict +
//! committee by settlement time — it does not re-run the synchronous `engine::run_job`),
//! binds the job's escrow pot via [`EscrowLedger::for_job`], moves value with the
//! `ChainHooks` ops, and returns the game's [`SettlementOutcome`]. `confirmed`/`disputed`
//! delegate the budget split to the frozen game `settlement.rs` and add the bond-return
//! the engine does inline (`engine.rs:251-277`); `cancel`/`timeout`/`unavailable` are the
//! net-new lifecycle outcomes the game never produces.
//!
//! ## Conservation (every resolver)
//! After it returns, `total_supply()` is unchanged (no mint) and `escrowed_for(&job_id)`
//! is 0 (the pot is fully drained — every escrowed unit is paid out or burned). Both are
//! required: `total_supply` alone cannot catch a stranded pot (escrow is inside supply).
//!
//! WIRE-IN (P1 founder patch-spec): `storage/state.rs` gains a per-`job_id` escrow ledger
//! and routes the terminal handlers through these five functions; `core/token.rs` tracks
//! the burned supply; `SubmitJob` is dropped from the existing `is_burn`/`burn_amount` path.

use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::SettlementOutcome;
use commputer_pouw::params::GameParams;
use commputer_pouw::settlement::{bps, settle_committee_disputed, settle_confirmed_sampled};
// The resolvers are generic over `Ledger` now (was a concrete `EscrowLedger`); the `&mut impl Ledger`
// coerces to the `&mut dyn ChainHooks` the frozen `settle_*` take, so `ChainHooks` is no longer named
// here. `EscrowLedger` is used only by the tests below.
use crate::escrow_ledger::Ledger;

/// Lifecycle-only resolution fees (the `Confirmed`/`Disputed` splits use `GameParams`).
/// `GameParams` is a frozen game file, so these live here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolutionParams {
    /// G7 cancel: fraction of the budget burned on a pre-claim cancellation (200 = 2%).
    pub cancel_burn_bps: u32,
    /// G7 timeout: the submitter's compensation share of the slashed executor bond
    /// (2_000 = 20%; mirrors `dispute_bounty_bps`, the analogous harmed-party share).
    pub timeout_submitter_comp_bps: u32,
}

impl Default for ResolutionParams {
    fn default() -> Self {
        Self { cancel_burn_bps: 200, timeout_submitter_comp_bps: 2_000 }
    }
}

/// `Cancel` (G7, pre-claim only): the submitter withdraws before any executor claims, so
/// only the budget is escrowed. Burn `cancel_burn_bps` of the budget; refund the rest.
pub fn resolve_cancel(
    l: &mut impl Ledger,
    rp: &ResolutionParams,
    job_id: [u8; 32],
    budget: u64,
    submitter: ParticipantId,
) -> SettlementOutcome {
    l.for_job(job_id);
    let burn = bps(budget, rp.cancel_burn_bps);
    let refund = budget - burn;
    l.pay(submitter, refund);
    l.burn(burn);
    SettlementOutcome { submitter_refunded: refund, burned: burn, ..Default::default() }
}

/// `Timeout` (G7): the executor claimed then missed the deadline. Refund the full budget;
/// compensate the submitter `timeout_submitter_comp_bps` of the slashed executor bond;
/// burn the remainder of the bond. (Timeout is pre-verification, so no committee bonds
/// exist.) The full bond is recorded in `slashed` as a log.
pub fn resolve_timeout(
    l: &mut impl Ledger,
    rp: &ResolutionParams,
    job_id: [u8; 32],
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
) -> SettlementOutcome {
    l.for_job(job_id);
    l.pay(submitter, budget);
    let comp = bps(executor_bond, rp.timeout_submitter_comp_bps);
    l.pay(submitter, comp);
    l.burn(executor_bond - comp);
    SettlementOutcome {
        submitter_refunded: budget + comp,
        burned: executor_bond - comp,
        slashed: vec![(executor, executor_bond)],
        ..Default::default()
    }
}

/// `Unavailable` (G8): the data-availability obligation failed. No-fault teardown — refund
/// the full budget, return the executor bond intact (NOT slashed), burn nothing. Takes no
/// `ResolutionParams`: the decision is a flat full refund (G8's withholding penalty is
/// deferred; re-introducing it is a single `burn` over this structure).
pub fn resolve_unavailable(
    l: &mut impl Ledger,
    job_id: [u8; 32],
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
) -> SettlementOutcome {
    l.for_job(job_id);
    l.pay(submitter, budget);
    l.pay(executor, executor_bond);
    SettlementOutcome {
        submitter_refunded: budget,
        bonds_returned: executor_bond,
        ..Default::default()
    }
}

/// `Confirmed` verdict (sampled committee): split the budget 85/10/5 and return every
/// bond. Verdict + committee are on-chain data by settlement time. Delegates the budget
/// split to the frozen game `settle_confirmed_sampled` (which returns `bonds_returned: 0`),
/// then returns the executor + committee bonds and records `bonds_returned` itself —
/// mirroring `engine.rs:251-258`.
#[allow(clippy::too_many_arguments)]
pub fn resolve_confirmed(
    l: &mut impl Ledger,
    p: &GameParams,
    job_id: [u8; 32],
    budget: u64,
    worker: ParticipantId,
    executor_bond: u64,
    committee: &[ParticipantId],
    verifier_bond: u64,
) -> SettlementOutcome {
    l.for_job(job_id);
    let mut out = settle_confirmed_sampled(l, p, budget, worker, committee);
    l.pay(worker, executor_bond);
    for v in committee {
        l.pay(*v, verifier_bond);
    }
    out.bonds_returned = executor_bond + committee.len() as u64 * verifier_bond;
    out
}

/// `Disputed` verdict (committee proved the executor wrong): refund the full budget,
/// slash the executor bond (bounty to honest verifiers + burn the rest), and return every
/// committee member's bond — the catch bounty went to the honest subset, but ALL committee
/// bonds return (only the executor's Be is slashed on a committee Disputed). Delegates the
/// budget/bond split to the frozen game `settle_committee_disputed` and records the
/// returned committee bonds itself — mirroring `engine.rs:260-277`.
#[allow(clippy::too_many_arguments)]
pub fn resolve_disputed(
    l: &mut impl Ledger,
    p: &GameParams,
    job_id: [u8; 32],
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
    committee: &[ParticipantId],
    honest_verifiers: &[ParticipantId],
    verifier_bond: u64,
) -> SettlementOutcome {
    l.for_job(job_id);
    let mut out =
        settle_committee_disputed(l, p, budget, submitter, executor, executor_bond, honest_verifiers);
    for v in committee {
        l.pay(*v, verifier_bond);
    }
    out.bonds_returned = committee.len() as u64 * verifier_bond;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escrow_ledger::EscrowLedger; // the tests drive the concrete reference ledger
    use commputer_pouw::oracle::ChainHooks; // for .escrow/.pay/.burn on the concrete EscrowLedger
    use commputer_pouw::economics::{budget_min, executor_bond_min, run_priced_job, verifier_bond_min};
    use commputer_pouw::engine::JobInputs;
    use commputer_pouw::ids::JobId;
    use commputer_pouw::job::{Job, JobSpec, Verdict};
    use commputer_pouw::oracle::{ByteEq, IteratedHashVm};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// Credit each actor, bind the job, and escrow each actor's contribution into the
    /// job's pot — the post-admission state every resolver assumes. Returns total_supply
    /// after funding (the conserved value to assert against).
    fn fund_pot(l: &mut EscrowLedger, job_id: [u8; 32], parts: &[(ParticipantId, u64)]) -> u64 {
        for (who, amt) in parts {
            l.credit(*who, *amt);
        }
        l.for_job(job_id);
        for (who, amt) in parts {
            l.escrow(*who, *amt);
        }
        l.total_supply()
    }

    #[test]
    fn resolution_params_defaults_match_recorded_decisions() {
        let rp = ResolutionParams::default();
        assert_eq!(rp.cancel_burn_bps, 200, "G7 cancel = 2%");
        assert_eq!(rp.timeout_submitter_comp_bps, 2_000, "G7 timeout submitter comp = 20%");
    }

    #[test]
    fn disputed_refunds_submitter_bounties_honest_returns_committee_bonds() {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap();                // 3_960
        let e_bond = executor_bond_min(f, budget, &p).unwrap(); // 3_960
        let v_bond = verifier_bond_min(f, &p).unwrap();         // 1_650
        let job = [5u8; 32];
        let (submitter, executor) = (pid(0), pid(9));
        let committee = [pid(10), pid(11), pid(12)];
        let honest = [pid(10), pid(11)]; // 2 of 3 revealed the vindicated value

        let mut l = EscrowLedger::new();
        let mut parts = vec![(submitter, budget), (executor, e_bond)];
        for c in &committee {
            parts.push((*c, v_bond));
        }
        let total0 = fund_pot(&mut l, job, &parts);

        let out = resolve_disputed(
            &mut l, &p, job, budget, submitter, executor, e_bond, &committee, &honest, v_bond,
        );

        // dispute_bounty_bps = 2000 → bounty = 20% of 3_960 = 792, split 2 ways = 396 each.
        assert_eq!(out.submitter_refunded, budget, "full budget refunded");
        assert_eq!(out.verifiers_paid, 792, "20% of Be to the 2 honest verifiers");
        assert_eq!(out.burned, e_bond - 792, "remainder of the slashed bond (3960-792=3168)");
        assert_eq!(out.bonds_returned, 3 * v_bond, "all 3 committee bonds returned");
        assert_eq!(out.slashed, vec![(executor, e_bond)]);
        assert_eq!(l.balance_of(&submitter), budget);
        assert_eq!(l.balance_of(&honest[0]), 396 + v_bond, "bounty share + bond back");
        assert_eq!(l.balance_of(&committee[2]), v_bond, "non-honest member: bond back only");
        assert_eq!(l.balance_of(&executor), 0, "executor bond slashed");
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn confirmed_splits_85_10_5_returns_all_bonds_conserves() {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap();                // 3_960
        let e_bond = executor_bond_min(f, budget, &p).unwrap(); // 3_960
        let v_bond = verifier_bond_min(f, &p).unwrap();         // 1_650
        let job = [4u8; 32];
        let (submitter, executor) = (pid(0), pid(9));
        let committee = [pid(10), pid(11), pid(12)];

        let mut l = EscrowLedger::new();
        let mut parts = vec![(submitter, budget), (executor, e_bond)];
        for c in &committee {
            parts.push((*c, v_bond));
        }
        let total0 = fund_pot(&mut l, job, &parts);

        let out = resolve_confirmed(&mut l, &p, job, budget, executor, e_bond, &committee, v_bond);

        assert_eq!(out.worker_paid, 3_366, "85% of 3_960");
        assert_eq!(out.verifiers_paid, 396, "10% split across 3 (132 each)");
        assert_eq!(out.burned, 198, "5%");
        assert_eq!(out.bonds_returned, e_bond + 3 * v_bond, "exec + 3 committee bonds");
        assert_eq!(l.balance_of(&executor), 3_366 + e_bond, "worker share + bond back");
        assert_eq!(l.balance_of(&committee[0]), 132 + v_bond);
        assert_eq!(l.balance_of(&submitter), 0, "confirmed: submitter not refunded");
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn unavailable_full_refund_returns_bond_burns_nothing() {
        let job = [3u8; 32];
        let (submitter, executor) = (pid(0), pid(9));
        let mut l = EscrowLedger::new();
        let total0 = fund_pot(&mut l, job, &[(submitter, 10_000), (executor, 10_000)]);

        let out = resolve_unavailable(&mut l, job, 10_000, submitter, executor, 10_000);

        assert_eq!(out.submitter_refunded, 10_000, "full budget refunded (no penalty)");
        assert_eq!(out.bonds_returned, 10_000, "executor bond returned intact");
        assert_eq!(out.burned, 0, "no-fault: nothing burned");
        assert_eq!(l.balance_of(&submitter), 10_000);
        assert_eq!(l.balance_of(&executor), 10_000, "executor made whole");
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn timeout_refunds_budget_comps_submitter_20pct_burns_rest() {
        let rp = ResolutionParams::default();
        let job = [2u8; 32];
        let (submitter, executor) = (pid(0), pid(9));
        let mut l = EscrowLedger::new();
        let total0 = fund_pot(&mut l, job, &[(submitter, 10_000), (executor, 10_000)]);

        let out = resolve_timeout(&mut l, &rp, job, 10_000, submitter, executor, 10_000);

        assert_eq!(out.submitter_refunded, 12_000, "budget 10_000 + 20% comp 2_000");
        assert_eq!(out.burned, 8_000, "80% of the slashed bond");
        assert_eq!(out.slashed, vec![(executor, 10_000)], "full bond logged slashed");
        assert_eq!(l.balance_of(&submitter), 12_000);
        assert_eq!(l.balance_of(&executor), 0, "executor lost the bond");
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed_for(&job), 0);
    }

    #[test]
    fn cancel_burns_2pct_refunds_98pct_and_conserves() {
        let rp = ResolutionParams::default();
        let job = [1u8; 32];
        let submitter = pid(0);
        let mut l = EscrowLedger::new();
        let total0 = fund_pot(&mut l, job, &[(submitter, 10_000)]);

        let out = resolve_cancel(&mut l, &rp, job, 10_000, submitter);

        assert_eq!(out.burned, 200, "2% of 10_000 burned");
        assert_eq!(out.submitter_refunded, 9_800, "98% refunded");
        assert_eq!(l.balance_of(&submitter), 9_800);
        assert_eq!(l.total_supply(), total0, "supply conserved");
        assert_eq!(l.escrowed_for(&job), 0, "pot drained");
    }

    /// `resolve_confirmed`, fed the committee the engine selects, must leave the IDENTICAL
    /// ledger end-state as driving the same all-honest job through `run_priced_job`. Pins
    /// the standalone resolver to the engine so they cannot silently diverge.
    #[test]
    fn resolve_confirmed_matches_run_priced_job_end_state() {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap();
        let e_bond = executor_bond_min(f, budget, &p).unwrap();
        let v_bond = verifier_bond_min(f, &p).unwrap();
        let (submitter, executor) = (pid(0), pid(9));
        let committee = [pid(10), pid(11), pid(12)]; // exactly k=3 ⇒ committee == whole pool

        // Path A: the full synchronous engine. run_priced_job escrows internally, so we
        // only credit + bind the job first (see escrow_ledger.rs happy_path).
        let job_a = JobId::derive(&[7; 32], &[9; 32], &submitter, 0);
        let mut la = EscrowLedger::new();
        la.credit(submitter, budget);
        la.credit(executor, e_bond);
        for c in &committee {
            la.credit(*c, v_bond);
        }
        let total0 = la.total_supply();
        la.for_job(job_a.0);
        let job = Job {
            id: job_a,
            submitter,
            spec: JobSpec { program_hash: [7; 32], input_hash: [9; 32] },
            budget,
        };
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: b"in",
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &committee,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let vm = IteratedHashVm { rounds: 10 };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);
        let (verdict, _) =
            run_priced_job(&mut la, &p, &inputs, f, &vm, &ByteEq, &stake, &mut rng).unwrap();
        assert!(matches!(verdict, Verdict::Confirmed { .. }), "all-honest run confirms");

        // Path B: the standalone resolver, fed the same committee, manually escrowed.
        let job_b = [0xBBu8; 32];
        let mut lb = EscrowLedger::new();
        lb.credit(submitter, budget);
        lb.credit(executor, e_bond);
        for c in &committee {
            lb.credit(*c, v_bond);
        }
        lb.for_job(job_b);
        lb.escrow(submitter, budget);
        lb.escrow(executor, e_bond);
        for c in &committee {
            lb.escrow(*c, v_bond);
        }
        let _ = resolve_confirmed(&mut lb, &p, job_b, budget, executor, e_bond, &committee, v_bond);

        // Identical end-state: every actor balance, supply, no stranded escrow.
        for who in [submitter, executor, committee[0], committee[1], committee[2]] {
            assert_eq!(la.balance_of(&who), lb.balance_of(&who), "balance mismatch");
        }
        assert_eq!(la.total_supply(), total0);
        assert_eq!(lb.total_supply(), total0);
        assert_eq!(la.total_escrowed(), 0);
        assert_eq!(lb.total_escrowed(), 0);
    }

    /// A resolver asked to move more than the funded pot holds panics (inherits
    /// EscrowLedger's checked_sub policy): a cancel claiming a 100 budget against a pot of
    /// only 50 over-draws on the refund.
    #[test]
    #[should_panic(expected = "pay exceeds job escrow")]
    fn resolver_overdraw_panics() {
        let rp = ResolutionParams::default();
        let job = [6u8; 32];
        let submitter = pid(0);
        let mut l = EscrowLedger::new();
        fund_pot(&mut l, job, &[(submitter, 50)]);
        resolve_cancel(&mut l, &rp, job, 100, submitter); // refund 98 > pot 50 → panic
    }
}
