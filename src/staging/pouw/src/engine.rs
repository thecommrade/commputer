//! Engine (spec §6) — the one place that knows the full sequence of a job's life.
//!
//! `run_job` is a thin orchestrator: it wires the phases (escrow + bond → execute →
//! sample? → commit-reveal → verdict → escalate on NoQuorum/challenge → settle) by
//! calling the prior modules. It holds **no money logic** — every unit moves through
//! [`crate::settlement`] / [`crate::escalation`] / [`crate::trap`]. Randomness (the
//! sample draw, the trap draw, verifier salts) comes from a caller-supplied seeded RNG,
//! so a run is fully reproducible from `(inputs, seed)`.
//!
//! WIRE-IN: this is the prototype driver. A real node replaces the `&mut dyn ChainHooks`
//! with a chain-state adapter, the [`ExecutionOracle`] with a WASM runtime, and the
//! modeled `seed`/RNG with a block-hash / anchor VRF — the orchestration is unchanged.

use crate::committee::select_committee;
use crate::commit_reveal::make_commitment;
use crate::escalation::{Escalation, Trigger, resolve as resolve_escalation};
use crate::ids::ParticipantId;
use crate::job::{Job, Reveal, SettlementOutcome, Verdict};
use crate::oracle::{ChainHooks, EquivalenceOracle, ExecutionOracle};
use crate::params::GameParams;
use crate::settlement::{
    settle_committee_disputed, settle_confirmed_sampled, settle_confirmed_unsampled,
};
use rand::Rng;

/// The executor's claim strategy: given the true result hash, return the hash the
/// executor actually claims. Honest ⇒ identity; a cheater returns something else.
pub type ClaimFn<'a> = dyn Fn(&[u8; 32]) -> [u8; 32] + 'a;

/// A verifier's reveal strategy: given `(verifier, true result hash, executor's claimed
/// hash)`, return the hash the verifier reveals. Honest ⇒ true hash; rubber-stamp ⇒
/// the executor's claimed hash.
pub type RevealFn<'a> = dyn Fn(&ParticipantId, &[u8; 32], &[u8; 32]) -> [u8; 32] + 'a;

/// The challenge strategy for an *unsampled, optimistically-accepted* result (spec §6.9
/// challenge path). Given `(true result hash, executor's claimed hash)`, return
/// `Some(challenger)` if a staked actor steps forward to dispute the claim during the
/// challenge window, or `None` if the claim is left to settle optimistically. A rational
/// challenger disputes exactly when it can see the executor's claim is wrong; an absent
/// or apathetic challenger returns `None`. Supplied as data so the engine never decides
/// *whether* to challenge — only wires the consequence.
pub type ChallengeFn<'a> = dyn Fn(&[u8; 32], &[u8; 32]) -> Option<ParticipantId> + 'a;

/// Everything `run_job` needs that is not the chain/oracle wiring or the params.
///
/// The behavioural model is supplied as data, so a test (or the simulation) controls
/// exactly what each actor reveals. The engine never invents a result: it asks the
/// [`ExecutionOracle`] for the true output and then maps each participant through the
/// caller-supplied closures to get the hash they actually claim/reveal.
pub struct JobInputs<'a> {
    /// The job, with its escrowed-by-`run_job` budget.
    pub job: Job,
    /// The raw input bytes the oracle runs on (its hash is committed in `job.spec`).
    pub input: &'a [u8],
    /// Who executes the job and how much bond they post.
    pub executor: ParticipantId,
    pub executor_bond: u64,
    /// The hash the executor *claims*, given the true result hash. An honest executor
    /// returns the true hash; a cheater returns something else. Lets the test/sim drive
    /// the executor's strategy without the engine knowing about strategies.
    pub executor_claim: &'a ClaimFn<'a>,
    /// The candidate verifier pool the committee/panel is drawn from (stake-weighted).
    pub candidates: &'a [ParticipantId],
    /// Each selected verifier's posted bond (uniform `verifier_bond` in the prototype).
    pub verifier_bond: u64,
    /// The hash a given verifier *reveals*, as a function of (verifier, true result hash,
    /// executor's claimed hash). Honest ⇒ true hash; rubber-stamp ⇒ executor's claim.
    pub verifier_reveal: &'a RevealFn<'a>,
    /// Whether a challenger disputes an *unsampled* (optimistically-accepted) result, and
    /// who. `Some(challenger)` forces the challenge-escalation path; `None` lets the claim
    /// settle optimistically (Confirmed-unsampled). The challenger posts `challenger_bond`.
    pub challenge: &'a ChallengeFn<'a>,
    /// The bond a challenger posts to dispute an unsampled result (uniform in the
    /// prototype; `params::challenger_bond`). Only escrowed when `challenge` returns `Some`.
    pub challenger_bond: u64,
}

/// Hash a 32-byte result into the 32-byte `result_hash` the game compares on. The toy
/// VM already returns a 32-byte digest, but we re-hash to a fixed array so any oracle
/// output length collapses to the comparison domain deterministically.
fn result_hash(output: &[u8]) -> [u8; 32] {
    crate::ids::hash_parts(&[output])
}

/// Drive ONE job through the whole game and settle it. Returns the binding
/// [`Verdict`] and the [`SettlementOutcome`] for inspection. `run_job` escrows the
/// budget and every posted bond itself (from each actor's balance), so the caller only
/// needs to have `credit`ed the actors beforehand. `stake_of` weights committee
/// selection; `eq` decides result equivalence; `rng` supplies the sample/trap/salt draws.
#[allow(clippy::too_many_arguments)]
pub fn run_job(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    inputs: &JobInputs,
    exec_oracle: &dyn ExecutionOracle,
    eq: &dyn EquivalenceOracle,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
    rng: &mut dyn rand::RngCore,
) -> (Verdict, SettlementOutcome) {
    let job = &inputs.job;

    // --- Phase 1: Submit. Escrow the budget and the executor bond. ---
    l.escrow(job.submitter, job.budget);
    l.escrow(inputs.executor, inputs.executor_bond);

    // --- Phase 2: Execute. The executor runs the oracle and fixes its claim BEFORE the
    // committee is known (the seed below is drawn after). ---
    let true_output = exec_oracle.run(&job.spec, inputs.input);
    let true_hash = result_hash(&true_output);
    let executor_hash = (inputs.executor_claim)(&true_hash);

    // --- Phase 3: Sample. Draw whether this job is proactively verified. ---
    let sampled = rng.gen_range(0..10_000u32) < p.sample_rate_bps;

    if !sampled {
        // --- Unsampled: optimistically accepted, but it enters a challenge window in
        // which any staked actor may force an escalation (spec §6.9 challenge path). Ask
        // the challenge strategy whether a challenger steps forward. ---
        match (inputs.challenge)(&true_hash, &executor_hash) {
            None => {
                // No challenge: the claim survives its window and settles as
                // Confirmed-unsampled (85% worker / 15% burn — no committee existed).
                let outcome = settle_confirmed_unsampled(l, p, job.budget, inputs.executor);
                (Verdict::Confirmed { result_hash: executor_hash }, outcome)
            }
            Some(challenger) => {
                // A challenger disputes the optimistic acceptance. Escrow its bond, then
                // resolve on the re-execution panel via the challenge trigger. The panel
                // re-executes honestly (it is the protocol's own re-execution), so its
                // reveals are the true hash; the panel verdict decides loser-pays.
                resolve_challenge(l, p, inputs, challenger, true_hash, executor_hash, stake_of, eq, rng)
            }
        }
    } else {
        run_sampled(l, p, inputs, eq, stake_of, rng, true_hash, executor_hash)
    }
}

/// Wire the challenge-escalation phase for an unsampled, optimistically-accepted result
/// (spec §6.9 challenge path). Escrows the challenger bond, selects the re-execution
/// panel (which re-executes honestly ⇒ reveals the true hash), and dispatches through
/// [`resolve_escalation`] with [`Trigger::Challenge`]. The panel verdict decides:
/// `Disputed` ⇒ the challenger was right; `Confirmed` ⇒ a false challenge. Holds no money
/// logic itself — every unit moves through [`crate::settlement`] via the escalation call.
#[allow(clippy::too_many_arguments)]
fn resolve_challenge(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    inputs: &JobInputs,
    challenger: ParticipantId,
    true_hash: [u8; 32],
    executor_hash: [u8; 32],
    stake_of: &dyn Fn(&ParticipantId) -> u64,
    eq: &dyn EquivalenceOracle,
    rng: &mut dyn rand::RngCore,
) -> (Verdict, SettlementOutcome) {
    let job = &inputs.job;
    // The challenger stakes its bond to dispute the optimistic acceptance.
    l.escrow(challenger, inputs.challenger_bond);

    // Pre-select the panel so its bonds are escrowed before resolving.
    let panel_seed = draw_seed(rng);
    let panel = select_committee(
        &panel_seed,
        inputs.candidates,
        &inputs.executor,
        p.k_escalate,
        stake_of,
    );
    for m in &panel {
        l.escrow(*m, inputs.verifier_bond);
    }
    // The panel re-executes honestly: every panelist reveals the true hash.
    let panel_reveals: Vec<Reveal> = panel
        .iter()
        .map(|m| Reveal { verifier: *m, result_hash: true_hash, salt: [0; 32] })
        .collect();

    let esc = Escalation {
        seed: panel_seed,
        candidates: inputs.candidates,
        budget: job.budget,
        executor: inputs.executor,
        executor_hash,
        executor_bond: inputs.executor_bond,
        panel_reveals: &panel_reveals,
        panel_bond: inputs.verifier_bond,
    };
    resolve_escalation(
        l,
        p,
        &esc,
        Trigger::Challenge {
            submitter: job.submitter,
            challenger,
            challenger_bond: inputs.challenger_bond,
        },
        eq,
        stake_of,
    )
}

/// Wire the sampled path: select the committee, run commit-reveal, take the verdict, and
/// settle or escalate (on NoQuorum). Holds no money logic — every unit moves through
/// [`crate::settlement`] / [`crate::escalation`].
#[allow(clippy::too_many_arguments)]
fn run_sampled(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    inputs: &JobInputs,
    eq: &dyn EquivalenceOracle,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
    rng: &mut dyn rand::RngCore,
    true_hash: [u8; 32],
    executor_hash: [u8; 32],
) -> (Verdict, SettlementOutcome) {
    let job = &inputs.job;

    // --- Sampled: select the committee, run commit-reveal, take the verdict. ---
    let committee = select_committee(
        &draw_seed(rng),
        inputs.candidates,
        &inputs.executor,
        p.k,
        stake_of,
    );
    // Each committee member posts a verifier bond into escrow.
    for v in &committee {
        l.escrow(*v, inputs.verifier_bond);
    }

    // Commit-reveal: each verifier commits to the hash its strategy reveals, then opens
    // it. The commitment binds the reveal (hiding/binding proven in commit_reveal.rs);
    // the engine carries both so a real protocol's two-round structure is represented.
    let reveals: Vec<Reveal> = committee
        .iter()
        .map(|v| {
            let revealed = (inputs.verifier_reveal)(v, &true_hash, &executor_hash);
            let salt = draw_seed(rng);
            // Bind the reveal to a commitment (constructed, then immediately opened here).
            let _commitment = make_commitment(v, &revealed, &salt, inputs.verifier_bond);
            Reveal { verifier: *v, result_hash: revealed, salt }
        })
        .collect();

    let quorum = p.quorum(committee.len());
    let verdict = crate::verdict::compute_verdict(&reveals, &executor_hash, quorum, eq);

    // --- Settle / escalate per the verdict. ---
    let outcome = match verdict {
        Verdict::Confirmed { .. } => {
            let out = settle_confirmed_sampled(l, p, job.budget, inputs.executor, &committee);
            // `settle_confirmed_sampled` only splits the budget; the caller returns the
            // bonds. The executor was vindicated, so return its bond; the committee did
            // honest confirming work, so return theirs.
            l.pay(inputs.executor, inputs.executor_bond);
            return_committee_bonds(l, &committee, inputs.verifier_bond);
            out
        }
        Verdict::Disputed { correct_hash } => {
            // The committee reached quorum AGAINST the executor on `correct_hash`. The
            // catch bounty goes to the verifiers who revealed that vindicated value —
            // NOT merely anyone who disagreed with the executor (a third, also-wrong
            // reveal earns no bounty). Their own bonds are returned below.
            let honest: Vec<ParticipantId> = verifiers_revealing(&reveals, &correct_hash, eq);
            let out = settle_committee_disputed(
                l,
                p,
                job.budget,
                job.submitter,
                inputs.executor,
                inputs.executor_bond,
                &honest,
            );
            return_committee_bonds(l, &committee, inputs.verifier_bond);
            out
        }
        Verdict::NoQuorum => {
            // Escalate to the larger panel. The panel re-executes honestly (it is the
            // protocol's own re-execution), so its reveals are the true hash.
            let panel_seed = draw_seed(rng);
            // Pre-select the panel so we can escrow its bonds before resolving.
            let panel = select_committee(
                &panel_seed,
                inputs.candidates,
                &inputs.executor,
                p.k_escalate,
                stake_of,
            );
            for m in &panel {
                l.escrow(*m, inputs.verifier_bond);
            }
            let panel_reveals: Vec<Reveal> = panel
                .iter()
                .map(|m| Reveal { verifier: *m, result_hash: true_hash, salt: [0; 32] })
                .collect();
            let committee_bonds: Vec<u64> = vec![inputs.verifier_bond; committee.len()];
            let esc = Escalation {
                seed: panel_seed,
                candidates: inputs.candidates,
                budget: job.budget,
                executor: inputs.executor,
                executor_hash,
                executor_bond: inputs.executor_bond,
                panel_reveals: &panel_reveals,
                panel_bond: inputs.verifier_bond,
            };
            let (_v, out) = resolve_escalation(
                l,
                p,
                &esc,
                Trigger::NoQuorum {
                    submitter: job.submitter,
                    committee_reveals: &reveals,
                    committee_bonds: &committee_bonds,
                },
                eq,
                stake_of,
            );
            out
        }
    };

    (verdict, outcome)
}

/// Draw a fresh 32-byte seed from the RNG (sample seed, salts, panel seed).
fn draw_seed(rng: &mut dyn rand::RngCore) -> [u8; 32] {
    let mut s = [0u8; 32];
    rng.fill_bytes(&mut s);
    s
}

/// Return each committee member's posted verifier bond from escrow (honest-path helper).
fn return_committee_bonds(l: &mut dyn ChainHooks, committee: &[ParticipantId], bond: u64) {
    for v in committee {
        l.pay(*v, bond);
    }
}

/// The committee members who revealed the value the committee reached quorum on AGAINST
/// the executor — i.e. a value NOT equivalent to the executor's claim. These are the
/// honest verifiers eligible for the `Disputed` catch bounty.
/// Verifiers whose revealed value equals `value` (e.g. the quorum-vindicated hash
/// on a Disputed verdict). The catch bounty goes to those who actually produced the
/// correct result — not merely anyone who disagreed with the executor (a third,
/// also-wrong reveal earns no bounty).
fn verifiers_revealing(
    reveals: &[Reveal],
    value: &[u8; 32],
    eq: &dyn EquivalenceOracle,
) -> Vec<ParticipantId> {
    reveals
        .iter()
        .filter(|r| eq.equiv(&r.result_hash, value))
        .map(|r| r.verifier)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{JobId, ParticipantId};
    use crate::job::{Job, JobSpec};
    use crate::oracle::{ByteEq, IteratedHashVm, Ledger};
    use crate::params::GameParams;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// An all-honest, sampled job: honest executor (claims the true hash), honest
    /// committee (every verifier reveals the true hash). Expect `Confirmed` with an
    /// 85/10/5 settlement of a 100 budget, and `total_supply` invariant (no mint), with
    /// no value stranded in escrow.
    #[test]
    fn all_honest_sampled_job_confirms_and_settles_85_10_5() {
        let p = GameParams::default(); // sample_rate_bps = 10_000 ⇒ always sampled
        let mut l = Ledger::new();

        let submitter = pid(0);
        let executor = pid(9);
        // A candidate pool large enough for a k=3 committee, excluding the executor.
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();

        // Credit every actor so the engine can escrow their budget/bonds itself.
        l.credit(submitter, 100); // budget
        l.credit(executor, p.executor_bond); // executor bond
        for c in &candidates {
            l.credit(*c, p.verifier_bond); // each potential verifier's bond
        }
        let total0 = l.total_supply();

        let spec = JobSpec { program_hash: [7; 32], input_hash: [9; 32] };
        let job = Job {
            id: JobId::derive(&[7; 32], &[9; 32], &submitter, 0),
            submitter,
            spec,
            budget: 100,
        };

        let honest_claim = |true_hash: &[u8; 32]| *true_hash;
        let honest_reveal =
            |_v: &ParticipantId, true_hash: &[u8; 32], _exec: &[u8; 32]| *true_hash;
        // Sampled job ⇒ the challenge strategy is never consulted; supply a no-op.
        let no_challenge = |_true: &[u8; 32], _exec: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: b"in",
            executor,
            executor_bond: p.executor_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: p.verifier_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };

        let vm = IteratedHashVm { rounds: 1000 };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);

        let (verdict, out) =
            run_job(&mut l, &p, &inputs, &vm, &ByteEq, &stake, &mut rng);

        // Verdict is Confirmed on the true hash.
        match verdict {
            Verdict::Confirmed { .. } => {}
            other => panic!("expected Confirmed, got {other:?}"),
        }
        // 85/10/5 split of a 100 budget. The 10% verifier pool is split evenly across
        // the k=3 committee: 10/3 = 3 each ⇒ 9 paid, and the indivisible 1-unit remainder
        // is burned with the 5% protocol slice (global rounding rule), so burn = 6.
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 9);
        assert_eq!(out.burned, 6);
        // Worker + verifiers + burn must reconstruct the whole budget (no value lost).
        assert_eq!(out.worker_paid + out.verifiers_paid + out.burned, 100);
        // The executor was paid the worker share and got its bond back.
        assert_eq!(l.balance_of(&executor), 85 + p.executor_bond);
        // Conservation: no mint, and every escrow pot fully drained.
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed(), 0);
    }

    /// An UNSAMPLED job whose executor cheated (claims a wrong hash) is challenged by a
    /// rational challenger during the challenge window (spec §6.9 challenge path). The
    /// engine must wire the challenge: select the re-execution panel (re-executes honestly
    /// ⇒ the true hash), and `escalation::resolve` with `Trigger::Challenge` returns
    /// `Disputed`-via-challenge. The submitter is refunded, the challenger is rewarded
    /// from the slashed executor bond, the panel is paid, the remainder is burned, and
    /// supply is invariant with no escrow stranded. This exercises the phase the prior
    /// engine declined to wire.
    #[test]
    fn unsampled_cheating_executor_challenged_routes_to_disputed_and_conserves() {
        // Never sample, so every job goes through the challenge window.
        let mut p = GameParams::default();
        p.sample_rate_bps = 0;
        let mut l = Ledger::new();

        let submitter = pid(0);
        let executor = pid(9);
        let challenger = pid(8);
        // Pool large enough for a k_escalate = 7 panel, excluding the executor.
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();

        l.credit(submitter, 100); // budget
        l.credit(executor, p.executor_bond); // executor bond
        l.credit(challenger, p.challenger_bond); // challenger bond
        for c in &candidates {
            l.credit(*c, p.verifier_bond); // each potential panelist's bond
        }
        let total0 = l.total_supply();

        let spec = JobSpec { program_hash: [7; 32], input_hash: [9; 32] };
        let job = Job {
            id: JobId::derive(&[7; 32], &[9; 32], &submitter, 0),
            submitter,
            spec,
            budget: 100,
        };

        // Cheating executor: claims a hash that is NOT the true one.
        let cheat_claim = |_true_hash: &[u8; 32]| [0xAB; 32];
        let honest_reveal =
            |_v: &ParticipantId, true_hash: &[u8; 32], _exec: &[u8; 32]| *true_hash;
        // Rational challenger: disputes exactly when the executor's claim differs from
        // the true hash (i.e. when it can see the executor cheated).
        let rational_challenge = |true_hash: &[u8; 32], exec: &[u8; 32]| {
            if true_hash != exec { Some(challenger) } else { None }
        };
        let inputs = JobInputs {
            job,
            input: b"in",
            executor,
            executor_bond: p.executor_bond,
            executor_claim: &cheat_claim,
            candidates: &candidates,
            verifier_bond: p.verifier_bond,
            verifier_reveal: &honest_reveal,
            challenge: &rational_challenge,
            challenger_bond: p.challenger_bond,
        };

        let vm = IteratedHashVm { rounds: 1000 };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(7);

        let (verdict, out) =
            run_job(&mut l, &p, &inputs, &vm, &ByteEq, &stake, &mut rng);

        // The panel re-executed honestly and found the executor wrong.
        match verdict {
            Verdict::Disputed { .. } => {}
            other => panic!("expected Disputed, got {other:?}"),
        }
        // Submitter refunded the full budget (no useful work).
        assert_eq!(out.submitter_refunded, 100);
        // Challenger rewarded challenger_reward_bps (10%) of the 100 executor bond = 10.
        assert_eq!(out.challenger_paid, 10);
        // Panel splits escalation_reward_bps (10%) of 100 = 10 across 7 panelists:
        // 10/7 = 1 each ⇒ 7 paid; the executor bond is the only thing slashed.
        assert_eq!(out.panel_paid, 7);
        assert_eq!(out.slashed, vec![(executor, p.executor_bond)]);
        // Burn = executor bond - challenger reward - panel paid = 100 - 10 - 7 = 83.
        assert_eq!(out.burned, 83);
        // Submitter got the refund back; challenger got bond + reward.
        assert_eq!(l.balance_of(&submitter), 100);
        assert_eq!(l.balance_of(&challenger), p.challenger_bond + 10);
        // The cheating executor's bond was consumed (challenged successfully).
        assert_eq!(l.balance_of(&executor), 0);
        // Conservation: no mint, every escrow pot drained.
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.escrowed(), 0);
    }
}
