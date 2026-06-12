//! Real-fuel economics sweep (fuel-economics spec §5) — feature-gated.
//!
//! WHAT: measures actual fuel from real wasm programs, prices every role's
//! budget/bonds from the spec §3 formulas, and sweeps parameter corners under
//! the ADDITIVE event model with honest-equilibrium scoring. The verdict line
//! is the cycle's headline deliverable.
//!
//! ADDITIVE EVENTS (spec §2.1): per job, ONE real event (sampled w.p. s ⇒ paid
//! committee review, else unsampled) AND, independently, a trap round w.p. t —
//! implemented as a second settle_one_job call. This deliberately differs from
//! the toy mode's trap-precedence draw; the formulas price THIS model.
//!
//! HONEST-EQUILIBRIUM SCORING (spec §5.6): honest-role EVs come from an
//! all-honest committee run (no rubber-stamper ⇒ no slashes ⇒ zero jackpot
//! income); cheat EVs come from the mixed run. Without this split, jackpot
//! income from a guaranteed cheater (~2× the Bv margin) would let under-priced
//! corners pass.

use crate::agents::{Executor, Verifier};
use crate::tournament::{pid, settle_one_job, JobDeltas, JobModel, StratStats};
use commputer_pouw::economics::{
    budget_min, executor_bond_min, verifier_bond_min, work_cost,
};
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::JobSpec;
use commputer_pouw::params::GameParams;
use commputer_pouw::wasm::{ProgramStore, WasmLimits, WasmOracle};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha2::{Digest, Sha256};

/// One measured program class: name + measured fuel.
pub struct FuelClass {
    pub name: &'static str,
    pub measured_fuel: u64,
}

/// Measure fuel for the three real programs (one wasmi execution each — the
/// tournament itself is pure ledger math afterwards).
pub fn measure_classes() -> Vec<FuelClass> {
    let fixtures: [(&'static str, &[u8]); 3] = [
        ("guest", include_bytes!("../src/wasm/fixtures/guest_example.wasm")),
        ("light", include_bytes!("fixtures/light.wasm")),
        ("heavy", include_bytes!("fixtures/heavy.wasm")),
    ];
    fixtures
        .into_iter()
        .map(|(name, bytes)| {
            let mut store = ProgramStore::new();
            let program_hash = store.insert(bytes.to_vec());
            let input = b"sweep".to_vec();
            let input_hash: [u8; 32] = Sha256::digest(&input).into();
            let oracle = WasmOracle::new(store, WasmLimits::default());
            let out = oracle.execute(&JobSpec { program_hash, input_hash }, &input);
            assert!(out.result.is_ok(), "{name}: fixture must run, got {:?}", out.result);
            FuelClass { name, measured_fuel: out.fuel_consumed }
        })
        .collect()
}

/// A sweep corner: regime knobs + which cap mode prices it.
#[derive(Clone, Copy, Debug)]
pub struct Corner {
    pub s_bps: u32,
    pub t_bps: u32,
    pub v_bps: u32,
    pub k: usize,
    /// false ⇒ price at the global WasmLimits cap; true ⇒ tight cap (F == measured).
    pub tight_cap: bool,
}

/// Per-corner result: formula prices + EVs from both populations.
#[allow(dead_code)] // all fields are founder-readable output; not all consumed by table()
pub struct CornerResult {
    pub corner: Corner,
    pub budget: u64,
    pub executor_bond: u64,
    pub verifier_bond: u64,
    /// honest-equilibrium run (all-honest committee — no jackpot income possible):
    pub honest_executor_ev: f64,
    pub honest_verifier_ev: f64,
    /// mixed run (one rubber-stamp seat):
    pub cheat_executor_ev: f64,
    pub lazy_executor_ev: f64,
    pub rubber_stamp_ev: f64,
    /// the mixed run's honest-verifier EV — the rubber-stamp comparison baseline
    /// (spec §5.7: the cheat must lose relative to honesty IN ITS OWN population).
    pub mixed_honest_verifier_ev: f64,
    pub safe: bool,
}

/// Build the corner's GameParams: worker absorbs the verifier-slice change
/// (burn stays 500), k_escalate = 2k+1 (spec §5.4 bookkeeping rules).
fn corner_params(c: &Corner, price_per_mfuel: u64) -> GameParams {
    let p = GameParams {
        sample_rate_bps: c.s_bps,
        p_trap_bps: c.t_bps,
        verifier_bps: c.v_bps,
        worker_bps: 10_000 - 500 - c.v_bps,
        burn_bps: 500,
        k: c.k,
        k_escalate: 2 * c.k + 1,
        price_per_mfuel,
        ..GameParams::default()
    };
    debug_assert!(p.validate().is_ok());
    p
}

/// Fold one settle_one_job outcome into the strategy buckets. Mirrors
/// run_tournament's fold blocks; cost = realized work_cost when the strategy
/// does the work.
#[allow(clippy::too_many_arguments)]
fn fold(
    deltas: &JobDeltas,
    executor_strat: Executor,
    realized_cost: u64,
    he: &mut StratStats, ce: &mut StratStats, le: &mut StratStats,
    hv: &mut StratStats, rs: &mut StratStats,
) {
    if let Some((_who, delta, caught)) = deltas.executor {
        let cost = if executor_strat.does_work() { realized_cost as i64 } else { 0 };
        let bucket = match executor_strat {
            Executor::Honest => &mut *he,
            Executor::Cheat => &mut *ce,
            Executor::Lazy => &mut *le,
        };
        bucket.plays += 1;
        bucket.ev += delta - cost;
        if caught { bucket.caught += 1; }
    }
    for (strat, delta, caught) in &deltas.verifiers {
        let executor = pid(1); // matches run_population's executor id
        let cost = if strat.does_work(&executor) { realized_cost as i64 } else { 0 };
        let bucket = match strat {
            Verifier::Honest => &mut *hv,
            Verifier::RubberStamp => &mut *rs,
            Verifier::Collude(_) => continue, // unreachable: our committees never seat colluders
        };
        bucket.plays += 1;
        bucket.ev += *delta - cost;
        if *caught { bucket.caught += 1; }
    }
}

/// Run one population at one corner for one fuel class. `rubber_seat` controls
/// the population: false ⇒ all-honest committee (honest-equilibrium scoring);
/// true ⇒ one rubber-stamp seat (cheat measurement).
fn run_population(
    jobs: u64,
    p: &GameParams,
    budget: u64,
    realized_cost: u64,
    rubber_seat: bool,
    seed: u64,
) -> (StratStats, StratStats, StratStats, StratStats, StratStats) {
    let mut rng = StdRng::seed_from_u64(seed);
    let submitter = pid(0);
    let executor = pid(1);
    let committee: Vec<(ParticipantId, Verifier)> = (0..p.k)
        .map(|i| {
            let strat = if rubber_seat && i == p.k - 1 { Verifier::RubberStamp } else { Verifier::Honest };
            (pid(10 + i as u8), strat)
        })
        .collect();
    let true_hash = commputer_pouw::ids::hash_parts(&[b"real-fuel"]);
    let model = JobModel { p, submitter, executor, budget, true_hash, committee: &committee };

    let mut he = StratStats::default();
    let mut ce = StratStats::default();
    let mut le = StratStats::default();
    let mut hv = StratStats::default();
    let mut rs = StratStats::default();

    for _ in 0..jobs {
        let executor_strat = if rubber_seat {
            match rng.gen_range(0..3u32) {
                0 => Executor::Honest,
                1 => Executor::Cheat,
                _ => Executor::Lazy,
            }
        } else {
            Executor::Honest // honest-equilibrium population
        };
        let sampled = rng.gen_range(0..10_000u32) < p.sample_rate_bps;
        let trap = rng.gen_range(0..10_000u32) < p.p_trap_bps;

        // ADDITIVE: the real event always settles (never displaced by a trap)...
        let deltas = settle_one_job(&model, executor_strat, sampled, false);
        fold(&deltas, executor_strat, realized_cost, &mut he, &mut ce, &mut le, &mut hv, &mut rs);
        // ...and a trap round is an ADDITIONAL synthetic event.
        if trap {
            let trap_deltas = settle_one_job(&model, executor_strat, false, true);
            fold(&trap_deltas, executor_strat, realized_cost, &mut he, &mut ce, &mut le, &mut hv, &mut rs);
        }
    }
    (he, ce, le, hv, rs)
}

/// The spec §5.4 grid: 3 × 2 × 3 × 2 × 2 = 72 corners.
pub fn default_grid() -> Vec<Corner> {
    let mut g = Vec::new();
    for s_bps in [2_500u32, 5_000, 10_000] {
        for t_bps in [1_000u32, 2_500] {
            for v_bps in [1_000u32, 2_500, 4_000] {
                for k in [3usize, 5] {
                    for tight_cap in [false, true] {
                        g.push(Corner { s_bps, t_bps, v_bps, k, tight_cap });
                    }
                }
            }
        }
    }
    g
}

/// Sweep one fuel class over the grid. CRITICAL WIRING (spec §5.4 "budgets/bonds
/// set AT the formula minimums"): the formula bonds MUST be assigned into the
/// corner's GameParams — settle_one_job escrows/slashes p.executor_bond and
/// p.verifier_bond, NOT the CornerResult fields. Unpriceable corners (BadParams)
/// are printed and skipped, never silently dropped.
pub fn run_sweep(grid: &[Corner], class: &FuelClass, jobs: u64, seed: u64) -> Vec<CornerResult> {
    let mut out = Vec::new();
    for (i, c) in grid.iter().enumerate() {
        // Scale price so work_cost(measured) lands near 1_000 (readable EVs;
        // the regime is scale-invariant in price).
        let price = (1_000 / class.measured_fuel.div_ceil(1_000_000).max(1)).max(1);
        let fuel_cap = if c.tight_cap { class.measured_fuel } else { WasmLimits::default().fuel };
        let mut p = corner_params(c, price);
        let realized_cost = work_cost(class.measured_fuel, price);

        let priced = budget_min(fuel_cap, &p).and_then(|b| {
            let eb = executor_bond_min(fuel_cap, b, &p)?;
            let vb = verifier_bond_min(fuel_cap, &p)?;
            Ok((b, eb, vb))
        });
        let (budget, e_bond, v_bond): (u64, u64, u64) = match priced {
            Ok(t) => t,
            Err(e) => {
                println!("corner {i} unpriceable ({e:?}) — skipped");
                continue;
            }
        };
        // THE LOAD-BEARING LINES: settlement must escrow the formula bonds.
        p.executor_bond = e_bond;
        p.verifier_bond = v_bond;

        let (he, _, _, hv, _) =
            run_population(jobs, &p, budget, realized_cost, false, seed ^ i as u64);
        let (_, ce, le, hv_mixed, rs) =
            run_population(jobs, &p, budget, realized_cost, true, seed ^ i as u64 ^ 0xA5A5);

        out.push(CornerResult {
            corner: *c,
            budget,
            executor_bond: e_bond,
            verifier_bond: v_bond,
            honest_executor_ev: he.mean_ev(),
            honest_verifier_ev: hv.mean_ev(),
            cheat_executor_ev: ce.mean_ev(),
            lazy_executor_ev: le.mean_ev(),
            rubber_stamp_ev: rs.mean_ev(),
            mixed_honest_verifier_ev: hv_mixed.mean_ev(),
            safe: ce.mean_ev() <= 0.0
                && le.mean_ev() <= 0.0
                && rs.mean_ev() < hv_mixed.mean_ev() // the cheat loses in ITS OWN population
                && he.mean_ev() > 0.0                 // honest-equilibrium run
                && hv.mean_ev() > 0.0,                // honest-equilibrium run
        });
    }
    out
}

/// Founder-readable corner table + the machine-greppable verdict line.
pub fn table(results: &[CornerResult]) -> String {
    let mut s = String::new();
    // Caption (main.rs sweeps classes[0] = guest; update caption if sweep order changes)
    s.push_str("sweep class: guest (headline; light/heavy validated via fixtures)\n");
    s.push_str("legend: hx/hv = honest executor/verifier EV (honest-equilibrium run); cheat/lazy/rstamp = mixed-run EVs; v_bond = per-verifier stake at the formula minimum\n");
    s.push_str(&format!(
        "{:<26} {:>9} {:>9} {:>11} {:>11} {:>11} {:>11} {:>11} {:>6}\n",
        "corner (s/t/v/k/cap)", "budget", "v_bond", "hx_ev", "hv_ev", "cheat", "lazy", "rstamp", "SAFE?"
    ));
    s.push_str(&format!("{}\n", "-".repeat(112)));
    for r in results {
        let cap = if r.corner.tight_cap { "tight" } else { "100M" };
        s.push_str(&format!(
            "{:<26} {:>9} {:>9} {:>11.1} {:>11.1} {:>11.1} {:>11.1} {:>11.1} {:>6}\n",
            format!("{}/{}/{}/{}/{}", r.corner.s_bps, r.corner.t_bps, r.corner.v_bps, r.corner.k, cap),
            r.budget,
            r.verifier_bond,
            r.honest_executor_ev,
            r.honest_verifier_ev,
            r.cheat_executor_ev,
            r.lazy_executor_ev,
            r.rubber_stamp_ev,
            if r.safe { "yes" } else { "NO" },
        ));
    }
    let n = results.iter().filter(|r| r.safe).count();
    if n >= 1 {
        s.push_str(&format!(
            "\nREAL-FUEL ECONOMICS: {n} safe corners — HONEST WORK PROFITABLE, CHEATING LOSES MONEY\n"
        ));
    } else {
        s.push_str("\nREAL-FUEL ECONOMICS: NO SAFE CORNER — every swept regime fails; see table\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep machinery runs end-to-end on a 2-corner grid; determinism
    /// pinned; honest-equilibrium population has zero rubber-stamp plays
    /// (the jackpot-exclusion property, structurally).
    #[test]
    fn sweep_smoke_and_jackpot_exclusion() {
        let classes = measure_classes();
        let grid = vec![
            Corner { s_bps: 5_000, t_bps: 2_500, v_bps: 4_000, k: 3, tight_cap: true },
            Corner { s_bps: 2_500, t_bps: 1_000, v_bps: 1_000, k: 3, tight_cap: false },
        ];
        let results = run_sweep(&grid, &classes[0], 500, 7);
        assert_eq!(results.len(), 2);
        // determinism: same seed, same result
        let again = run_sweep(&grid, &classes[0], 500, 7);
        assert_eq!(results[0].honest_verifier_ev, again[0].honest_verifier_ev);

        // The jackpot-exclusion property, asserted (not just claimed): an
        // all-honest population never seats a rubber-stamper.
        let p = {
            let mut p = corner_params(&grid[0], 1_000);
            p.executor_bond = 4_000;
            p.verifier_bond = 2_000;
            p
        };
        let (_, _, _, _, rs) = run_population(200, &p, 4_000, 1_000, false, 7);
        assert_eq!(rs.plays, 0, "all-honest population must never seat a rubber-stamper");
    }
}
