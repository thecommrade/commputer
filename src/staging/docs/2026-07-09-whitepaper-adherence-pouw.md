# PoUW Compute-Jobs — Whitepaper-Adherence Gap Report + Completion Plan

**Date:** 2026-07-09
**Author:** agent (read-only audit synthesis) · Branch `agent-testnet-20260707`
**Scope:** the as-built Proof-of-Useful-Work compute-job PAY-OUT feature (submit → escrow → claim →
execute → commit/reveal verification → settle) measured clause-by-clause against
`protocol/whitepaper/WHITEPAPER.md` §3 (Core Principles), §7 (Tokenomics / Three Burn Mechanisms),
§9 (Network Architecture / Compute Jobs). **This doc modifies no code.** It creates only itself.

**Purpose.** Answer one founder question: *does the as-built PoUW pay-out feature adhere to the
whitepaper, and what must we do to complete it faithfully?* Separate **"built wrong / contradicts
spec" (fix before advertising)** from **"not built yet but legitimately §4-📋-Planned" (fine — the
honest state; just don't over-claim it)**.

**Provenance.** Synthesis of a 6-dimension adversarial audit (verification game; resource-profile
routing; the 51/49 split + dynamic reserve; dynamic job pricing; escrow/burn/emission; sandbox +
confidentiality), each cross-checked whitepaper-says / code-does / gap / fix / severity, with every
claim cited to `file:line`. Load-bearing cites spot-verified against the tree at
`agent-testnet-20260707` (the BLOCKER at `lifecycle.rs:646-661` + `settlement.rs`, and the escalation
plan commit `674593b`).

**STATUS.** The PoUW **money path is LIVE + e2e-proven** (apply arms, in-apply committee draw,
`settle_due_jobs`, five conserving terminal resolvers; §1.4 multinode gate PASS). The **Compute
Jobs** subsection of the whitepaper is explicitly tagged **📋 = Planned (Phase 3+)** by the §4
legend (WHITEPAPER.md:49) and the 📋 marker at WHITEPAPER.md:324. That status is the frame for
everything below: it makes *unbuilt* dimensions defensible, but it does **not** excuse *built-wrong*
economics (the BLOCKER) or the three enshrined-never-change Core Principles (§3), which are **not**
roadmap-gated.

> **Feature-status legend (WHITEPAPER.md:49):** ✅ Live in Phase 1 · 🔧 In Flight (Phase 2) ·
> 📋 Planned (Phase 3+).

---

## §1 — VERDICT

**The as-built PoUW pay-out is economically SOUND and CONSERVED, and structurally faithful to the
whitepaper's verification game — with one BLOCKER that inverts an anti-collusion invariant, and a
set of MAJOR divergences that are all in the "specified-behavior-not-yet-built" or
"built-but-soft/unwired" class rather than fund-loss bugs.** The core machinery the whitepaper
promises — stake-weighted committee sampling, strict commit-before-reveal, quorum-triggered dispute,
executor-bond slash + submitter refund, escrow-not-destroy with an executor/verifier/burn three-way
split, a 100%-fee-burn + fixed-2B-cap + Bitcoin-halving emission, and a sandbox the job "cannot
access the validator's system" — is genuinely present and tested (§2). **The one thing that is built
*wrong* and must be fixed before the verification game can be honestly advertised is the
rubber-stamp forfeiture (§332): a verifier that reveals the executor's WRONG hash and is out-voted
gets its full bond back at zero cost, making rubber-stamping a free option** — the exact deterrent
the whitepaper's forfeiture clause exists to create is absent (BLOCKER, §3.1). The remaining top gaps
are honest-roadmap or wiring shortfalls that public materials must not claim as done: **resource-profile
routing does not exist in the live claim path and the job's declared `resources`/`max_duration` are
dropped from on-chain state entirely** (MAJOR, §3.2/3.3); **the "Protocol-enforced" 51/49 split is
producer-side soft scheduling, not a consensus rule, and its "dynamic reserve" is pinned at the 5%
floor because churn is hardcoded to zero** (MAJOR, §3.4/3.5); **dynamic load-based pricing is unbuilt
in the live path — only a static floor is enforced, and near-capacity jobs are silently deferred
rather than priced out** (MAJOR, §3.6/3.7); **automatic capacity-milestone burns are not wired**
(MAJOR, §3.8); and **job-content confidentiality ("the validator cannot see the job contents") is
not merely unbuilt but architecturally foreclosed by the re-execution verification model the same
section specifies** — a founder/whitepaper decision, not a code fix (MAJOR, §3.9). Net: the built
economics are internally consistent and safe to run as an honestly-labeled alpha; the work to reach
full adherence is (1) fix the one inverted invariant now, then (2) build the routing/pricing/enforcement
features as the Compute-Jobs 📋 line items land, while (3) softening or scoping the handful of
whitepaper sentences the chosen architecture cannot satisfy.

**Tally:** 1 BLOCKER · 8 MAJOR · 11 MINOR · 9 ADHERES.

---

## §2 — ADHERES (already faithful to the whitepaper)

These clauses are implemented as specified. Each is a genuine build that meets or exceeds the 📋 bar.

### 2.1 Verification game: committee sampling, commit-before-reveal, quorum dispute, slash + refund
- **WP §332 (WHITEPAPER.md:332)** — "a stake-weighted committee … sampled to independently
  re-execute … committing to their results before revealing them. If the committee reaches quorum
  against the executor's claim, the job is disputed, the executor's bond is slashed, and the submitter
  is refunded."
- **Code:** stake-weighted draw excluding the executor — `select_committee`
  (`src/staging/pouw/src/committee.rs:5-24`; `higher_stake_selected_more_often` test :43). Strict
  commit→reveal ordering enforced by on-chain phase gates: `record_commit` requires `Phase::Committing`
  and rejects past `commit_by` (`src/staging/pouw-onchain/src/lifecycle.rs:539-559`); `record_reveal`
  requires `Phase::Revealing` + rejects past `reveal_by` (`lifecycle.rs:562-581`); `advance()`
  transitions Committing→Revealing only when `height > commit_by` (`lifecycle.rs:586-591`);
  `reveal_matches` binds each reveal to its prior commitment (`lifecycle.rs:573`). Quorum =
  ceil(2/3·committee) (`src/staging/pouw/src/params.rs:37,76`) vs the largest reveal equivalence class
  in `compute_verdict` → Disputed when that class ≥ quorum and ≠ executor hash
  (`src/staging/pouw/src/verdict.rs:26-35`). Disputed slashes the full executor bond and refunds the
  full budget (`src/staging/pouw/src/settlement.rs:102-128`). Live via `lifecycle_settle`
  (`src/storage/src/state.rs:3606-3640`).
- **Verdict:** ADHERES.

### 2.2 Escrow-not-destroy + executor/verifier/burn three-way split
- **WP §326 (WHITEPAPER.md:326)** — "escrowed by the protocol, not destroyed: once the result is
  verified it is split between the executor …, the verifiers …, and a protocol burn." **§69** —
  "handled separately via escrow and a stake-weighted verification committee."
- **Code:** `SubmitJobV2` escrows, never burns (`is_burn()=false`, `burn_amount()=ZERO`;
  `src/core/src/transaction.rs:592-597`); apply moves balance→per-job pot via `escrow_into_job`
  without touching `total_burned` (`src/storage/src/state.rs:1523-1563`). Confirmed split =
  85% executor / 10% verifiers / 5% burn (`src/staging/pouw/src/params.rs:38`
  worker_bps=8_500 / verifier_bps=1_000 / burn_bps=500, sum-to-10_000 validated :53), delegated to
  the frozen `settle_confirmed_sampled` (`settlement.rs:54-66`). Conservation tests pin
  3_366/396/198 on a 3_960 budget, pot drained to 0, total_supply unchanged.
- **Verdict:** ADHERES (whitepaper mandates no specific percentages; 85/10/5 is a faithful
  realization that still burns a slice on every verified job).

### 2.3 The 51/49 split *math* + dynamic-reserve *formula* (the algorithm, not its enforcement/inputs)
- **WP §3 #1/#4 (WHITEPAPER.md:35,41)** — 51% flagship reserve; reserve subtracted *before* the split;
  `Reserve% = 5% + 10%×churn`, min 5% / max 15%.
- **Code:** `capacity::admit` (`src/staging/pouw-onchain/src/capacity.rs:93`) reserves the dynamic
  churn-based reserve first (`available_slots` :78), then gives flagship a hard 51% floor
  (`flagship_reserve_bps=5_100` :35) with the 49% remainder to others, work-conserving + deterministic
  + input-order-independent (:84-139). Flagship detection matches "tagged by core dev team" via
  `l2::is_flagship` / `FLAGSHIP_L2_ID="commputer-analytics-l2"` (`src/core/src/l2.rs:22,47`). Reserve
  formula `dynamic_reserve_bps` (floor 500 / max 1500 / coeff 1000; tests 1%→5.1%, 50%→10%, 100%→15%
  at `capacity.rs:196-203`). Tests at `capacity.rs:217-266` verify the 51/49 floors + overflow.
- **Verdict:** ADHERES *for the algorithm executed*. (Its enforcement posture and churn input are
  separate gaps — §3.4/§3.5.)

### 2.4 Transaction-fee burns — 100%, no treasury
- **WP §279 (WHITEPAPER.md:279)** — "100% of transaction fees are burned. No treasury split … Supply
  only goes down."
- **Code:** full `tx.fee` debited and 100% added to `total_burned`, no producer/treasury credit
  (`src/storage/src/state.rs:1214-1221`); `split_fee(fee)` returns `FeeSplit{burn:fee}`
  (`src/core/src/token.rs:185`). Block reward pays only the halving emission, never a fee cut.
- **Verdict:** ADHERES.

### 2.5 Fixed 2B cap — supply only decreases
- **WP §221-225 / §69** — "2,000,000,000 $COMME. Fixed. Final … can only decrease through burns."
- **Code:** `TOTAL_SUPPLY = 2_000_000_000 × 100_000_000` (`token.rs:10`). Only two mint paths: genesis
  (guarded against >cap and re-application, `state.rs:771-839`) and `credit_block_reward` capped to
  `remaining_supply` (`state.rs:706-728`). Settlement resolvers only pay/burn from an already-escrowed
  pot (never mint); burns only increment `total_burned`.
- **Verdict:** ADHERES.

### 2.6 Bitcoin-style halving emission
- **WP §227-239** — Bitcoin halving model; era-0 ≈15.85 COMME/block; halve every 63,072,000 blocks.
- **Code:** `INITIAL_BLOCK_REWARD=1_585_489_599` raw (15.85489599 COMME), `HALVING_INTERVAL=63_072_000`,
  `MAX_HALVINGS=32`, `block_reward(h)=era≥32?0:INITIAL>>(h/INTERVAL)` (`token.rs:14-22`); single source
  of truth via `consensus/emission.rs:20`. Geometric series sums to just under 2e9 COMME; `credit_block_reward`
  additionally caps to remaining supply.
- **Verdict:** ADHERES.

### 2.7 Sandbox — "the job cannot access the validator's system"
- **WP §330 (WHITEPAPER.md:330)** — "The job cannot access the validator's system."
- **Code:** `WasmOracle` runs guests on wasmi with a completely EMPTY `Linker` — zero host functions
  registered (`src/staging/pouw/src/wasm/oracle.rs:105`). The determinism gate structurally rejects
  any module declaring an import section (`src/staging/pouw/src/wasm/validation.rs:47`) or a start
  section (:49). No WASI, no syscalls, no filesystem/network handle reachable. Program bytes are pure
  in-memory content-addressed blobs (`store.rs`).
- **Verdict:** ADHERES — satisfied by construction, not merely by config; exceeds the 📋 bar.

### 2.8 Sandbox — "resource limits are enforced by the protocol" (the deterministically-enforceable meters)
- **WP §330** — "Resource limits are enforced by the protocol."
- **Code:** `WasmLimits` (`src/staging/pouw/src/wasm/limits.rs:32-43`) enforces fuel=100M via
  `consume_fuel` (`oracle.rs:32,93`), memory=64MiB via `StoreLimits` (`oracle.rs:85-92`),
  call-depth/stack caps (`oracle.rs:35-36`), input/output=10MiB (`oracle.rs:70,128`); `validation.rs`
  forbids `memory.grow`/`table.grow` (:98-99). Every cap folded into a SHA-256 `config_fingerprint`
  (`limits.rs:48-64`) so any disagreement diverges loudly. Wall-clock "max duration" is deliberately
  NOT a meter (`limits.rs:18-21`) because it is non-deterministic; fuel is the deterministic duration
  proxy.
- **Verdict:** ADHERES for the fuel/memory meters a re-execution game can safely enforce. (GPU/storage/
  bandwidth caps + `max_duration` belong to Routing — §3.3, unbuilt.)

---

## §3 — GAPS TO FIX (ranked BLOCKER → MAJOR → MINOR)

Each gap: **whitepaper says / code does / divergence / fix (wire-up vs real feature) / effort.**
"Contradicts-spec" = built wrong, fix before advertising. "Roadmap-📋" = legitimately unbuilt; the
only obligation is **do not over-claim it** (§5).

### ▲ BLOCKER

#### 3.1 Rubber-stamp verifier does NOT forfeit its bond — the §332 anti-collusion invariant is inverted
- **WP says (WHITEPAPER.md:332):** "Correct verifiers earn a share of the job's budget; a verifier
  who **rubber-stamps a wrong result forfeits its own bond**." This is the economic deterrent that
  makes colluding with a cheating executor costly.
- **Code does:** In the live primary Disputed round every revealer's bond is FULLY RETURNED, including
  the minority who revealed the executor's wrong hash. `lifecycle.rs` Disputed branch computes
  `honest` = only revealers whose `result_hash == correct_hash`, then calls `resolve_disputed` passing
  **ALL** revealers (`&revealed_ids`) as the `committee` arg (`lifecycle.rs:646-661`; verified — the
  branch passes `&revealed_ids` alongside `&honest`). `resolve_disputed`
  (`src/staging/pouw-onchain/src/settlement_resolution.rs:181-201`) delegates to the frozen
  `settle_committee_disputed` (`src/staging/pouw/src/settlement.rs:102-128`), which slashes ONLY the
  executor bond and then `for v in committee { l.pay(*v, verifier_bond) }` — returning every revealer's
  bond. Its own doc comment (`settlement.rs:174-179`) states "ALL committee bonds return." `apply_reveal`
  (`state.rs:1970-2002`) accepts any hash that opens the commitment, with no penalty for revealing the
  wrong one. The NoQuorum terminal is identical: `resolve_escalation_fallback`
  (`settlement_resolution.rs:130-146`) pays every revealer's bond back. The ONLY forfeiture in the
  primary round is the commit-no-reveal burn for NON-revealers (`lifecycle.rs:628-631`) — i.e.
  abstention/griefing, NOT rubber-stamping.
- **Divergence — CONTRADICTS SPEC.** Rubber-stamping is a **free option**: costless if the executor+
  verifier collusion fails (honest quorum wins → Disputed → wrong-side bond returned), and profitable
  if it succeeds (enough rubber-stamps flip quorum → Confirmed → wrong-side revealers are paid the 10%
  verifier share **and** get their bond back). The whitepaper's forfeiture clause exists precisely to
  make rubber-stamping costly-on-failure; as built that deterrent does not exist. (The frozen game
  *has* the slash-wrong-side machinery — `settle_noquorum_confirmed`/`settle_noquorum_disputed` slash
  `rejected_verifiers`, `settlement.rs:246-351` — but it is wired only into the escalation branches,
  which are unreachable on the primary Disputed terminal and inert live.)
- **Fix — REAL FEATURE (changes money flow + conservation), localized to the NON-frozen `pouw-onchain`
  layer so the frozen `pouw` crate stays byte-identical:**
  1. In `lifecycle.rs` Disputed branch also compute `wrong_side` = revealers whose `result_hash !=
     correct_hash`.
  2. In `resolve_disputed`, return bonds ONLY to `honest_verifiers`; BURN each `wrong_side` bond and
     record it in `SettlementOutcome.slashed` (mirror the `rejected_verifier` pattern already in
     `settle_noquorum_disputed`).
  3. Apply the analogous forfeiture on the **Confirmed** terminal to a dissenting revealer (reveal ≠
     confirmed value) in `resolve_confirmed`.
  4. **Founder decision:** forfeited bonds BURNED (conservative, matches the executor-bond remainder)
     vs added to the honest-verifier bounty pool — the whitepaper requires only that they are NOT
     returned.
  5. Update the B10 golden-equivalence + conservation tests that currently ASSERT the wrong behavior
     (`settlement_resolution.rs` `disputed_refunds_...` :268 asserts `bonds_returned == 3·v_bond`
     "all 3 committee bonds returned"; :272 asserts the non-honest member gets its bond back). These
     tests must be re-pinned to the corrected forfeiture.
- **Effort:** Medium. Localized to `pouw-onchain` (no protected file, no frozen-crate edit, no borsh/
  state-root schema change — it re-partitions an already-escrowed pot). The main cost is re-deriving
  conservation and rewriting the tests that lock in the current (wrong) behavior. **Do before the
  verification game is advertised as anti-collusion-secure.**

### ▲ MAJOR

#### 3.2 Resource-profile routing absent from the live claim path
- **WP says (WHITEPAPER.md:328):** "The network matches the job to validators with the right resource
  profile. A GPU-intensive job routes to validators with GPUs. A storage-heavy job routes to validators
  with disk space."
- **Code does:** The live V2 pull-based claim path performs ZERO resource matching. `plan_executor_actions`
  (`src/node/src/executor_planner.rs:149-221`) gates a claim only on: not-in-flight, not-already-ours,
  `now ≤ claim_by`, escrow affordability (`max(budget, executor_bond)` while keeping
  `min_balance_reserve`), and `max_concurrent_claims`. On-chain `apply_claim_job`
  (`src/storage/src/state.rs:1843-1922`) gates only on zero-addr / already-claimed / `is_validator` /
  known+unexpired job / claim window / `pot==budget` / bond escrow. Any validator claims any job it can
  afford regardless of machine fit.
- **Divergence — CONTRADICTS the routing spec** *but does NOT break economic invariants*: a mis-fitting
  executor that produces a wrong/absent result is caught by the verification game (slash/timeout/refund),
  so funds stay conserved. This is a specified-behavior divergence. Mitigation: the whole §322 Compute
  Jobs subsection is 📋 Planned, and the live kernel is single-profile CPU-WASM, so no true multi-profile
  routing is exercisable yet.
- **Fix — REAL FEATURE (depends on 3.3):** (1) persist `resources` into on-chain job state (3.3);
  (2) add a `ValidatorCapacity` self-descriptor from the node's calibrated proof scores; (3) add a
  `can_handle(job)` gate to `plan_executor_actions` BEFORE the affordability check. The predicate
  already exists — `ValidatorCapacity::can_handle` (`src/consensus/src/job_assignment.rs:23-32`) — and
  can be reused (do NOT resurrect the V1 push model). Interim honest step: the node should at minimum
  refuse to claim jobs whose declared profile it cannot serve.
- **Effort:** Medium (real feature), gated on the schema change in 3.3. Deferrable while the executor
  is CPU-WASM-only — but then §328 must not be advertised as done (§5).

#### 3.3 The job's `resources` + `max_duration` are DROPPED from on-chain state (root data-layer gap)
- **WP says (WHITEPAPER.md:326):** the job spec "describes resource requirements (CPU, GPU, RAM,
  storage, bandwidth), maximum duration" — implying the network retains and acts on them; §328 routing
  depends on it.
- **Code does:** `SubmitJobV2` carries the full profile (`src/core/src/transaction.rs:122-130`;
  `ResourceRequirements` has all 5 channels + `max_duration_secs`, `src/core/src/compute.rs:18-24`).
  But the apply handler DROPS both when writing state: `PendingJobRecord` is built with only
  `{submitter, budget, program_hash, input_hash, da_root, submitted_height, claim_by}`
  (`src/storage/src/state.rs:1565-1573`); the struct omits resources/max_duration
  (`state.rs:3254-3267`) with an explicit "l2_id/fee/resources deliberately excluded" comment
  (:3251-3252). The executor snapshot therefore cannot see them — `build_chain_view` maps
  `PendingJobRecord`→`OpenJob` with no resource fields (`src/node/src/executor_loop.rs:163-173`;
  `OpenJob` at `executor_planner.rs:76-86`).
- **Divergence:** the spec'd profile is accepted + validated at submit, then discarded from all
  consensus state before any routing/claim decision. This is the ROOT gap that makes 3.2 impossible —
  the matcher has nothing to match against.
- **Fix — REAL FEATURE + on-disk SCHEMA CHANGE:** add `resources: ResourceRequirements` and
  `max_duration_secs: u64` to `PendingJobRecord` AND to the borsh-serialized / state-root-folded DTO
  (a versioned schema bump, since the layout is a STABLE on-disk schema per the struct's own warning),
  populate them in the `SubmitJobV2` apply arm, and surface them through `OpenJob`. Requires a
  genesis/migration boundary because it changes the persisted encoding.
- **Effort:** Medium. Coordinate with a genesis reset / schema-version boundary (aligns with the
  protected-enforcement batch at the alpha genesis reset in the production plan).

#### 3.4 "Protocol-enforced" 51/49 split is producer-side SOFT scheduling, not a consensus rule
- **WP says (WHITEPAPER.md:35, 328):** Core Principle #1 — "51% of all network compute is reserved …
  **Protocol-enforced**"; flagship jobs "get priority access to 51% of capacity. All other jobs share
  the remaining 49%."
- **Code does:** `capacity::admit` is invoked ONLY during block assembly by the producer
  (`src/node/src/event_loop.rs:3217`) as SOFT scheduling. The consensus apply path does NOT enforce it:
  `SubmitJobV2` apply (`state.rs:1531`) and `apply_claim_job` (`state.rs:1843`) contain no
  flagship/capacity/51-49 check. Self-described as "PRODUCER-SIDE ONLY … NEVER apply-enforced, NEVER
  persisted" (`state.rs:212-215`, `capacity.rs:1-7`).
- **Divergence — the enshrined "Protocol-enforced" word is not satisfied.** A malicious producer can
  pack a block 100% with its own non-flagship jobs, starving flagship/communal products, and every peer
  accepts the block (apply does no split validation, no consensus penalty). The split is bypassable by
  any single producer. Core Principle #1 is a §3 enshrined never-change rule and is NOT roadmap-gated.
- **Fix — REAL FEATURE (fork-capable consensus rule):** apply must reject a block whose admitted
  compute-job set violates the deterministic `admit()` decision for that height — threading per-block
  churn/capacity params + the full pending-set into apply so validators recompute the canonical
  admission and reject deviating blocks (exactly why v1 kept it soft). **Interim honest step:** document
  the divergence and gate the "Protocol-enforced" claim behind 📋 until closed.
- **Effort:** Large (new consensus rule + fork semantics). Must be closed before the split is advertised
  as *enforced*; §9 being 📋 makes soft-scheduling defensible *for now* but Principle #1's wording is not.

#### 3.5 Dynamic reserve pinned at the 5% floor — churn input hardcoded to zero
- **WP says (WHITEPAPER.md:41):** Core Principle #4 — `Reserve% = 5% + 10%×churn_rate`, min 5% / max
  15%; "The network automatically holds more when things are volatile."
- **Code does:** the formula is correct (`capacity.rs:70 dynamic_reserve_bps`; tests 1%→5.1%, 50%→10%,
  100%→15% at :196-203) but the churn INPUT is hardcoded zero at every live call site:
  `validator_churn_bps(0,0,0)` at `event_loop.rs:3200`, and `let churn = 0.0; // TODO` at the RPC
  display path `event_loop.rs:2942`. `validator_churn_bps` (`capacity.rs:147`) is implemented but never
  fed a real validator-set delta.
- **Divergence:** the "dynamic" half of the enshrined principle is dead. During a 100% churn event the
  reserve stays 5% instead of 15%, so the buffer meant to protect active products/user sessions during
  validator loss does not exist. Formula present but structurally inert.
- **Fix — REAL FEATURE (medium):** compute the per-epoch validator-set delta (joined/left/prev_count —
  the node already tracks the validator set for consensus) and feed `validator_churn_bps(prev,joined,left)`
  into the `admit()` call (`event_loop.rs:3200`) and the RPC breakdown (`event_loop.rs:2942`). All the
  math exists + is tested; only the epoch-delta accounting + wiring is missing.
- **Effort:** Medium; touches the protected `event_loop.rs` (founder-gated).

#### 3.6 Dynamic load-based pricing unbuilt — only a STATIC floor is enforced
- **WP says (WHITEPAPER.md:334, 271):** "Pricing: Dynamic, based on network load. When the network has
  surplus capacity, jobs are cheap. When capacity is scarce, prices rise steeply … Enforced when compute
  jobs go live."
- **Code does:** the live `SubmitJobV2` apply gate enforces exactly one budget check —
  `comme_budget.raw() < MIN_JOB_BUDGET` where `MIN_JOB_BUDGET=1_000_000` raw (0.01 COMME)
  (`src/storage/src/state.rs:1539-1544`; `src/core/src/compute.rs:84`) — then a balance check +
  `escrow_into_job`. No load signal (`active_jobs`, capacity, pending/lifecycle counts,
  `CapacityParams.total_slots`) is read. The submitter names an arbitrary budget above a static floor.
- **Divergence:** a job at 95% load costs the same floor as one at 5% load — the opposite of the
  specified model. (Defensible under 📋 + the whitepaper's own "Enforced when compute jobs go live," but
  it must not be claimed as built.)
- **Fix — REAL FEATURE:** snapshot a deterministic load state during apply (active = job_lifecycles +
  pending count; capacity = `CapacityParams.total_slots`) and reject `comme_budget < dynamic_min` via a
  deterministic INTEGER fixed-point curve. `consensus/src/job_pricing.rs::compute_load_multiplier`
  (:82) is the right shape and already integer/fixed-point — adapt it, make it fully deterministic +
  genesis-parameterized, and do NOT use the f64 `burst_pricing` path. Needs a consensus-params addition
  (`base_rate`) + a consensus-safe load snapshot; coordinate with the money-path flip.
- **Effort:** Medium-Large (consensus-params + apply-gate change on the money path).

#### 3.7 Near-capacity backpressure is silent DEFERRAL, not the specified prohibitive price
- **WP says (WHITEPAPER.md:334, 271):** "Near full capacity, the price becomes prohibitive — the
  protocol's way of saying 'the network needs more validators, not more jobs.'"
- **Code does:** overload backpressure is QUANTITY-based. `capacity::admit` rations slots and pushes
  overflow into `Admission.deferred` (`capacity.rs:58-59`) to a later block, ordered by
  `priority = tx.fee` (the ordinary tx fee, `state.rs:3025`; `capacity.rs:171-172`) — not the job budget
  or a load-derived price. No surge/prohibitive price is applied. The surge curve that would implement
  this — `compute_surge_multiplier`, doubling per 5% above 90% utilization
  (`consensus/src/job_pricing.rs:93-109`) — is dead code.
- **Divergence:** near full capacity the protocol DEFERS jobs silently rather than pricing them out; the
  specified economic signal ("stop buying, start recruiting validators") is absent. Deferral is related
  but distinct and does not satisfy the pricing spec.
- **Fix — REAL FEATURE:** fold the surge multiplier into the dynamic-minimum admission gate from 3.6 so
  near-capacity submissions must meet a steeply-rising (eventually prohibitive) minimum budget or be
  rejected at apply — instead of, or in addition to, being queued. Keep it integer/deterministic.
- **Effort:** Medium (rides on 3.6). Both 📋-defensible today; neither may be claimed as built.

#### 3.8 Automatic capacity-milestone burns are not wired
- **WP says (WHITEPAPER.md:69, 269):** §69 marks "Milestone burns trigger automatically when the network
  hits capacity thresholds" as ✅ (live). §269: "Capacity milestones (total compute, storage, RAM
  thresholds) trigger automatic on-chain burns."
- **Code does:** `check_milestone(current_validators, config)` (`core/src/milestones.rs:15`) exists and
  `TxKind::MilestoneBurn` IS applied as a real burn (`storage/state.rs:1308-1331`: debits sender, bumps
  `total_burned`, requires non-zero signed sender). BUT `check_milestone()` and
  `MilestoneConfig::default_config()` have ZERO production callers (referenced only in unit tests). No
  per-block/epoch hook fires a `MilestoneBurn` on threshold crossing — it only exists as a
  manually/protocol-submitted tx. Worse, `check_milestone` models only VALIDATOR-COUNT thresholds
  (`milestones.rs:11`) — which the whitepaper classifies as *campaign-announced, NOT automatic* — while
  the CAPACITY thresholds (compute/storage/RAM) it says fire automatically are not implemented at all.
- **Divergence:** the ✅-claimed "automatic on-chain capacity milestone burns" are not wired; the burn
  primitive exists but nothing triggers it, and the only threshold logic that exists covers the wrong
  (adoption) category. One of the three burn mechanisms is present as a payload type but inert as an
  *automatic mechanism* — and it is tagged ✅, not 📋, so this is a *contradiction of a live claim*.
- **Fix — SMALL-TO-MEDIUM (wire-up + small feature):** add an epoch/block-boundary hook (event_loop
  block-processing or a ChainState post-apply step) that (a) tracks cumulative network compute/storage/
  RAM capacity, (b) calls an extended `check_milestone` against CAPACITY thresholds with once-only
  dedup (persist a fired-milestone set), and (c) injects a protocol `MilestoneBurn` from the zero
  address that apply already burns. The burn apply + TxKind already exist (wire-up); the
  capacity-threshold accounting + dedup is a small real feature.
- **Effort:** Small-to-Medium. Because §69 tags this ✅, either wire it or correct the tag.

#### 3.9 Job-content confidentiality is not merely unbuilt — the chosen verification model forecloses it
- **WP says (WHITEPAPER.md:330):** "The validator cannot see the job contents."
- **Code does:** job contents are handled entirely IN PLAINTEXT. The submitter publishes the
  `program‖input` envelope to the DA layer with no encryption — `encode_job_blob` =
  `[program_len:u32][program][input]` (`da_publisher.rs:125-126`), Reed-Solomon-coded + Merkle-committed
  for AVAILABILITY/INTEGRITY only. The executor fetches + splits it in the clear and re-executes
  (`executor_loop.rs:284-292`), and EVERY sampled verifier reconstructs the identical plaintext blob and
  re-executes it (`da_attestation.rs:132-150`; `verifier_loop.rs:204-212`). A repo-wide grep for
  encrypt/decrypt/cipher/AES/ChaCha/sealed/TEE/SGX/enclave/FHE across pouw/da/pouw-onchain/node returns
  nothing — no confidentiality primitive exists.
- **Divergence — CONTRADICTS + ARCHITECTURALLY INCOMPATIBLE.** Confidentiality is not implemented, and
  it is incompatible with the verification model specified in the SAME section: §332 samples the
  committee to "independently re-execute the work." Re-execution mathematically REQUIRES every verifier
  to read program+input in cleartext, so "the validator cannot see the job contents" can never hold
  under re-execution verification. This is not a wire-up — it needs a different primitive (ZK
  proof-of-execution, TEE remote attestation, or FHE), none of which is built or designed. §322 being
  📋 makes the *build* gap roadmap-acceptable; the problem is that launch/whitepaper materials assert a
  property the chosen architecture forecloses.
- **Fix — FOUNDER DECISION (whitepaper is PROTECTED):** either (a) drop/soften the "validator cannot
  see the job contents" sentence so the spec matches the re-execution design that is actually built and
  proven, OR (b) commit to a confidentiality-preserving verification subsystem (ZK-proof-of-correct-
  execution or TEE attestation *replacing* re-execution) as a Phase-3 line item. Until then, launch
  materials must not assert job-content confidentiality.
- **Effort:** N/A for code (either a one-sentence whitepaper edit by the founder, or a Phase-3 research
  subsystem). This is a §5 must-not-over-claim item.

### ▲ MINOR

#### 3.10 §332 "share of the job's budget" on dispute — wording, not a defect
- WP §332 says correct verifiers "earn a share of the job's budget." On the DISPUTE terminal the full
  budget is refunded to the submitter and honest verifiers are instead paid `dispute_bounty_bps=2_000`
  (20%) of the slashed EXECUTOR BOND (`settlement.rs:102-128`; `resolve_disputed`
  `settlement_resolution.rs:181-201`). Paying dispute verifiers *from the budget* while also refunding
  it in full would double-spend and break conservation, so the bond-funded reward is the economically
  sound reading; the "share of budget" promise is honored in the Confirmed case at 10%
  (`settlement.rs:47-69`). **Fix:** wording/interpretation only — treat §332 as the general Confirmed
  reward (already honored), or carve the dispute reward out of the refunded budget instead of
  full-refunding. No safety/economic defect.

#### 3.11 A correct resource matcher exists but is DEAD CODE (V1-only)
- `ValidatorCapacity::can_handle` + `assign_jobs` (the 51/49 split with capacity fitting) exist at
  `src/consensus/src/job_assignment.rs:23-48` but have ZERO non-test callers and operate on the LEGACY
  V1 `PoolJob` push model, not the live V2 escrow pull model. The routing logic is
  "partial/built-but-orphaned," which LOWERS the fix cost for 3.2. **Fix:** lift only the `can_handle`
  predicate into `plan_executor_actions` (once 3.3 lands); do not resurrect the V1 push model.

#### 3.12 `max_duration` + GPU/storage/RAM/bandwidth have zero effect on execution
- `max_duration_secs` is dropped with `resources` (3.3) and never reaches execution. The kernel
  (`src/node/src/pouw_executor.rs:40-53`) is deterministic CPU-only WASM bounded by fuel+memory, "No
  wall-clock" (`pouw_executor.rs:7`); there is no GPU path and no duration enforcement. A job declaring
  GPU/storage needs is "executed" by a CPU run that ignores them and still confirms. **Fix:** roadmap —
  when multi-profile execution lands, enforce `max_duration` + per-channel limits at the sandbox
  boundary. For alpha honesty, either reject `SubmitJobV2` with non-CPU resource requirements at
  admission, or document that only the CPU-WASM profile is executable today.

#### 3.13 Task decomposition / result reassembly is entirely unbuilt (§320)
- WP §320: "Decomposes large tasks into desktop-sized pieces and reassembles results." No such logic
  exists (grep for decompos/split-task/chunk-job/reassemble/subtask in the job path returns nothing;
  the only "split" is the DA-blob codec `split_job_blob`, `executor_planner.rs:240-265`, which is data
  availability, not task decomposition). A job is an atomic unit claimed + executed whole. **Fix:**
  genuine large Phase-3+ feature, legitimately deferred per 📋. No corrective action now — just don't
  claim it as shipped.

#### 3.14 The live split is over 100 abstract genesis-fixed "slots," not real network compute
- WP §35: "51% of all network compute." `admit()` splits `cp_total_slots()=100`
  (`src/core/src/genesis.rs:199`; `capacity.rs:34`) — a fixed genesis constant; a "slot" is one
  admittable job-tx per block, not derived from measured CPU/GPU/RAM/storage/bandwidth, and a 1-core job
  and a 64-GPU job each cost one slot. **Fix:** roadmap — derive `total_slots` (or a resource-vector
  budget) from the live aggregate proof-scored capacity and size each job's slot cost by its declared
  footprint (fields already exist on `PoolJob`). Acceptable v1 proxy given 📋; record the divergence.

#### 3.15 Three parallel 51/49 implementations — two dead, one live (audit hazard)
- LIVE: `capacity.rs::admit` (wired at `event_loop.rs:3217`). DEAD #1:
  `consensus/src/job_assignment.rs::assign_jobs` + `CapacityTracker` (zero non-test callers). DEAD #2:
  `storage/src/job_pool.rs::route_jobs_with_capacity` (zero callers; only its sibling
  `capacity_breakdown` is wired, to RPC display at `event_loop.rs:2944`). Plus a second reserve formula
  `token.rs:46 dynamic_reserve_percent` (float) distinct from the live bps version. **Fix:** cleanup —
  delete/deprecate the two dead engines and consolidate the reserve formula on `capacity.rs` (or make
  `token.rs::dynamic_reserve_percent` delegate) so there is one source of truth. Correctness-drift /
  audit hazard, not a live-path break.

#### 3.16 Emergency 51%→data-protection redeploy absent (§336)
- WP §35/§336: on sudden capacity loss the 51% is redeployed to protect user data. Not implemented —
  `capacity.rs:12-13` explicitly scopes it out; no loss detector, no redeploy, and no communal storage
  product to preserve to. **Fix:** roadmap, correctly deferred (nothing to protect until §8/§9 storage
  products exist). Track it so the enshrined safeguard isn't forgotten when storage lands.

#### 3.17 49% "equal division / no whale advantages" not enforced in the job path (§37)
- WP §37: the 49% is "split equally among qualifying holders per tier. Pure equal division. No whale
  advantages." Within the 49% class `admit()` orders jobs by `priority = fee` highest-first
  (`capacity.rs:103-104,171`); a higher-fee submitter is admitted ahead of others. No per-holder/per-tier
  equal-division or anti-whale guard exists in the admission path. **Fix:** roadmap — implement per-tier
  equal-division for the 49% pool (the tier system `src/core/src/tier.rs` exists but is not consulted at
  admission), OR document that burst-compute job admission is fee-priced-by-design and Principle #2's
  equal division governs standing tier allocation, not burst job ordering. Currently neither is wired.

#### 3.18 Burst-compute pricing (gold-standard peg) not enforced against `burn_amount` (🔧, deferred)
- WP §271 (tagged 🔧): burst-compute burn price "tied to the gold standard benchmark scores … Enforced
  when compute jobs go live." The BURN half is wired (`TxKind::BurstCompute{burn_amount}` debits +
  `total_burned`, `storage/state.rs:1295-1306`); the PRICING half is not — `BurstPriceCalculator` and
  `BURST_COMPUTE_ANNUAL_COST` (33 COMME/ref-node-year, `token.rs:80`) have no production callers, so
  apply accepts any affordable `burn_amount`. **Fix:** when compute jobs go live, compute the required
  price and reject under-priced burns. Matches the whitepaper's own 🔧 "Enforced when compute jobs go
  live" — roadmap-deferred, not built-wrong.

#### 3.19 No-fault teardown terminals burn zero of the budget (within §326 carve-outs)
- WP §326: "every job still permanently removes a slice of supply." Confirmed burns 5% and Cancel burns
  2% (`settlement_resolution.rs:57-60`), but the no-fault/no-verdict terminals burn ZERO:
  `resolve_unavailable` refunds 100% + returns bond (`settlement_resolution.rs:102-109`);
  `resolve_escalation_fallback` (NoQuorum→Escalate, the D2 zero-comp inert stand-in) returns
  budget+all bonds with `burned:0` (:135-145). The whitepaper's own sentence carves out the
  incorrect-result refund path, and these are no-fault/no-verdict cases, so this is within the spec's
  carve-outs, not a contradiction of confirmed-job economics. **Fix:** no correctness change required;
  if the founder wants the literal "every job burns" invariant even for no-fault teardowns, add a small
  flat teardown-withholding burn (G8's deferred withholding penalty is a ~one-line re-add). Roadmap/
  cosmetic — and the escalation stand-in is replaced by the real 2nd panel per the EscalationRound plan
  (`674593b`).

---

## §4 — COMPLETION PLAN (ordered, phased)

Legend for each item: **[FIX-NOW]** = built-wrong / contradicts a live (✅) claim, close before
advertising · **[SCHEMA-FLIP]** = needs the alpha genesis reset / on-disk schema boundary ·
**[ROADMAP-📋]** = legitimately deferred, must NOT be over-claimed (§5) · **[FOUNDER]** = needs a
founder decision.

### Phase 0 — the one built-wrong economic fix (do first, standalone on `agent-*`)
1. **[FIX-NOW] Rubber-stamp forfeiture (§3.1).** Re-partition the Disputed + Confirmed pots in the
   non-frozen `pouw-onchain` layer: burn/redirect wrong-side revealer bonds, re-pin the B10 +
   conservation tests. No protected file, no frozen-crate edit, no schema change. **[FOUNDER]** decide
   burn vs bounty for forfeited bonds. This makes the verification game honestly anti-collusion before
   anything is advertised. Buildable + testable now on the agent branch; inert until the money path is
   already live (which it is), so it is a pure economics correction.

### Phase 1 — honest-alpha guardrails (small, no schema break)
2. **[FIX-NOW or tag-fix] Milestone burns (§3.8).** Either wire the epoch capacity-milestone hook +
   dedup + zero-address `MilestoneBurn` injection, OR (founder) correct the §69 ✅ tag to 🔧/📋. As-is a
   ✅ claim is inaccurate.
3. **[ROADMAP-📋 honesty] Alpha admission guard (§3.12).** Reject `SubmitJobV2` whose declared profile
   is non-CPU (GPU/storage/RAM/bandwidth) at admission, OR document that only the CPU-WASM profile is
   executable today — so a job can't declare needs the network silently ignores.
4. **[cleanup] Dead 51/49 engines (§3.15).** Delete/deprecate `job_assignment.rs::assign_jobs` +
   `job_pool.rs::route_jobs_with_capacity`; consolidate the reserve formula on `capacity.rs`. Removes an
   audit hazard; no behavior change.

### Phase 2 — resource routing (rides the alpha genesis / schema-version boundary)
5. **[SCHEMA-FLIP] Persist `resources` + `max_duration` (§3.3).** Versioned DTO + `PendingJobRecord`
   bump, populate at `SubmitJobV2` apply, surface through `OpenJob`. Land at the same genesis reset as
   the protected-enforcement batch (Track-2 phase 2–4 application plan,
   `2026-07-09-track2-phase234-application-plan.md`).
6. **[wire-up] `can_handle` claim gate (§3.2/§3.11).** Lift the existing
   `ValidatorCapacity::can_handle` predicate into `plan_executor_actions` before the affordability check;
   add a `ValidatorCapacity` self-descriptor from the node's proof scores. Depends on item 5.

### Phase 3 — dynamic pricing + true enforcement (consensus-params + apply-gate)
7. **[SCHEMA-FLIP] Dynamic load-based minimum (§3.6).** Deterministic integer curve over a consensus-safe
   load snapshot + a `base_rate` consensus param; reject `budget < dynamic_min`. Adapt
   `job_pricing.rs::compute_load_multiplier` (integer only; never the f64 `burst_pricing` path). Coordinate
   with the money-path flip.
8. **[SCHEMA-FLIP] Surge / prohibitive near-capacity price (§3.7).** Fold `compute_surge_multiplier` into
   item 7's gate so near-capacity submissions must meet a steeply-rising minimum or be rejected at apply.
9. **[FOUNDER, consensus rule] Apply-enforced 51/49 (§3.4).** Thread per-block churn/capacity params +
   the full pending-set into apply so validators recompute the canonical `admit()` and reject deviating
   blocks. Fork-capable — founder must approve making the split a hard consensus rule. Until then the
   "Protocol-enforced" wording is gated behind 📋.
10. **[wire-up, protected] Real churn into the reserve (§3.5).** Feed the per-epoch validator-set delta
    into `validator_churn_bps` at `event_loop.rs:3200`/`:2942`. Touches protected `event_loop.rs`
    (founder-gated); the math already exists.

### Phase 4 — legitimately deferred, TRACK + DON'T OVER-CLAIM
11. **[ROADMAP-📋] Task decomposition / reassembly (§3.13)** — genuine Phase-3+ feature.
12. **[ROADMAP-📋] Resource-unit capacity accounting (§3.14)** — replace the 100-slot proxy with
    proof-scored aggregate capacity + per-job footprint cost.
13. **[ROADMAP-📋] Emergency 51%→data-protection redeploy (§3.16)** — blocked until communal storage
    products exist; track so the enshrined safeguard isn't forgotten.
14. **[ROADMAP-📋/FOUNDER] 49% equal-division vs fee-priced burst (§3.17)** — implement per-tier equal
    division OR document burst admission as fee-priced-by-design.
15. **[ROADMAP-🔧] Burst-compute gold-standard pricing (§3.18)** — enforce when compute jobs go live.
16. **[FOUNDER, whitepaper] Job-content confidentiality (§3.9)** — the single decision with no code path
    under re-execution: either soften the WHITEPAPER.md:330 sentence to match the built + proven
    re-execution design, OR commit a Phase-3 ZK/TEE confidentiality-preserving verification subsystem.
    The whitepaper is PROTECTED; this is a founder edit or a research line item, not an agent change.

### Cross-references (tie-in)
- **EscalationRound on-chain plan** (`src/staging/docs/2026-07-09-escalation-round-onchain-plan.md`,
  commit `674593b`) — replaces the D2 zero-comp `resolve_escalation_fallback` stand-in (§3.19) with the
  real 2nd panel that *does* slash rejected verifiers via the frozen
  `settle_noquorum_confirmed`/`settle_noquorum_disputed`. **Sequencing note:** the Phase-0 rubber-stamp
  fix (§3.1) and the EscalationRound wire-up touch the same wrong-side-forfeiture machinery — land the
  primary-round forfeiture (§3.1) first so the primary Disputed terminal matches the escalation
  terminal's slash-wrong-side semantics.
- **Track-2 PoUW actors** (`2026-07-09-track2-pouw-actors-plan.md`) + **phase 2-4 application plan**
  (`2026-07-09-track2-phase234-application-plan.md`) — the executor/verifier/DA loops that make pots
  PAY OUT (Confirmed 85/10/5) instead of REFUND. Items 5-6 (persist resources + `can_handle`) extend the
  executor loop those plans wire in.
- **Production plan / THE MAP v2** (`2026-07-05-production-plan.md`) — schema-flip items 5,7,8 align with
  the protected-enforcement batch at the alpha genesis reset; D2 (NoQuorum zero-comp) and D9
  (compile-anchored params) are the relevant locked founder decisions.
- **Readiness assessment / LIVE-ENABLEMENT GATE** (`2026-06-23-pouw-readiness-assessment.md`) — the B1-B10
  atomicity constraint governs any apply-path/schema change in Phases 2-3.

---

## §5 — DO NOT OVER-CLAIM

Public / launch / whitepaper-marketing materials MUST NOT assert the following as done. Each is either
built-wrong (fix first) or legitimately unbuilt-and-📋. Keeping these honest is what lets the rest of the
whitepaper's strong claims stand.

1. **"A verifier who rubber-stamps a wrong result forfeits its own bond" (WHITEPAPER.md:332).** NOT true
   in the live primary round — the wrong-side bond is returned at zero cost (§3.1). Do not describe the
   verification game as anti-collusion-secure until Phase 0 lands.
2. **"The network matches the job to validators with the right resource profile … A GPU-intensive job
   routes to validators with GPUs" (WHITEPAPER.md:328).** No resource matching exists in the live claim
   path and the profile is dropped from state (§3.2/§3.3). Do not claim resource routing.
3. **"Pricing: Dynamic, based on network load … prices rise steeply … the price becomes prohibitive"
   (WHITEPAPER.md:334; §271).** Only a static floor is enforced; overload is silent deferral (§3.6/§3.7).
   Do not claim dynamic/surge pricing (the whitepaper's own "Enforced when compute jobs go live" is the
   honest framing).
4. **"51% … Protocol-enforced" (WHITEPAPER.md:35).** The split is producer-side SOFT scheduling,
   bypassable by any single producer; it is not a consensus rule (§3.4). Do not call it enforced until
   Phase 3 item 9.
5. **"A dynamic reserve … holds more when things are volatile" (WHITEPAPER.md:41).** The reserve is pinned
   at the 5% floor (churn hardcoded 0); the dynamic behavior does not occur (§3.5).
6. **"The validator cannot see the job contents" (WHITEPAPER.md:330).** Job contents are plaintext to the
   executor and every verifier, and re-execution verification forecloses confidentiality entirely (§3.9).
   Do not assert job-content confidentiality under the current architecture — this needs a founder
   whitepaper edit or a Phase-3 ZK/TEE subsystem.
7. **"Milestone burns trigger automatically when the network hits capacity thresholds" — tagged ✅
   (WHITEPAPER.md:69).** No automatic trigger is wired; only validator-count (campaign) logic exists,
   which is the wrong category (§3.8). Either wire it or correct the ✅ tag.
8. **"Decomposes large tasks into desktop-sized pieces and reassembles results" (WHITEPAPER.md:320).**
   Entirely unbuilt (§3.13) — legitimately 📋, but not shipped.
9. **"Resource limits are enforced by the protocol" — the GPU/storage/RAM/bandwidth + max-duration
   dimensions (WHITEPAPER.md:330/326).** Only fuel + memory are enforced; the other channels and
   `max_duration` have zero effect (§3.12). The CPU-WASM fuel/memory claim is honest; the multi-channel
   claim is not yet.

**What IS honest to claim today:** a live, conserved PoUW money path with stake-weighted committee
sampling, strict commit-before-reveal, quorum-triggered dispute, executor-bond slash + submitter refund,
an escrow-not-destroy 85/10/5 executor/verifier/burn split, a 100%-fee-burn + fixed-2B-cap +
Bitcoin-halving emission, and a by-construction sandbox the job cannot use to touch the validator's
system — with the Compute-Jobs routing/pricing/confidentiality layer honestly marked 📋 Planned.
