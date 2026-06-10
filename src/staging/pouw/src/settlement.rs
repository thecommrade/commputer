//! The terminal settlement branches (spec §6.9), integer basis-point math.
//!
//! This task (Task 8) covers the three non-escalation branches:
//! `Confirmed` (sampled), `Confirmed` (unsampled), and committee `Disputed`.
//! Task 9 adds escalation; Task 10 adds traps.
//!
//! Settlement discipline: bonds and the budget are posted via `ChainHooks::escrow`
//! by the *caller*. Here we move value ONLY with `pay`/`burn` (from escrow); we never
//! call `ChainHooks::slash` on an escrowed bond. Slashed amounts are recorded in
//! `SettlementOutcome.slashed` as a log — the value itself is burned. Every rounding
//! remainder is routed to `burn` so no unit is ever minted or stranded, which keeps
//! `Ledger::total_supply` invariant on every branch.

use crate::ids::ParticipantId;
use crate::job::SettlementOutcome;
use crate::oracle::ChainHooks;
use crate::params::GameParams;

/// `floor(amount * bps / 10_000)` in u128 space, so no overflow on u64 inputs.
pub fn bps(amount: u64, bps: u32) -> u64 {
    (amount as u128 * bps as u128 / 10_000) as u64
}

/// Pay `pool` evenly across `recipients` (from escrow), returning the total actually
/// paid out. Any indivisible remainder is left in escrow for the caller to burn — this
/// is what keeps `total_supply` exact. With an empty recipient list, nothing is paid.
fn pay_even(l: &mut dyn ChainHooks, pool: u64, recipients: &[ParticipantId]) -> u64 {
    let n = recipients.len() as u64;
    if n == 0 {
        return 0;
    }
    let each = pool / n;
    if each == 0 {
        return 0;
    }
    for r in recipients {
        l.pay(*r, each);
    }
    each * n
}

/// `Confirmed`, sampled committee (spec §6.9): the budget splits 85% worker / 10%
/// verifiers / 5% protocol burn. The verifier pool is shared evenly across the committee;
/// whatever is left in the budget escrow after paying the worker and the verifiers —
/// the protocol burn slice plus any rounding remainder — is burned. All bonds are
/// returned by the caller; nothing is slashed.
pub fn settle_confirmed_sampled(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    worker: ParticipantId,
    verifiers: &[ParticipantId],
) -> SettlementOutcome {
    let worker_pay = bps(budget, p.worker_bps);
    let verifier_pool = bps(budget, p.verifier_bps);
    l.pay(worker, worker_pay);
    let verifiers_paid = pay_even(l, verifier_pool, verifiers);
    // Burn everything left in the budget escrow: the 5% protocol slice plus any
    // rounding remainder from the worker/verifier shares (global rounding rule).
    let burned = budget - worker_pay - verifiers_paid;
    l.burn(burned);
    SettlementOutcome {
        worker_paid: worker_pay,
        verifiers_paid,
        burned,
        bonds_returned: 0,
        ..Default::default()
    }
}

/// `Confirmed`, unsampled job that survived its challenge window (spec §6.9): there
/// was no committee, so the 10% verifier slice has no recipient and is burned along
/// with the 5% protocol slice — the worker is paid 85% and the remaining 15% (plus any
/// rounding remainder) is burned. All bonds are returned by the caller.
pub fn settle_confirmed_unsampled(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    worker: ParticipantId,
) -> SettlementOutcome {
    let worker_pay = bps(budget, p.worker_bps);
    l.pay(worker, worker_pay);
    // Burn the entire remainder of the budget escrow (verifier slice + protocol slice
    // + rounding): nobody is paid for verification that did not happen.
    let burned = budget - worker_pay;
    l.burn(burned);
    SettlementOutcome {
        worker_paid: worker_pay,
        verifiers_paid: 0,
        burned,
        bonds_returned: 0,
        ..Default::default()
    }
}

/// `Disputed` by a sampled committee (spec §6.9): the executor was proven wrong. The
/// submitter is refunded the full budget (they got no useful work). The executor bond
/// is slashed and split — honest verifiers share a catch bounty of
/// `dispute_bounty_bps · executor_bond`; the remainder of the bond (plus any rounding
/// remainder) is burned. The slash is recorded in `slashed` as a log only; the bond is
/// already in escrow, so we move it with `pay`/`burn` and never call `ChainHooks::slash`.
pub fn settle_committee_disputed(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
    honest_verifiers: &[ParticipantId],
) -> SettlementOutcome {
    // Refund the full budget to the submitter (from escrow).
    l.pay(submitter, budget);
    // Catch bounty to honest verifiers, from the slashed executor bond.
    let bounty_pool = bps(executor_bond, p.dispute_bounty_bps);
    let verifiers_paid = pay_even(l, bounty_pool, honest_verifiers);
    // Burn whatever is left of the executor bond escrow: the non-bounty remainder
    // plus any rounding remainder from the even split.
    let burned = executor_bond - verifiers_paid;
    l.burn(burned);
    SettlementOutcome {
        verifiers_paid,
        burned,
        submitter_refunded: budget,
        bonds_returned: 0,
        slashed: vec![(executor, executor_bond)],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::oracle::Ledger;
    use crate::params::GameParams;
    fn pid(n: u8) -> ParticipantId { ParticipantId([n; 32]) }

    #[test]
    fn confirmed_sampled_split_85_10_5() {
        let p = GameParams::default();
        let (worker, v1, v2) = (pid(1), pid(2), pid(3));
        let mut l = Ledger::new();
        // submitter funded the escrow; here we just escrow 100 directly for the test.
        l.credit(pid(0), 100); l.escrow(pid(0), 100);
        let total0 = l.total_supply();
        let out = settle_confirmed_sampled(&mut l, &p, 100, worker, &[v1, v2]);
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 10);
        assert_eq!(out.burned, 5);
        assert_eq!(l.balance_of(&worker), 85);
        assert_eq!(l.total_supply(), total0); // no mint
    }

    #[test]
    fn confirmed_unsampled_burns_the_verifier_slice() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        l.credit(pid(0), 100); l.escrow(pid(0), 100);
        let out = settle_confirmed_unsampled(&mut l, &p, 100, pid(1));
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.burned, 15); // 5% protocol + 10% unclaimed verifier slice
        assert_eq!(out.verifiers_paid, 0);
    }

    #[test]
    fn committee_disputed_refunds_submitter_and_bounties_from_bond() {
        let p = GameParams::default(); // executor_bond 100, dispute_bounty 20%
        let mut l = Ledger::new();
        let (submitter, exec, v1) = (pid(0), pid(9), pid(1));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget escrowed
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond escrowed
        let total0 = l.total_supply();
        let out = settle_committee_disputed(&mut l, &p, 100, submitter, exec, 100, &[v1]);
        assert_eq!(out.submitter_refunded, 100);
        assert_eq!(out.verifiers_paid, 20);   // 20% of the 100 bond
        assert_eq!(out.burned, 80);           // remaining bond burned
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.total_supply(), total0);
    }
}
