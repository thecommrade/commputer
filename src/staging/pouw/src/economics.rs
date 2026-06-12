//! Fuel-priced economics for the verification game (fuel-economics spec §3/§4).
//! What: pricing formulas (work_cost, budget_min, bonds), the degenerate-params
//! guard, validate_economics, and the run_priced_job enforcement wrapper.
//! Wired in: src/lib.rs (`pub mod economics`). The game (engine.rs, settlement.rs)
//! is byte-identical — enforcement is a NEW entry point delegating to run_job.
//! Spec: src/staging/docs/2026-06-11-fuel-economics-design.md

use crate::params::GameParams;

/// One mega-fuel: the pricing granularity (spec §2 — keeps integer math at test scale).
const MFUEL: u64 = 1_000_000;
const BPS: u128 = 10_000;

/// Token cost of `fuel_cap` fuel at `price_per_mfuel` (ceil to whole mega-fuels).
/// u128 intermediates, ONE final saturation (spec §3 integer rule).
/// (Why one saturation: a saturating-u64 CHAIN under-prices — a saturated numerator divides back down. Over-strict is safe; lenient is not. Consensus-destined rule, frozen — spec §3.)
pub fn work_cost(fuel_cap: u64, price_per_mfuel: u64) -> u64 {
    let mfuels = fuel_cap.div_ceil(MFUEL) as u128;
    sat64(mfuels * price_per_mfuel as u128)
}

fn sat64(x: u128) -> u64 {
    x.try_into().unwrap_or(u64::MAX)
}

/// ceil(n/d). Caller guarantees d > 0 (the guard refuses zero divisors).
fn ceil_div(n: u128, d: u128) -> u128 {
    n.div_ceil(d)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconViolation {
    BudgetBelowMin { budget: u64, min: u64 },
    ExecutorBondBelowMin { bond: u64, min: u64 },
    VerifierBondBelowMin { bond: u64, min: u64 },
    /// Degenerate pricing params (spec §3 guard: zero s/t/worker/verifier bps or
    /// k == 0) — NOT merely a GameParams::validate() failure; validate() permits
    /// these (the unpriced game legally simulates them), pricing refuses them.
    BadParams(&'static str),
}

/// The spec §3 degenerate-params guard: a job cannot be PRICED in a regime with
/// no proactive catching, no traps, no paid role, or no committee.
/// Also enforces semantic upper bounds to prevent u128 wrap in budget_min /
/// *_bond_min (probability bps capped at 10_000; multiplier fields capped at
/// 100_000 / 1_000 respectively).
fn guard(p: &GameParams) -> Result<(), EconViolation> {
    if p.sample_rate_bps == 0 {
        return Err(EconViolation::BadParams("sample_rate_bps == 0 (no proactive catching)"));
    }
    if p.p_trap_bps == 0 {
        return Err(EconViolation::BadParams("p_trap_bps == 0 (no trap policing)"));
    }
    if p.worker_bps == 0 {
        return Err(EconViolation::BadParams("worker_bps == 0 (executor unpaid)"));
    }
    if p.verifier_bps == 0 {
        return Err(EconViolation::BadParams("verifier_bps == 0 (verifiers unpaid)"));
    }
    if p.k == 0 {
        return Err(EconViolation::BadParams("k == 0 (no committee)"));
    }
    if p.sample_rate_bps > 10_000 {
        return Err(EconViolation::BadParams("sample_rate_bps > 10_000 (probability > 1)"));
    }
    if p.p_trap_bps > 10_000 {
        return Err(EconViolation::BadParams("p_trap_bps > 10_000 (probability > 1)"));
    }
    if p.profit_margin_bps > 100_000 {
        return Err(EconViolation::BadParams("profit_margin_bps > 100_000 (10x cap, overflow bound)"));
    }
    if p.bond_safety_bps > 100_000 {
        return Err(EconViolation::BadParams("bond_safety_bps > 100_000 (10x cap, overflow bound)"));
    }
    if p.k > 1_000 {
        return Err(EconViolation::BadParams("k > 1_000 (overflow bound)"));
    }
    Ok(())
}

/// budget_min = max(Bx, Bv) — fuel-economics spec §3.
///   Bx (executor profitable):  B ≥ wc·margin/worker
///   Bv (each committee slot profitable under the ADDITIVE event model):
///       B ≥ k·wc·(s+t)·margin/(v·s)   — the two 10_000 factors cancel.
/// Worst-case u128 (bounds ENFORCED by guard): wc(2^64−1)·k(≤1e3)·(s+t)(≤2e4)·margin(≤1e5) ≈ 3.7e31 « u128::MAX ≈ 3.4e38.
pub fn budget_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let wc = work_cost(fuel_cap, p.price_per_mfuel) as u128;
    let (s, t) = (p.sample_rate_bps as u128, p.p_trap_bps as u128);
    let margin = p.profit_margin_bps as u128;
    let bx = ceil_div(wc * margin, p.worker_bps as u128);
    let bv = ceil_div(
        p.k as u128 * wc * (s + t) * margin,
        p.verifier_bps as u128 * s,
    );
    Ok(sat64(bx.max(bv)))
}

/// max(ceil(wc·safety/s), budget) — the parent-spec Be ≥ B rule preserved; under
/// the additive model the executor's catch rate is exactly s. For k ≥ 2 the
/// formula term never binds against the Bv-driven budget (spec §3 note).
pub fn executor_bond_min(fuel_cap: u64, budget: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let wc = work_cost(fuel_cap, p.price_per_mfuel) as u128;
    let formula = ceil_div(wc * p.bond_safety_bps as u128, p.sample_rate_bps as u128);
    Ok(sat64(formula.max(budget as u128)))
}

/// ceil(wc·safety·(s+t)/(10_000·t)) — a rubber-stamper saves a FULL re-execution
/// and is caught only on trap events (per-event probability t/(s+t)).
pub fn verifier_bond_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let wc = work_cost(fuel_cap, p.price_per_mfuel) as u128;
    let (s, t) = (p.sample_rate_bps as u128, p.p_trap_bps as u128);
    Ok(sat64(ceil_div(wc * p.bond_safety_bps as u128 * (s + t), BPS * t)))
}

use crate::engine::{run_job, JobInputs};
use crate::ids::ParticipantId;
use crate::job::{SettlementOutcome, Verdict};
use crate::oracle::{ChainHooks, EquivalenceOracle, ExecutionOracle};

/// Enforce the spec §3 minimums against a job's funding. Pure read — no ledger
/// access, no side effects; safe to call before any escrow.
pub fn validate_economics(
    inputs: &JobInputs,
    fuel_cap: u64,
    p: &GameParams,
) -> Result<(), EconViolation> {
    let min_budget = budget_min(fuel_cap, p)?;
    if inputs.job.budget < min_budget {
        return Err(EconViolation::BudgetBelowMin { budget: inputs.job.budget, min: min_budget });
    }
    let min_e = executor_bond_min(fuel_cap, inputs.job.budget, p)?;
    if inputs.executor_bond < min_e {
        return Err(EconViolation::ExecutorBondBelowMin { bond: inputs.executor_bond, min: min_e });
    }
    let min_v = verifier_bond_min(fuel_cap, p)?;
    if inputs.verifier_bond < min_v {
        return Err(EconViolation::VerifierBondBelowMin { bond: inputs.verifier_bond, min: min_v });
    }
    Ok(())
}

/// THE enforcement surface (spec §4): validate, then delegate to the
/// byte-identical engine::run_job. The on-chain cycle wires the real submit
/// path to this same check; engine::run_job remains the unpriced core.
#[allow(clippy::too_many_arguments)]
pub fn run_priced_job(
    l: &mut dyn ChainHooks,
    p: &GameParams,
    inputs: &JobInputs,
    fuel_cap: u64,
    exec_oracle: &dyn ExecutionOracle,
    eq: &dyn EquivalenceOracle,
    stake_of: &dyn Fn(&ParticipantId) -> u64,
    rng: &mut dyn rand::RngCore,
) -> Result<(Verdict, SettlementOutcome), EconViolation> {
    validate_economics(inputs, fuel_cap, p)?;
    Ok(run_job(l, p, inputs, exec_oracle, eq, stake_of, rng))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::GameParams;

    #[test]
    fn work_cost_ceiling_edges() {
        assert_eq!(work_cost(0, 1), 0);
        assert_eq!(work_cost(1, 1), 1);                 // ceil(1/1M) = 1 mfuel
        assert_eq!(work_cost(999_999, 1), 1);           // spec §7 lists the 1M−1 edge explicitly
        assert_eq!(work_cost(1_000_000, 1), 1);
        assert_eq!(work_cost(1_000_001, 1), 2);
        assert_eq!(work_cost(100_000_000, 1), 100);     // the default cap
        assert_eq!(work_cost(2_000_000, 100), 200);
        // saturation: huge price × huge fuel exceeds u64 → clamps high (over-strict).
        assert_eq!(work_cost(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn degenerate_params_are_refused_not_panicked() {
        let zero = |f: fn(&mut GameParams)| {
            let mut p = GameParams::default();
            f(&mut p);
            p
        };
        let cases: [(GameParams, &str); 5] = [
            (zero(|p| p.sample_rate_bps = 0), "sample"),
            (zero(|p| p.p_trap_bps = 0), "trap"),
            (zero(|p| p.worker_bps = 0), "worker"),
            (zero(|p| p.verifier_bps = 0), "verifier"),
            (zero(|p| p.k = 0), "k"),
        ];
        for (p, what) in cases {
            for r in [
                budget_min(100_000_000, &p),
                executor_bond_min(100_000_000, 100, &p),
                verifier_bond_min(100_000_000, &p),
            ] {
                match r {
                    Err(EconViolation::BadParams(msg)) => {
                        assert!(msg.contains(what), "{what}: got {msg:?}")
                    }
                    other => panic!("{what}: expected BadParams, got {other:?}"),
                }
            }
        }
    }

    /// Hand-verified at the spec's defaults: F=100M, price=1 ⇒ wc=100;
    /// k=3, s=10_000, t=1_000, v=1_000, w=8_500, margin=12_000, safety=15_000.
    #[test]
    fn formulas_at_spec_defaults() {
        let p = GameParams::default();
        let f = 100_000_000u64;
        // Bx = ceil(100·12_000/8_500) = 142;  Bv = ceil(3·100·11_000·12_000/(1_000·10_000)) = 3_960
        assert_eq!(budget_min(f, &p), Ok(3_960));
        // formula term ceil(100·15_000/10_000)=150 never binds vs budget (spec §3 note)
        assert_eq!(executor_bond_min(f, 3_960, &p), Ok(3_960));
        assert_eq!(executor_bond_min(f, 100, &p), Ok(150)); // formula binds only when budget < it
        // ceil(100·15_000·11_000/(10_000·1_000)) = 1_650
        assert_eq!(verifier_bond_min(f, &p), Ok(1_650));
    }

    /// Spec §3 integer rule: never under-price at extremes. Every *_min must be
    /// >= the saturated work_cost — u128 intermediates with ONE final saturation
    /// (a saturating-u64 chain would collapse the numerator and under-price).
    #[test]
    fn no_underpricing_at_extremes() {
        let mut p = GameParams::default();
        p.price_per_mfuel = u64::MAX;
        let f = u64::MAX;
        let wc = work_cost(f, p.price_per_mfuel); // saturates to u64::MAX
        assert_eq!(wc, u64::MAX);
        assert!(budget_min(f, &p).unwrap() >= wc);
        assert!(executor_bond_min(f, 0, &p).unwrap() >= wc);
        assert!(verifier_bond_min(f, &p).unwrap() >= wc);
    }

    /// Bx can bind when the worker slice is thin: with v=9_000, w=500,
    /// Bv = ceil(3·100·11_000·12_000/(9_000·10_000)) = 440 and
    /// Bx = ceil(100·12_000/500) = 2_400 ⇒ budget_min = 2_400.
    #[test]
    fn bx_binds_when_worker_slice_is_thin() {
        let mut p = GameParams::default();
        p.worker_bps = 500;
        p.verifier_bps = 9_000;
        p.burn_bps = 500;
        assert_eq!(budget_min(100_000_000, &p), Ok(2_400));
    }

    /// Upper-bound guard rejects each over-limit field with the correct needle
    /// substring (spec §3 overflow-bound enforcement).
    #[test]
    fn upper_bound_params_are_refused() {
        let over = |f: fn(&mut GameParams)| {
            let mut p = GameParams::default();
            f(&mut p);
            p
        };
        let cases: [(GameParams, &str); 5] = [
            (over(|p| p.sample_rate_bps = 10_001), "sample_rate_bps >"),
            (over(|p| p.p_trap_bps = 10_001),      "p_trap_bps >"),
            (over(|p| p.profit_margin_bps = 100_001), "profit_margin_bps >"),
            (over(|p| p.bond_safety_bps = 100_001),   "bond_safety_bps >"),
            (over(|p| p.k = 1_001),                   "k >"),
        ];
        for (p, needle) in cases {
            for r in [
                budget_min(100_000_000, &p),
                executor_bond_min(100_000_000, 100, &p),
                verifier_bond_min(100_000_000, &p),
            ] {
                match r {
                    Err(EconViolation::BadParams(msg)) => {
                        assert!(msg.contains(needle), "{needle}: got {msg:?}")
                    }
                    other => panic!("{needle}: expected BadParams, got {other:?}"),
                }
            }
        }
    }

    use crate::engine::{run_job, JobInputs};
    use crate::ids::{JobId, ParticipantId};
    use crate::job::{Job, JobSpec, Verdict};
    use crate::oracle::{ByteEq, IteratedHashVm, Ledger};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// A fully-funded-at-minimum world for the default params.
    fn priced_world() -> (GameParams, Ledger, ParticipantId, ParticipantId, Vec<ParticipantId>, u64, u64, u64) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap();                // 3_960
        let e_bond = executor_bond_min(f, budget, &p).unwrap(); // 3_960
        let v_bond = verifier_bond_min(f, &p).unwrap();         // 1_650
        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();
        let mut l = Ledger::new();
        l.credit(submitter, budget);
        l.credit(executor, e_bond);
        for c in &candidates {
            l.credit(*c, v_bond);
        }
        (p, l, submitter, executor, candidates, budget, e_bond, v_bond)
    }

    fn job_for(submitter: ParticipantId, budget: u64) -> Job {
        let spec = JobSpec { program_hash: [7; 32], input_hash: [9; 32] };
        Job { id: JobId::derive(&[7; 32], &[9; 32], &submitter, 0), submitter, spec, budget }
    }

    #[test]
    fn underfunded_job_rejected_with_zero_side_effects() {
        let (p, mut l, submitter, executor, candidates, budget, e_bond, v_bond) = priced_world();
        let total0 = l.total_supply();
        let bal0 = l.balance_of(&submitter);

        let job = job_for(submitter, budget - 1); // one below minimum
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: b"in",
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let vm = IteratedHashVm { rounds: 10 };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(1);

        let r = run_priced_job(&mut l, &p, &inputs, 100_000_000, &vm, &ByteEq, &stake, &mut rng);
        assert_eq!(
            r.unwrap_err(),
            EconViolation::BudgetBelowMin { budget: budget - 1, min: budget }
        );
        // The check runs BEFORE any escrow: nothing moved.
        assert_eq!(l.total_supply(), total0);
        assert_eq!(l.balance_of(&submitter), bal0);
        assert_eq!(l.escrowed(), 0);
    }

    #[test]
    fn each_violation_variant_fires() {
        let (p, mut l, submitter, executor, candidates, budget, e_bond, v_bond) = priced_world();
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let mk = |budget: u64, e_bond: u64, v_bond: u64| JobInputs {
            job: job_for(submitter, budget),
            input: b"in",
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let vm = IteratedHashVm { rounds: 10 };
        let stake = |_: &ParticipantId| 1u64;
        let f = 100_000_000;

        let mut rng = StdRng::seed_from_u64(1);
        let r = run_priced_job(&mut l, &p, &mk(budget, e_bond - 1, v_bond), f, &vm, &ByteEq, &stake, &mut rng);
        assert!(matches!(r, Err(EconViolation::ExecutorBondBelowMin { .. })));

        let r = run_priced_job(&mut l, &p, &mk(budget, e_bond, v_bond - 1), f, &vm, &ByteEq, &stake, &mut rng);
        assert!(matches!(r, Err(EconViolation::VerifierBondBelowMin { .. })));

        let mut bad = p.clone();
        bad.p_trap_bps = 0;
        let r = run_priced_job(&mut l, &bad, &mk(budget, e_bond, v_bond), f, &vm, &ByteEq, &stake, &mut rng);
        assert!(matches!(r, Err(EconViolation::BadParams(_))));
    }

    /// At-minimum funding passes, and run_priced_job is EXACTLY run_job after the
    /// check: same seed ⇒ same verdict + same settlement outcome.
    #[test]
    fn priced_run_is_bare_run_after_the_check() {
        let (p, _l0, submitter, executor, candidates, budget, e_bond, v_bond) = priced_world();
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let inputs = JobInputs {
            job: job_for(submitter, budget),
            input: b"in",
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let vm = IteratedHashVm { rounds: 10 };
        let stake = |_: &ParticipantId| 1u64;

        // Ledger does NOT implement Clone (game file, untouchable): build two
        // identical worlds from the deterministic helper instead.
        let (_, mut l_priced, ..) = priced_world();
        let mut rng = StdRng::seed_from_u64(42);
        let (v1, o1) = run_priced_job(&mut l_priced, &p, &inputs, 100_000_000, &vm, &ByteEq, &stake, &mut rng)
            .expect("at-minimum funding must pass");

        let (_, mut l_bare, ..) = priced_world();
        let mut rng = StdRng::seed_from_u64(42);
        let (v2, o2) = run_job(&mut l_bare, &p, &inputs, &vm, &ByteEq, &stake, &mut rng);

        assert!(matches!(v1, Verdict::Confirmed { .. }));
        assert_eq!(format!("{v1:?}"), format!("{v2:?}"));
        assert_eq!(o1, o2);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::engine::JobInputs;
    use crate::ids::{JobId, ParticipantId};
    use crate::job::{Job, JobSpec};
    use crate::params::GameParams;
    use proptest::prelude::*;

    /// Pricing-legal GameParams: nonzero divisors, valid split, strict margin.
    fn econ_params() -> impl Strategy<Value = GameParams> {
        (
            1u32..=10_000,        // sample_rate_bps
            1u32..=10_000,        // p_trap_bps
            1u32..=9_998,         // verifier_bps (leaves >=1 each for worker+burn)
            1usize..=9,           // k
            1u64..=1_000_000,     // price_per_mfuel
            10_001u32..=30_000,   // profit_margin_bps (strict)
            10_000u32..=30_000,   // bond_safety_bps
        )
            .prop_map(|(s, t, v, k, price, margin, safety)| {
                let worker = 10_000 - v - 1; // burn gets 1
                GameParams {
                    sample_rate_bps: s,
                    p_trap_bps: t,
                    worker_bps: worker,
                    verifier_bps: v,
                    burn_bps: 1,
                    k,
                    k_escalate: 2 * k + 1,
                    price_per_mfuel: price,
                    profit_margin_bps: margin,
                    bond_safety_bps: safety,
                    ..GameParams::default()
                }
            })
    }

    /// Mixed-scale fuel: ceil-boundary region, mid-scale, and full-range — so
    /// monotonicity is exercised where the math is live, not just at saturation.
    fn fuel() -> impl Strategy<Value = u64> {
        prop_oneof![
            0u64..=4 * 1_000_000,    // the MFUEL ceil-boundary region
            0u64..=1 << 40,          // mid-scale (no saturation at small prices)
            0u64..=u64::MAX,         // full range incl. the saturation regime
        ]
    }

    proptest! {
        /// Monotone in fuel AND in price (spec §7); minimums never panic on legal params.
        #[test]
        fn formulas_monotone_and_total(p in econ_params(), f1 in fuel(), f2 in fuel()) {
            prop_assert!(p.validate().is_ok(), "econ_params must generate validate()-legal params");
            let (lo, hi) = (f1.min(f2), f1.max(f2));
            prop_assert!(budget_min(lo, &p).unwrap() <= budget_min(hi, &p).unwrap());
            prop_assert!(verifier_bond_min(lo, &p).unwrap() <= verifier_bond_min(hi, &p).unwrap());
            prop_assert!(executor_bond_min(lo, 0, &p).unwrap() <= executor_bond_min(hi, 0, &p).unwrap());
            // price-monotonicity: same fuel, cheaper price never prices higher.
            let mut cheaper = p.clone();
            cheaper.price_per_mfuel = p.price_per_mfuel.saturating_sub(1).max(1);
            prop_assert!(budget_min(hi, &cheaper).unwrap() <= budget_min(hi, &p).unwrap());
            prop_assert!(verifier_bond_min(hi, &cheaper).unwrap() <= verifier_bond_min(hi, &p).unwrap());
            prop_assert!(executor_bond_min(hi, 0, &cheaper).unwrap() <= executor_bond_min(hi, 0, &p).unwrap());
        }

        /// budget_min dominates BOTH of its component constraints.
        #[test]
        fn budget_min_dominates_components(p in econ_params(), f in fuel()) {
            let wc = work_cost(f, p.price_per_mfuel) as u128;
            let bx = (wc * p.profit_margin_bps as u128).div_ceil(p.worker_bps as u128);
            let bv = (p.k as u128 * wc * (p.sample_rate_bps as u128 + p.p_trap_bps as u128)
                * p.profit_margin_bps as u128)
                .div_ceil(p.verifier_bps as u128 * p.sample_rate_bps as u128);
            let b = budget_min(f, &p).unwrap() as u128;
            // compare in saturated space: b is the saturated max of bx/bv
            prop_assert!(b >= bx.min(u64::MAX as u128));
            prop_assert!(b >= bv.min(u64::MAX as u128));
        }

        /// Degenerate params NEVER panic — they error (each zeroed divisor).
        #[test]
        fn degenerate_params_error_not_panic(f in 0u64..=u64::MAX, which in 0usize..5) {
            let mut p = GameParams::default();
            match which {
                0 => p.sample_rate_bps = 0,
                1 => p.p_trap_bps = 0,
                2 => p.worker_bps = 0,
                3 => p.verifier_bps = 0,
                _ => p.k = 0,
            }
            prop_assert!(matches!(budget_min(f, &p), Err(EconViolation::BadParams(_))));
            prop_assert!(matches!(executor_bond_min(f, 0, &p), Err(EconViolation::BadParams(_))));
            prop_assert!(matches!(verifier_bond_min(f, &p), Err(EconViolation::BadParams(_))));
        }

        /// Funded-at-minimum always passes validate_economics; any single
        /// component one-below-minimum fails with the MATCHING variant — over
        /// the whole legal regime space, not just the defaults (spec §7).
        #[test]
        fn minimum_funding_is_the_exact_boundary(p in econ_params(), f in fuel()) {
            let b = budget_min(f, &p).unwrap();
            let eb = executor_bond_min(f, b, &p).unwrap();
            let vb = verifier_bond_min(f, &p).unwrap();
            prop_assert!(eb >= b, "Be >= B must hold by the max(…, budget) construction");

            let claim = |h: &[u8; 32]| *h;
            let reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
            let challenge = |_: &[u8; 32], _: &[u8; 32]| None;
            let candidates: Vec<ParticipantId> =
                (10u8..30).map(|n| ParticipantId([n; 32])).collect();
            let submitter = ParticipantId([0; 32]);
            let mk = |budget: u64, e_bond: u64, v_bond: u64| JobInputs {
                job: Job {
                    id: JobId::derive(&[7; 32], &[9; 32], &submitter, 0),
                    submitter,
                    spec: JobSpec { program_hash: [7; 32], input_hash: [9; 32] },
                    budget,
                },
                input: b"in",
                executor: ParticipantId([9; 32]),
                executor_bond: e_bond,
                executor_claim: &claim,
                candidates: &candidates,
                verifier_bond: v_bond,
                verifier_reveal: &reveal,
                challenge: &challenge,
                challenger_bond: p.challenger_bond,
            };

            prop_assert!(validate_economics(&mk(b, eb, vb), f, &p).is_ok());
            if b > 0 {
                let r = validate_economics(&mk(b - 1, eb, vb), f, &p);
                let ok = matches!(r, Err(EconViolation::BudgetBelowMin { .. }));
                prop_assert!(ok, "expected BudgetBelowMin, got {:?}", r);
            }
            if eb > 0 {
                let r = validate_economics(&mk(b, eb - 1, vb), f, &p);
                let ok = matches!(r, Err(EconViolation::ExecutorBondBelowMin { .. }));
                prop_assert!(ok, "expected ExecutorBondBelowMin, got {:?}", r);
            }
            if vb > 0 {
                let r = validate_economics(&mk(b, eb, vb - 1), f, &p);
                let ok = matches!(r, Err(EconViolation::VerifierBondBelowMin { .. }));
                prop_assert!(ok, "expected VerifierBondBelowMin, got {:?}", r);
            }
        }
    }
}
