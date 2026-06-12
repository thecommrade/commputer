# `commputer-pouw` — Proof-of-Useful-Work verification & settlement game

A self-contained, deterministic Rust prototype of the PoUW verification-and-settlement
game, plus a Monte-Carlo simulation that **empirically proves cheating is EV-negative**
at a concrete parameter regime.

This is a **staging prototype** (`src/staging/pouw/`). It is not wired into the chain. It
exists to (a) make the spec executable, (b) prove the economics close with real,
conservation-checked money movement, and (c) freeze the trait seams where the real WASM
runtime / approximate-equivalence oracle / chain adapter plug in later.

**Spec:** [`src/staging/docs/2026-06-10-pouw-verification-game-design.md`](../docs/2026-06-10-pouw-verification-game-design.md)
**Plan:** [`src/staging/docs/2026-06-10-pouw-verification-game-plan.md`](../docs/2026-06-10-pouw-verification-game-plan.md)

---

## What it is

One job's life, from submission to settlement, modelled deterministically:

1. A submitter escrows a **budget**; an executor posts a **bond** and produces a result.
2. With probability `sample_rate`, a **stake-weighted committee** is drawn (deterministic,
   unpredictable until the seed is revealed) to re-verify the result.
3. Verifiers **commit-reveal** their result hashes (binding + hiding).
4. A **quorum** over the reveals yields a `Verdict` — `Confirmed` / `Disputed` / `NoQuorum`.
5. **Settlement** moves money through one of the terminal branches (integer basis-point
   math, no floats), conserving value exactly. `NoQuorum` and challenges **escalate** to a
   larger panel. A fraction of verification rounds are **traps** (synthetic known-wrong
   claims) that slash rubber-stampers and pay the honest a jackpot from the slashed bonds.

All money is `u64` raw units; all fractions are integer **basis points** (`bps`, /10 000),
so settlement is deterministic and conservation is exact. No unit is ever minted: every
payout/burn is sourced from the escrowed budget and posted bonds (see the conservation
property test, below).

---

## How to run the sim

From the workspace root (`<repo-root>/src`):

```bash
cargo run -p commputer-pouw --bin pouw-sim
```

It plays 50 000 seeded jobs at the tuned safe regime and prints a table like:

```
PoUW Monte-Carlo tournament — 50000 jobs, seeded
regime: sample_rate=5000bps  p_trap=2500bps  executor_bond=100  verifier_bond=20  C_exec=40  C_ver=3
strategy                  plays   caught %  net EV/play
------------------------------------------------------
executor:honest           12390       0.0%       45.000
executor:cheat            12443      49.8%       -7.210
executor:lazy             12512      50.8%       -8.934
verifier:honest           62746       0.0%        3.608
verifier:rubberstamp      31373      40.3%       -7.478

verdict: HONEST PLAY DOMINATES — every modeled cheat is EV-negative.
```

The run is reproducible (fixed seed), so the numbers above are what you should see.

### How to read the table

- **strategy** — the modelled actor. Executors: `honest` (runs the job, claims the truth),
  `cheat` (claims a plausible wrong hash), `lazy` (returns garbage). Verifiers: `honest`
  (re-executes, reveals the truth) vs `rubberstamp` (echoes the executor's claim without
  working).
- **plays** — how many jobs (executors) or verification rounds (verifiers) that strategy
  played. The executor strategy is drawn uniformly each job; the committee is a fixed
  honest-majority panel (two honest seats, one rubber-stamp seat), which is why the
  verifier rows have many more plays.
- **caught %** — the fraction of that strategy's plays that ended with its bond **slashed**
  (a cheating executor caught by a sampling committee; a rubber-stamper caught by a trap).
- **net EV/play** — mean net profit per play, in budget units: rewards earned **minus**
  bonds lost **minus** the modelled compute cost (`C_exec`/`C_ver`). This is the number
  that matters: it is computed from **real ledger movement** through the production
  settlement code, not from a formula we asserted.

The **verdict line** is the deliverable claim, machine-checked by
`tournament::tests::honest_play_dominates_at_safe_regime`:

```
cheat_executor_ev  <=  0  <  honest_executor_ev      (cheating an executor loses; honesty profits)
lazy_executor_ev   <=  0
rubber_stamp_ev    <      honest_verifier_ev          (rubber-stamping a verifier loses)
```

---

## The safe-parameter regime (the deliverable)

The regime is **non-trivial** — sampling is 50% (not "verify everything") and both
economic inequalities hold by margin, not by brute force. Only the two plan-authorized
knobs are moved off `GameParams::default()`; everything else (the 85/10/5 split, quorum,
bonds) is inherited.

| knob | value | why |
|---|---|---|
| `sample_rate_bps` | `5_000` (50%) | proactively verify half of jobs |
| `p_trap_bps` | `2_500` (25%) | a quarter of verification rounds are traps |
| `executor_bond` | `100` (default) | the value at stake on a caught cheat |
| `verifier_bond` | `20` (default) | the value at stake on a caught rubber-stamp |
| `C_exec` (modelled cost) | `40` | honest execution cost |
| `C_ver` (modelled cost) | `3` | honest re-verification cost |

The two inequalities the regime satisfies (spec §7):

- **Executor:** `P(sampled)·executor_bond = 0.50·100 = 50 > C_exec = 40`. A cheating
  executor's expected slash exceeds the compute it saves, so cheating is EV-negative, while
  an honest executor earns `worker_reward − C_exec = 85 − 40 = 45`.
- **Verifier:** `p_trap·verifier_bond = 0.25·20 = 5 > C_ver = 3`. A rubber-stamper's
  expected trap slash exceeds the work it skips, widened further by the trap jackpot honest
  verifiers split off slashed stampers.

`C_exec`/`C_ver` are the **only** modelled-not-executed numbers (the toy VM has no
realistic absolute cost; spec §11 expresses the regime as cost *ratios*). Everything else
— who gets slashed, who splits a jackpot, the 85/10/5 split — is the real settlement code.
The regime is defined in code at `sim/tournament.rs::safe_regime()`.

---

## How to test

```bash
cargo test -p commputer-pouw      # 31 unit + 7 sim + 1 conservation property — all green
```

The most important test is the **conservation property** (`tests/conservation.rs`,
proptest): for randomized budgets/bonds/participant sets it drives **every** settlement
branch and asserts `Ledger::total_supply()` is invariant from before-escrow to
after-settle — i.e. no unit is ever minted and none is stranded. This is the backbone that
makes the EV numbers trustworthy.

---

## The seams (where the real system plugs in)

The game logic is written against three traits in `src/oracle.rs`. The prototype ships
deterministic impls; the production system swaps the impls **without touching the game**.

| seam (trait) | prototype impl | production replacement | who wires it |
|---|---|---|---|
| `ExecutionOracle` | `IteratedHashVm` (iterated SHA-256) | a real **WASM/WASI** runtime | follow-up cycle |
| `EquivalenceOracle` | `ByteEq` (`a == b`) | **approximate / semantic** equivalence (tolerance) | follow-up cycle (B/C) |
| `ChainHooks` | in-memory `Ledger` | adapter onto real **`ChainState` / `JobPool` / `event_loop`** | **founder (protected files)** |

Both `verdict::compute_verdict` and `engine::run_job` take `&dyn EquivalenceOracle` and
`&dyn ExecutionOracle`, so the swap is one line at the call site. The seam is **proven**,
not just claimed: `verdict::tests::approximate_equivalence_oracle_swaps_in_against_the_same_engine`
defines a stub "approximate" oracle and shows the same `compute_verdict` yields a different
(correct) verdict under it — the entire cost of "deterministic-first" (spec §8, §12.3).

### What is still mocked / founder-only

- **Execution** is a toy VM, not real useful work.
- **Committee selection** is a monotone-in-stake hash sortition, not a real VRF.
- **Value** lives in an in-memory ledger, not on-chain — bonds, escrow, slashing, and the
  trap jackpot are all modelled there.
- The **`ChainHooks` → chain adapter** and any wiring into `event_loop.rs` / `token.rs` are
  **protected-file work** reserved for the founder. Nothing here is wired into the chain.

---

## Module map

```
src/
  params.rs        GameParams (all knobs, bps) + invariant validation
  ids.rs           ParticipantId / JobId + hashing helpers
  job.rs           data model: JobSpec, Job, ExecutorClaim, Commitment, Reveal, Verdict, SettlementOutcome
  oracle.rs        the three trait seams + deterministic impls (IteratedHashVm, ByteEq, Ledger)
  committee.rs     deterministic stake-weighted committee selection from a seed
  commit_reveal.rs commitment construction + reveal verification (binding + hiding)
  verdict.rs       quorum over reveals via EquivalenceOracle -> Verdict
  settlement.rs    the terminal settlement branches (integer bps; the only place money moves)
  escalation.rs    K_escalate panel resolution (challenge path + NoQuorum path)
  trap.rs          synthetic trap round + jackpot from slashed bonds
  engine.rs        drives ONE job through the whole game (thin orchestrator)
tests/
  conservation.rs  proptest: every settlement branch balances exactly
sim/
  agents.rs        adversarial executor/verifier strategies
  tournament.rs    Monte-Carlo driver + per-strategy EV metrics (the proof)
  main.rs          pouw-sim entry: prints the regime table
```

`engine.rs` is the only module that knows the full sequence; `settlement.rs` is the only
place money moves.

## Final-review notes (for the on-chain wiring cycle)

An independent holistic review (2026-06-10) **approved** the crate: conservation correct on
all branches, the engine a genuine thin orchestrator, and the EV deliverable measured through
the *real* settlement code (non-tautological). Three notes, all in `engine.rs`, all bearing on
adversary behaviours the spec explicitly defers — **none is a conservation hole or
economic-soundness break.** Glance at these when wiring this onto the real chain:

1. **(fixed, `a48d91b`)** On a sampled `Disputed` verdict the catch bounty now pays verifiers
   who revealed the quorum-vindicated `correct_hash`, not merely anyone who disagreed with the
   executor.
2. **Wrong-side committee verifiers are not slashed on a real-job `Disputed` path** — their
   bonds are returned. Rubber-stamp deterrence rests entirely on **trap jobs** (the sim
   confirms they catch rubber-stampers ~40% of the time, making them EV-negative). Spec §6.9's
   "slash rejected-value committee verifiers" sub-rule is therefore realised via traps, not on
   the Disputed real-job path. A faithful on-chain version should slash them there too — it
   would only strengthen the incentives (and require re-checking the EV regime).
3. **Commit-reveal is built and unit-tested (`commit_reveal.rs`) but the engine does not call
   `reveal_matches`** during orchestration — harmless in this deterministic prototype (no
   equivocating adversary), but the on-chain version must enforce it (spec §6.5: discard a
   non-matching reveal and penalise) once verifiers can commit one value and reveal another.

---

## WASM runtime (`wasm-runtime` feature)

### How to run

```bash
# From <repo-root>/src (the workspace root):
cargo test -p commputer-pouw --features wasm-runtime
```

The default `cargo test -p commputer-pouw` (no feature flag) builds **39 tests** (31 unit +
7 sim + 1 conservation property) and does not pull in wasmi at all. Adding
`--features wasm-runtime` brings the total to **87 tests** (62 unit + 7 sim + 1 conservation +
17 integration): the 31 new in-crate wasm unit tests raise the unit count to 62, and the 17
integration tests are added on top of the unchanged sim and conservation counts. Wasmi enters
the dependency graph only under that flag.

### What it is

`WasmOracle` (`src/wasm/oracle.rs`) is the production `ExecutionOracle` that replaces the
`IteratedHashVm` placeholder behind the **unchanged trait seam** in `src/oracle.rs`. Swapping
the two is a one-line change at the call site; the verification game, settlement, and
conservation property tests are untouched.

Design doc: `src/staging/docs/2026-06-11-wasm-execution-runtime-design.md`
Plan doc: `src/staging/docs/2026-06-11-wasm-execution-runtime-plan.md`

### Guest ABI contract

Every program submitted to the network must export exactly:

| export | type | role |
|---|---|---|
| `memory` | memory, min == max | fixed linear memory; no growth |
| `alloc(i32) -> i32` | func | host calls to allocate `len` bytes, returns ptr |
| `run(i32, i32) -> i64` | func | host calls with `(in_ptr, in_len)`, returns packed i64 |

**Packed return encoding:** `run` packs `(out_ptr, out_len)` as a single `i64`:

```
packed = (((out_ptr as u64) << 32) | (out_len as u64)) as i64
```

**Signed-shift footgun:** `i64` is a *signed* type in WASM. A guest that shifts a pointer
value with `i64.shl` and then sign-extends the result will silently corrupt the upper half
when the pointer has bit 31 set. The host decodes `packed` by casting to `u64` first (see
`abi::unpack`), so a mis-packed value produces garbage pointers that the bounds check rejects
deterministically — but the guest author must use unsigned arithmetic throughout.

Zero host imports are allowed. Any import section causes an immediate gate rejection (rule 7);
the host nondeterminism surface is structurally absent, not merely denied by config.

### Determinism gate (reject rules)

`src/wasm/validation.rs` applies two layers. Layer 1 validates under a locked
`WasmFeatures` allow-list; layer 2 scans constructs the feature flags cannot express.

| # | reject rule | layer |
|---|---|---|
| 1 | floats (f32/f64 instructions or types) | 1 — FLOATS subtracted from WASM1 |
| 2 | SIMD / relaxed-SIMD | 1 — not in GATE_FEATURES |
| 3 | threads / atomics (shared memory) | 1 — not in GATE_FEATURES |
| 4 | `memory.grow` or `table.grow` opcode present anywhere | 2 — opcode scan |
| 5 | memory or table with `min != max` (unbounded or growable) | 2 — memory/table section scan |
| 6 | memory max exceeding 64 MiB cap (1024 pages) | 2 — page count check |
| 7 | any import section present | 2 — structural check |
| 8 | missing or ill-typed required exports: `memory`, `alloc(i32)->i32`, `run(i32,i32)->i64` (see `abi.rs`) | 2 — export section scan + `abi::bind` typed get |
| 9 | `start` section present | 2 — structural check |

CONSENSUS-COUPLING: `GATE_FEATURES` is a compile-time constant. Changing it is a
validation-policy change — `VALIDATION_VERSION` in `limits.rs` must be bumped in the same
commit, or the policy drift will not fail loud (the fingerprint folds the version, not the
raw bits).

### Limits and consensus criticality

| limit | default |
|---|---|
| fuel | 100 000 000 instructions |
| max memory | 64 MiB |
| max call depth | 1 024 frames |
| max stack height | 1 048 576 (1 << 20) |
| max input / output | 10 MiB each |

**These are consensus-critical.** The exact wasmi pin (`= 1.0.9` in `Cargo.toml`) and
every limit above are folded directly into `WasmLimits::config_fingerprint()`. `GATE_FEATURES`
is covered *indirectly* — any change to it MUST be accompanied by a `VALIDATION_VERSION` bump
(see the coupling note in validation.rs), which then diverges the fingerprint.
The fingerprint is embedded in every outcome digest (both ok and error). Two nodes that
disagree on any single value diverge on every job — loudly, by design. Upgrading the engine
version or changing any limit is a **coordinated protocol change**, not a silent bump.

### Canonical outcome digest

The trait method `ExecutionOracle::run` always returns a 32-byte digest:

- **Success:** `sha256(DOMAIN ‖ fingerprint ‖ 0x00 ‖ output)`
- **Any error:** `sha256(DOMAIN ‖ fingerprint ‖ 0x01)`

`DOMAIN = b"commputer-pouw-wasm-v1"`.

One sentinel covers every error kind (gate rejection, hash mismatch, out-of-fuel, runtime
trap, ABI violation). Which specific error occurred is local log data only — it is never
exposed in the digest. This eliminates any covert trap channel between executor and verifier.

`ProgramUnavailable` is the one error that is not inherently deterministic — it depends on
whether the node's local store holds the program bytes. The current prototype populates every
node's store before running, so this is safe. The production DA (data-availability) cycle
decides whether an unavailable program causes abstain or sentinel; that decision is deferred.

**Error-sentinel settlement policy (founder-locked, spec §6):** a job whose agreed outcome is the error sentinel settles `Confirmed` with the executor paid the full worker share. The protective end: an out-of-fuel job effectively burned the full fuel budget, so paying less would enable executor-griefing via designed-to-OOF submissions; the generous end: an instant-error guest burns ~0 fuel yet collects the worker share, but this is submitter-self-inflicted — no third party can inject errors into another's deterministic job (the ProgramUnavailable caveat above is the lone exception, deferred to the DA cycle), and wash-trading errors loses at least the burn share per job. Refining this (paying OOF differently from gate-rejects) requires fuel-consumed in the claim format — a deferred game change (spec §10.2).

### Fuel metering

Wasmi's built-in instruction fuel counter is the **only** consensus meter. Wall-clock time is
explicitly forbidden as a meter — it is non-deterministic and would cause false disputes
between nodes running on different hardware or under different load. On an out-of-fuel trap,
wasmi leaves a small remainder (consumed < budget); the consensus property is that `fuel_consumed`
is **equal** across nodes given identical (program, input, limits, engine version), not any
particular absolute value. `fuel_consumed` is recorded in `ExecOutcome` and available for the
future cost-coupling cycle but is **not yet wired into settlement** — in v1, a failing program
settles `Confirmed` and the executor is paid 85% (the deliberate bootstrap policy; see deferred
items below).

### Guest rebuild

`guest-example/build-guest.sh` rebuilds `src/wasm/fixtures/guest_example.wasm` from the Rust
source in `guest-example/src/`. The flags satisfy all gate rules:
`-C link-arg=--initial-memory=1048576 -C link-arg=--max-memory=1048576` (min==max memory,
rule 5), `-C link-arg=-zstack-size=131072` (bounded stack), `-C target-cpu=mvp` (plain integer
MVP, no post-MVP surprises). The bump arena allocator in the guest source uses no `memory.grow`
(rule 4).

Current checked-in artifact sha256:
`32695999cdee9a0e31dc29024abcd3e7fbd37ab85ede1a93896b24bd2bbc55e5`
Built with: `rustc 1.94.0 (4a4ef493e 2026-03-02)` (run `build-guest.sh` to confirm; it prints
the toolchain beside the artifact hash).

### FOUNDER CI NOTE (cross-arch gate — REQUIRED for testnet)

Same-arch determinism (x86_64 → x86_64) is demonstrated by the `two_independent_oracles_agree_exactly`
and `determinism_properties::independent_oracles_always_agree` tests, which assert byte-identical
digests and identical `fuel_consumed` across fresh engine + store instances on this machine.

**The cross-arch gate — the same corpus on x86_64 AND aarch64 runners asserting byte-identical
digests and identical `fuel_consumed` — is a REQUIRED CI step for the testnet path and has NOT
yet been run.** Interpreter-by-construction makes the risk low (wasmi evaluates the WASM
instruction stream without generating native code), but the gate makes it checked. This must be
wired into CI before the testnet launch gate.

### Deferred items

- **Fuel → economics:** The v1 policy that a failing program settles `Confirmed` and the
  executor is paid 85% is deliberate bootstrap. The full fuel→settlement coupling (partial
  payment, gas refund, executor cost amortisation) is a follow-up cycle.
- **DA / fetching:** `ProgramUnavailable` resolution — whether a node abstains or emits the
  error sentinel when it cannot fetch the program bytes — depends on the DA layer design.
- **On-chain consensus params (founder):** `WasmLimits` and `VALIDATION_VERSION` live in
  staging; moving them into the chain's consensus params is protected-file work.
- **Floats / AI-equivalence:** The float ban is conservative. The approximate-equivalence
  oracle seam (`EquivalenceOracle`) exists for a future cycle that relaxes this for AI tasks.
- **Stack-height static analysis:** The `max_stack_height` wasmi config cap is dynamic (trap
  at runtime); static analysis of the WASM operand stack at validate-time is not yet done.
- **Compiled-module cache:** Wasmi compiles (`CompilationMode::Eager`) on every
  `Module::new` call. A per-node module cache keyed by `program_hash` is a follow-up
  performance optimisation; it does not affect consensus.

### Fuel economics (fuel-economics spec, 2026-06-11)

Run the real-fuel sweep: `cargo run -p commputer-pouw --features wasm-runtime --bin pouw-sim --release`.

The full 72-row table is reproducible via the command above with the fixed `DEFAULT_SEED`.

#### Measured fuel per class

```
class  guest: measured fuel        36117
class  light: measured fuel      2400009
class  heavy: measured fuel     60000009
sweep class: guest (headline; light/heavy validated via fixtures)
```

#### Verdict

```
REAL-FUEL ECONOMICS: 48 safe corners — HONEST WORK PROFITABLE, CHEATING LOSES MONEY
```

#### Legend

```
legend: hx/hv = honest executor/verifier EV (honest-equilibrium run); cheat/lazy/rstamp = mixed-run EVs; v_bond = per-verifier stake at the formula minimum
corner (s/t/v/k/cap)          budget    v_bond       hx_ev       hv_ev       cheat        lazy      rstamp  SAFE?
```

Column key: `s` = sample_rate_bps, `t` = p_trap_bps, `v` = verifier_bps (share of budget), `k` = committee size, `cap` = 100M (global WasmLimits fuel cap) or `tight` (fuel cap set to measured fuel exactly).

#### All safe tight-cap corners (the real story)

Of the 72 corners, 48 are safe. The 24 safe tight-cap rows below are the operational story: these are the corners where the fuel cap is set to the actual measured fuel for the program class, so bonds are honestly sized.

```
5000/1000/1000/3/tight         43200      9000     35720.0       207.8     -4013.8     -4304.8     -1020.2    yes
5000/1000/1000/5/tight         72000      9000     60200.0       195.9     -4306.6     -7923.9     -1175.2    yes
5000/1000/2500/3/tight         17280      9000     11096.0       206.1     -3261.2     -2602.8     -1063.8    yes
5000/1000/2500/5/tight         28800      9000     19160.0       200.9     -3719.3     -4589.4     -1025.4    yes
5000/1000/4000/3/tight         10800      9000      4940.0       199.8     -2534.6     -2098.7     -1067.8    yes
5000/1000/4000/5/tight         18000      9000      8900.0       221.4     -4264.6     -3797.9     -1030.5    yes
5000/2500/1000/3/tight         54000      4500     44900.0       201.2     -4820.8     -4855.6     -1032.0    yes
5000/2500/1000/5/tight         90000      4500     75500.0       218.9     -5189.1     -6308.9     -1079.2    yes
5000/2500/2500/3/tight         21600      4500     14120.0       209.4     -3593.3     -2967.2     -1100.1    yes
5000/2500/2500/5/tight         36000      4500     24200.0       219.8     -4938.0     -5702.5     -1068.1    yes
5000/2500/4000/3/tight         13500      4500      6425.0       207.9     -3108.2     -3168.0     -1124.5    yes
5000/2500/4000/5/tight         22500      4500     11375.0       197.0     -5167.7     -4748.3     -1118.6    yes
10000/1000/1000/3/tight        39600     16500     32660.0       194.0    -39600.0    -39600.0     -1157.1    yes
10000/1000/1000/5/tight        66000     16500     55100.0       199.7    -66000.0    -66000.0     -1129.6    yes
10000/1000/2500/3/tight        15840     16500     10088.0       202.2    -15840.0    -15840.0     -1013.3    yes
10000/1000/2500/5/tight        26400     16500     17480.0       208.5    -26400.0    -26400.0     -1079.4    yes
10000/1000/4000/3/tight         9900     16500      4445.0       201.1     -9900.0     -9900.0     -1092.0    yes
10000/1000/4000/5/tight        16500     16500      8075.0       207.1    -16500.0    -16500.0      -975.1    yes
10000/2500/1000/3/tight        45000      7500     37250.0       182.5    -45000.0    -45000.0     -1091.9    yes
10000/2500/1000/5/tight        75000      7500     62750.0       204.6    -75000.0    -75000.0     -1147.7    yes
10000/2500/2500/3/tight        18000      7500     11600.0       210.4    -18000.0    -18000.0     -1061.6    yes
10000/2500/2500/5/tight        30000      7500     20000.0       195.7    -30000.0    -30000.0     -1095.9    yes
10000/2500/4000/3/tight        11250      7500      5187.0       193.3    -11250.0    -11250.0     -1171.6    yes
10000/2500/4000/5/tight        18750      7500      9312.0       188.4    -18750.0    -18750.0     -1085.3    yes
```

#### Why 100M-cap corners fail

The 24 100M-cap corners in the s=2500 (25% sampling) half all fail. Representative failing rows:

```
2500/1000/1000/3/100M        5040000    525000   4283000.0    118049.5   2100000.0   1996456.4   -108887.7     NO
2500/2500/2500/5/100M        4800000    300000   3359000.0    122700.4   1358369.9   1475580.9   -113936.3     NO
2500/2500/4000/3/100M        1800000    300000    989000.0    117765.3    290997.8    310412.5   -112092.6     NO
```

Why: at a 100M-cap the formula prices the budget against a 100M-fuel job, making it enormous (millions of units). Against that budget, an undetected cheat's payoff (the full worker share of the huge budget) dwarfs the expected slash (bond sized to the 100M-cap formula minimum). The lesson is direct: the safe regime requires that fuel caps are honest, i.e., priced to the actual program's fuel usage — a per-job fuel cap, which is the deferred fuel-coupling cycle (spec §10.2). The 24 safe 100M-cap corners (the s=5000 and s=10000 rows) survive because their sampling rate is high enough that the catch probability alone closes the inequality even at inflated scale.

#### Recommended regime

**`5000/2500/2500/5/tight`** — s=50% sampling, t=25% trap rate, v=25% verifier share, k=5 committee, tight cap — is the safe corner with the lowest verifier-capital requirement (v_bond=4500) among the tight-cap corners with the highest honest-verifier EV margin (hv_ev=219.8).

Adopting any corner as new `GameParams` defaults is a founder decision — note the `v_bond` column (a verifier must stake this per seat; at tight-cap/guest-class the minimum is 4500 units).

**Deliberate policy reminder:** failing programs settle `Confirmed` with the executor paid the full worker share (see the error-sentinel settlement policy paragraph above).
