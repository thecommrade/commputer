# Proof-of-Useful-Work — Verification & Settlement Game (Design Spec)

**Date:** 2026-06-10
**Status:** Design approved (brainstorm) → *this doc* → spec review → founder review → implementation plan
**Branch:** `agent-wire-testnet-20260610` (agent branch; staging-only, never merged directly)
**Author context:** First slice of the larger PoUW / compute-market layer. Deterministic-first, self-contained, **runnable** prototype + simulation harness. No protected-file edits.

---

## 1. Why this exists

The mission is that ordinary people **own and get paid for** their compute. The chain's whole point is to coordinate that fairly. The unsolved core is **verification**: how does the network know a worker actually did the useful computation it was paid for, *without* every other node redoing the work?

A subsystem map (2026-06-10, 5-agent sweep) found that the compute-job layer is an extensive **skeleton with no muscle**:

- **Wired & live:** four tx kinds (`SubmitJob/ClaimJob/CompleteJob/DisputeJob`) drive a persisted `JobPool` state machine via `event_loop::process_job_tx`. So *bookkeeping* is on-chain.
- **Dead scaffolding** (compiles, unit-tested, zero runtime callers): `consensus/job_verification.rs` (committee + hash-quorum), `consensus/dispute.rs` (3-re-executor ⅔-majority + 50% slash), `wasm_executor` (a SHA-256 *stub* — no job is ever actually run), the pricing/billing stack, and ~8 redundant `#[allow(dead_code)]` node modules.
- **Verification today = none.** `CompleteJob` stores the executor's **self-reported** `result_hash` verbatim. `DisputeJob` flips a status flag with no teeth.

So correctness is established by **trust in the executor**. This spec designs the thing that replaces trust with a game in which **cheating loses money**.

This is genuine frontier territory — trustlessly verifying *arbitrary* useful computation is unsolved in general. We therefore scope hard (§2) and compose primitives that are each proven in production (§4.5).

## 2. Goals & non-goals

**Goals**
1. A self-contained Rust library that implements the verification-and-settlement **protocol** for **deterministic** jobs.
2. A **simulation harness** that *empirically proves* the incentive properties: honest play is the best response; every cheating strategy is EV-negative within an identified parameter regime.
3. Clean **trait seams** so a real execution runtime, an approximate-equivalence oracle, and on-chain wiring drop in later **without changing the game**.

**Non-goals (each a committed follow-up cycle — see §11)**
- A real execution runtime (WASM/WASI sandbox). Execution here is a deterministic *oracle*.
- Data availability (storing/serving job inputs & outputs; replication).
- On-chain commitment of verdicts and wiring into `event_loop`/`JobPool`/`dispute.rs` (protected files; founder).
- Non-deterministic / approximate verification (AI inference). The pluggable equivalence oracle is the seam for this.
- A real VRF/randomness beacon (committee selection uses a modeled seed).

## 3. Locked design decisions (from the brainstorm)

| Decision | Choice | Rationale |
|---|---|---|
| Slice | Verification & settlement game | The hard, reusable heart; reuses the dead `job_verification`/`dispute` math. |
| Workload | **Deterministic-first**, behind a pluggable `EquivalenceOracle` | Re-execution + hash-compare only works on deterministic output. Most AI inference is *not* bit-deterministic; the oracle is the seam for cycles B/C. |
| Economics | **Hybrid escrow → 85% worker / 10% verifiers / 5% burn**; slashed stake burned; submitter refunded on proven-bad | The game needs value to move on the verdict (carrot/cut/stick); the burn slice preserves the supply-only-shrinks thesis. |
| Threat model | **Commit-reveal + VRF/stake committee + trap jobs** | Defeats the **verifier's dilemma** (rubber-stamping) — the attack that quietly kills most verification schemes. |
| Topology | **C: sampled committee + escalation** | Cost-tunable (`~(1 + K·sample_rate)×`), strong on the contested case. Cheap-common / strong-contested. |

### 4.5 Precedent (why these primitives, not invention)
No top-tier L1 does exactly this for *arbitrary* compute, but each piece is battle-proven:
- **Optimistic rollups (Arbitrum/Optimism):** optimistic acceptance + challenge window + **interactive bisection** to a single disputed step + bond slashing. → our escalation/dispute spine.
- **Truebit:** **forced errors** (announced-wrong solver outputs + jackpots) to keep verifiers honest. → our trap jobs.
- **Avalanche/Snowball (the consensus this chain already runs):** agreement by **repeated random sub-sampling**. → our sampled committee.
- **Filecoin:** pays for *verified useful work* (storage) with staked collateral that's **slashed** on failed proofs. → our stake-and-slash settlement (different verification *family* — proofs — which is a later cycle).
- **Bittensor (cautionary):** verifies AI by *subjective validator scoring*, repeatedly gamed by collusion. → exactly why we start with objective deterministic re-execution, not scoring.

## 4. Architecture

A self-contained crate at **`src/staging/pouw/`**, added to the workspace `members` so it **compiles, runs, and is tested** (edits `src/Cargo.toml` — non-protected — only to add the member). It depends only on `sha2`, `rand` (seeded), and `proptest` (dev). It does **not** depend on the node/consensus/storage crates; instead it defines its own abstract `ChainHooks` so it can be lifted into the real chain later behind an adapter.

```
src/staging/pouw/
  Cargo.toml
  src/
    lib.rs            // re-exports; crate-level docs
    job.rs            // Job, JobId, JobSpec, ParticipantId, budgets/bonds
    oracle.rs         // ExecutionOracle, EquivalenceOracle, ChainHooks (the seams)
    committee.rs      // seed-based, stake-weighted, post-commit committee selection
    commit_reveal.rs  // Commitment, Reveal; binding + hiding helpers
    trap.rs           // TrapJob injection + detection (verifier's-dilemma killer)
    verdict.rs        // quorum over reveals via EquivalenceOracle -> Verdict
    escalation.rs     // NoQuorum/challenge -> larger re-execution quorum (wires dispute.rs logic)
    settlement.rs     // escrow, 85/10/5, slashing, refunds, the incentive accounting
    engine.rs         // drives one job through the whole game (orchestrator)
    params.rs         // GameParams (K, sample_rate, p_trap, quorum, bonds, split)
  sim/                // the proof harness (separate bin or tests/)
    agents.rs         // honest/lazy/cheating executor; honest/rubber-stamp/colluding verifier
    tournament.rs     // Monte-Carlo driver + metrics
```

Each module has one purpose and a small interface; `engine.rs` is the only place that knows the full sequence.

## 5. Data model (key types)

```rust
pub struct ParticipantId(pub [u8; 32]);          // a staked actor (executor or verifier)
pub struct JobId(pub [u8; 32]);                  // = H(spec_hash ‖ input_hash ‖ submitter ‖ nonce)

pub struct JobSpec {                              // deterministic by construction in this cycle
    pub program_hash: [u8; 32],                  // identifies the deterministic program
    pub input_hash:   [u8; 32],                  // commitment to the input bytes
}

pub struct Job {
    pub id: JobId,
    pub submitter: ParticipantId,
    pub spec: JobSpec,
    pub budget: u64,                              // escrowed at submit
}

pub struct ExecutorClaim { pub executor: ParticipantId, pub result_hash: [u8; 32], pub bond: u64 }

pub struct Commitment { pub verifier: ParticipantId, pub commit: [u8; 32] } // H(result_hash ‖ salt ‖ verifier)
pub struct Reveal     { pub verifier: ParticipantId, pub result_hash: [u8; 32], pub salt: [u8; 32] }

pub enum Verdict {
    Confirmed { result_hash: [u8; 32] },         // committee agrees WITH the executor
    Disputed  { correct_hash: [u8; 32] },        // committee agrees on a DIFFERENT value
    NoQuorum,                                    // -> escalate
}

pub struct SettlementOutcome {
    pub worker_paid: u64, pub verifiers_paid: u64, pub burned: u64,
    pub submitter_refunded: u64,
    pub slashed: Vec<(ParticipantId, u64)>,      // executor and/or dishonest verifiers
}
```

## 6. The verification game (one job's life)

Phases, driven by `engine.rs`:

1. **Submit.** `budget` is escrowed via `ChainHooks::escrow`. Executor claims and posts `bond ≥ budget` (so a slash ≥ the cheating gain). 
2. **Execute.** Executor runs `ExecutionOracle::run(spec, input)`; posts `ExecutorClaim{ result_hash }`. The claim is fixed before verifiers are known (binds the executor; enables unpredictable selection).
3. **Sample.** A `seed` (modeled VRF; later a block hash / anchor VRF, *unknowable before step 2*) draws **K** verifiers, **stake-weighted**, excluding the executor (`committee.rs`). Only a `sample_rate` fraction of jobs get a full committee; **trap jobs cover the unsampled population probabilistically**.
4. **Commit.** Each selected verifier independently runs the job and submits `Commitment = H(result_hash ‖ salt ‖ id)`. Hiding ⇒ no verifier can copy another's or the executor's answer.
5. **Reveal.** After all commit (or a timeout), verifiers reveal `(result_hash, salt)`. A reveal that doesn't match its commitment is discarded and the verifier penalized (no-show).
6. **Verdict** (`verdict.rs`, quorum = `ceil(2/3·K)` agreeing under `EquivalenceOracle`):
   - committee value **==** executor claim → `Confirmed`.
   - committee value **!=** executor claim → `Disputed` (executor was wrong).
   - no value reaches quorum → `NoQuorum` → **escalate**.
7. **Traps** (`trap.rs`, probability `p_trap`). The protocol injects a job whose **true** answer it knows but presents the executor-result as a **deliberately wrong** hash. Any verifier who reveals the *wrong* answer (i.e., rubber-stamped, or computed-and-lied) is **slashed**; verifiers who reveal the *true* answer get a **jackpot**. This makes "skip the work and echo the executor" strictly EV-negative.
8. **Escalation** (`escalation.rs`). `NoQuorum` or a staked **challenge** triggers a larger re-execution quorum (K′ > K, e.g. 7), ⅔-majority binding. This is precisely the logic the dead `consensus/dispute.rs` `resolve_dispute` already encodes (3 re-executors, ⅔, 50% slash) — generalized and finally given a caller. The loser (executor or a false challenger) is slashed.
9. **Settle** (`settlement.rs`). On the final verdict:
   - `Confirmed`: release escrow **85% worker / 10% to revealing-honest verifiers / 5% burn**; bonds returned.
   - `Disputed`: executor **bond slashed (burned)**; **submitter refunded** the budget (minus the verifier reward funded from the slash); honest verifiers paid from the slash.
   - All slashed stake is **burned** (preserves the deflationary thesis).

## 7. Economics — the one inequality everything serves

For honesty to be the dominant strategy:

```
E[gain from cheating]  <  P(caught) · slash
```

- **Lazy/cheating executor:** gain ≈ saved compute `C_exec`. `P(caught) = P(sampled) + P(trap) − overlap`. Require `(P(sampled)+P(trap)) · bond > C_exec` for all jobs ⇒ pick `bond ≥ budget` and tune `sample_rate`, `p_trap`.
- **Rubber-stamp verifier:** gain ≈ saved verify cost `C_ver`. Require `p_trap · verifier_bond > C_ver`.
- **Collusion:** executor + `f` colluding committee members win only if they reach quorum; `f ≥ ceil(2/3·K)` of a *stake-weighted, post-commit-unpredictable* committee is required — the sim reports the stake fraction at which this becomes feasible, and escalation raises the bar further.

These parameters (`K`, `sample_rate`, `p_trap`, `bond`, split) are **knobs the simulation sweeps**; the identified safe regime *is* a primary deliverable.

## 8. The pluggable seams (`oracle.rs`)

```rust
/// Deterministic execution. Now: a toy VM (iterated hashing / tiny op set).
/// Later: a real WASM/WASI runtime. The game never sees inside.
pub trait ExecutionOracle { fn run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8>; }

/// "Are these two results equivalent?"  Now: byte/hash equality (deterministic).
/// Cycle B (reproducible AI): same, with pinned envs. Cycle C (approximate):
/// tolerance / semantic equivalence. THE GAME LOGIC IS UNCHANGED ACROSS B/C.
pub trait EquivalenceOracle { fn equiv(&self, a: &[u8; 32], b: &[u8; 32]) -> bool; }

/// Abstract stake/value operations. Now: an in-memory ledger.
/// Later: an adapter onto real ChainState / JobPool / event_loop (founder, protected files).
pub trait ChainHooks {
    fn escrow(&mut self, who: ParticipantId, amount: u64);
    fn pay(&mut self, to: ParticipantId, amount: u64);
    fn burn(&mut self, amount: u64);
    fn slash(&mut self, who: ParticipantId, amount: u64); // burned
    fn stake_of(&self, who: &ParticipantId) -> u64;
}
```

The deterministic `EquivalenceOracle` impl is literally `a == b`. That is the entire cost of "deterministic-first": one trait method, swapped later, with the incentive game untouched.

## 9. Simulation & testing — how we *prove* it

**Property tests (`proptest`, seeded):**
- Honest executor + honest committee ⇒ always `Confirmed` and exact 85/10/5 settlement.
- Cheating executor ⇒ caught with probability ≥ target; net EV < 0 over many jobs.
- Rubber-stamp verifier ⇒ trap-slashed; net EV < 0.
- Collusion of `f` verifiers ⇒ fails below the stake fraction implied by K/quorum; succeeds only above it (documents the bound).
- `NoQuorum` ⇒ escalation produces a binding verdict and slashes the loser.
- Settlement conserves value (escrow in = paid + burned + refunded; no mint).

**Monte-Carlo tournament (`sim/`):** many jobs, a mix of agent strategies, seeded RNG. Reports: % cheats caught, honest-vs-cheat average profit (cheat must be ≤ 0), and a **best-response check** that "honest" is the Nash strategy under the chosen params. Output is a short table the founder can read.

**Success = the prototype compiles, the property tests pass, and the tournament demonstrates a concrete parameter regime where every modeled cheating strategy loses money.**

## 10. Out of scope → committed follow-up cycles

1. **Execution runtime** — real metered WASM/WASI sandbox behind `ExecutionOracle`.
2. **Data availability** — content-addressed input/output blobs, retrieval, replication/DA sampling so verifiers can fetch what the executor ran on.
3. **On-chain wiring** — commit verdicts into state root; wire into `event_loop`/`JobPool` and finally call the real `dispute.rs`; replace the modeled seed with the anchor VRF. (Protected files → founder.)
4. **Cycle B** — reproducible-AI equivalence (quantized/fixed-seed/pinned-kernel) via `EquivalenceOracle`.
5. **Cycle C** — approximate/semantic equivalence for general float/GPU inference.
6. **Real VRF/beacon** for committee selection.

## 11. Risks & open questions

- **Toy-VM realism.** The deterministic `ExecutionOracle` is a stand-in; conclusions about *compute cost* are only as real as the toy cost model. Mitigation: parameterize `C_exec`/`C_ver` so the regime is expressed in ratios, not absolute cycles.
- **Sampling vs. trap balance.** Lowering `sample_rate` saves compute but leans harder on `p_trap`; the sim must show the trade-off curve, not a single point.
- **Escalation recursion ("who watches the watchers").** Escalation raises committee size but is itself a committee; we cap escalation depth and rely on the (larger) stake-weighted quorum + slashing, exactly as rollups cap their bisection. Documented as a known bound, not a claim of infinite recursion safety.
- **Collusion realism.** Stake-weighting means a wealthy colluder is the real threat; the sim reports the *stake fraction* threshold, and we state it plainly rather than claiming collusion is impossible.
- **Mapping to the chain later.** `ChainHooks` is designed to map onto the existing escrow/burn/slash/stake operations, but the real wiring touches protected files; the prototype delivers the trait + an adapter *sketch*, and the founder owns the integration.

## 12. Success criteria (definition of done for this slice)

1. `cargo test -p commputer-pouw` (or the chosen crate name) is green: property tests pass.
2. The tournament binary runs and prints a regime where all modeled cheating strategies are EV-negative.
3. The three trait seams exist and the deterministic impls are swappable (a stub "approximate" oracle compiles against the same game, proving the seam).
4. Zero protected-file edits; the only non-`staging/` change is adding the crate to `src/Cargo.toml` `members`.
5. A short README in the crate explains how to run the sim and read its output.
