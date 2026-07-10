# On-chain EscalationRound — build plan (replace the zero-comp refund STAND-IN with a real 2nd panel)

Author: agent (read-only survey) · Branch `agent-testnet-20260707` @ `6b7c39e` · 2026-07-09
Status: PLAN ONLY — no code edited. Scope is NON-PROTECTED throughout (storage `state.rs`/`rocks.rs`,
`pouw-onchain` `lifecycle.rs`/`settlement_resolution.rs`/`escalation_round.rs`, node `verifier_loop.rs`).
The FROZEN `src/staging/pouw/` crate is USED, never modified. No protected node file
(`main.rs`/`event_loop.rs`/`config.rs`), no `genesis.json`, no whitepaper.

> **HEADLINE FINDING.** The escalation *game logic is already built and tested* in
> `src/staging/pouw-onchain/src/escalation_round.rs` (637 lines, 8 passing unit tests incl. a full
> primary→escalate→panel e2e at `escalation_round.rs:575`). It is a complete multi-block
> `EscalationRound` state machine that draws the `k_escalate` panel, runs commit/reveal, forfeits
> non-revealers, and settles via the FROZEN `settle_noquorum_confirmed`/`settle_noquorum_disputed`.
> **It is NOT referenced anywhere in `src/storage/` or `src/node/`** (verified by grep). So this is
> ~80% a WIRE-UP of an audited, conserving module — not a rebuild. The three genuine deltas are:
> (1) generalise its ledger parameter from the concrete `EscrowLedger` to `&mut impl Ledger`;
> (2) integrate it into `ChainState`/`state.rs` (a second map + open-on-Escalate + tail draw +
> Commit/Reveal routing + advance/settle/drain); (3) add a persistence DTO + RocksDB CF (the only
> substantial NEW code). Plus one follow-on: teach `verifier_loop::build_verifier_views` to surface
> escalation-panel membership, or the panel never acts on a live node.

---

## 1. THE GAP (current NoQuorum path vs target)

**Current (inert stand-in).** A sampled committee that splits yields `Verdict::NoQuorum`
(`verdict.rs:34`, reached when the largest equivalence class `< quorum`). The primary lifecycle
`settle` maps NoQuorum to `Terminal::Escalate(EscalationHandoff{…})` and HOLDS the pot
(`lifecycle.rs:662-671`; held escrow = `budget + Be + revealers·Bv`). On-chain,
`settle_due_jobs` (`state.rs:937`) drives `lifecycle_settle_and_drain` (`state.rs:3647`), which on
`Terminal::Escalate` calls **`resolve_escalation_fallback` (`settlement_resolution.rs:130`)** —
the D2-FINAL zero-comp terminal: refund the full budget, return `Be` intact, return every revealer
bond, **burn nothing, pay no one, run no second panel** (`settlement_resolution.rs:135-145`). Then
the lifecycle is drained (`state.rs:3676`). The e2e negative-control
`pouw_noquorum_refunds_submitter_pays_no_worker_comp` (`pouw_payout_e2e.rs:457`) pins exactly this:
no verifier acts → NoQuorum → Escalate → submitter fully refunded, executor bond back, zero comp,
zero burn. Griefing (forcing NoQuorum) is profit-NEUTRAL, never punished.

**Target.** On NoQuorum, instead of `resolve_escalation_fallback`, OPEN a second verification round:
draw a larger `k_escalate` panel from a fresh consensus seed, run its own commit/reveal across blocks,
then settle via the frozen escalation money-math — the panel either vindicates the executor
(`Confirmed` → pay 85/10/5, vindicated original verifiers paid, wrong-side original verifiers slashed)
or rejects it (`Disputed` → refund submitter, slash executor bond, honest original verifiers +
panel rewarded), or — ONLY if the larger panel ALSO fails to reach quorum — falls back to a bounded
final refund. This is precisely what `escalation_round.rs` already implements; the gap is purely that
nothing on-chain instantiates it.

## 2. THE FROZEN API + the already-built adapter

### 2a. Frozen reference: `escalation::resolve` (`pouw/src/escalation.rs:69`)
```
pub fn resolve(l: &mut dyn ChainHooks, p: &GameParams, esc: &Escalation, trigger: Trigger,
               eq: &dyn EquivalenceOracle, stake_of: &dyn Fn(&ParticipantId)->u64)
    -> (Verdict, SettlementOutcome)
```
- `Trigger::NoQuorum { submitter, committee_reveals: &[Reveal], committee_bonds: &[u64] }`
  (`escalation.rs:44`) — the original committee's reveals + held bonds, to be partitioned by the
  panel verdict.
- `Escalation { seed, candidates: &[ParticipantId], budget, executor, executor_hash, executor_bond,
  panel_reveals: &[Reveal], panel_bond }` (`escalation.rs:55-64`).
- Internally: `panel = select_committee(seed, candidates, executor, p.k_escalate, stake_of)`
  (`escalation.rs:77`); `panel_bonds = vec![panel_bond; panel.len()]` (`:84`);
  `verdict = compute_verdict(panel_reveals, executor_hash, p.quorum(panel.len()), eq)` (`:86`); then
  dispatches to `settle_noquorum_confirmed` / `settle_noquorum_disputed` (`:126-182`), partitioning
  the original committee via `partition_committee` (`:191`). Second-NoQuorum is a bounded terminal:
  `settle_noquorum_disputed` with an EMPTY honest set and the WHOLE original committee slashed
  (`escalation.rs:165-182`, cited "bounded escalation depth — spec §11").

**Why we do NOT call `escalation::resolve` directly on-chain (critical).** It assumes a *fully
participating* panel: it re-derives the panel from the seed and returns `panel_bond` to **every**
selected member (`return_bonds` over the full `panel`, `settlement.rs:274-279/332-334`). On a live
chain the panel is DA-gated — abstainers never commit, so they never escrow a bond ("effective
committee = who committed", `lifecycle.rs:11-13`). Paying an un-escrowed panelist would over-draw the
pot and break conservation. The already-built `escalation_round.rs:6-9` documents exactly this and
therefore calls the LOWER-LEVEL frozen `settle_noquorum_confirmed`/`settle_noquorum_disputed`
(`settlement.rs:247`/`:305`) with the ACTUAL effective panel + its real bonds — the same layering the
primary resolvers already use (`resolve_confirmed`/`resolve_disputed` wrap `settle_confirmed_sampled`/
`settle_committee_disputed`, `settlement_resolution.rs:154`/`:181`). So `escalation::resolve` is the
**golden reference/oracle** (drive it with a full panel in tests to pin the on-chain path); the
on-chain path uses the frozen `settle_noquorum_*` primitives via `EscalationRound`.

### 2b. Panel-size + params (`pouw/src/params.rs`)
- `k = 3`, `k_escalate = 7` (`params.rs:34`); `validate()` enforces `k_escalate > k` (`params.rs:59`).
- `quorum(committee_size) = ceil(quorum_num/quorum_den · size)` (`params.rs:76`); with 2/3 defaults,
  `quorum(7) = 5`.
- Panel reward economics ALREADY exist: `escalation_reward_bps = 1000` (10% of slashed bonds to the
  panel) and `challenger_reward_bps = 1000` (`params.rs:42-43`) are consumed by `settle_noquorum_*`.
  **The escalation panel is already paid a premium by the frozen math** — no new economics needed.

### 2c. The already-built `EscalationRound` (`pouw-onchain/src/escalation_round.rs`)
A deterministic multi-block state machine, structurally a sibling of `JobLifecycle`:
- `PanelPhase { Committing, Revealing, Settled }` (`:28`); `PanelDeadlines { commit_by, reveal_by }`
  (`:35`) — NO claim/result sub-phase (the executor's result is already fixed from round 1).
- `open(handoff, job_id, candidates, seed, params, deadlines, stake_of)` (`:94`) draws
  `panel = select_committee(seed, candidates, executor, k_escalate, stake_of)` (`:113`) and starts in
  `Committing`.
- `record_commit(&mut EscrowLedger, Commitment, height)` (`:144`) — validates
  phase/window/panel-membership/bond/no-double-commit, escrows the bond.
- `record_reveal(Reveal, height)` (`:167`); `advance(height)` (`:189`) drives Committing→Revealing.
- `settle(&mut EscrowLedger, eq)` (`:198`) — burns commit-no-reveal forfeitures (`:204-221`),
  computes `verdict = compute_verdict(reveals, executor_hash, quorum(k_escalate), eq)` (`:227`), then:
  - `Confirmed` → `settle_noquorum_confirmed` over (vindicated/rejected partition of the ORIGINAL
    committee, effective panel) (`:230-238`);
  - `Disputed` → `settle_noquorum_disputed` (`:239-247`);
  - `NoQuorum` → bounded terminal: `settle_noquorum_disputed` with empty honest set, whole original
    committee slashed (`:248-258`, mirrors `escalation.rs:165-182`).
  Returns `EscalationOutcome::{Confirmed,Disputed,NoQuorum}(SettlementOutcome)` (`:60`). Idempotent
  (caches `settled`, `:199-201`, `:547` test). Conserves on every terminal (every test asserts
  `escrowed_for(&job)==0` and `total_supply()==total0`).

**What `EscalationHandoff` already carries vs what's missing** (`lifecycle.rs:80-89`): carries
`budget, submitter, executor, executor_hash, executor_bond, committee_reveals, committee_bonds,
verifier_bond` — everything `EscalationRound::open` consumes EXCEPT the second-round-owned inputs it
takes as separate args: **`candidates`, `seed`, `params`, `deadlines`, `stake_of`**. The handoff
comment (`lifecycle.rs:76-78`) deliberately omits candidates/seed as "second-round-owned". On-chain,
`candidates` must be sourced from the settling `JobLifecycle` (its claim-time snapshot) — see §3/§5.

## 3. THE LIFECYCLE DESIGN (how the escalation round fits the state machine)

**Chosen shape: a SECOND per-job state machine in a SECOND map** (`escalation_rounds:
HashMap<[u8;32], EscalationRound>` on `ChainState`), NOT new phases bolted onto `JobLifecycle`.
Rationale: (a) the `EscalationRound` type already exists, tested, with its own phase enum/deadlines/
settle; (b) it keeps the primary `JobLifecycle` (and its `JobLifecycleRecord` on-disk schema)
untouched except for one small accessor; (c) at most one escalation round is ever live per job, so a
parallel map keyed by `job_id` is clean; (d) it mirrors the existing `pending_jobs` vs
`job_lifecycles` split (two maps, two CFs, one state-root fold each). The rejected alternative
(re-entering `Committing` with an escalation flag / adding `EscalatingCommitting`/`EscalatingRevealing`
to `Phase`) would force a `JobLifecycleRecord` schema change AND re-implement panel settlement inside
`JobLifecycle::settle`, duplicating `escalation_round.rs`.

**The SECOND committee draw (deterministic, in-apply).** Add
`draw_escalation_panels(block_hash)` to run in the block tail alongside
`draw_committees_for_completed_jobs` (`state.rs:3568`, called from `apply_txs_with_rollback:869`).
For every freshly-opened escalation round whose panel is not yet drawn:
- seed = `hash_parts(&[&block_hash.0, &job_id, b"escalate"])` (frozen `ids::hash_parts`), distinct
  from the primary seed `hash_parts(&[&block_hash.0, &job_id])` (`state.rs:3581`) so the panel draw
  cannot collide with the round-1 committee draw.
- candidates = the settling lifecycle's claim-time `candidates` snapshot (already sorted +
  state-rooted, `state.rs:1900`) MINUS the round-1 `committee` (`life.committee()`, `lifecycle.rs:361`);
  the executor is auto-excluded inside `select_committee` (`committee.rs:13`). This reuses consensus
  state already in the root and sidesteps "eligible at which height". (Add a `pub fn candidates(&self)`
  accessor to `JobLifecycle` — it is currently private; `to_record().candidates` also exposes it.)
- `k_escalate`, `stake_of` (`state.rs:3417`, active bonded only) exactly as the round-1 draw.
- **Timing option (open+draw in the SAME tail):** simplest is to open the round AND draw its panel in
  the same block tail where the primary settled (both are money-free, deterministic, seeded by that
  block's hash). Then Commit/Reveal begin the next block. See §4 for the invariants.

**New commit_by/reveal_by**, anchored at the escalation-open height `H` (the settling block's height):
`commit_by = H + phase_windows.commit_blocks`, `reveal_by = commit_by + phase_windows.reveal_blocks`
(reuse the existing `PhaseWindows`, `consensus_params.rs`; `state.rs:1904-1908` is the round-1 anchor
pattern). No result window — the executor's hash is already known. (Optionally add dedicated
`escalate_commit_blocks`/`escalate_reveal_blocks` to `PhaseWindows` for a longer panel window; founder
knob, not required for a first slice.)

**How many rounds.** EXACTLY ONE escalation panel, matching the frozen bound (`escalation.rs:164`
"bounded escalation depth", `escalation_round.rs:248` NoQuorum is a terminal, not another Escalate).
No recursion. This is a founder-confirmable knob (see §7).

**Terminal routing after the panel round.** `EscalationRound::settle` → `EscalationOutcome`:
`Confirmed`/`Disputed` are real terminals (pot fully drained by the frozen `settle_noquorum_*`);
`NoQuorum` is the bounded final terminal (submitter refunded, executor bond burned, whole round-1
committee slashed, panel keeps reward+bonds — `escalation_round.rs:248-258`). All three drain the pot
to 0, so after settle the escalation round is REMOVED from `escalation_rounds` (at-most-once, mirrors
`lifecycle_settle_and_drain`'s remove at `state.rs:3676`). **The old `resolve_escalation_fallback`
becomes dead on the live path** but is KEPT (still used by B10 tests + as a documented emergency
knob).

## 4. DETERMINISM / CONSENSUS INVARIANTS

This is CONSENSUS-AFFECTING: it changes NoQuorum settlement (different pot movements → different
`SettlementOutcomeRec` → different state root). It rides the **not-yet-executed alpha genesis reset**
(the chain isn't live; per MEMORY, `main`/testnet are local-only, nothing deployed) — no separate
migration. Every transition MUST be a pure function of consensus state (same discipline as the round-1
draw, `state.rs:3559-3567`). Invariants to hold and test:

1. **Panel draw determinism.** Seed = `hash(block_hash‖job_id‖"escalate")` — `block.hash()` is
   node-independent once finalized. Candidates = a SORTED, state-rooted snapshot (claim-time list
   minus round-1 committee — both already in the root). `stake_of` reads only `bonded_stake`. `k =
   k_escalate` (genesis param). NO HashMap iteration order, RNG, or wall-clock. Iterate the
   escalation-open set in SORTED `job_id` order before drawing (mirror `state.rs:3579`,
   `settle_due_jobs:948/954`).
2. **Open-on-Escalate must be deterministic + rollback-safe.** The open happens inside
   `apply_txs_with_rollback`'s envelope (`state.rs:860`), so a rejected block rewinds it. Add
   `escalation_rounds` to `capture_pre_block` (`state.rs:894`) and `rollback_to_pre_block`
   (`state.rs:911`) — otherwise a rolled-back block leaks a half-opened round → fork.
3. **`settle_due_jobs` ordering.** Escalation `advance`+`settle` join the same SORTED-key sweep
   (`state.rs:953-965`); the pinned `ByteEq` oracle (`state.rs:940`) is reused (its fingerprint is
   already consensus-pinned).
4. **State-root fold.** `escalation_rounds` folds into `state_root` exactly like `job_lifecycles`
   (`state.rs:641-650`): sorted by job_id, length-prefixed borsh of the new `EscalationRoundRecord`.
   The record MUST hold only Vec/Option/primitive/array/tuple fields (borsh-canonical) — no
   HashMap/HashSet (same rule as `lifecycle.rs:106-107`).
5. **Params re-injection across restart (the C1 hazard).** `EscalationRound` embeds `GameParams`
   (`escalation_round.rs:80`), which is genesis-anchored and NOT persisted. `from_record` must
   re-inject `game_params` on load (mirror `JobLifecycle::from_record`, `lifecycle.rs:420`), AND
   `set_consensus_params` (`state.rs:3699`) must rebuild every in-memory escalation round through its
   DTO with the installed params (mirror the round-1 rebuild loop `state.rs:3720-3723`) — else a
   restarted node settles with default params while peers use genesis params → HARD FORK.
6. **Two draw sites must never disagree.** Because we settle via `settle_noquorum_*` over the ON-CHAIN
   drawn panel (not `escalation::resolve`'s internal draw), there is exactly ONE authoritative draw
   (the on-chain tail draw). This is a feature — do NOT also call `escalation::resolve` on-chain (it
   would re-draw and could diverge; see §2a).
7. **Idempotency.** `EscalationRound::settle` caches its terminal (`escalation_round.rs:199`); the
   drain removes the round; a re-org replay reproduces identical open/draw/settle heights by
   construction (same block-tail composition).

## 5. BUILD STEPS (all NON-PROTECTED; frozen `pouw/` untouched)

Order is additive: each step compiles and the chain still passes existing tests until the final
routing flip in step 6.

- **S1 — generalise the ledger param** (`pouw-onchain/src/escalation_round.rs`). Change
  `record_commit(&mut EscrowLedger,…)` and `settle(&mut EscrowLedger,…)` to `&mut impl Ledger`
  (`:144`, `:198`). `Ledger: ChainHooks` (`escrow_ledger.rs:136`) so it coerces to the
  `&mut dyn ChainHooks` the frozen `settle_noquorum_*` take — identical to how
  `settlement_resolution.rs:154/181` were generalised. Existing tests keep passing (`EscrowLedger:
  Ledger`). MINOR edit to an existing staging file → note to founder.
- **S2 — persistence DTO** (`pouw-onchain/src/escalation_round.rs`). Add `EscalationRoundRecord`
  (borsh, mirror-of-primitives) + `to_record()`/`from_record(rec, params)` + field-level converters,
  copying the `JobLifecycleRecord` pattern verbatim (`lifecycle.rs:100-281`): mirror
  `Commitment`/`Reveal`/`SettlementOutcome`/`ParticipantId` to `[u8;32]`/primitive rows, omit
  `params` (re-injected on load). Add a `pub fn candidates(&self)` accessor to `JobLifecycle`
  (`lifecycle.rs`) for the draw's candidate source.
- **S3 — RocksDB CF** (`src/storage/src/rocks.rs`). Add `CF_ESCALATION` alongside `CF_LIFECYCLE`
  (`rocks.rs:25`); register it in every CF list (`rocks.rs:79/382/653/675`); add
  `put/batch_put/batch_delete/all_escalation_rounds` (mirror `put_lifecycle:533` /
  `batch_put_lifecycle:571`). Load in `ChainState::open` (`state.rs:388-392`) with `from_record`.
- **S4 — `ChainState` field + plumbing** (`src/storage/src/state.rs`). Add
  `pub escalation_rounds: HashMap<[u8;32], EscalationRound>` next to `job_lifecycles`
  (`state.rs:224`); init in `new()` (`:301`) and `open()`; add to: `capture_pre_block`/`rollback`
  (`:894`/`:911`); `state_root` fold as a 6th Policy-B section (`:665`, after pending_jobs);
  `batch_map_deltas` + `commit_map_mirrors` + a `persisted_escalation_keys` mirror
  (`:2817-2822`/`:2834`); `is_empty`/reset paths (`:2066`, `:2640`); debug/JSON dumps (`:264`, `:561`).
- **S5 — the 2nd draw** (`state.rs`). `fn draw_escalation_panels(&mut self, block_hash)` — SORTED
  job_ids of undrawn open rounds; per job, seed `hash_parts(&[&block_hash.0,&job_id,b"escalate"])`,
  borrow-dance the round out of the map (mirror `draw_committees_for_completed_jobs:3582-3592`), draw
  with `stake_of`. If open+draw happen in one tail (§3), fold this into the open step instead.
- **S6 — settle routing (THE FLIP)** (`state.rs`). In `lifecycle_settle_and_drain`
  (`state.rs:3647`): on `Terminal::Escalate(h)`, INSTEAD of `resolve_escalation_fallback`
  (`:3672`), capture `candidates = life.candidates()` and `committee = life.committee()` from the
  settling lifecycle BEFORE it is drained, filter candidates by removing `committee`, open an
  `EscalationRound` (seed from this block's hash, `PanelDeadlines` anchored at this height, genesis
  `game_params`) and insert it into `escalation_rounds`; the primary lifecycle still drains (its
  Escalate terminal is recorded, pot stays held for the round). Then add escalation `advance`+`settle`
  to `settle_due_jobs` (`state.rs:953-965`): a parallel SORTED sweep over `escalation_rounds`, calling
  a new `escalation_settle_and_drain(job_id)` that pre-validates the pot (mirror the
  `escrowed_for_job == expected` guard `state.rs:3628-3635/3660-3669`), settles via the `ChainLedger`
  view (`state.rs:3636`), and removes the round.
- **S7 — Commit/Reveal routing** (`state.rs`). `apply_commit` (`:1934`) and `apply_reveal` (`:1970`)
  currently target `job_lifecycles` only. When a job_id has an ACTIVE escalation round (and no live
  primary lifecycle), route Commit→`EscalationRound::record_commit`, Reveal→`record_reveal`
  (self-`advance` first, like `apply_reveal:1993`). Keep the zero-address + validator gates
  (`:1943-1955`). The admission-window/soundness pre-checks (`state.rs:3787-3802`,
  `select_applicable_txs`) must recognise escalation-round Commit/Reveal windows too (C3), else valid
  panel txs get dropped as permanently-doomed.
- **S8 (FOLLOW-ON) — verifier-loop panel surfacing** (`src/node/src/verifier_loop.rs`).
  `build_verifier_views` (`verifier_loop.rs:107`) reads ONLY `job_lifecycles` and tests membership via
  `committee.contains(&me.0)` (`:117`). It does NOT see `escalation_rounds`, so on a live node the
  panel would NEVER commit/reveal → EVERY escalation round NoQuorums. Add an escalation arm: surface
  `EscalationRound` panels this node is on (map `PanelPhase`→`VerifierPhase`, reuse the durable
  `SaltStore` path). NON-PROTECTED (verifier_loop.rs is not in the protected list) but an existing
  node file → founder note. Mark as a fast-follow: without it the on-chain machinery is correct but
  the panel is un-driven except in the e2e harness (which hand-drives txs).

## 6. TEST STRATEGY

- **Unit (already present, keep):** `escalation_round.rs:335-635` covers open/commit/reveal/advance/
  settle, Confirmed/Disputed/NoQuorum terminals, forfeiture, too-few-panelists, idempotency, and a
  full primary→escalate→panel e2e — all conserving. After S1 (ledger generalisation) these still run
  on `EscrowLedger`.
- **Unit (new, in `state.rs` tests):** panel-draw determinism (two nodes / two block-timestamp
  perturbations → identical panel + identical state root, mirror the round-1 determinism test at
  `state.rs:8744-8759`); the open-on-Escalate transition; rollback safety (a rejected block leaves
  `escalation_rounds` byte-identical); restart round-trip (`to_record`→`from_record`→identical
  settle) + the `set_consensus_params` rebuild (C1) for escalation rounds.
- **Golden oracle:** a test that drives `escalation::resolve(Trigger::NoQuorum,…)` with a FULL panel
  and asserts its `SettlementOutcome` equals `EscalationRound::settle`'s outcome for the same
  all-participate inputs — pins the on-chain path to the frozen reference (pattern:
  `resolve_confirmed_matches_run_priced_job_end_state`, `settlement_resolution.rs:366`).
- **B10 golden-equivalence (must still hold):** the existing staging-vs-chain harness
  (`state.rs:4652-4790`) proves `EscrowLedger ≡ ChainState` for primary terminals; extend `run_on_both`
  with an escalation leg (drive the second round on BOTH backends, assert field-for-field terminal +
  per-participant balance + conservation). Non-vacuous because the two ledgers are independent.
- **e2e (`src/node/tests/pouw_payout_e2e.rs`):** extend the harness. The current NoQuorum
  negative-control (`:457`) will CHANGE behaviour once S6 lands — update it to assert the escalation
  round OPENS. Add two driven scenarios: (a) round-1 NoQuorum (3-way split via distinct verifier
  hashes) → panel drawn → panel Confirms → executor + panel + vindicated verifiers PAID; (b) round-1
  NoQuorum → panel also NoQuorum (panel split) → bounded final refund + executor-bond burn. This needs
  S8 (or the harness hand-feeds panel Commit/Reveal txs, as it already hand-feeds the Disputed fraud
  path at `:586-598`). Assert money conservation every block (`conserved()`, `:217`).

## 7. RISKS / OPEN QUESTIONS / EFFORT / FOUNDER DECISIONS

**Clean additive first slice (recommended):** S1–S6 (generalise ledger + persistence DTO + CF +
ChainState plumbing + tail draw + settle routing) + unit/B10/golden tests. This makes the on-chain
NoQuorum path run a real 2nd panel and settle correctly, verified by tests that HAND-DRIVE panel txs.
Estimate: ~1–1.5 focused sessions (most logic is copy-of-pattern; `escalation_round.rs` already exists).

**Full feature:** add S7 wiring nuance (mempool admission windows for escalation txs) + S8
(verifier-loop panel surfacing) + the extended e2e that drives real loops. S8 is the difference
between "consensus-correct but panel un-driven" and "a live node actually runs the panel."

**Risks / open questions**
- **R1 (panel liveness).** Without S8 no honest node commits to escalation panels → every live
  escalation NoQuorums to the bounded refund. Correct + conserving, but economically the same as the
  stand-in until S8. Sequence S8 with (or immediately after) S6.
- **R2 (candidate source at settle).** `EscalationHandoff` deliberately omits candidates
  (`lifecycle.rs:76-78`); they must be read from the settling `JobLifecycle` BEFORE it drains. If a
  future change drains the lifecycle earlier, the candidate snapshot is lost. Mitigate by capturing
  candidates+committee into the `EscalationRound` at open (it already stores its drawn `panel`).
- **R3 (partial panel / DA-gate).** Handled by design (use `settle_noquorum_*` over the effective
  panel, NOT `escalation::resolve`) — but this is the single most important correctness point; the
  golden-oracle test (§6) must lock it.
- **R4 (schema/state-root).** Adding a 6th Policy-B fold section + a new CF changes the state root and
  on-disk layout. Acceptable ONLY because nothing is deployed and it rides the alpha genesis reset
  (`lifecycle.rs:162-165` already anticipates this). If anything ships before this lands, it needs a
  versioned migration — so land it PRE-reset.
- **R5 (double-settle across maps).** A job must never be live in both `job_lifecycles` and
  `escalation_rounds` with an undrained pot simultaneously in a way that double-pays. The primary
  lifecycle's Escalate terminal drains its own resolvable slice to 0 EXCEPT the held pot, which the
  escalation round then owns. Assert `escrowed_for_job` invariants at both hand-off and final settle.

**Founder decisions**
- **D1 — `k_escalate` value.** Default 7 (`params.rs:34`). Keep, or set at genesis via
  `set_consensus_params`? (Larger panel = stronger but needs a deeper eligible-validator pool at
  alpha; with few validators, `k_escalate=7` may exceed available candidates → instant bounded
  NoQuorum. **This is the biggest practical decision** — see below.)
- **D2 — max escalation rounds.** Recommend EXACTLY 1 (matches frozen bound). Confirm.
- **D3 — escalation premium.** Already paid by `settle_noquorum_*` (`escalation_reward_bps=1000`). No
  new economics; confirm the 10% split is intended for the on-chain path.
- **D4 — dedicated panel windows.** Reuse `phase_windows.{commit,reveal}_blocks`, or add
  `escalate_*_blocks` to `PhaseWindows`? Reuse is simpler; a dedicated (longer) window gives the
  larger panel more time to DA-fetch + re-execute.

### SINGLE BIGGEST RISK / DECISION
**`k_escalate=7` vs the alpha validator pool (D1 + R1).** The panel is drawn from *eligible bonded
validators minus the round-1 committee minus the executor*; reaching `quorum(7)=5` requires at least
~8–10 independent eligible validators actively running the (not-yet-built, S8) escalation arm of the
verifier loop. If the alpha testnet has too few validators, every escalation round deterministically
bounded-NoQuorums to a refund — indistinguishable in outcome from today's stand-in, just with more
machinery. The founder must either (a) lower `k_escalate` for alpha, and (b) commit to landing S8 so
panels are actually driven — otherwise the real 2nd panel is correct on paper but inert in practice.
