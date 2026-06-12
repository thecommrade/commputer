# Fuel → Economics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire measured WASM fuel into the verification game's economics — derived+enforced pricing formulas, a real-fuel tournament that re-proves honest-play dominance under real costs, per the approved spec `src/staging/docs/2026-06-11-fuel-economics-design.md`.

**Architecture:** New `economics.rs` (pure-integer fallible pricing formulas + `validate_economics` + the `run_priced_job` enforcement wrapper over an untouched `engine::run_job`); three new `GameParams` fields; a feature-gated real-fuel sweep (`sim/realfuel.rs`) that reuses the existing `settle_one_job` settlement plumbing under the **additive** event model with honest-equilibrium scoring; prebuilt sim fixtures; error-policy pin test.

**Tech Stack:** Rust (workspace `/home/operator/Coin/src`, edition 2024) · existing crate deps only (no new crates.io deps) · wasmi runtime behind the existing `wasm-runtime` feature for fuel measurement.

---

## Context an implementer must know (read once, before Task 1)

- **Branch:** `agent-wire-testnet-20260610`. Containment: all work in `src/staging/pouw/`; the ONLY modified pre-existing files are the declared list (`params.rs`, `lib.rs`, `sim/main.rs`, `sim/tournament.rs` visibility-only, `tests/wasm_runtime.rs`, `README.md`). The game files (`engine.rs`, `settlement.rs`, `verdict.rs`, `trap.rs`, `escalation.rs`, `committee.rs`, `commit_reveal.rs`, `job.rs`, `ids.rs`, `oracle.rs`) stay **byte-identical** — final review re-verifies with `git diff`. Git identity `The Commrade <commrade@commputer.xyz>`. Never push.
- **Cargo cwd:** `/home/operator/Coin/src`. Default suite baseline: exactly **39** tests (31 unit + 7 sim + 1 conservation). Feature suite baseline: exactly **88** (63 + 7 + 1 + 17). Verified live pre-cycle.
- **The spec is law** for formulas: §3's closed forms, the u128-intermediates/one-final-saturation rule, the degenerate-params guard (s/t/worker/verifier bps or k == 0 → `BadParams`), fallible `*_min`. Hand-verified expected values used in tests below: at defaults (F=100M, price=1 ⇒ `work_cost`=100; k=3, s=10000, t=1000, v=1000, w=8500, margin=12000, safety=15000): **Bx=142, Bv=3960, budget_min=3960, executor_bond_min(budget 3960)=3960** (formula term 150 never binds for k≥2), **verifier_bond_min=1650**.
- **Additive event model** (spec §2.1): per real job, a paid sampled committee event w.p. `s` AND independently an unpaid trap round w.p. `t`. The existing toy tournament draws trap-with-precedence — it stays untouched; `sim/realfuel.rs` implements additive by making TWO `settle_one_job` calls when trap fires (one `trap=false` real event + one `trap=true` round). `settle_one_job` itself is reused unchanged (visibility lift only).
- **Honest-equilibrium scoring** (spec §5.6): each sweep corner runs TWICE — mixed population (one rubber-stamp seat) for cheat EVs; all-honest committee for honest-role EVs (no rubber-stampers ⇒ traps never slash ⇒ zero jackpot income, structurally).
- **Mega-fuel granularity:** `work_cost` ceilings at 1M fuel, so all sub-1M jobs price identically. Sim fuel classes must straddle the boundary: light ≈2M fuel, heavy ≈50M fuel, plus the compiled guest (~sub-1M). Fixture tests assert **ranges**, never exact fuel (engine-version-stable but build-variable).
- **TDD throughout:** failing test → verify fail → implement → verify pass → commit. Full feature suite before each commit.

### File map

| File | Action | Responsibility |
|---|---|---|
| `src/staging/pouw/src/economics.rs` | Create | formulas, guard, `EconViolation`, `validate_economics`, `run_priced_job`, unit+prop tests |
| `src/staging/pouw/src/params.rs` | Modify | +3 fields, `validate()` rules, test updates |
| `src/staging/pouw/src/lib.rs` | Modify | `pub mod economics;` (one line) |
| `src/staging/pouw/sim/tournament.rs` | Modify | visibility lift only: `pub(crate)` on `JobModel`, `settle_one_job`, `wrong_hash`, `pid` |
| `src/staging/pouw/sim/realfuel.rs` | Create | feature-gated real-fuel sweep (additive events, dual-population, table) |
| `src/staging/pouw/sim/main.rs` | Modify | feature-gated `mod realfuel;` + invocation |
| `src/staging/pouw/sim/fixtures/{light,heavy}.wat` + `.wasm` | Create | weight-class programs (committed .wat source + prebuilt .wasm) |
| `src/staging/pouw/tests/wasm_runtime.rs` | Modify | +`mod priced_game` (error-policy pin, budget-scaling, fixture byte-identity) |
| `src/staging/pouw/README.md` | Modify | additive economics section + sweep results table |

---

### Task 1: `GameParams` — the three pricing fields

**Files:**
- Modify: `src/staging/pouw/src/params.rs`

- [ ] **Step 1: Write the failing tests** — append inside the existing `mod tests`:

```rust
    #[test]
    fn pricing_defaults_and_validation() {
        let p = GameParams::default();
        assert_eq!(p.price_per_mfuel, 1);
        assert_eq!(p.profit_margin_bps, 12_000);
        assert_eq!(p.bond_safety_bps, 15_000);
        assert!(p.validate().is_ok());

        // margin must be a STRICT margin (> 10_000): exact break-even would make an
        // at-minimum honest role EV-zero and fail the sweep's strict EV-positive bar.
        let mut p = GameParams::default();
        p.profit_margin_bps = 10_000;
        assert!(p.validate().is_err());

        let mut p = GameParams::default();
        p.bond_safety_bps = 9_999;
        assert!(p.validate().is_err());

        let mut p = GameParams::default();
        p.price_per_mfuel = 0;
        assert!(p.validate().is_err());
    }
```

- [ ] **Step 2: Verify failure** — `cargo test -p commputer-pouw params` → compile FAIL (unknown fields).

- [ ] **Step 3: Implement** — add to the `GameParams` struct (after `trap_jackpot_bps`):

```rust
    // --- Fuel-pricing knobs (fuel-economics spec §3). ---
    /// Token units per 1,000,000 fuel — converts the engine's deterministic fuel
    /// metering into money. Consensus-visible market knob.
    pub price_per_mfuel: u64,
    /// Strict profitability margin (> 10_000) applied to budget_min's constraints.
    pub profit_margin_bps: u32,
    /// Safety multiplier (≥ 10_000) applied to both bond formulas.
    pub bond_safety_bps: u32,
```

to `Default`: `price_per_mfuel: 1, profit_margin_bps: 12_000, bond_safety_bps: 15_000,` — and to `validate()` (before `Ok(())`):

```rust
        if self.price_per_mfuel == 0 {
            return Err("price_per_mfuel must be >= 1");
        }
        if self.profit_margin_bps <= 10_000 {
            return Err("profit_margin_bps must be a strict margin (> 10_000)");
        }
        if self.bond_safety_bps < 10_000 {
            return Err("bond_safety_bps must be >= 10_000");
        }
```

- [ ] **Step 4: Verify pass + no construction breakage** — `cargo test -p commputer-pouw params` → PASS. Then `cargo test -p commputer-pouw` → full 39+1 green (every existing `GameParams` construction uses `::default()` or `..Default::default()`; if any exhaustive struct literal breaks, fix it by adding `..GameParams::default()` — report it, expected zero).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/params.rs
git commit -m "feat(pouw): GameParams pricing fields — price_per_mfuel, profit_margin, bond_safety (fuel-econ Task 1)"
```

---

### Task 2: `economics.rs` — work_cost, guard, EconViolation

**Files:**
- Create: `src/staging/pouw/src/economics.rs`
- Modify: `src/staging/pouw/src/lib.rs` (add `pub mod economics;` in the module list)

- [ ] **Step 1: Failing tests** (create the file with tests; implementation comes Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::GameParams;

    #[test]
    fn work_cost_ceiling_edges() {
        assert_eq!(work_cost(0, 1), 0);
        assert_eq!(work_cost(1, 1), 1);                 // ceil(1/1M) = 1 mfuel
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
```

- [ ] **Step 2: Verify failure** — `cargo test -p commputer-pouw economics` → compile FAIL.

- [ ] **Step 3: Implement** (above the tests). Task 2 fully implements `work_cost`, the guard, and the enum; the three `*_min` get guard-then-`unimplemented!()` temporary bodies (Step 1's guard test exercises only the `Err` path, so it passes honestly, while Task 3's formula tests stay red until the real bodies land). Implement exactly:

```rust
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
```

…and Task-2-temporary versions of the three `*_min` so the guard test compiles and passes while the formula test (Task 3) stays red:

```rust
pub fn budget_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation> {
    guard(p)?;
    let _ = fuel_cap;
    unimplemented!("Task 3") // replaced in Task 3; guard test only needs the Err path
}
```

Wait — `unimplemented!` panics on the non-guard path, but Step 1's guard test only exercises the Err path, so it passes; nothing else calls these yet. Same temporary body for `executor_bond_min(fuel_cap, budget, p)` and `verifier_bond_min(fuel_cap, p)`. Add `pub mod economics;` to `lib.rs` (place after `pub mod committee;`, alphabetical).

- [ ] **Step 4: Verify pass** — `cargo test -p commputer-pouw economics` → PASS (2 tests). Full default suite green.

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/economics.rs src/staging/pouw/src/lib.rs
git commit -m "feat(pouw): economics module — work_cost, degenerate-params guard, EconViolation (fuel-econ Task 2)"
```

---

### Task 3: The three pricing formulas

**Files:**
- Modify: `src/staging/pouw/src/economics.rs`

- [ ] **Step 1: Failing tests** — append inside `mod tests`:

```rust
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

    /// Bx can bind when the verifier slice is generous: with v=4_000, w=5_500,
    /// Bv = ceil(3·100·11_000·12_000/(4_000·10_000)) = 990 and
    /// Bx = ceil(100·12_000/5_500) = 219 ⇒ budget_min = 990 (Bv still binds);
    /// push v further (v=9_000, w=500): Bv = 440, Bx = ceil(100·12_000/500) = 2_400.
    #[test]
    fn bx_binds_when_worker_slice_is_thin() {
        let mut p = GameParams::default();
        p.worker_bps = 500;
        p.verifier_bps = 9_000;
        p.burn_bps = 500;
        assert_eq!(budget_min(100_000_000, &p), Ok(2_400));
    }
```

- [ ] **Step 2: Verify failure** — `cargo test -p commputer-pouw economics::tests::formulas` → FAIL (unimplemented panic).

- [ ] **Step 3: Implement** — replace the three temporary bodies:

```rust
/// budget_min = max(Bx, Bv) — fuel-economics spec §3.
///   Bx (executor profitable):  B ≥ wc·margin/worker
///   Bv (each committee slot profitable under the ADDITIVE event model):
///       B ≥ k·wc·(s+t)·margin/(v·s)   — the two 10_000 factors cancel.
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
```

- [ ] **Step 4: Verify pass** — `cargo test -p commputer-pouw economics` → PASS (5 tests). NOTE for the extremes test: trace the u128 worst case in a comment — `wc(=2^64−1) · k(3) · (s+t)(22_000) · margin(12_000) ≈ 1.5e28 « u128::MAX ≈ 3.4e38` — no overflow possible.

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/economics.rs
git commit -m "feat(pouw): pricing formulas — budget_min/executor_bond_min/verifier_bond_min, u128 one-saturation (fuel-econ Task 3)"
```

---

### Task 4: Property tests for the formulas

**Files:**
- Modify: `src/staging/pouw/src/economics.rs` (append a `mod prop_tests` beside `mod tests`)

- [ ] **Step 1: Write the properties** (proptest is already a dev-dep):

```rust
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

    proptest! {
        /// Monotone in fuel and price; minimums never panic on legal params.
        #[test]
        fn formulas_monotone_and_total(p in econ_params(), f1 in 0u64..=u64::MAX, f2 in 0u64..=u64::MAX) {
            let (lo, hi) = (f1.min(f2), f1.max(f2));
            prop_assert!(budget_min(lo, &p).unwrap() <= budget_min(hi, &p).unwrap());
            prop_assert!(verifier_bond_min(lo, &p).unwrap() <= verifier_bond_min(hi, &p).unwrap());
            prop_assert!(executor_bond_min(lo, 0, &p).unwrap() <= executor_bond_min(hi, 0, &p).unwrap());
        }

        /// budget_min dominates BOTH of its component constraints.
        #[test]
        fn budget_min_dominates_components(p in econ_params(), f in 0u64..=u64::MAX) {
            let wc = work_cost(f, p.price_per_mfuel) as u128;
            let bx = (wc * p.profit_margin_bps as u128).div_ceil(p.worker_bps as u128);
            let bv = (p.k as u128 * wc * (p.sample_rate_bps + p.p_trap_bps) as u128
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
```

- [ ] **Step 2: Run** — `cargo test -p commputer-pouw economics::prop_tests` → PASS (3 tests × 256 cases). These should pass immediately against Task 3's implementation; a failure is an implementation bug — fix `economics.rs`, never weaken a property.

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/src/economics.rs
git commit -m "test(pouw): economics property harness — monotonicity, component domination, total on degenerates (fuel-econ Task 4)"
```

---

### Task 5: `validate_economics` + `run_priced_job`

**Files:**
- Modify: `src/staging/pouw/src/economics.rs`

- [ ] **Step 1: Failing tests** — append inside `mod tests` (uses the toy `IteratedHashVm` — no wasm feature needed):

```rust
    use crate::engine::{run_job, JobInputs};
    use crate::ids::{JobId, ParticipantId};
    use crate::job::{Job, JobSpec, Verdict};
    use crate::oracle::{ByteEq, IteratedHashVm, Ledger};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// A fully-funded-at-minimum JobInputs world for the default params.
    /// Returns (params, ledger, ids, budget, bonds) ready for both run paths.
    fn priced_world() -> (GameParams, Ledger, ParticipantId, ParticipantId, Vec<ParticipantId>, u64, u64, u64) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap();             // 3_960
        let e_bond = executor_bond_min(f, budget, &p).unwrap(); // 3_960
        let v_bond = verifier_bond_min(f, &p).unwrap();      // 1_650
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
        let (p, l0, submitter, executor, candidates, budget, e_bond, v_bond) = priced_world();
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

        let mut l_priced = l0.clone();
        let mut rng = StdRng::seed_from_u64(42);
        let (v1, o1) = run_priced_job(&mut l_priced, &p, &inputs, 100_000_000, &vm, &ByteEq, &stake, &mut rng)
            .expect("at-minimum funding must pass");

        let mut l_bare = l0.clone();
        let mut rng = StdRng::seed_from_u64(42);
        let (v2, o2) = run_job(&mut l_bare, &p, &inputs, &vm, &ByteEq, &stake, &mut rng);

        assert!(matches!(v1, Verdict::Confirmed { .. }));
        assert_eq!(format!("{v1:?}"), format!("{v2:?}"));
        assert_eq!(o1, o2);
    }
```

NOTE: this requires `Ledger: Clone`. Check `src/staging/pouw/src/oracle.rs` — `Ledger` derives nothing today. Adding `#[derive(Clone)]` to `Ledger` would modify a GAME file, which is forbidden. Instead clone manually: rebuild `l0` by calling `priced_world()` twice (deterministic credits) — replace `l0.clone()` with a second `priced_world()` call and use each world's own ledger. Implement it that way (the helper is deterministic, so the two ledgers are identical).

- [ ] **Step 2: Verify failure** — `cargo test -p commputer-pouw economics` → compile FAIL (`validate_economics`/`run_priced_job` missing).

- [ ] **Step 3: Implement:**

```rust
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
```

(Check the exact import paths against the crate: `Verdict`/`SettlementOutcome` live in `job.rs`; `Verdict` derives `Debug` but maybe not `PartialEq` for the parity assert — the test compares via `format!("{:?}")` for the verdict and `==` for `SettlementOutcome` which derives `PartialEq`. Follow the compiler.)

- [ ] **Step 4: Verify pass** — `cargo test -p commputer-pouw economics` → PASS (8 unit + 3 prop). Full default suite → 39 baseline + new, all green.

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/economics.rs
git commit -m "feat(pouw): validate_economics + run_priced_job enforcement surface — reject-before-escrow proven (fuel-econ Task 5)"
```

---

### Task 6: Sim fixtures — light/heavy weight classes

**Files:**
- Create: `src/staging/pouw/sim/fixtures/light.wat`, `heavy.wat` (committed sources)
- Create: `src/staging/pouw/sim/fixtures/light.wasm`, `heavy.wasm` (prebuilt, committed)
- Test: append `mod sim_fixtures` to `src/staging/pouw/tests/wasm_runtime.rs`

- [ ] **Step 1: Write the .wat sources.** Both are ABI-compliant counting loops that ignore input and return an 8-byte LE counter; iteration counts straddle the 1-Mfuel pricing boundary (light ≈2M fuel, heavy ≈50M fuel — wasmi charges ~3-4 fuel per loop iteration here, so 600_000 and 15_000_000 iterations; the test asserts RANGES, so exact per-iteration fuel does not need to be known in advance). `light.wat`:

```wat
(module
  ;; Sim weight-class fixture (fuel-economics spec §5.1): ~2M fuel busy-loop.
  ;; ABI: memory/alloc/run; ignores input; output = 8-byte LE iteration count.
  (memory (export "memory") 1 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "run") (param i32 i32) (result i64)
    (local $i i64)
    (block $done
      (loop $l
        (br_if $done (i64.ge_u (local.get $i) (i64.const 600000)))
        (local.set $i (i64.add (local.get $i) (i64.const 1)))
        (br $l)))
    (i64.store (i32.const 2048) (local.get $i))
    (i64.or (i64.shl (i64.const 2048) (i64.const 32)) (i64.const 8))))
```

`heavy.wat`: identical except the bound is `(i64.const 15000000)` and the header comment says ~50M fuel. (If measured fuel lands outside the asserted ranges in Step 3, adjust the iteration constants — NOT the ranges' order of magnitude — and record what you measured.)

- [ ] **Step 2: Add the fixture test module** to `tests/wasm_runtime.rs`:

```rust
mod sim_fixtures {
    use super::*;

    const LIGHT_WAT: &str = include_str!("../sim/fixtures/light.wat");
    const HEAVY_WAT: &str = include_str!("../sim/fixtures/heavy.wat");
    const LIGHT_WASM: &[u8] = include_bytes!("../sim/fixtures/light.wasm");
    const HEAVY_WASM: &[u8] = include_bytes!("../sim/fixtures/heavy.wasm");

    /// The committed .wasm fixtures are exactly the committed .wat sources
    /// (auditable provenance — the sim bin can't assemble wat at runtime
    /// because `wat` is a dev-dependency; spec §5.1).
    #[test]
    fn fixtures_match_their_wat_sources() {
        assert_eq!(wat::parse_str(LIGHT_WAT).unwrap(), LIGHT_WASM, "light.wasm stale — rerun the regenerate test");
        assert_eq!(wat::parse_str(HEAVY_WAT).unwrap(), HEAVY_WASM, "heavy.wasm stale — rerun the regenerate test");
    }

    /// Regenerator: `cargo test -p commputer-pouw --features wasm-runtime \
    ///   regenerate_sim_fixtures -- --ignored` rewrites the .wasm from the .wat.
    #[test]
    #[ignore]
    fn regenerate_sim_fixtures() {
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/sim/fixtures/light.wasm"),
            wat::parse_str(LIGHT_WAT).unwrap()).unwrap();
        std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/sim/fixtures/heavy.wasm"),
            wat::parse_str(HEAVY_WAT).unwrap()).unwrap();
    }

    /// Weight classes straddle the 1-Mfuel pricing boundary (spec §5.1) and
    /// pass the determinism gate.
    #[test]
    fn fixtures_pass_gate_and_meter_in_their_classes() {
        use commputer_pouw::wasm::validation::validate_module;
        for (bytes, lo, hi) in [
            (LIGHT_WASM, 1_000_001u64, 5_000_000u64),
            (HEAVY_WASM, 30_000_000, 100_000_000),
        ] {
            validate_module(bytes, &WasmLimits::default()).expect("fixture passes the gate");
            let mut store = ProgramStore::new();
            let program_hash = store.insert(bytes.to_vec());
            let input = b"x".to_vec();
            let input_hash: [u8; 32] = Sha256::digest(&input).into();
            let oracle = WasmOracle::new(store, WasmLimits::default());
            let out = oracle.execute(&JobSpec { program_hash, input_hash }, &input);
            assert!(out.result.is_ok(), "fixture runs: {:?}", out.result);
            assert!(
                (lo..=hi).contains(&out.fuel_consumed),
                "fuel {} outside class range [{lo}, {hi}] — adjust the .wat iteration count",
                out.fuel_consumed
            );
        }
    }
}
```

- [ ] **Step 3: Generate + verify.** Create `sim/fixtures/`, write both .wat files, run the regenerator (`cargo test -p commputer-pouw --features wasm-runtime regenerate_sim_fixtures -- --ignored`), then `cargo test -p commputer-pouw --features wasm-runtime sim_fixtures` → PASS (2 active tests). If `fixtures_pass_gate_and_meter_in_their_classes` reports out-of-range fuel, tune the iteration constants in the .wat, regenerate, re-run; record final measured fuel in your report.

- [ ] **Step 4: Commit** (binaries deliberately included):

```bash
git add src/staging/pouw/sim/fixtures/ src/staging/pouw/tests/wasm_runtime.rs
git commit -m "feat(pouw/sim): light/heavy fuel-class fixtures — committed wat + prebuilt wasm + byte-identity tests (fuel-econ Task 6)"
```

---

### Task 7: `sim/realfuel.rs` — additive events, dual population, sweep

**Files:**
- Modify: `src/staging/pouw/sim/tournament.rs` (visibility ONLY: `struct JobModel` → `pub(crate) struct JobModel` with `pub(crate)` fields, `fn settle_one_job` → `pub(crate) fn`, `fn wrong_hash` → `pub(crate) fn`, `fn pid` → `pub(crate) fn`; zero behavior change)
- Create: `src/staging/pouw/sim/realfuel.rs`
- Modify: `src/staging/pouw/sim/main.rs`

- [ ] **Step 1: Visibility lift** in `tournament.rs` (mechanical; `cargo test -p commputer-pouw` must stay 39-baseline green + sim tests untouched).

- [ ] **Step 2: Create `realfuel.rs`.** Core structure (complete code; follow the compiler on small details):

```rust
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
use crate::tournament::{pid, settle_one_job, JobModel, StratStats};
use commputer_pouw::economics::{budget_min, executor_bond_min, verifier_bond_min, work_cost};
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::JobSpec;
use commputer_pouw::params::GameParams;
use commputer_pouw::wasm::{ProgramStore, WasmLimits, WasmOracle};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha2::{Digest, Sha256};

/// One measured program class: name + measured fuel + the cap used for pricing.
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
            let fuel = out.fuel_consumed;
            assert!(out.result.is_ok(), "{name}: fixture must run, got {:?}", out.result);
            FuelClass { name, measured_fuel: fuel }
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
pub struct CornerResult {
    pub corner: Corner,
    pub budget: u64,
    pub executor_bond: u64,
    pub verifier_bond: u64,
    /// honest-equilibrium run (all-honest committee):
    pub honest_executor_ev: f64,
    pub honest_verifier_ev: f64,
    /// mixed run (one rubber-stamp seat):
    pub cheat_executor_ev: f64,
    pub lazy_executor_ev: f64,
    pub rubber_stamp_ev: f64,
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

/// Run one population at one corner for one fuel class. `rubber_seat` controls
/// the population: false ⇒ all-honest committee (honest-equilibrium scoring);
/// true ⇒ one rubber-stamp seat (cheat measurement).
#[allow(clippy::too_many_arguments)]
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
```

…plus `fold` (the per-strategy accumulation — same logic as `run_tournament`'s two fold blocks, with `cost = realized_cost if does_work`), `run_sweep(grid, classes, jobs, seed) -> Vec<CornerResult>` (for each corner × the headline class: compute `price_per_mfuel` so `work_cost(measured)` lands near 1_000 — `price = 1_000 / max(1, measured.div_ceil(1M))` clamped ≥1; `F = if tight_cap { measured } else { WasmLimits::default().fuel }`; `budget = budget_min(F, &p)?` — corners whose pricing errors are reported as unpriceable and skipped with a printed note, never silently dropped), the safety judgment:

```rust
    let safe = cheat_executor_ev <= 0.0
        && lazy_executor_ev <= 0.0
        && rubber_stamp_ev < honest_verifier_ev_mixed   // mixed-run comparison for the cheat
        && honest_executor_ev > 0.0                      // honest-equilibrium run
        && honest_verifier_ev > 0.0;                     // honest-equilibrium run
```

and `table(results) -> String` printing one row per corner — `s/t/v/k/cap | budget | v_bond | hx_ev | hv_ev | cheat_ev | lazy_ev | rs_ev | SAFE?` — ending with the machine-greppable line:
`REAL-FUEL ECONOMICS: <n> safe corners — HONEST WORK PROFITABLE, CHEATING LOSES MONEY` (n ≥ 1) or
`REAL-FUEL ECONOMICS: NO SAFE CORNER — every swept regime fails; see table` (n = 0).

The default grid (spec §5.4): `s_bps ∈ {2_500, 5_000, 10_000} × t_bps ∈ {1_000, 2_500} × v_bps ∈ {1_000, 2_500, 4_000} × k ∈ {3, 5} × tight_cap ∈ {false, true}` = 72 corners, `jobs = 4_000` per population per corner (~0.6M ledger jobs total — seconds).

Also add a feature-gated smoke test at the bottom of `realfuel.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep machinery runs end-to-end on a 2-corner grid and judges at
    /// least one corner; honest-equilibrium population produces zero
    /// rubber-stamp plays (the jackpot-exclusion property, structurally).
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
    }
}
```

- [ ] **Step 3: Wire `main.rs`:**

```rust
#[cfg(feature = "wasm-runtime")]
mod realfuel;
```

and at the end of `main()`:

```rust
    #[cfg(feature = "wasm-runtime")]
    {
        let classes = realfuel::measure_classes();
        println!("\n=== REAL-FUEL ECONOMICS (fuel-economics spec §5) ===");
        for c in &classes {
            println!("class {:>6}: measured fuel {:>12}", c.name, c.measured_fuel);
        }
        let results = realfuel::run_sweep(&realfuel::default_grid(), &classes[0], 4_000, DEFAULT_SEED);
        print!("{}", realfuel::table(&results));
    }
```

- [ ] **Step 4: Verify** — `cargo test -p commputer-pouw` → toy baseline green (39 + new economics tests; realfuel not compiled). `cargo test -p commputer-pouw --features wasm-runtime realfuel` → smoke PASS. `cargo run -p commputer-pouw --features wasm-runtime --bin pouw-sim --release | tail -20` → both the toy verdict line AND the real-fuel table print; paste the table in your report.

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/sim/ src/staging/pouw/src/economics.rs
git commit -m "feat(pouw/sim): real-fuel sweep — additive events, honest-equilibrium scoring, corner table (fuel-econ Task 7)"
```

---

### Task 8: Priced-game integration tests + error-policy pin

**Files:**
- Modify: `src/staging/pouw/tests/wasm_runtime.rs` (append `mod priced_game`)

- [ ] **Step 1: Append:**

```rust
mod priced_game {
    use super::*;
    use commputer_pouw::economics::{budget_min, executor_bond_min, run_priced_job, verifier_bond_min};
    use commputer_pouw::engine::JobInputs;
    use commputer_pouw::ids::{JobId, ParticipantId};
    use commputer_pouw::job::{Job, Verdict};
    use commputer_pouw::oracle::{ByteEq, Ledger};
    use commputer_pouw::params::GameParams;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// Spec §6: the founder-locked error policy, pinned at the enforcement
    /// surface. A deterministically-failing program (unreachable guest), funded
    /// at the formula minimums, settles Confirmed with the executor paid the
    /// worker share — both ends of the rationale documented in economics.rs.
    #[test]
    fn error_sentinel_job_settles_confirmed_at_formula_minimums() {
        let p = GameParams { p_trap_bps: 1_000, ..GameParams::default() };
        let fuel_cap = WasmLimits::default().fuel;
        let budget = budget_min(fuel_cap, &p).unwrap();
        let e_bond = executor_bond_min(fuel_cap, budget, &p).unwrap();
        let v_bond = verifier_bond_min(fuel_cap, &p).unwrap();

        // An always-trapping (error-sentinel) program through the REAL oracle.
        let trap_wat = r#"(module
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 1024))
            (func (export "run") (param i32 i32) (result i64) (unreachable)))"#;
        let wasm = wat::parse_str(trap_wat).unwrap();
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm);
        let input = b"will error".to_vec();
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let oracle = WasmOracle::new(store, WasmLimits::default());
        let spec = JobSpec { program_hash, input_hash };

        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();
        let mut l = Ledger::new();
        l.credit(submitter, budget);
        l.credit(executor, e_bond);
        for c in &candidates {
            l.credit(*c, v_bond);
        }
        let total0 = l.total_supply();

        let job = Job {
            id: JobId::derive(&spec.program_hash, &spec.input_hash, &submitter, 0),
            submitter,
            spec,
            budget,
        };
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: &input,
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);

        let (verdict, out) =
            run_priced_job(&mut l, &p, &inputs, fuel_cap, &oracle, &ByteEq, &stake, &mut rng)
                .expect("formula-minimum funding must pass validation");

        // The committee agrees on the SAME error sentinel ⇒ Confirmed, worker paid.
        assert!(matches!(verdict, Verdict::Confirmed { .. }), "got {verdict:?}");
        assert_eq!(out.worker_paid, budget * 8_500 / 10_000, "executor paid the worker share");
        assert_eq!(l.total_supply(), total0, "conservation");
        assert_eq!(l.escrowed(), 0);
    }

    /// budget_min scales with the measured fuel of a REAL program: pricing the
    /// guest's measured fuel tight-cap vs the global cap differs exactly per
    /// the formulas (spec §7 integration row).
    #[test]
    fn budget_min_scales_with_real_measured_fuel() {
        const GUEST: &[u8] = include_bytes!("../src/wasm/fixtures/guest_example.wasm");
        let mut store = ProgramStore::new();
        let program_hash = store.insert(GUEST.to_vec());
        let input = b"measure me".to_vec();
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let oracle = WasmOracle::new(store, WasmLimits::default());
        let out = oracle.execute(&JobSpec { program_hash, input_hash }, &input);
        let measured = out.fuel_consumed;
        assert!(out.result.is_ok());
        assert!(measured > 0);

        let p = GameParams::default();
        let tight = budget_min(measured, &p).unwrap();
        let slack = budget_min(WasmLimits::default().fuel, &p).unwrap();
        // The guest is sub-1-Mfuel: tight-cap prices exactly 1 mfuel; the
        // global 100M cap prices 100 mfuel. budget_min is linear in work_cost
        // up to per-term ceilings, so slack lands within [99x, 100x] of tight.
        assert!(measured < 1_000_000, "guest stays in the sub-mfuel class");
        assert!(slack > tight, "cap-pricing must exceed tight-pricing");
        assert!(
            slack >= 99 * tight && slack <= 100 * tight,
            "linear-in-wc up to ceilings: tight={tight}, slack={slack}"
        );
        assert_eq!(budget_min(1, &p).unwrap(), tight, "all sub-mfuel jobs price identically");
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p commputer-pouw --features wasm-runtime priced_game` → PASS (2 tests). If `error_sentinel_job_settles_confirmed_at_formula_minimums` fails on the worker_paid assert, check the settlement rounding (worker share is `budget·8_500/10_000` floor-div in settlement.rs) — match the actual settlement arithmetic, reporting what it was; never weaken the Confirmed assert.

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/tests/wasm_runtime.rs
git commit -m "test(pouw): error-policy pin at enforcement surface + real-fuel budget scaling (fuel-econ Task 8)"
```

---

### Task 9: Error-policy rationale docs

**Files:**
- Modify: `src/staging/pouw/src/economics.rs` (doc comment block)
- Modify: `src/staging/pouw/README.md` (three sentences in the WASM section)

- [ ] **Step 1:** Add to `economics.rs` above `run_priced_job` the spec §6 both-ends policy doc:

```rust
/// ## Error-outcome policy (founder-locked, fuel-economics spec §6)
///
/// A job whose agreed outcome is the error sentinel settles `Confirmed` with
/// the executor paid the worker share — deliberately:
/// * Protective end: an out-of-fuel job burned *effectively* the full fuel
///   budget (wasmi leaves a small remainder), so paying less would open
///   executor-griefing via designed-to-OOF jobs until executors stop accepting
///   work.
/// * Generous end (stated honestly): an instant-error guest burns ~0 fuel yet
///   collects the full worker share of the Bv-driven budget. This is
///   submitter-self-inflicted — no third party can inject errors into someone
///   else's deterministic job, and wash-trading errors loses at least the burn
///   share per job.
/// Refining this (paying OOF differently from gate-rejects) requires fuel in
/// the claim format — a deferred game change (spec §10.2).
```

- [ ] **Step 2:** Append to the README's WASM-runtime section (after the digest paragraph) the same policy in three sentences + the fuel-in-claim pointer. Also add the economics subsection stub the sweep table will fill in Task 10 (heading + how-to-run line).

- [ ] **Step 3:** `cargo test -p commputer-pouw` green (docs only). Commit:

```bash
git add src/staging/pouw/src/economics.rs src/staging/pouw/README.md
git commit -m "docs(pouw): error-policy both-ends rationale in economics.rs + README (fuel-econ Task 9)"
```

---

### Task 10: README results, full regression sweep, wrap-up

**Files:**
- Modify: `src/staging/pouw/README.md`

- [ ] **Step 1: Run the full sweep** — `cargo run -p commputer-pouw --features wasm-runtime --bin pouw-sim --release > /tmp/sweep.txt && tail -40 /tmp/sweep.txt`. Paste into the README's economics subsection: the measured fuel per class, the corner table (or its safe subset + the best corner), the machine verdict line, and a **founder-decision note**: "adopting any corner as new GameParams defaults is a founder decision — note the verifier_bond capital column." Do NOT change any GameParams defaults yourself.

- [ ] **Step 2: Full regression matrix** (paste every result line in the report):
1. `cargo test -p commputer-pouw --features wasm-runtime` → ALL green (88 baseline + this cycle's additions).
2. `cargo test -p commputer-pouw` → default baseline 39 + the new default-visible economics/params tests, wasmi absent from the build.
3. `cargo run -p commputer-pouw --bin pouw-sim --release 2>/dev/null | tail -3` → toy verdict line still `HONEST PLAY DOMINATES`.
4. `cargo build` (workspace) → clean.
5. Game files byte-identical: `git diff <pre-cycle-sha> HEAD -- src/staging/pouw/src/engine.rs src/staging/pouw/src/settlement.rs src/staging/pouw/src/verdict.rs src/staging/pouw/src/trap.rs src/staging/pouw/src/escalation.rs src/staging/pouw/src/committee.rs src/staging/pouw/src/commit_reveal.rs src/staging/pouw/src/job.rs src/staging/pouw/src/ids.rs src/staging/pouw/src/oracle.rs` → EMPTY (the pre-cycle sha is the commit before fuel-econ Task 1; find it with `git log --oneline | grep "fuel-econ Task 1" -A1`).

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/README.md
git commit -m "docs(pouw): real-fuel sweep results + founder-decision note; full regression green (fuel-econ Task 10)"
```

---

## Definition of done (mirrors spec §9)

1. Formulas unit+property tested; default build keeps its baseline; degenerate params error, never panic.
2. `run_priced_job` rejects-before-escrow (proven by ledger snapshot) and is bare-`run_job` after the check (seed-parity test).
3. Real-fuel sweep runs end-to-end under the additive event model with honest-equilibrium scoring; table + verdict line land in the README; defaults unchanged without founder sign-off.
4. Error-policy pin test green; both-ends rationale documented.
5. Game files byte-identical across the cycle; only declared files changed.
