# Fuel → Economics Coupling — Design

**Date:** 2026-06-11
**Status:** Approved by founder (scope + all sections) — this document is the committed spec.
**Branch:** `agent-wire-testnet-20260610` (agent work; staging only; no protected files).
**Parents:** `2026-06-10-pouw-verification-game-design.md` (the game; its §7 inequality and §11
"toy-VM realism" risk) and `2026-06-11-wasm-execution-runtime-design.md` (the runtime; its §10.1
deferred item — this cycle).

---

## 1. Goal

Make the verification game's economics **real**. The WASM runtime now measures actual fuel
(`ExecOutcome.fuel_consumed`, deterministic across nodes), but the economic story still rests on
the toy constants `C_exec=40 / C_ver=3`. This cycle:

1. **Derives enforceable pricing formulas** — `budget_min(F)`, `executor_bond_min(F)`,
   `verifier_bond_min(F)` as functions of the declared fuel cap and the game parameters,
   including the cost the toy model hid: **verification is full re-execution and costs exactly
   as much as execution**.
2. **Re-proves the incentive result against real fuel** — the tournament prices every agent's EV
   from measured fuel of real wasm programs, and the success bar tightens: every modeled cheat
   EV-negative **and every honest role EV-positive**.
3. **Enforces the formulas at submit time** — via a new `run_priced_job` entry point that
   validates before delegating to the **byte-identical** existing game.

### Founder-locked decisions (made during brainstorm, 2026-06-11)

- **Coupling depth: sim + enforcement.** Settlement's verified money paths are untouched;
  enforcement is a new pre-check, not a settlement change.
- **Error policy: keep v1.** An error-sentinel job settles `Confirmed` with the executor paid
  the worker share. Rationale (now documented + test-pinned): the worst error case —
  out-of-fuel — means the executor burned the *full* fuel budget; paying less would let
  malicious submitters grief executors with designed-to-OOF jobs until executors stop accepting
  work. The gate-reject overpay is submitter-self-inflicted and bounded. Revisit only when fuel
  enters the claim format (deferred, §10).
- **Work unit: raw wasmi fuel.** 1 work unit = 1 fuel unit as the pinned engine meters it.
  Documented caveat: fuel ≈ instruction count, not wall-clock; a weighted per-operator table is
  a future consensus-versioned refinement (deferred, §10).

### Non-goals (deferred, §10)

Per-job fuel caps in `JobSpec`; fuel in the claim format; gas-style refunds of unused fuel;
weighted operator cost tables; on-chain wiring of the enforcement surface (founder).

## 2. The model

All money is integer token units; all work is fuel (u64). Notation: bps values are /10_000.

- **`F`** — the declared fuel cap. v1: the global `WasmLimits.fuel` (default 100,000,000);
  per-job caps arrive with the on-chain job format (deferred). Enforcement prices the **cap**
  (known at submit time, and the executor may legally burn up to it); the sim uses **measured**
  fuel for realized costs. Jobs whose measured fuel is far below the cap overpay — that is the
  no-refund v1 model (gas-style refunds deferred).
- **`price_per_mfuel`** — token units per 1,000,000 fuel (new `GameParams` field, default 1).
  Mega-fuel granularity keeps integer math at test scale: the default cap prices at
  `ceil(100M/1M)·1 = 100` tokens, matching the existing tests' budget≈100 world.
- **`work_cost(F) = ceil(F / 1_000_000) · price_per_mfuel`** — the token cost of doing the
  work once. In the sim, this same function prices the executor's (and each verifier's)
  *physical* cost of a (re-)execution; the protocol does not distinguish "price" from "cost"
  in v1 — `price_per_mfuel` is the consensus-visible knob for both.

### 2.1 The headline structural fact

A committee verification **re-executes the entire job**: an honest verifier's cost per sampled
job is `work_cost(F)` — identical to the executor's, not the toy model's 7.5%. Therefore the
true cost of a *verified* job is roughly

```
total ≈ work_cost(F) · (1 + (s + t) · k)        s = sample rate, t = trap rate, k = committee size
```

and **the submitter's budget must fund it** (minus the trap share, which is unpaid work the
verifier-profit constraint below charges to the paid events). At the current defaults
(s=1.0, t=0.1, k=3) a job costing 100 to compute needs a budget of ~3,300 to make verification
break even at the 10% verifier slice. This is not a bug; it is what replicated verification
costs. The sweep's job (§5) is to find parameter corners that price it honestly, and later
cycles (sampling fewer verifiers, fraud proofs, SNARKs) attack the multiplier itself.

## 3. The formulas (`economics.rs`)

All formulas use ceiling division (never round value away in favor of a cheater) and saturating
integer arithmetic; all are pure functions of `(F, &GameParams)`.

**Executor-profit constraint.** The honest executor receives `worker_bps·B/10_000` and pays at
most `work_cost(F)`. Require profitability with margin:

```
B ≥ work_cost(F) · profit_margin_bps / worker_bps                       (Bx)
```

**Verifier-profit constraint.** Per real job, a verification event occurs at rate `s` (paid:
the committee splits `verifier_bps·B/10_000`) and a trap event at rate `t` (unpaid in the
honest equilibrium — jackpots only fire when someone rubber-stamps). A candidate is selected
into either event with the same probability, so per-verifier expected revenue and cost per real
job are proportional to `s · (verifier_bps·B/10_000)/k` and `(s+t) · work_cost(F)` per selected
slot. Requiring non-negative EV per slot with margin:

```
B ≥ k · work_cost(F) · (s_bps + t_bps) · profit_margin_bps / (verifier_bps · s_bps) · ...     (Bv)
```

(exact integer form in the module: `ceil_mul_div` chains; the spec's plan freezes the tested
expression). **`budget_min(F) = max(Bx, Bv)`** — and Bv dominates by an order of magnitude at
current defaults, which is the point of this cycle.

**Executor bond.** A lazy executor saves at most `work_cost(F)` and is caught by proactive
sampling with probability `s` (deliberate tightening of the parent spec's `max(s, t)` bound:
in the implemented game, traps test *verifiers*, not executors — using `s` alone is strictly
more conservative; challenges on unsampled jobs add un-modeled catch probability, also
a-fortiori). Ignoring the forfeited worker share (a-fortiori):

```
executor_bond_min(F) = max( ceil(work_cost(F) · bond_safety_bps / s_bps),  budget )
```

The `max(…, budget)` term preserves the parent spec's `Be ≥ B` rule (slash ≥ value at stake).

**Verifier bond.** A rubber-stamper saves `work_cost(F)` per skipped re-execution and is caught
exactly when the event is a trap — probability `t/(s+t)` per event:

```
verifier_bond_min(F) = ceil( work_cost(F) · bond_safety_bps · (s_bps + t_bps) / (10_000 · t_bps) )
```

At the toy defaults this is ~16.5× `work_cost(F)` (vs the toy's flat 20) — real fuel makes
rubber-stamping a *large* theft that traps must police with large bonds or higher trap rates;
the sweep explores that trade-off.

**New `GameParams` fields** (declared modification of `params.rs`, a staging file we own):

| field | default | constraint (`validate()`) |
|---|---|---|
| `price_per_mfuel: u64` | 1 | ≥ 1 |
| `profit_margin_bps: u32` | 12_000 | ≥ 10_000 (must be a real margin) |
| `bond_safety_bps: u32` | 15_000 | ≥ 10_000 |

Existing split fields (`worker_bps` etc.) are unchanged *as code*; their **default values** are
sweep outputs — if the sweep's recommended regime moves the split (e.g. a larger verifier
slice), the new defaults are presented to the founder with the sweep table before adoption.

**Zero-rate guard.** The bond formulas divide by `s_bps` (executor) and `t_bps` (verifier), and
the game legally runs with either at 0 (existing tests set `sample_rate_bps = 0` to force the
challenge path). Pricing therefore REFUSES rather than divides: `validate_economics` (and every
`*_min` function) returns `EconViolation::BadParams` when `s_bps == 0` or `t_bps == 0` — a job
cannot be *priced* in a regime with no proactive catching, even though the unpriced game can
still simulate one. `GameParams::validate()` itself stays permissive (the game allows it); the
restriction is enforcement-surface-only.

## 4. Module design

New file **`src/staging/pouw/src/economics.rs`** (NOT feature-gated — pure integer math with no
wasmi dependency; usable by the default-build sim and tests):

```rust
pub fn work_cost(fuel_cap: u64, price_per_mfuel: u64) -> u64;        // ceil(F/1M)·p, saturating
pub fn budget_min(fuel_cap: u64, p: &GameParams) -> u64;             // max(Bx, Bv)
pub fn executor_bond_min(fuel_cap: u64, budget: u64, p: &GameParams) -> u64;
pub fn verifier_bond_min(fuel_cap: u64, p: &GameParams) -> u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconViolation {                  // values carried for log-quality messages
    BudgetBelowMin { budget: u64, min: u64 },
    ExecutorBondBelowMin { bond: u64, min: u64 },
    VerifierBondBelowMin { bond: u64, min: u64 },
    BadParams(&'static str),              // GameParams::validate() failure
}

pub fn validate_economics(job: &JobInputs, fuel_cap: u64, p: &GameParams)
    -> Result<(), EconViolation>;

/// THE enforcement surface (spec §1 point 3): validate, then delegate to the
/// byte-identical engine::run_job. The on-chain cycle wires the real submit
/// path to this same check; engine::run_job remains the unpriced core.
#[allow(clippy::too_many_arguments)]
pub fn run_priced_job(
    l: &mut dyn ChainHooks, p: &GameParams, inputs: &JobInputs, fuel_cap: u64,
    exec_oracle: &dyn ExecutionOracle, eq: &dyn EquivalenceOracle,
    stake_of: &dyn Fn(&ParticipantId) -> u64, rng: &mut dyn rand::RngCore,
) -> Result<(Verdict, SettlementOutcome), EconViolation>;
```

`lib.rs` gains `pub mod economics;` (one line, declared). `engine.rs`, `settlement.rs`,
`verdict.rs`, and every other game file remain **byte-identical** — re-verified by the final
review exactly as last cycle (empty `git diff` over the game files).

## 5. Simulation: real fuel in, new safe regime out

`sim/` changes (declared modifications; the sim is ours):

- **Toy mode unchanged** — default build still runs today's tournament and still prints
  `HONEST PLAY DOMINATES` (regression-pinned).
- **Real-fuel mode** (`#[cfg(feature = "wasm-runtime")]`, run via
  `cargo run -p commputer-pouw --features wasm-runtime --bin pouw-sim`):
  1. Measures fuel by executing real programs through `WasmOracle`: the checked-in
     `guest_example.wasm` plus two wat fixtures spanning weight classes (a light ~10K-fuel
     transform and a heavy ~10M-fuel loop), giving a 3-point fuel distribution.
  2. Prices every agent's realized costs from **measured** fuel and all budgets/bonds from the
     **cap** via `economics.rs` (the enforcement view and the realized view, both live).
  3. Sweeps a small grid over `(sample_rate_bps, p_trap_bps, verifier_bps split, k)` with
     budgets/bonds set AT the formula minimums, and reports per-corner: each strategy's EV
     (honest executor, lazy executor, honest verifier, rubber-stamp verifier) and the verdict.
  4. Prints the regime table; a corner passes iff **all cheats EV-negative AND all honest roles
     EV-positive**. Output ends with a machine-greppable verdict line:
     `REAL-FUEL ECONOMICS: <n> safe corners — HONEST WORK PROFITABLE, CHEATING LOSES MONEY`
     (or a loud failure line if no corner passes — which is itself a publishable finding).
- The recommended regime (the best corner) is written into the README and presented to the
  founder; adopting it as new `GameParams` defaults is a founder decision at spec-review time
  of the *results*, not silently.

## 6. Error-policy pin

One new game-level test (in the wasm integration suite): a deterministically-failing program
(e.g. an `unreachable` guest) driven through `run_priced_job` with formula-minimum funding
settles `Confirmed` with the executor paid the worker share — pinning the founder-locked
anti-griefing policy at the enforcement surface. `economics.rs` carries the policy rationale as
a doc comment; the README's WASM section gains three sentences (policy + rationale + the
fuel-in-claim future that would refine it).

## 7. Testing

- **Unit (economics.rs):** `work_cost` ceil-division edges (0, 1, 1M−1, 1M, 1M+1, u64::MAX
  saturation); each formula at hand-computed values for the default params; `max(…, budget)`
  binding in `executor_bond_min`; `validate_economics` accept/reject per violation variant;
  `GameParams::validate()` rejecting bad new fields.
- **Property (proptest):** monotonicity — every formula non-decreasing in `F` and in
  `price_per_mfuel`; `budget_min ≥ Bx` and `≥ Bv` component bounds; a funded-at-minimum job
  always passes `validate_economics`, any single component one-below-minimum always fails with
  the matching variant.
- **Integration:** `run_priced_job` rejects an underfunded job *without any escrow side
  effects* (ledger snapshot unchanged — the check runs before money moves) and accepts +
  settles a formula-minimum honest job identically to bare `run_job` (same verdict, same
  outcome); the §6 error-policy pin; one real-fuel test (feature-gated) measuring the guest's
  fuel and asserting `budget_min` scales as the formulas demand.
- **Sim:** toy-mode regression (default build, old verdict line); real-fuel mode smoke test +
  the sweep run documented in the README with its table.
- **Regression:** default 39-test baseline untouched; feature suite (88 + new) green; game
  files byte-identical.

## 8. Risks

- **The regime may shrink or vanish.** Real verification costs may leave no parameter corner
  where the 10% slice works — the likely outcome is the sweep recommending a larger
  `verifier_bps`. That is a *finding*, not a failure; defaults change only with founder
  sign-off.
- **Cap-vs-measured gap.** Pricing the cap overcharges small jobs (no refunds in v1). Bounded
  by the submitter's own cap choice once per-job caps exist; documented.
- **Toy-mode divergence.** Two sim modes can drift apart; mitigated by sharing the tournament
  core and differing only in the cost source (constants vs measured fuel).
- **Conservatism stack-up.** Every bound is a-fortiori (ignore forfeited shares, ignore
  challenge catches, ceil everywhere); stacked conservatism can over-price budgets. Accepted
  for v1: over-pricing is safe, under-pricing is not; noted for the gas-refund cycle.

## 9. Success criteria (definition of done)

1. `economics.rs` formulas unit/property-tested green; default build still 39-baseline.
2. `run_priced_job` enforces the formulas (reject-without-side-effects proven) and delegates
   to a byte-identical game.
3. Real-fuel tournament runs end-to-end and prints a regime table; at least the headline
   verdict line is produced (pass or loud-fail), and the result + recommended regime land in
   the README for founder review.
4. The error-policy pin test passes; policy rationale documented.
5. Game files (`engine.rs`, `settlement.rs`, `verdict.rs`, escalation/trap/committee/
   commit_reveal/job/ids) byte-identical across the cycle; only declared files change
   (`economics.rs` new, `params.rs` fields, `lib.rs` line, `sim/*`, tests, README).

## 10. Deferred (named follow-ups)

1. **Per-job fuel caps** in the job format (on-chain cycle; founder).
2. **Fuel in the claim** — lets settlement distinguish OOF from gate-reject and refine the
   error policy; a game change (claim format + verdict on pairs).
3. **Gas-style refunds** of unused fuel (settlement money-path change + conservation rework).
4. **Weighted per-operator cost table** (consensus-versioned; needs calibration data).
5. **Cheaper verification** (committee sampling of execution, fraud proofs, SNARKs) — attacks
   the `(s+t)·k` replication multiplier itself.
