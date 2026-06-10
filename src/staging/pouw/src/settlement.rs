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

/// Return each `(holder, bond)` pair to its holder from escrow, summing the total
/// returned. Used to release the bonds of honest panelists / vindicated verifiers /
/// the innocent executor on the escalation branches. `holders` and `bonds` must be
/// the same length (each holder posted the corresponding bond into escrow).
fn return_bonds(l: &mut dyn ChainHooks, holders: &[ParticipantId], bonds: &[u64]) -> u64 {
    let mut total = 0;
    for (h, &b) in holders.iter().zip(bonds.iter()) {
        l.pay(*h, b);
        total += b;
    }
    total
}

// ---- Task 9: escalation settlement (spec §6.9 escalation outcomes) -----------------
//
// All four branches are loser-pays and fully escrow-funded: the budget `B`, the
// executor bond `Be`, the challenger bond `Bc` (challenge path only), the original
// committee verifier bonds (NoQuorum path only), and the panel verifier bonds are all
// already in escrow (the caller `escrow`'d them). Settlement only redistributes with
// `pay`/`burn`; slashed bonds are burned from escrow and recorded in
// `SettlementOutcome.slashed` as a log. Every rounding remainder is routed to `burn`
// so `Ledger::total_supply` is invariant and no escrow is stranded.

/// *(i) Challenge path — executor guilty (`Disputed`-via-challenge).* A challenger
/// disputed an unsampled, optimistically-accepted result and was right. The submitter
/// is refunded the full budget. The challenger gets its bond `Bc` back **plus** a
/// reward of `challenger_reward_bps · Be`; the honest panel splits
/// `escalation_reward_bps · Be`; the remainder of the slashed executor bond is burned.
/// All panel bonds are returned. The executor slash is recorded as a log only.
#[allow(clippy::too_many_arguments)]
pub fn settle_disputed_via_challenge(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
    challenger: ParticipantId,
    challenger_bond: u64,
    panel: &[ParticipantId],
    panel_bonds: &[u64],
) -> SettlementOutcome {
    // Refund the full budget to the submitter (they got no useful work).
    l.pay(submitter, budget);
    // Challenger: return its bond, then pay the reward from the slashed executor bond.
    let challenger_reward = bps(executor_bond, p.challenger_reward_bps);
    l.pay(challenger, challenger_bond);
    l.pay(challenger, challenger_reward);
    // Panel: split the escalation reward from the slashed executor bond.
    let panel_pool = bps(executor_bond, p.escalation_reward_bps);
    let panel_paid = pay_even(l, panel_pool, panel);
    // Burn whatever is left of the executor bond escrow (non-reward remainder + rounding).
    let burned = executor_bond - challenger_reward - panel_paid;
    l.burn(burned);
    // Return all panel bonds (honest re-executors).
    let bonds_returned = challenger_bond + return_bonds(l, panel, panel_bonds);
    SettlementOutcome {
        submitter_refunded: budget,
        challenger_paid: challenger_reward,
        panel_paid,
        burned,
        bonds_returned,
        slashed: vec![(executor, executor_bond)],
        ..Default::default()
    }
}

/// *(i) Challenge path — false challenge (`Confirmed`-via-challenge).* The challenger
/// was wrong; the executor's (unsampled) result stands. The worker settles 85/10/5 of
/// the budget — but with no committee on an unsampled job, the 10% verifier slice is
/// burned along with the 5% protocol slice. The challenger bond `Bc` is slashed: the
/// honest panel splits `escalation_reward_bps · Bc`, the remainder of `Bc` is burned.
/// The executor bond is returned. The submitter is NOT refunded (they got useful work).
#[allow(clippy::too_many_arguments)]
pub fn settle_false_challenge(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    worker: ParticipantId,
    executor_bond: u64,
    challenger: ParticipantId,
    challenger_bond: u64,
    panel: &[ParticipantId],
    panel_bonds: &[u64],
) -> SettlementOutcome {
    // Worker paid 85%; the verifier + protocol slices of the budget are burned.
    let worker_pay = bps(budget, p.worker_bps);
    l.pay(worker, worker_pay);
    let budget_burn = budget - worker_pay;
    // Panel reward from the slashed challenger bond.
    let panel_pool = bps(challenger_bond, p.escalation_reward_bps);
    let panel_paid = pay_even(l, panel_pool, panel);
    let challenger_burn = challenger_bond - panel_paid;
    let burned = budget_burn + challenger_burn;
    l.burn(burned);
    // Executor bond returned (the executor was innocent); panel bonds returned.
    l.pay(worker, executor_bond);
    let bonds_returned = executor_bond + return_bonds(l, panel, panel_bonds);
    SettlementOutcome {
        worker_paid: worker_pay,
        panel_paid,
        burned,
        bonds_returned,
        slashed: vec![(challenger, challenger_bond)],
        ..Default::default()
    }
}

/// *(ii) NoQuorum path — panel agrees with the executor (`Confirmed`).* A sampled
/// committee split; the panel re-executed and vindicated the executor. The worker
/// settles 85/10/5: the 85% to the worker, the 10% verifier slice split across the
/// vindicated original verifiers, the 5% protocol slice burned. Original verifiers who
/// revealed a rejected value are slashed; their bonds fund `escalation_reward_bps ·
/// slashed` to the panel, the remainder burned. The executor bond and all vindicated /
/// panel bonds are returned. There is no challenger bond on this (protocol-initiated)
/// path.
#[allow(clippy::too_many_arguments)]
pub fn settle_noquorum_confirmed(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    executor: ParticipantId,
    executor_bond: u64,
    vindicated_verifiers: &[ParticipantId],
    vindicated_bonds: &[u64],
    rejected_verifiers: &[ParticipantId],
    rejected_bonds: &[u64],
    panel: &[ParticipantId],
    panel_bonds: &[u64],
) -> SettlementOutcome {
    // Budget: 85% worker / 10% vindicated verifiers / 5% burn.
    let worker_pay = bps(budget, p.worker_bps);
    let verifier_pool = bps(budget, p.verifier_bps);
    l.pay(executor, worker_pay);
    let verifiers_paid = pay_even(l, verifier_pool, vindicated_verifiers);
    let budget_burn = budget - worker_pay - verifiers_paid;
    // Slashed wrong-side verifier bonds fund the panel reward; remainder burned.
    let total_slashed: u64 = rejected_bonds.iter().sum();
    let panel_pool = bps(total_slashed, p.escalation_reward_bps);
    let panel_paid = pay_even(l, panel_pool, panel);
    let slashed_burn = total_slashed - panel_paid;
    let burned = budget_burn + slashed_burn;
    l.burn(burned);
    // Return the executor bond, the vindicated verifiers' bonds, and the panel bonds.
    let bonds_returned = {
        l.pay(executor, executor_bond);
        executor_bond
            + return_bonds(l, vindicated_verifiers, vindicated_bonds)
            + return_bonds(l, panel, panel_bonds)
    };
    let slashed = rejected_verifiers
        .iter()
        .zip(rejected_bonds.iter())
        .map(|(v, &b)| (*v, b))
        .collect();
    SettlementOutcome {
        worker_paid: worker_pay,
        verifiers_paid,
        panel_paid,
        burned,
        bonds_returned,
        slashed,
        ..Default::default()
    }
}

/// *(ii) NoQuorum path — panel rejects the executor (`Disputed`).* A sampled committee
/// split; the panel re-executed and proved the executor wrong. The submitter is
/// refunded the full budget. The executor bond `Be` is slashed and split: the honest
/// original verifiers (who revealed the correct answer) and the panel share
/// `(challenger_reward_bps + escalation_reward_bps) · Be` — with no challenger, the
/// challenger-reward share accrues to the honest verifiers who surfaced the split — and
/// the remainder of `Be` is burned. Rejected-value original verifiers are also slashed
/// (burned). Honest verifiers' and panel bonds are returned.
#[allow(clippy::too_many_arguments)]
pub fn settle_noquorum_disputed(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    budget: u64,
    submitter: ParticipantId,
    executor: ParticipantId,
    executor_bond: u64,
    honest_verifiers: &[ParticipantId],
    honest_bonds: &[u64],
    rejected_verifiers: &[ParticipantId],
    rejected_bonds: &[u64],
    panel: &[ParticipantId],
    panel_bonds: &[u64],
) -> SettlementOutcome {
    // Refund the full budget to the submitter.
    l.pay(submitter, budget);
    // Split the slashed executor bond: challenger-reward share -> honest verifiers
    // (no challenger exists), escalation-reward share -> panel; remainder burned.
    let verifier_pool = bps(executor_bond, p.challenger_reward_bps);
    let panel_pool = bps(executor_bond, p.escalation_reward_bps);
    let verifiers_paid = pay_even(l, verifier_pool, honest_verifiers);
    let panel_paid = pay_even(l, panel_pool, panel);
    let executor_burn = executor_bond - verifiers_paid - panel_paid;
    // Rejected-value original verifiers are also slashed (their bonds burned).
    let rejected_slashed: u64 = rejected_bonds.iter().sum();
    let burned = executor_burn + rejected_slashed;
    l.burn(burned);
    // Return honest verifiers' and panel bonds.
    let bonds_returned =
        return_bonds(l, honest_verifiers, honest_bonds) + return_bonds(l, panel, panel_bonds);
    let mut slashed = vec![(executor, executor_bond)];
    slashed.extend(
        rejected_verifiers
            .iter()
            .zip(rejected_bonds.iter())
            .map(|(v, &b)| (*v, b)),
    );
    SettlementOutcome {
        submitter_refunded: budget,
        verifiers_paid,
        panel_paid,
        burned,
        bonds_returned,
        slashed,
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
        let burned0 = l.burned;
        let out = settle_confirmed_sampled(&mut l, &p, 100, worker, &[v1, v2]);
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 10);
        assert_eq!(out.burned, 5);
        assert_eq!(l.balance_of(&worker), 85);
        assert_eq!(l.total_supply(), total0); // no mint
        // Conservation, the non-vacuous way: the burn actually happened (the ledger's
        // burned counter moved by 5), and the escrow is fully drained — no value is
        // stranded in escrow. total_supply alone cannot catch a stranded escrow because
        // escrow is already inside total_supply; these two assertions can.
        assert_eq!(l.burned - burned0, 5);   // 5 units truly left circulation
        assert_eq!(l.escrowed(), 0);         // nothing stranded in escrow
    }

    #[test]
    fn confirmed_unsampled_burns_the_verifier_slice() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        l.credit(pid(0), 100); l.escrow(pid(0), 100);
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_confirmed_unsampled(&mut l, &p, 100, pid(1));
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.burned, 15); // 5% protocol + 10% unclaimed verifier slice
        assert_eq!(out.verifiers_paid, 0);
        assert_eq!(l.total_supply(), total0); // no mint
        assert_eq!(l.burned - burned0, 15);   // 15 units truly left circulation
        assert_eq!(l.escrowed(), 0);          // nothing stranded in escrow
    }

    #[test]
    fn committee_disputed_refunds_submitter_and_bounties_from_bond() {
        let p = GameParams::default(); // executor_bond 100, dispute_bounty 20%
        let mut l = Ledger::new();
        let (submitter, exec, v1) = (pid(0), pid(9), pid(1));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget escrowed
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond escrowed
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_committee_disputed(&mut l, &p, 100, submitter, exec, 100, &[v1]);
        assert_eq!(out.submitter_refunded, 100);
        assert_eq!(out.verifiers_paid, 20);   // 20% of the 100 bond
        assert_eq!(out.burned, 80);           // remaining bond burned
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.total_supply(), total0);
        // Both escrow pots (budget + executor bond) must end fully drained: 100 budget
        // refunded, bond split 20 bounty / 80 burned. Assert the burn truly happened and
        // no unit is stranded in either pot.
        assert_eq!(l.burned - burned0, 80);   // 80 units truly left circulation
        assert_eq!(l.escrowed(), 0);          // neither pot leaves stranded value
    }

    // ---- Task 9: escalation branches ------------------------------------------------

    /// (i) Challenge path, executor guilty: submitter refunded full B; the challenger
    /// gets Bc back + challenger_reward_bps·Be; the honest panel splits
    /// escalation_reward_bps·Be; remainder of Be burned. Panel bonds returned.
    #[test]
    fn disputed_via_challenge_pays_challenger_and_panel_from_executor_bond() {
        let p = GameParams::default(); // Be 100, Bc 50, challenger_reward 10%, escalation_reward 10%
        let mut l = Ledger::new();
        let (submitter, exec, challenger) = (pid(0), pid(9), pid(8));
        let (pa, pb) = (pid(1), pid(2));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget B=100
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond Be=100
        l.credit(challenger, 50); l.escrow(challenger, 50);  // challenger bond Bc=50
        l.credit(pa, 20); l.escrow(pa, 20);                  // panel bonds
        l.credit(pb, 20); l.escrow(pb, 20);
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_disputed_via_challenge(
            &mut l, &p, 100, submitter, exec, 100, challenger, 50, &[pa, pb], &[20, 20],
        );
        assert_eq!(out.submitter_refunded, 100);
        assert_eq!(out.challenger_paid, 10);   // 10% of Be (the reward; bond returned separately)
        assert_eq!(out.panel_paid, 10);        // 10% of Be split across the 2 panelists (5 each)
        assert_eq!(out.burned, 80);            // remainder of Be
        assert_eq!(out.bonds_returned, 50 + 40); // Bc back + both panel bonds
        // challenger ends with Bc (50) refunded + 10 reward = 60; panelists get bond + 5 each
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.balance_of(&challenger), 60);
        assert_eq!(l.balance_of(&pa), 25);
        assert_eq!(l.balance_of(&pb), 25);
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.burned - burned0, 80);
        assert_eq!(l.escrowed(), 0);
        assert_eq!(out.slashed, vec![(exec, 100)]);
    }

    /// (i) Challenge path, false challenge: loser = challenger. Worker settles 85/10/5
    /// (the 10% burned, no committee on an unsampled job); the honest panel splits
    /// escalation_reward_bps·Bc; remainder of Bc burned; executor bond returned.
    #[test]
    fn false_challenge_consumes_challenger_bond_and_pays_worker() {
        let p = GameParams::default(); // Bc 50, escalation_reward 10%
        let mut l = Ledger::new();
        let (submitter, worker, challenger) = (pid(0), pid(9), pid(8));
        let (pa, pb) = (pid(1), pid(2));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget B=100
        l.credit(worker, 100); l.escrow(worker, 100);        // executor (worker) bond Be=100
        l.credit(challenger, 50); l.escrow(challenger, 50);  // challenger bond Bc=50
        l.credit(pa, 20); l.escrow(pa, 20);
        l.credit(pb, 20); l.escrow(pb, 20);
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_false_challenge(
            &mut l, &p, 100, worker, 100, challenger, 50, &[pa, pb], &[20, 20],
        );
        assert_eq!(out.worker_paid, 85);       // 85% of B
        assert_eq!(out.panel_paid, 4);         // 10% of Bc = 5; split 2-ways = 4 (2 each)
        // burn = B-worker (15: 10% verifier slice + 5% protocol) + Bc remainder (50-4=46) = 61
        assert_eq!(out.burned, 61);
        assert_eq!(out.bonds_returned, 100 + 40); // executor bond back + both panel bonds
        assert_eq!(l.balance_of(&worker), 85 + 100); // worker pay + executor bond returned
        assert_eq!(l.balance_of(&submitter), 0);     // submitter NOT refunded; got useful work
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.burned - burned0, 61);
        assert_eq!(l.escrowed(), 0);
        assert_eq!(out.slashed, vec![(challenger, 50)]);
    }

    /// (ii) NoQuorum path, panel agrees with executor (Confirmed): worker 85/10/5;
    /// vindicated original verifiers get bonds back + split the 10%; rejected-value
    /// original verifiers slashed → fund escalation_reward_bps·slashed to the panel,
    /// remainder burned. Executor bond returned. No Bc.
    #[test]
    fn noquorum_confirmed_vindicates_correct_verifiers_slashes_wrong() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        let (submitter, exec) = (pid(0), pid(9));
        let (good1, good2, bad) = (pid(1), pid(2), pid(3)); // original verifiers
        let (pa, pb) = (pid(4), pid(5));                    // panel
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget B=100
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond Be=100
        l.credit(good1, 20); l.escrow(good1, 20);
        l.credit(good2, 20); l.escrow(good2, 20);
        l.credit(bad, 20); l.escrow(bad, 20);                // wrong-side verifier bond
        l.credit(pa, 20); l.escrow(pa, 20);
        l.credit(pb, 20); l.escrow(pb, 20);
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_noquorum_confirmed(
            &mut l, &p, 100, exec, 100,
            &[good1, good2], &[20, 20], // vindicated verifiers + their bonds
            &[bad], &[20],             // rejected verifiers + their (slashed) bonds
            &[pa, pb], &[20, 20],      // panel + bonds
        );
        assert_eq!(out.worker_paid, 85);       // 85% of B to executor (the worker)
        assert_eq!(out.verifiers_paid, 10);    // 10% of B split across vindicated verifiers (5 each)
        // panel reward = 10% of slashed(20) = 2; remainder of slashed(18) burned.
        assert_eq!(out.panel_paid, 2);
        // burn = 5% protocol (5) + slashed remainder (18) = 23
        assert_eq!(out.burned, 23);
        // bonds returned: executor 100 + 2 vindicated verifiers (40) + 2 panel (40) = 180
        assert_eq!(out.bonds_returned, 100 + 40 + 40);
        assert_eq!(l.balance_of(&exec), 85 + 100);   // worker pay + bond back
        assert_eq!(l.balance_of(&good1), 5 + 20);    // verifier slice + bond back
        assert_eq!(l.balance_of(&bad), 0);           // slashed
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.burned - burned0, 23);
        assert_eq!(l.escrowed(), 0);
        assert_eq!(out.slashed, vec![(bad, 20)]);
    }

    /// (ii) NoQuorum path, panel rejects executor (Disputed): submitter refunded full B;
    /// Be slashed → honest original verifiers + panel split
    /// (challenger_reward_bps + escalation_reward_bps)·Be (with no challenger, the
    /// challenger-reward share accrues to the honest verifiers who surfaced the split),
    /// remainder of Be burned; rejected-value original verifiers also slashed (burned).
    #[test]
    fn noquorum_disputed_refunds_submitter_splits_executor_bond() {
        let p = GameParams::default(); // challenger_reward 10% + escalation_reward 10%
        let mut l = Ledger::new();
        let (submitter, exec) = (pid(0), pid(9));
        let (good1, good2, bad) = (pid(1), pid(2), pid(3));
        let (pa, pb) = (pid(4), pid(5));
        l.credit(submitter, 100); l.escrow(submitter, 100);  // budget B=100
        l.credit(exec, 100); l.escrow(exec, 100);            // executor bond Be=100
        l.credit(good1, 20); l.escrow(good1, 20);
        l.credit(good2, 20); l.escrow(good2, 20);
        l.credit(bad, 20); l.escrow(bad, 20);                // rejected verifier
        l.credit(pa, 20); l.escrow(pa, 20);
        l.credit(pb, 20); l.escrow(pb, 20);
        let total0 = l.total_supply();
        let burned0 = l.burned;
        let out = settle_noquorum_disputed(
            &mut l, &p, 100, submitter, exec, 100,
            &[good1, good2], &[20, 20], // honest verifiers (revealed correct) + bonds
            &[bad], &[20],             // rejected verifiers + slashed bonds
            &[pa, pb], &[20, 20],      // panel + bonds
        );
        assert_eq!(out.submitter_refunded, 100);
        // challenger-reward share (10% of Be = 10) -> honest verifiers; escalation share
        // (10% of Be = 10) -> panel. verifiers_paid = 10 (5 each), panel_paid = 10 (5 each).
        assert_eq!(out.verifiers_paid, 10);
        assert_eq!(out.panel_paid, 10);
        // burn = Be remainder (100 - 10 - 10 = 80) + slashed rejected verifier bond (20) = 100
        assert_eq!(out.burned, 100);
        // bonds returned: 2 honest verifiers (40) + 2 panel (40) = 80
        assert_eq!(out.bonds_returned, 40 + 40);
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.balance_of(&good1), 5 + 20);   // share + bond back
        assert_eq!(l.balance_of(&bad), 0);          // slashed
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.burned - burned0, 100);
        assert_eq!(l.escrowed(), 0);
        assert_eq!(out.slashed, vec![(exec, 100), (bad, 20)]);
    }
}
