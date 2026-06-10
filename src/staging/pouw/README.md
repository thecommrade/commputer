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

From the workspace root (`/home/operator/Coin/src`):

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
