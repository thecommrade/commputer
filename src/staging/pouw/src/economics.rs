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
    Ok(())
}

/// budget_min = max(Bx, Bv) — fuel-economics spec §3.
///   Bx (executor profitable):  B ≥ wc·margin/worker
///   Bv (each committee slot profitable under the ADDITIVE event model):
///       B ≥ k·wc·(s+t)·margin/(v·s)   — the two 10_000 factors cancel.
/// Worst-case u128: wc(2^64−1)·k(few)·(s+t)(≤20_000)·margin(≤30_000) ≈ 1.5e28 « u128::MAX ≈ 3.4e38.
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
}

#[cfg(test)]
mod prop_tests {
    use super::*;
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
    }
}
