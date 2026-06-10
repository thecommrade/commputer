//! Monte-Carlo tournament (plan Task 14) — **the proof**.
//!
//! WHAT THIS DOES: runs many seeded jobs against a configured mix of the agent
//! strategies from [`crate::agents`] and accumulates, per strategy, its net profit
//! (rewards earned − bonds lost − modeled compute cost) plus the global "% of cheats
//! caught". The deliverable claim it must demonstrate, at the tuned [`safe_regime`]
//! params, is the dominance of honest play:
//!
//! ```text
//!   cheat_executor_ev  <=  0  <  honest_executor_ev          (cheating an executor loses)
//!   rubber_stamp_ev    <      honest_verifier_ev             (rubber-stamping a verifier loses)
//! ```
//!
//! HOW IT IS A PROOF (and not a hand-wave): the money is **real** — every unit moves
//! through the same [`crate::settlement`] / [`crate::trap`] branches and the same
//! conservation-checked [`Ledger`] the engine uses, so a strategy's EV is literally the
//! change in its ledger balance (minus the compute it modeled-spent), not a formula we
//! asserted. The only modeled-not-executed quantities are the per-actor **compute costs**
//! `C_exec` / `C_ver` (the toy VM has no realistic cost; spec §11 says express the regime
//! in cost *ratios*, which is exactly what [`SimCosts`] holds) and the **draws**
//! (`sampled?` / `trap?`), taken from a seeded RNG with the real `sample_rate_bps` /
//! `p_trap_bps`. Everything else — who gets slashed, who splits the trap jackpot, the
//! 85/10/5 split — is the production settlement code.
//!
//! WHERE THIS IS WIRED IN: `sim/main.rs` (the `pouw-sim` binary) calls
//! [`run_tournament`] with [`safe_regime`] and prints [`Report::table`]. The unit test at
//! the bottom is the Task-14 deliverable assertion and runs under
//! `cargo test -p commputer-pouw`.
//!
//! SPEC: §7 (the one inequality), §9 (how we prove it), §10 (sim) of
//! `src/staging/docs/2026-06-10-pouw-verification-game-design.md`.

use crate::agents::{Executor, Verifier};
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::Reveal;
use commputer_pouw::oracle::{ChainHooks, Ledger};
use commputer_pouw::params::GameParams;
use commputer_pouw::settlement::{
    settle_committee_disputed, settle_confirmed_sampled, settle_confirmed_unsampled,
};
use commputer_pouw::trap::settle_trap;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

/// Modeled compute costs, in budget units (spec §11: the regime is expressed as cost
/// *ratios* to the budget/bonds, since the toy VM has no realistic absolute cost). These
/// are the ONLY modeled-not-executed numbers in an actor's EV — everything else is real
/// ledger movement. An actor pays the cost iff its strategy actually does the work
/// ([`Executor::does_work`] / [`Verifier::does_work`]); a lazy/rubber-stamp actor saves it,
/// which is precisely the gain the slash must dominate.
#[derive(Clone, Copy, Debug)]
pub struct SimCosts {
    /// `C_exec`: cost an honest executor pays to run the job. Must stay below the worker
    /// reward (else honesty itself is unprofitable) and below `P(caught)·executor_bond`
    /// (else cheating pays — the §7 executor inequality).
    pub c_exec: u64,
    /// `C_ver`: cost an honest verifier pays to re-execute. The §7 verifier inequality is
    /// `p_trap·verifier_bond > C_ver`; pick `c_ver` below that bound with margin.
    pub c_ver: u64,
}

/// The tuned, *documented* safe regime — the primary deliverable of this task (spec §7,
/// §12). It is deliberately **non-trivial**: sampling is 50% (not "verify everything"),
/// and both §7 inequalities hold by margin rather than by brute force.
///
/// Returns `(params, costs)` such that, with the strategy mix in [`run_tournament`]:
///   * executor inequality: `P(sampled)·executor_bond = 0.50·100 = 50 > C_exec = 40`,
///     so a cheating executor is EV-negative while an honest one earns `85 − 40 = 45`;
///   * verifier inequality: `p_trap·verifier_bond = 0.25·20 = 5 > C_ver = 3`,
///     so a rubber-stamp verifier is EV-negative relative to honest (margin 2/round,
///     widened further by the trap jackpot honest verifiers split off slashed stampers).
///
/// Only the three knobs the plan authorizes are moved off `GameParams::default()`:
/// `sample_rate_bps` (10000 → 5000) and `p_trap_bps` (1000 → 2500); `executor_bond` is
/// left at its default 100. The split, quorum, and bonds are untouched.
pub fn safe_regime() -> (GameParams, SimCosts) {
    // Only the two authorized knobs move off the defaults; everything else (executor_bond
    // 100, the 85/10/5 split, quorum, bonds) is inherited via `..Default::default()`.
    let p = GameParams {
        sample_rate_bps: 5_000, // tuned: proactively verify 50% of jobs (not all)
        p_trap_bps: 2_500,      // tuned: 25% of verification rounds are traps
        ..GameParams::default()
    };
    debug_assert!(p.validate().is_ok(), "safe_regime must satisfy GameParams invariants");
    (p, SimCosts { c_exec: 40, c_ver: 3 })
}

/// Per-strategy running totals. `ev` is the net profit (positive = made money). `caught`
/// counts the jobs/rounds on which this strategy was slashed (lost its bond) — the global
/// "% cheats caught" the founder reads off the table.
#[derive(Clone, Copy, Debug, Default)]
pub struct StratStats {
    /// Number of jobs (executors) or verification rounds (verifiers) this strategy played.
    pub plays: u64,
    /// How many of those plays ended with this strategy's bond slashed.
    pub caught: u64,
    /// Net profit across all plays, in budget units (can be negative).
    pub ev: i64,
}

impl StratStats {
    /// Mean profit per play (the EV the deliverable inequalities compare). Zero plays ⇒ 0.
    pub fn mean_ev(&self) -> f64 {
        if self.plays == 0 { 0.0 } else { self.ev as f64 / self.plays as f64 }
    }
    /// Fraction of plays on which this strategy was caught (slashed), in `[0, 1]`.
    pub fn caught_frac(&self) -> f64 {
        if self.plays == 0 { 0.0 } else { self.caught as f64 / self.plays as f64 }
    }
}

/// The tournament's output: one [`StratStats`] per modeled strategy, plus the params it
/// ran under (so the table can print the regime). The named accessors are the exact
/// quantities the deliverable inequalities (and the unit test) compare.
#[derive(Clone, Debug)]
pub struct Report {
    pub params: GameParams,
    pub costs: SimCosts,
    pub jobs: u64,
    pub honest_executor: StratStats,
    pub cheat_executor: StratStats,
    pub lazy_executor: StratStats,
    pub honest_verifier: StratStats,
    pub rubber_stamp: StratStats,
}

impl Report {
    pub fn honest_executor_ev(&self) -> f64 { self.honest_executor.mean_ev() }
    pub fn cheat_executor_ev(&self) -> f64 { self.cheat_executor.mean_ev() }
    pub fn lazy_executor_ev(&self) -> f64 { self.lazy_executor.mean_ev() }
    pub fn honest_verifier_ev(&self) -> f64 { self.honest_verifier.mean_ev() }
    pub fn rubber_stamp_ev(&self) -> f64 { self.rubber_stamp.mean_ev() }

    /// A small, founder-readable metrics table (strategy, plays, caught %, net EV/play).
    pub fn table(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "PoUW Monte-Carlo tournament — {} jobs, seeded\n",
            self.jobs
        ));
        s.push_str(&format!(
            "regime: sample_rate={}bps  p_trap={}bps  executor_bond={}  verifier_bond={}  \
             C_exec={}  C_ver={}\n",
            self.params.sample_rate_bps,
            self.params.p_trap_bps,
            self.params.executor_bond,
            self.params.verifier_bond,
            self.costs.c_exec,
            self.costs.c_ver,
        ));
        s.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>12}\n",
            "strategy", "plays", "caught %", "net EV/play"
        ));
        s.push_str(&format!("{}\n", "-".repeat(54)));
        let row = |name: &str, st: &StratStats| {
            format!(
                "{:<22} {:>8} {:>9.1}% {:>12.3}\n",
                name,
                st.plays,
                st.caught_frac() * 100.0,
                st.mean_ev()
            )
        };
        s.push_str(&row("executor:honest", &self.honest_executor));
        s.push_str(&row("executor:cheat", &self.cheat_executor));
        s.push_str(&row("executor:lazy", &self.lazy_executor));
        s.push_str(&row("verifier:honest", &self.honest_verifier));
        s.push_str(&row("verifier:rubberstamp", &self.rubber_stamp));
        s.push_str("\nverdict: ");
        let exec_ok = self.cheat_executor_ev() <= 0.0
            && self.lazy_executor_ev() <= 0.0
            && self.honest_executor_ev() > 0.0;
        let ver_ok = self.rubber_stamp_ev() < self.honest_verifier_ev();
        if exec_ok && ver_ok {
            s.push_str("HONEST PLAY DOMINATES — every modeled cheat is EV-negative.\n");
        } else {
            s.push_str("REGIME FAILS — a cheat is not EV-negative; retune.\n");
        }
        s
    }
}

/// Fixed roster of distinct participant ids the tournament draws from. Index 0 is the
/// submitter; the rest are a candidate verifier pool. Ids are deterministic so a run is
/// reproducible from its seed alone.
fn pid(n: u8) -> ParticipantId {
    ParticipantId([n; 32])
}

/// One executor's outcome on one job, decided by the same draws/settlement the engine
/// uses. Returns `(ledger_balance_delta, caught)` for the executor's strategy bucket; the
/// caller layers the modeled compute cost on top. The job's budget and the executor bond
/// are escrowed here and fully settled, so the ledger stays conserved.
///
/// `committee` are the verifiers reviewing this job (with their strategies); `sampled` and
/// `trap` were drawn by the caller from the RNG. The executor's true result is "hash A";
/// an honest executor claims it, a cheat/lazy executor claims a wrong hash.
struct JobModel<'a> {
    p: &'a GameParams,
    submitter: ParticipantId,
    executor: ParticipantId,
    budget: u64,
    /// The true result hash for this job (what an honest executor claims and an honest
    /// verifier reveals).
    true_hash: [u8; 32],
    /// The reviewing committee: each member's id and verifier strategy.
    committee: &'a [(ParticipantId, Verifier)],
}

/// Outcome buckets for a single simulated job, keyed by strategy, so the tournament can
/// fold them into the running [`StratStats`]. Amounts are ledger deltas (compute cost is
/// added by the caller, which knows each actor's strategy).
#[derive(Default)]
struct JobDeltas {
    /// `(participant, ledger_delta, caught)` for the executor.
    executor: Option<(ParticipantId, i64, bool)>,
    /// `(verifier_strategy_is_rubber, ledger_delta, caught)` per committee member, in order.
    verifiers: Vec<(Verifier, i64, bool)>,
}

/// Settle one fully-modeled job against a fresh ledger and report the per-actor deltas.
///
/// This routes through the *real* settlement/trap branches:
///   * **trap round** (`trap == true`): the protocol plants a wrong claim; honest
///     verifiers reveal the truth, rubber-stampers echo the planted-wrong and are slashed
///     via [`settle_trap`]. (Executors are not the subject of a trap — a trap tests the
///     committee — so the executor neither earns nor is slashed on a trap round.)
///   * **sampled, non-trap** (`sampled == true`): a committee reviews the executor's real
///     claim. Honest verifiers reveal the truth; a cheating executor's wrong claim draws a
///     `Disputed` verdict (bond slashed via [`settle_committee_disputed`]); an honest
///     executor is `Confirmed` (85/10/5 via [`settle_confirmed_sampled`]).
///   * **unsampled** (`sampled == false`): optimistic acceptance with no committee
///     ([`settle_confirmed_unsampled`]) — the executor is paid 85% whether or not it
///     cheated (this is exactly the gap sampling/traps must make unprofitable in
///     aggregate).
fn settle_one_job(
    m: &JobModel,
    executor_strat: Executor,
    sampled: bool,
    trap: bool,
) -> JobDeltas {
    let p = m.p;
    let mut l = Ledger::new();
    let mut deltas = JobDeltas::default();

    // Accounting discipline: every actor is credited the bond it will post BEFORE we record
    // its baseline, so posting the bond (escrow) is a real `-bond` outflow against that
    // baseline and returning it is a matching `+bond` — they net to zero, and a slash shows
    // up as the bond never coming back. Thus an actor's `delta = final_balance - baseline`
    // is exactly its net profit: rewards earned minus any bond lost (the submitter's budget
    // is its own stake and is excluded from every actor's EV).
    for (v, _) in m.committee {
        l.credit(*v, p.verifier_bond);
    }
    let ver_baseline: Vec<i64> = m.committee.iter().map(|(v, _)| l.balance_of(v) as i64).collect();

    if trap {
        // --- Trap round: synthetic, no budget. The protocol presents a planted-wrong
        // claim to the committee and knows the true answer. Only the committee is on
        // trial; the executor sits it out. ---
        let planted_wrong = wrong_hash(&m.true_hash, 0xEE);
        for (v, _) in m.committee {
            l.escrow(*v, p.verifier_bond); // each verifier posts its bond
        }
        // On a trap, the "executor claim" the verifier sees IS the planted-wrong hash; a
        // rubber-stamper echoes it (caught), an honest verifier reveals the truth.
        let reveals: Vec<Reveal> = m
            .committee
            .iter()
            .map(|(v, strat)| Reveal {
                verifier: *v,
                result_hash: strat.reveal(v, &m.true_hash, &planted_wrong, &m.executor),
                salt: [0; 32],
            })
            .collect();
        let bonds = |_: &ParticipantId| p.verifier_bond;
        // settle_trap slashes rubber-stampers' bonds and pays the jackpot to honest
        // verifiers (real money, conserved). It does NOT return honest bonds — caller does.
        let _ = settle_trap(&mut l, p, planted_wrong, m.true_hash, &reveals, &bonds);
        for (i, (v, strat)) in m.committee.iter().enumerate() {
            let stamped =
                strat.reveal(v, &m.true_hash, &planted_wrong, &m.executor) == planted_wrong;
            if !stamped {
                l.pay(*v, p.verifier_bond); // honest bond returned by the caller
            }
            let delta = l.balance_of(v) as i64 - ver_baseline[i];
            deltas.verifiers.push((*strat, delta, stamped));
        }
        return deltas;
    }

    // --- Real job. Credit the executor's bond, record its baseline, then escrow. ---
    l.credit(m.submitter, m.budget);
    l.credit(m.executor, p.executor_bond);
    let exec_baseline = l.balance_of(&m.executor) as i64; // = executor_bond, pre-escrow
    l.escrow(m.submitter, m.budget);
    l.escrow(m.executor, p.executor_bond);

    let executor_hash = executor_strat.claim(&m.true_hash);

    if !sampled {
        // Optimistic acceptance: no committee, executor paid 85% regardless of correctness.
        let _ = settle_confirmed_unsampled(&mut l, p, m.budget, m.executor);
        l.pay(m.executor, p.executor_bond); // bond returned (nobody disputed)
        let delta = l.balance_of(&m.executor) as i64 - exec_baseline;
        deltas.executor = Some((m.executor, delta, false));
        // The committee did not review this job (unsampled), so its members get no play.
        return deltas;
    }

    // Sampled: the committee reviews. Each verifier posts its bond.
    for (v, _) in m.committee {
        l.escrow(*v, p.verifier_bond);
    }
    let reveals: Vec<Reveal> = m
        .committee
        .iter()
        .map(|(v, strat)| Reveal {
            verifier: *v,
            result_hash: strat.reveal(v, &m.true_hash, &executor_hash, &m.executor),
            salt: [0; 32],
        })
        .collect();

    // Verdict: a quorum agreeing WITH the executor ⇒ Confirmed; a quorum on a different
    // value ⇒ Disputed. With an honest-majority committee the verdict tracks the truth.
    let agree_with_exec = reveals.iter().filter(|r| r.result_hash == executor_hash).count();
    let quorum = p.quorum(m.committee.len());
    let confirmed = agree_with_exec >= quorum;

    if confirmed {
        let committee_ids: Vec<ParticipantId> = m.committee.iter().map(|(v, _)| *v).collect();
        let _ = settle_confirmed_sampled(&mut l, p, m.budget, m.executor, &committee_ids);
        l.pay(m.executor, p.executor_bond); // executor vindicated, bond back
        for (v, _) in m.committee {
            l.pay(*v, p.verifier_bond); // committee bonds returned
        }
        deltas.executor = Some((m.executor, l.balance_of(&m.executor) as i64 - exec_baseline, false));
        for (i, (v, strat)) in m.committee.iter().enumerate() {
            deltas
                .verifiers
                .push((*strat, l.balance_of(v) as i64 - ver_baseline[i], false));
        }
    } else {
        // Disputed: the committee reached quorum on a value other than the executor's claim.
        // Honest verifiers (revealed != executor_hash) split the catch bounty; the executor
        // bond is slashed (it never comes back ⇒ shows up as a negative delta).
        let honest: Vec<ParticipantId> = reveals
            .iter()
            .filter(|r| r.result_hash != executor_hash)
            .map(|r| r.verifier)
            .collect();
        let _ = settle_committee_disputed(
            &mut l,
            p,
            m.budget,
            m.submitter,
            m.executor,
            p.executor_bond,
            &honest,
        );
        for (v, _) in m.committee {
            l.pay(*v, p.verifier_bond); // all committee bonds returned (honest work)
        }
        deltas.executor =
            Some((m.executor, l.balance_of(&m.executor) as i64 - exec_baseline, true)); // caught & slashed
        for (i, (v, strat)) in m.committee.iter().enumerate() {
            deltas
                .verifiers
                .push((*strat, l.balance_of(v) as i64 - ver_baseline[i], false));
        }
    }

    deltas
}

/// A deterministic "wrong" hash distinct from `true_hash` (mirrors `agents::wrong_hash`,
/// which is module-private there). Used to plant a trap's wrong claim.
fn wrong_hash(true_hash: &[u8; 32], tag: u8) -> [u8; 32] {
    let mut h = commputer_pouw::ids::hash_parts(&[true_hash, &[tag]]);
    if &h == true_hash {
        h[0] ^= 0xFF;
    }
    h
}

/// Run the Monte-Carlo tournament: `jobs` seeded jobs under `(p, costs)`, with a mix of
/// executor and verifier strategies, and return the per-strategy [`Report`].
///
/// Strategy mix: each job's executor is drawn uniformly from {Honest, Cheat, Lazy} so all
/// three accrue plays; the reviewing committee is a fixed honest-majority panel with one
/// rubber-stamp seat, so honest and rubber-stamp verifiers both accrue plays under the
/// same draws. The committee is honest-majority on purpose — a quorum of honest verifiers
/// is the protocol's security assumption (spec §7 collusion bound); the sim measures the
/// EV of *deviating* against that backdrop, which is exactly the best-response question.
pub fn run_tournament(jobs: u64, p: &GameParams, costs: &SimCosts, seed: u64) -> Report {
    let mut rng = StdRng::seed_from_u64(seed);

    let submitter = pid(0);
    let executor = pid(1);
    // A k=3 committee: two honest seats and one rubber-stamp seat (honest majority, so an
    // honest claim is Confirmed and a cheat is Disputed, while the rubber-stamp seat still
    // accrues plays/EV to compare against honest).
    let committee: Vec<(ParticipantId, Verifier)> = vec![
        (pid(10), Verifier::Honest),
        (pid(11), Verifier::Honest),
        (pid(12), Verifier::RubberStamp),
    ];
    // The fixed true result every job's honest actors converge on (one job spec suffices:
    // the EV question is about strategies, not about which particular bytes are computed).
    let true_hash = commputer_pouw::ids::hash_parts(&[b"true-result"]);

    let mut honest_executor = StratStats::default();
    let mut cheat_executor = StratStats::default();
    let mut lazy_executor = StratStats::default();
    let mut honest_verifier = StratStats::default();
    let mut rubber_stamp = StratStats::default();

    let model = JobModel {
        p,
        submitter,
        executor,
        budget: 100,
        true_hash,
        committee: &committee,
    };

    for _ in 0..jobs {
        // Draw this job's executor strategy (uniform over the three).
        let executor_strat = match rng.gen_range(0..3u32) {
            0 => Executor::Honest,
            1 => Executor::Cheat,
            _ => Executor::Lazy,
        };
        // Draw sampled? and trap? exactly as the engine does.
        let sampled = rng.gen_range(0..10_000u32) < p.sample_rate_bps;
        let trap = rng.gen_range(0..10_000u32) < p.p_trap_bps;

        let deltas = settle_one_job(&model, executor_strat, sampled, trap);

        // Fold the executor outcome (skipped on a trap round, which tests verifiers only).
        if let Some((_who, delta, caught)) = deltas.executor {
            let does_work = executor_strat.does_work();
            let cost = if does_work { costs.c_exec as i64 } else { 0 };
            let bucket = match executor_strat {
                Executor::Honest => &mut honest_executor,
                Executor::Cheat => &mut cheat_executor,
                Executor::Lazy => &mut lazy_executor,
            };
            bucket.plays += 1;
            bucket.ev += delta - cost;
            if caught {
                bucket.caught += 1;
            }
        }

        // Fold each committee member's outcome into its strategy bucket.
        for (strat, delta, caught) in deltas.verifiers {
            let does_work = strat.does_work(&executor);
            let cost = if does_work { costs.c_ver as i64 } else { 0 };
            let bucket = match strat {
                Verifier::Honest => &mut honest_verifier,
                Verifier::RubberStamp => &mut rubber_stamp,
                // Colluders are exercised by their own unit tests; the tournament's fixed
                // committee uses only Honest/RubberStamp seats, so this arm is unreachable.
                Verifier::Collude(_) => continue,
            };
            bucket.plays += 1;
            bucket.ev += delta - cost;
            if caught {
                bucket.caught += 1;
            }
        }
    }

    Report {
        params: p.clone(),
        costs: *costs,
        jobs,
        honest_executor,
        cheat_executor,
        lazy_executor,
        honest_verifier,
        rubber_stamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE DELIVERABLE (plan Task 14 Step 1): over many seeded jobs at the tuned
    /// `safe_regime`, every modeled cheating strategy must be EV-negative relative to
    /// honest play. Concretely:
    ///   * `cheat_executor_ev <= 0 < honest_executor_ev`  (cheating an executor loses;
    ///     honesty profits), and the same for the lazy executor;
    ///   * `rubber_stamp_ev < honest_verifier_ev`  (rubber-stamping a verifier loses
    ///     relative to honest verification).
    #[test]
    fn honest_play_dominates_at_safe_regime() {
        let (p, costs) = safe_regime();
        let r = run_tournament(5_000, &p, &costs, 0xC0FFEE);

        // Sanity: every strategy actually got plays (the mix exercised them all).
        assert!(r.honest_executor.plays > 0, "honest executor never played");
        assert!(r.cheat_executor.plays > 0, "cheat executor never played");
        assert!(r.lazy_executor.plays > 0, "lazy executor never played");
        assert!(r.honest_verifier.plays > 0, "honest verifier never played");
        assert!(r.rubber_stamp.plays > 0, "rubber-stamp verifier never played");

        // Executor dominance: honesty is the only EV-positive executor strategy.
        assert!(
            r.honest_executor_ev() > 0.0,
            "honest executor must be EV-positive, got {}",
            r.honest_executor_ev()
        );
        assert!(
            r.cheat_executor_ev() <= 0.0,
            "cheating executor must be EV-negative, got {}",
            r.cheat_executor_ev()
        );
        assert!(
            r.lazy_executor_ev() <= 0.0,
            "lazy executor must be EV-negative, got {}",
            r.lazy_executor_ev()
        );
        assert!(
            r.cheat_executor_ev() <= 0.0 && 0.0 < r.honest_executor_ev(),
            "need cheat_executor_ev <= 0 < honest_executor_ev"
        );

        // Verifier dominance: rubber-stamping loses relative to honest verification.
        assert!(
            r.rubber_stamp_ev() < r.honest_verifier_ev(),
            "rubber-stamp EV {} must be < honest verifier EV {}",
            r.rubber_stamp_ev(),
            r.honest_verifier_ev()
        );

        // The catch rate on the sampled cheats is non-trivial (sampling actually bites).
        assert!(
            r.cheat_executor.caught_frac() > 0.0,
            "cheating executors should be caught some of the time"
        );
    }

    /// The tournament is reproducible: same seed ⇒ identical report.
    #[test]
    fn tournament_is_deterministic_for_a_fixed_seed() {
        let (p, costs) = safe_regime();
        let a = run_tournament(1_000, &p, &costs, 7);
        let b = run_tournament(1_000, &p, &costs, 7);
        assert_eq!(a.honest_executor.ev, b.honest_executor.ev);
        assert_eq!(a.cheat_executor.ev, b.cheat_executor.ev);
        assert_eq!(a.rubber_stamp.ev, b.rubber_stamp.ev);
        assert_eq!(a.honest_verifier.ev, b.honest_verifier.ev);
    }
}
