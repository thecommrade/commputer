//! Conservation property test (spec §9) — the single most important test of the
//! settlement layer. For randomized budgets, bonds, and participant sets it drives
//! **every** terminal settlement branch (Tasks 8-10) and asserts the §9 identity:
//!
//!   budget + Σ all bonds_in
//!     = worker_paid + verifiers_paid + challenger_paid + panel_paid
//!       + burned + submitter_refunded + bonds_returned
//!
//! No mint: every payout/burn is sourced from the escrowed inflows. We assert this
//! three independent ways, because each catches a different failure:
//!   1. `Ledger::total_supply()` is invariant from before-escrow to after-settle —
//!      no unit was minted or destroyed outside `burn`.
//!   2. `Ledger::escrowed() == 0` after settlement — nothing is stranded in escrow
//!      (the exact failure mode the "burn the rounding remainder" rule prevents;
//!      `total_supply` alone cannot see it, since escrow is inside total_supply).
//!   3. The `SettlementOutcome` outflow fields sum to exactly the inflows (`budget`
//!      plus every escrowed bond) — the §9 ledger identity, field by field.
//!
//! WIRE-IN: this is a dev-only integration test under `tests/`; it exercises the
//! public settlement/trap API and needs no production wiring.

use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::Reveal;
use commputer_pouw::oracle::{ChainHooks, Ledger};
use commputer_pouw::params::GameParams;
use commputer_pouw::settlement::{
    settle_committee_disputed, settle_confirmed_sampled, settle_confirmed_unsampled,
    settle_disputed_via_challenge, settle_false_challenge, settle_noquorum_confirmed,
    settle_noquorum_disputed,
};
use commputer_pouw::trap::settle_trap;
use proptest::prelude::*;

/// Distinct participant id from a small index (keeps every actor in a scenario unique
/// so per-actor balances are unambiguous; the conservation identity itself is over
/// aggregate supply, so the exact ids do not matter — only their distinctness does).
fn pid(n: u8) -> ParticipantId {
    ParticipantId([n; 32])
}

/// Sum the §9 outflow fields of an outcome. Bonds_returned is already an aggregate of
/// every honest bond `pay`'d back; `slashed` is a *log* only (its value is realised in
/// `burned`/`challenger_paid`/`panel_paid`), so it is deliberately NOT summed here.
fn total_outflow(o: &commputer_pouw::job::SettlementOutcome) -> u64 {
    o.worker_paid
        + o.verifiers_paid
        + o.challenger_paid
        + o.panel_paid
        + o.burned
        + o.submitter_refunded
        + o.bonds_returned
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// All seven terminal settlement branches (the 6 game branches + the two NoQuorum
    /// variants are distinct fns) plus the trap branch. A `branch` selector picks one;
    /// every numeric input is randomized within its valid range. For each branch we
    /// escrow exactly the inflows the branch consumes, settle, and assert the three
    /// conservation properties above.
    #[test]
    fn every_settlement_branch_conserves_value(
        branch in 0u8..8,
        budget in 0u64..1_000_000,
        executor_bond in 0u64..1_000_000,
        challenger_bond in 0u64..1_000_000,
        // small bonds for the up-to-3 committee / panel verifiers in each role
        v_bonds in proptest::collection::vec(0u64..100_000, 0..4),
        p_bonds in proptest::collection::vec(0u64..100_000, 0..4),
        r_bonds in proptest::collection::vec(0u64..100_000, 0..4),
    ) {
        let p = GameParams::default();
        let mut l = Ledger::new();

        // Disjoint id ranges per role so no actor is double-counted.
        let submitter = pid(0);
        let executor  = pid(1);
        let challenger = pid(2);
        let worker = executor; // the executor is the worker on confirmed branches
        let committee: Vec<ParticipantId> = (10u8..14).map(pid).collect();         // honest/vindicated verifiers
        let panel:     Vec<ParticipantId> = (20u8..24).map(pid).collect();         // escalation panel
        let rejected:  Vec<ParticipantId> = (30u8..34).map(pid).collect();         // wrong-side verifiers

        // Helper: credit-then-escrow `amount` from `who` (an inflow into the game).
        // Returns the escrowed amount so callers can sum the inflows.
        macro_rules! put {
            ($who:expr, $amt:expr) => {{
                let a: u64 = $amt;
                l.credit($who, a);
                l.escrow($who, a);
                a
            }};
        }

        // Build the named verifier/panel slices to the length of their bond vectors.
        let committee_slice = &committee[..v_bonds.len().min(committee.len())];
        let panel_slice      = &panel[..p_bonds.len().min(panel.len())];
        let rejected_slice   = &rejected[..r_bonds.len().min(rejected.len())];
        let v_bonds = &v_bonds[..committee_slice.len()];
        let p_bonds = &p_bonds[..panel_slice.len()];
        let r_bonds = &r_bonds[..rejected_slice.len()];

        let mut inflow: u64 = 0;
        // total_supply is captured AFTER all inflows are escrowed (the "before-escrow to
        // after-settle" invariant means: from the moment the game holds the escrowed
        // value, to the moment settlement finishes). `credit` is external funding that
        // happens before the game; escrowing it does not change supply, so we snapshot
        // here, just before each branch settles. Each arm escrows its inflows, then calls
        // `snap!()` to bind total0, then settles. The binding is deferred (no dummy init)
        // so every code path assigns it exactly once — keeping the build warning-free.
        let total0: u64;
        macro_rules! snap {
            () => {{
                total0 = l.total_supply();
            }};
        }

        let out = match branch {
            0 => {
                // settle_confirmed_sampled: only the budget is consumed here (bonds are
                // returned by the *caller* in the real flow, so they are not escrowed
                // into this fn's accounting). Inflow = budget.
                inflow += put!(submitter, budget);
                snap!();
                settle_confirmed_sampled(&mut l, &p, budget, worker, committee_slice)
            }
            1 => {
                // settle_confirmed_unsampled: budget only.
                inflow += put!(submitter, budget);
                snap!();
                settle_confirmed_unsampled(&mut l, &p, budget, worker)
            }
            2 => {
                // settle_committee_disputed: budget (refunded) + executor bond (slashed).
                inflow += put!(submitter, budget);
                inflow += put!(executor, executor_bond);
                snap!();
                settle_committee_disputed(
                    &mut l, &p, budget, submitter, executor, executor_bond, committee_slice,
                )
            }
            3 => {
                // settle_disputed_via_challenge: budget + executor bond + challenger bond
                // + panel bonds (all escrowed; panel bonds returned).
                inflow += put!(submitter, budget);
                inflow += put!(executor, executor_bond);
                inflow += put!(challenger, challenger_bond);
                for (pp, &b) in panel_slice.iter().zip(p_bonds.iter()) {
                    inflow += put!(*pp, b);
                }
                snap!();
                settle_disputed_via_challenge(
                    &mut l, &p, budget, submitter, executor, executor_bond,
                    challenger, challenger_bond, panel_slice, p_bonds,
                )
            }
            4 => {
                // settle_false_challenge: budget + executor bond (returned to worker)
                // + challenger bond (slashed) + panel bonds (returned).
                inflow += put!(submitter, budget);
                inflow += put!(worker, executor_bond);
                inflow += put!(challenger, challenger_bond);
                for (pp, &b) in panel_slice.iter().zip(p_bonds.iter()) {
                    inflow += put!(*pp, b);
                }
                snap!();
                settle_false_challenge(
                    &mut l, &p, budget, worker, executor_bond,
                    challenger, challenger_bond, panel_slice, p_bonds,
                )
            }
            5 => {
                // settle_noquorum_confirmed: budget + executor bond (returned) +
                // vindicated verifier bonds (returned) + rejected verifier bonds (slashed)
                // + panel bonds (returned).
                inflow += put!(submitter, budget);
                inflow += put!(executor, executor_bond);
                for (vv, &b) in committee_slice.iter().zip(v_bonds.iter()) {
                    inflow += put!(*vv, b);
                }
                for (rr, &b) in rejected_slice.iter().zip(r_bonds.iter()) {
                    inflow += put!(*rr, b);
                }
                for (pp, &b) in panel_slice.iter().zip(p_bonds.iter()) {
                    inflow += put!(*pp, b);
                }
                snap!();
                settle_noquorum_confirmed(
                    &mut l, &p, budget, executor, executor_bond,
                    committee_slice, v_bonds, rejected_slice, r_bonds, panel_slice, p_bonds,
                )
            }
            6 => {
                // settle_noquorum_disputed: budget (refunded) + executor bond (slashed)
                // + honest verifier bonds (returned) + rejected verifier bonds (slashed)
                // + panel bonds (returned).
                inflow += put!(submitter, budget);
                inflow += put!(executor, executor_bond);
                for (vv, &b) in committee_slice.iter().zip(v_bonds.iter()) {
                    inflow += put!(*vv, b);
                }
                for (rr, &b) in rejected_slice.iter().zip(r_bonds.iter()) {
                    inflow += put!(*rr, b);
                }
                for (pp, &b) in panel_slice.iter().zip(p_bonds.iter()) {
                    inflow += put!(*pp, b);
                }
                snap!();
                settle_noquorum_disputed(
                    &mut l, &p, budget, submitter, executor, executor_bond,
                    committee_slice, v_bonds, rejected_slice, r_bonds, panel_slice, p_bonds,
                )
            }
            _ => {
                // Trap branch (synthetic; no budget). Inflow = only the slashed
                // rubber-stamper bonds (honest verifiers' bonds are escrowed too but are
                // NOT consumed by settle_trap — the caller returns them — so they are not
                // part of THIS branch's settlement accounting and stay in escrow).
                let planted = [8u8; 32];
                let truth = [5u8; 32];
                // Reuse the `rejected` ids as rubber-stampers (reveal the planted hash)
                // and `committee` ids as honest (reveal the truth).
                let mut reveals: Vec<Reveal> = Vec::new();
                let mut bond_table: std::collections::HashMap<ParticipantId, u64> =
                    std::collections::HashMap::new();
                for (rr, &b) in rejected_slice.iter().zip(r_bonds.iter()) {
                    inflow += put!(*rr, b); // rubber-stamper bond (will be slashed)
                    bond_table.insert(*rr, b);
                    reveals.push(Reveal { verifier: *rr, result_hash: planted, salt: [0; 32] });
                }
                for vv in committee_slice.iter() {
                    // honest verifiers reveal the truth; their bonds are the caller's to
                    // return and are intentionally left out of `inflow` and un-escrowed.
                    reveals.push(Reveal { verifier: *vv, result_hash: truth, salt: [0; 32] });
                }
                let bonds = |who: &ParticipantId| *bond_table.get(who).unwrap_or(&0);
                snap!();
                settle_trap(&mut l, &p, planted, truth, &reveals, &bonds)
            }
        };

        // (1) No mint: supply is exactly what it was before any escrow.
        prop_assert_eq!(
            l.total_supply(), total0,
            "branch {}: total_supply changed (mint/destroy outside burn)", branch
        );
        // (2) Nothing stranded in escrow after a complete settlement.
        prop_assert_eq!(
            l.escrowed(), 0,
            "branch {}: {} units stranded in escrow", branch, l.escrowed()
        );
        // (3) §9 identity: outflow fields sum to exactly the escrowed inflows.
        prop_assert_eq!(
            total_outflow(&out), inflow,
            "branch {}: outflow {} != inflow {}", branch, total_outflow(&out), inflow
        );
    }
}
