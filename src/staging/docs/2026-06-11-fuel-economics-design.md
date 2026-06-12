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
  out-of-fuel — means the executor burned *effectively* the full fuel budget (wasmi leaves a
  small remainder); paying less would let
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

**Why cap-pricing is not exploitable** (so sweep reviewers need not re-litigate it): any
self-submission/wash-trade strategy loses at least the burn share of `B` per job plus the
uncaptured verifier slice, and executors preferring tiny-measured-fuel jobs is market
adverse-selection — the submitter overpays, no protocol-EV hole opens.

**Event model (pinned).** The formulas below and the §5 real-fuel tournament both use the
**additive** event model: per real job, a paid sampled verification occurs with probability
`s` AND, independently, an unpaid synthetic trap round occurs with probability `t` (expected
events per job per committee slot: `s + t`; a verifier's per-event trap probability:
`t/(s+t)`). NOTE: the existing toy-mode tournament draws trap-with-precedence instead (trap
`t`, else sampled `(1−t)·s`) — the toy mode and its regression line stay untouched, but the
real-fuel mode's event generator MUST implement the additive model so the sweep measures the
same world the formulas price. This divergence is deliberate and documented in the sim code.

## 3. The formulas (`economics.rs`)

All formulas are pure functions of `(F, &GameParams)` with ceiling division (never round value
away in favor of a cheater). **Integer rule (consensus-destined, frozen):** every intermediate
product is computed in `u128` (worst case ≈ 2^64·2^40, fits comfortably), with exactly ONE
final saturation to `u64::MAX` at the end — over-strict, never lenient. Saturating-u64 chains
are FORBIDDEN: a numerator that saturates before its division under-prices by orders of
magnitude, violating the invariant "over-pricing is safe, under-pricing is not" (§8). A
dedicated test pins no-underpricing at extremes (F=u64::MAX, huge `price_per_mfuel`): every
`*_min` result must be ≥ `work_cost(F)`'s own saturated value.

**Executor-profit constraint.** The honest executor receives `worker_bps·B/10_000` and pays at
most `work_cost(F)`. Require profitability with margin:

```
B ≥ work_cost(F) · profit_margin_bps / worker_bps                       (Bx)
```

**Verifier-profit constraint.** Per real job (additive event model, §2.1): a paid sampled
event occurs at rate `s` — the committee splits `verifier_bps·B/10_000` — and an unpaid trap
event at rate `t` (unpaid in the honest equilibrium — jackpots only fire when someone
rubber-stamps). A candidate is selected into either event kind with the same probability, and
the **cost of each selected event is one re-execution, `work_cost(F)`**; `(s+t)` is the
per-job event rate, not a per-event cost factor. Per selected slot, expected revenue is
`s·(verifier_bps·B/10_000)/k`-proportional and expected cost `(s+t)·work_cost(F)`-proportional
(the selection probability cancels). Requiring non-negative EV with margin gives the exact
closed form:

```
Bv:  B ≥ k · work_cost(F) · (s_bps + t_bps) · profit_margin_bps / (verifier_bps · s_bps)
```

(dimensionally complete as written — the two 10_000 factors cancel; computed as one u128
ceil-chain). **`budget_min(F) = max(Bx, Bv)`** — and Bv dominates by an order of magnitude at
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
(Under the additive event model the executor's catch rate is exactly `s`; note also that for
k ≥ 2 the formula term can never bind against the Bv-driven budget — `bond_safety ≈ 1.5×wc`
vs budget ≥ ~26×wc for any k ≥ 2, ≈ 39.6×wc at the k=3 defaults — so in practice
`executor_bond_min = budget`. Stated so the plan's tests assert the right branch.)

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
| `profit_margin_bps: u32` | 12_000 | **> 10_000** (strict — exact break-even would make an at-minimum honest role EV-zero and fail the sweep's strict EV-positive bar) |
| `bond_safety_bps: u32` | 15_000 | ≥ 10_000 |

Existing split fields (`worker_bps` etc.) are unchanged *as code*; their **default values** are
sweep outputs — if the sweep's recommended regime moves the split (e.g. a larger verifier
slice), the new defaults are presented to the founder with the sweep table before adoption.

**Degenerate-params guard (complete list).** The formulas divide by `s_bps` (executor bond,
Bv), `t_bps` (verifier bond), `worker_bps` (Bx), and `verifier_bps` (Bv) — and
`GameParams::validate()` legally permits every one of them to be 0 (any split summing to
10_000 passes; existing tests set `sample_rate_bps = 0` to force the challenge path; only
`k_escalate > k` constrains `k`). Pricing therefore REFUSES rather than divides or panics:
every `*_min` function and `validate_economics` returns `EconViolation::BadParams` when ANY of
`s_bps`, `t_bps`, `worker_bps`, `verifier_bps`, or `k` is 0. A job cannot be *priced* in a
regime with no proactive catching, no traps, no paid role, or no committee — even though the
unpriced game can still simulate those. `GameParams::validate()` itself stays permissive (the
game allows them); the restriction is enforcement-surface-only. Consequence for §4: the
`*_min` functions are **fallible** — they return `Result<u64, EconViolation>`.

## 4. Module design

New file **`src/staging/pouw/src/economics.rs`** (NOT feature-gated — pure integer math with no
wasmi dependency; usable by the default-build sim and tests):

```rust
pub fn work_cost(fuel_cap: u64, price_per_mfuel: u64) -> u64;        // ceil(F/1M)·p, u128 then one saturation
// Fallible (§3 degenerate-params guard): Err(BadParams) on any zero divisor / k == 0.
pub fn budget_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation>;      // max(Bx, Bv)
pub fn executor_bond_min(fuel_cap: u64, budget: u64, p: &GameParams) -> Result<u64, EconViolation>;
pub fn verifier_bond_min(fuel_cap: u64, p: &GameParams) -> Result<u64, EconViolation>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconViolation {                  // values carried for log-quality messages
    BudgetBelowMin { budget: u64, min: u64 },
    ExecutorBondBelowMin { bond: u64, min: u64 },
    VerifierBondBelowMin { bond: u64, min: u64 },
    /// Degenerate pricing params (the §3 guard: zero s/t/worker/verifier bps or k==0)
    /// — NOT merely a GameParams::validate() failure; validate() permits these.
    BadParams(&'static str),
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
     `guest_example.wasm` plus two **prebuilt .wasm fixtures checked in under `sim/`**
     spanning weight classes (a light ~10K-fuel transform and a heavy ~10M-fuel loop), giving
     a 3-point fuel distribution. (Prebuilt is mandatory: `wat` is a dev-dependency and cargo
     bins cannot see dev-deps — assembling .wat at bin runtime would force an undeclared
     Cargo.toml change. A tiny dev-gated test re-assembles the fixtures from committed .wat
     sources and asserts byte-identity, keeping them auditable.)
  2. Uses the **additive event model** (§2.1) for its event generator — pinned to match the
     formulas; the toy mode's trap-precedence draw is untouched.
  3. Prices every agent's realized costs from **measured** fuel and all budgets/bonds from the
     **cap** via `economics.rs` (the enforcement view and the realized view, both live).
  4. Sweeps a small grid over `(sample_rate_bps, p_trap_bps, verifier_bps, k)` with
     budgets/bonds set AT the formula minimums. Sweep bookkeeping rules: a `verifier_bps`
     change is absorbed by `worker_bps` (burn stays 500) so the split still sums to 10_000;
     corners with k ≥ 7 set `k_escalate = 2k+1` to keep `validate()` green. The grid includes
     a **tight-cap corner** (`F == measured fuel`, zero slack) — with the global 100M cap the
     small fixtures otherwise pass every corner on cap-overpricing cushion alone, and
     tight-cap is the stress that previews the deferred per-job-cap world.
  5. Reports per-corner: each strategy's EV (honest executor, lazy executor, honest verifier,
     rubber-stamp verifier) AND `verifier_bond_min` (the capital a verifier must stake — at
     low t it explodes, e.g. ~151×wc at t=1%, and the founder must see that next to any
     "safe" corner before adopting defaults).
  6. **Honest-equilibrium scoring:** a corner's "honest roles EV-positive" bar is judged on an
     all-honest-committee variant (or equivalently with jackpot/catch income excluded). The
     mixed population (with cheaters present) measures the cheats' EVs only. Rationale: with
     a permanent rubber-stamper seated, jackpot income (~2× the entire Bv margin at defaults)
     would let under-priced corners pass on revenue that vanishes once cheaters are deterred —
     the exact failure mode this cycle exists to rule out.
  7. Prints the regime table; a corner passes iff **all cheats EV-negative (mixed run) AND all
     honest roles EV-positive (honest-equilibrium run)**. Output ends with a machine-greppable
     verdict line:
     `REAL-FUEL ECONOMICS: <n> safe corners — HONEST WORK PROFITABLE, CHEATING LOSES MONEY`
     (or a loud failure line if no corner passes — which is itself a publishable finding).
- The recommended regime (the best corner) is written into the README and presented to the
  founder; adopting it as new `GameParams` defaults is a founder decision at spec-review time
  of the *results*, not silently.

## 6. Error-policy pin

One new game-level test (in the wasm integration suite): a deterministically-failing program
(e.g. an `unreachable` guest) driven through `run_priced_job` with formula-minimum funding
settles `Confirmed` with the executor paid the worker share — pinning the founder-locked
anti-griefing policy at the enforcement surface. The doc comment in `economics.rs` states the
policy **honestly, both ends**: the protective case (an out-of-fuel job burned *effectively*
the full fuel budget — wasmi leaves a small remainder — so paying less would open
executor-griefing via OOF-bombs) AND the generous case (an instant-error guest burns ~0 fuel
yet collects the full worker share of the big Bv-driven budget; submitter-self-inflicted —
no third party can inject errors into someone else's deterministic job, and wash-trading
errors loses ≥ the burn share per job). The README's WASM section gains three sentences
(policy + both-ends rationale + the fuel-in-claim future that would refine it).

## 7. Testing

- **Unit (economics.rs):** `work_cost` ceil-division edges (0, 1, 1M−1, 1M, 1M+1, u64::MAX
  saturation); each formula at hand-computed values for the default params; `max(…, budget)`
  binding in `executor_bond_min` (and the documented k≥2 fact that the formula term never
  binds at defaults); the **no-underpricing extremes test** (F=u64::MAX, huge
  `price_per_mfuel`: every `*_min` ≥ the saturated `work_cost(F)` — pins the u128-intermediates
  rule); the **degenerate-params guard** (each of s/t/worker/verifier bps and k at 0 →
  `BadParams`, never a panic — unit test per divisor); `validate_economics` accept/reject per
  violation variant; `GameParams::validate()` rejecting bad new fields (margin must be
  strictly > 10_000).
- **Property (proptest):** monotonicity — every formula non-decreasing in `F` and in
  `price_per_mfuel`; `budget_min ≥ Bx` and `≥ Bv` component bounds; a funded-at-minimum job
  always passes `validate_economics`, any single component one-below-minimum always fails with
  the matching variant; no input panics (degenerate params included — they error).
- **Integration:** `run_priced_job` rejects an underfunded job *without any escrow side
  effects* (ledger snapshot unchanged — the check runs before money moves) and accepts +
  settles a formula-minimum honest job identically to bare `run_job` (same verdict, same
  outcome); the §6 error-policy pin; one real-fuel test (feature-gated) measuring the guest's
  fuel and asserting `budget_min` scales as the formulas demand.
- **Sim:** toy-mode regression (default build, old verdict line); real-fuel mode smoke test +
  the sweep run documented in the README with its table.
- **Regression:** default baseline untouched — exactly 39 (31 unit + 7 sim + 1 conservation,
  verified live on this branch pre-cycle); feature suite grows from exactly 88
  (63 + 7 + 1 + 17) plus this cycle's additions, all green; game files byte-identical.

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
