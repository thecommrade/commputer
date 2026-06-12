//! Escalation panel resolution (spec §6.9, escalation outcomes).
//!
//! Two distinct triggers, both resolved by a `k_escalate` re-execution panel that
//! returns a binding quorum verdict:
//!
//! * **Challenge** — a staked challenger disputed an *unsampled, optimistically-accepted*
//!   result. The panel verdict decides: `Disputed` ⇒ the challenger was right
//!   ([`settlement::settle_disputed_via_challenge`]); `Confirmed` ⇒ a false challenge
//!   ([`settlement::settle_false_challenge`]).
//! * **NoQuorum** — a *sampled* committee split, so the protocol re-runs it with the
//!   larger panel (no challenger, no `Bc`). The panel verdict decides: `Confirmed` ⇒ the
//!   executor was vindicated ([`settlement::settle_noquorum_confirmed`]); `Disputed` ⇒
//!   the executor was rejected ([`settlement::settle_noquorum_disputed`]). The original
//!   committee verifiers are partitioned by the panel-decided value into vindicated
//!   (their bond is returned) and rejected (their bond is slashed).
//!
//! This module is a thin dispatcher: it selects the panel, runs [`compute_verdict`],
//! partitions the original committee where relevant, and calls the matching settlement
//! function. All money movement lives in [`crate::settlement`].

use crate::committee::select_committee;
use crate::ids::ParticipantId;
use crate::job::{Reveal, SettlementOutcome, Verdict};
use crate::oracle::{ChainHooks, EquivalenceOracle};
use crate::params::GameParams;
use crate::settlement::{
    settle_disputed_via_challenge, settle_false_challenge, settle_noquorum_confirmed,
    settle_noquorum_disputed,
};
use crate::verdict::compute_verdict;

/// What triggered the escalation. Carries only the trigger-specific inputs; the shared
/// inputs (budget, executor, executor bond, panel reveals) live in [`Escalation`].
pub enum Trigger<'a> {
    /// A challenger disputed an unsampled, optimistically-accepted result.
    Challenge {
        submitter: ParticipantId,
        challenger: ParticipantId,
        challenger_bond: u64,
    },
    /// A sampled committee split; the protocol re-runs it on the panel. Carries the
    /// original committee's reveals and the bond each member posted (same order), so the
    /// panel-decided value can partition them into vindicated / rejected.
    NoQuorum {
        submitter: ParticipantId,
        committee_reveals: &'a [Reveal],
        committee_bonds: &'a [u64],
    },
}

/// One escalation, resolved end-to-end. `panel_reveals` are the re-executors' revealed
/// result hashes (the engine supplies these); each panelist posted `panel_bond` of bond
/// in escrow. The panel is selected deterministically from `candidates` (stake-weighted,
/// excluding the executor) and the verdict is taken at the `k_escalate` quorum.
pub struct Escalation<'a> {
    pub seed: [u8; 32],
    pub candidates: &'a [ParticipantId],
    pub budget: u64,
    pub executor: ParticipantId,
    pub executor_hash: [u8; 32],
    pub executor_bond: u64,
    pub panel_reveals: &'a [Reveal],
    pub panel_bond: u64,
}

/// Resolve an escalation: select the panel, take its binding verdict, and dispatch to
/// the matching settlement branch. Returns the panel's [`Verdict`] alongside the
/// [`SettlementOutcome`]. `stake_of` weights panel selection; `eq` decides equivalence.
pub fn resolve(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    esc: &Escalation,
    trigger: Trigger,
    eq: &dyn EquivalenceOracle,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
) -> (Verdict, SettlementOutcome) {
    let panel = select_committee(
        &esc.seed,
        esc.candidates,
        &esc.executor,
        p.k_escalate,
        stake_of,
    );
    let panel_bonds: Vec<u64> = vec![esc.panel_bond; panel.len()];
    let quorum = p.quorum(panel.len());
    let verdict = compute_verdict(esc.panel_reveals, &esc.executor_hash, quorum, eq);

    let outcome = match trigger {
        Trigger::Challenge {
            submitter,
            challenger,
            challenger_bond,
        } => match verdict {
            // Panel rejects the executor: the challenge was correct.
            Verdict::Disputed { .. } => settle_disputed_via_challenge(
                l,
                p,
                esc.budget,
                submitter,
                esc.executor,
                esc.executor_bond,
                challenger,
                challenger_bond,
                &panel,
                &panel_bonds,
            ),
            // Panel agrees with the executor (or split): the challenge was false.
            Verdict::Confirmed { .. } | Verdict::NoQuorum => settle_false_challenge(
                l,
                p,
                esc.budget,
                esc.executor,
                esc.executor_bond,
                challenger,
                challenger_bond,
                &panel,
                &panel_bonds,
            ),
        },
        Trigger::NoQuorum {
            submitter,
            committee_reveals,
            committee_bonds,
        } => match verdict {
            // Panel vindicates the executor: original verifiers split by the worker value.
            Verdict::Confirmed { result_hash } => {
                let (vind, vind_bonds, rej, rej_bonds) =
                    partition_committee(committee_reveals, committee_bonds, &result_hash, eq);
                settle_noquorum_confirmed(
                    l,
                    p,
                    esc.budget,
                    esc.executor,
                    esc.executor_bond,
                    &vind,
                    &vind_bonds,
                    &rej,
                    &rej_bonds,
                    &panel,
                    &panel_bonds,
                )
            }
            // Panel rejects the executor: honest verifiers split by the correct value.
            Verdict::Disputed { correct_hash } => {
                let (honest, honest_bonds, rej, rej_bonds) =
                    partition_committee(committee_reveals, committee_bonds, &correct_hash, eq);
                settle_noquorum_disputed(
                    l,
                    p,
                    esc.budget,
                    submitter,
                    esc.executor,
                    esc.executor_bond,
                    &honest,
                    &honest_bonds,
                    &rej,
                    &rej_bonds,
                    &panel,
                    &panel_bonds,
                )
            }
            // The larger panel also failed to reach quorum: fall back to refunding the
            // submitter and burning the executor bond with no reward recipients, so no
            // value is minted or stranded. (Bounded escalation depth — spec §11.)
            Verdict::NoQuorum => settle_noquorum_disputed(
                l,
                p,
                esc.budget,
                submitter,
                esc.executor,
                esc.executor_bond,
                &[],
                &[],
                committee_reveals
                    .iter()
                    .map(|r| r.verifier)
                    .collect::<Vec<_>>()
                    .as_slice(),
                committee_bonds,
                &panel,
                &panel_bonds,
            ),
        },
    };
    (verdict, outcome)
}

/// Split the original committee into the members whose revealed value matches `decided`
/// (the panel-vindicated value) and those whose value was rejected, preserving each
/// member's posted bond. Returns `(matching, matching_bonds, rejected, rejected_bonds)`.
fn partition_committee(
    reveals: &[Reveal],
    bonds: &[u64],
    decided: &[u8; 32],
    eq: &dyn EquivalenceOracle,
) -> (Vec<ParticipantId>, Vec<u64>, Vec<ParticipantId>, Vec<u64>) {
    let mut matching = Vec::new();
    let mut matching_bonds = Vec::new();
    let mut rejected = Vec::new();
    let mut rejected_bonds = Vec::new();
    for (r, &b) in reveals.iter().zip(bonds.iter()) {
        if eq.equiv(&r.result_hash, decided) {
            matching.push(r.verifier);
            matching_bonds.push(b);
        } else {
            rejected.push(r.verifier);
            rejected_bonds.push(b);
        }
    }
    (matching, matching_bonds, rejected, rejected_bonds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ParticipantId;
    use crate::oracle::{ByteEq, Ledger};
    use crate::params::GameParams;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }
    fn rv(n: u8, h: u8) -> Reveal {
        Reveal {
            verifier: pid(n),
            result_hash: [h; 32],
            salt: [0; 32],
        }
    }

    /// A correct challenge against a wrong unsampled executor: the panel re-executes,
    /// finds the executor wrong (`Disputed`), and the disputed-via-challenge branch runs.
    /// Conservation must hold across the whole escrow set.
    #[test]
    fn resolve_challenge_executor_guilty_routes_to_disputed_and_conserves() {
        let p = GameParams::default(); // k_escalate = 7
        let mut l = Ledger::new();
        let submitter = pid(0);
        let executor = pid(9);
        let challenger = pid(8);
        // candidate pool large enough for a 7-member panel (exclude the executor).
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();
        // Escrow everything the branch will move.
        l.credit(submitter, 100);
        l.escrow(submitter, 100); // budget
        l.credit(executor, 100);
        l.escrow(executor, 100); // executor bond
        l.credit(challenger, 50);
        l.escrow(challenger, 50); // challenger bond
        // Pre-select the panel to know who must post bonds; selection is deterministic.
        let stake = |_: &ParticipantId| 1u64;
        let panel = select_committee(&[7; 32], &candidates, &executor, p.k_escalate, &stake);
        for m in &panel {
            l.credit(*m, 20);
            l.escrow(*m, 20); // each panelist's verifier bond
        }
        // Panel unanimously re-executes the *correct* value (7), executor claimed 9.
        let panel_reveals: Vec<Reveal> =
            panel.iter().map(|m| Reveal { verifier: *m, result_hash: [7; 32], salt: [0; 32] }).collect();
        let total0 = l.total_supply();

        let esc = Escalation {
            seed: [7; 32],
            candidates: &candidates,
            budget: 100,
            executor,
            executor_hash: [9; 32],
            executor_bond: 100,
            panel_reveals: &panel_reveals,
            panel_bond: 20,
        };
        let (verdict, out) = resolve(
            &mut l,
            &p,
            &esc,
            Trigger::Challenge { submitter, challenger, challenger_bond: 50 },
            &ByteEq,
            &stake,
        );
        assert_eq!(verdict, Verdict::Disputed { correct_hash: [7; 32] });
        assert_eq!(out.submitter_refunded, 100);
        assert_eq!(out.challenger_paid, 10); // 10% of Be
        assert_eq!(out.slashed, vec![(executor, 100)]);
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.total_supply(), total0); // no mint
        assert_eq!(l.escrowed(), 0); // nothing stranded
    }

    /// A sampled committee split → NoQuorum; the panel vindicates the executor. The
    /// original verifier who agreed with the panel value is paid; the one who didn't is
    /// slashed. Conservation holds.
    #[test]
    fn resolve_noquorum_panel_confirms_executor_and_conserves() {
        let p = GameParams::default();
        let mut l = Ledger::new();
        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();
        l.credit(submitter, 100);
        l.escrow(submitter, 100);
        l.credit(executor, 100);
        l.escrow(executor, 100);
        // Original committee split: good agreed with executor (9), bad said 5.
        let good = pid(1);
        let bad = pid(2);
        let committee_reveals = vec![rv(1, 9), rv(2, 5)];
        let committee_bonds = vec![20u64, 20u64];
        l.credit(good, 20);
        l.escrow(good, 20);
        l.credit(bad, 20);
        l.escrow(bad, 20);
        let stake = |_: &ParticipantId| 1u64;
        let panel = select_committee(&[3; 32], &candidates, &executor, p.k_escalate, &stake);
        for m in &panel {
            l.credit(*m, 20);
            l.escrow(*m, 20);
        }
        // Panel unanimously confirms the executor's value (9).
        let panel_reveals: Vec<Reveal> =
            panel.iter().map(|m| Reveal { verifier: *m, result_hash: [9; 32], salt: [0; 32] }).collect();
        let total0 = l.total_supply();

        let esc = Escalation {
            seed: [3; 32],
            candidates: &candidates,
            budget: 100,
            executor,
            executor_hash: [9; 32],
            executor_bond: 100,
            panel_reveals: &panel_reveals,
            panel_bond: 20,
        };
        let (verdict, out) = resolve(
            &mut l,
            &p,
            &esc,
            Trigger::NoQuorum {
                submitter,
                committee_reveals: &committee_reveals,
                committee_bonds: &committee_bonds,
            },
            &ByteEq,
            &stake,
        );
        assert_eq!(verdict, Verdict::Confirmed { result_hash: [9; 32] });
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 10); // only `good` is vindicated → gets the whole 10% pool
        assert_eq!(out.slashed, vec![(bad, 20)]);
        assert_eq!(l.balance_of(&executor), 85 + 100); // worker pay + bond back
        assert_eq!(l.balance_of(&bad), 0); // slashed
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed(), 0);
    }
}
