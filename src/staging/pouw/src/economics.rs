//! Fuel-priced economics for the verification game (fuel-economics spec §3/§4).
//! What: pricing formulas (work_cost, budget_min, bonds), the degenerate-params
//! guard, validate_economics, and the run_priced_job enforcement wrapper.
//! Wired in: src/lib.rs (`pub mod economics`). The game (engine.rs, settlement.rs)
//! is byte-identical — enforcement is a NEW entry point delegating to run_job.
//! Spec: src/staging/docs/2026-06-11-fuel-economics-design.md

use crate::params::GameParams;

/// One mega-fuel: the pricing granularity (spec §2 — keeps integer math at test scale).
const MFUEL: u64 = 1_000_000;
#[allow(dead_code)] // consumed by Task 3
const BPS: u128 = 10_000;

/// Token cost of `fuel_cap` fuel at `price_per_mfuel` (ceil to whole mega-fuels).
/// u128 intermediates, ONE final saturation (spec §3 integer rule).
pub fn work_cost(fuel_cap: u64, price_per_mfuel: u64) -> u64 {
    let mfuels = fuel_cap.div_ceil(MFUEL) as u128;
    sat64(mfuels * price_per_mfuel as u128)
}

fn sat64(x: u128) -> u64 {
    x.try_into().unwrap_or(u64::MAX)
}

/// ceil(n/d). Caller guarantees d > 0 (the guard refuses zero divisors).
#[allow(dead_code)] // consumed by Task 3
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

pub fn budget_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let _ = fuel_cap;
    unimplemented!("fuel-econ Task 3") // guard test only exercises the Err path
}

pub fn executor_bond_min(fuel_cap: u64, budget: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let _ = (fuel_cap, budget);
    unimplemented!("fuel-econ Task 3")
}

pub fn verifier_bond_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let _ = fuel_cap;
    unimplemented!("fuel-econ Task 3")
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
    fn degenerate_params_are_refused_not_paniced() {
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
}
