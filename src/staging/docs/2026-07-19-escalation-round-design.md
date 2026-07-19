# EscalationRound on-chain — APPROVED DESIGN (founder-signed 2026-07-19)

Status: DESIGN APPROVED — supersedes nothing; builds on the survey/plan
`2026-07-09-escalation-round-onchain-plan.md` (commit `674593b`), which remains the detailed
code-site reference (§ references below point into it). Branch `agent-testnet-20260707` @
`9f5d76a`. Prerequisite §3.1 wrong-side forfeiture landed in `2b957a0`; `escalation_round.rs`
is untouched since the plan and its unit suite is 11/11.

## Founder decisions (locked this session)

| # | Decision | Choice |
|---|----------|--------|
| F1 | Scope | **Full S1–S8** (consensus core + mempool admission + verifier-loop panel arm + e2e). Panels run on LIVE nodes, consistent with the proven live-payout milestone. |
| F2 | Panel viability gate (NEW, not in the plan) | **Gate at open**: drawn panel ≥ `quorum(k_escalate)` → open the real round; smaller → `resolve_escalation_fallback` (zero-comp refund, today's behavior). Structural shortage refunds harmlessly; behavioral abstention on a viable panel still hits the punitive bounded terminal (anti-griefing deterrent intact). |
| F3 | Panel windows (plan D4) | **Reuse round-1 `phase_windows.{commit,reveal}_blocks`**, anchored at the escalation-open height. No `PhaseWindows` schema change. No result window (executor hash already fixed). |
| F4 | `k_escalate` (plan D1) | Keep code default **7**; the F2 gate makes it safe at alpha scale; genesis may tune it at the reset. |
| F5 | Rounds (plan D2) | **Exactly one** escalation round (frozen bound; panel NoQuorum is a terminal, never a second escalation). |
| F6 | Panel premium (plan D3) | Confirmed: the existing `escalation_reward_bps = 1000` (10% of slashed bonds) inside the frozen `settle_noquorum_*` — no new economics. |

## Architecture (as approved)

1. **Second map, second machine.** `ChainState.escalation_rounds: HashMap<[u8;32], EscalationRound>`
   parallel to `job_lifecycles`. The existing `escalation_round.rs` type is used as-is except S1
   (ledger param generalised `EscrowLedger` → `&mut impl Ledger`). The frozen `escalation::resolve`
   is NEVER called on-chain (it assumes a fully-participating panel and re-draws internally — plan
   §2a); it is the golden test oracle. On-chain settles via the frozen `settle_noquorum_confirmed/
   disputed` over the EFFECTIVE (actually-committed) panel.
2. **Open + draw + gate (one deterministic tail step).** In `lifecycle_settle_and_drain` on
   `Terminal::Escalate`: capture the settling lifecycle's claim-time `candidates` MINUS its round-1
   `committee` (before drain; also store both into the round at open — plan R2); seed =
   `hash_parts(block_hash ‖ job_id ‖ "escalate")`; draw via `select_committee` with the round-1
   `stake_of`; then the F2 gate: `panel.len() >= quorum(k_escalate)` → insert an open
   `EscalationRound` (pot stays held; `PanelDeadlines` = open height + round-1 windows per F3);
   else → `resolve_escalation_fallback` immediately (round never opens). All inside the
   `apply_txs_with_rollback` envelope; `escalation_rounds` joins `capture_pre_block`/`rollback`.
3. **Settle + drain.** `settle_due_jobs` gains a parallel SORTED-key sweep over
   `escalation_rounds`: `advance(height)` → due → pot-preflight (`escrowed_for_job` == the
   handoff-held `budget + Be + round-1 revealers·Bv` PLUS the bonds escrowed by panel commits —
   mirroring the primary's guard) → `settle` via the `ChainLedger` view with the
   pinned `ByteEq` oracle → record outcome → remove the round. All three outcomes drain the pot
   to 0: Confirmed (executor 85/10/5 + panel premium + vindicated round-1 verifiers paid,
   wrong-side slashed), Disputed (submitter refunded, executor bond slashed, honest round-1 +
   panel rewarded), NoQuorum (bounded terminal: submitter refunded, executor bond burned, whole
   round-1 committee slashed, panel keeps reward+bonds). `resolve_escalation_fallback` stays in
   the codebase (F2-gate path + B10 tests + documented emergency knob).
4. **Tx routing.** `apply_commit`/`apply_reveal`: when `job_id` has an ACTIVE escalation round and
   no live primary lifecycle, route to `EscalationRound::record_commit`/`record_reveal`
   (self-`advance` first, like the primary reveal path). Zero-address + validator gates kept.
   `select_applicable_txs` (C3) recognises escalation-round windows so viable panel txs are
   requeued, not dropped.
5. **Persistence + params.** `EscalationRoundRecord` (borsh-canonical: Vec/Option/primitive only,
   no maps) + `to_record()`/`from_record(rec, params)`; `params` NOT persisted — re-injected on
   load AND rebuilt in `set_consensus_params` (C1 hazard, plan §4.5). New `CF_ESCALATION` in
   rocks.rs (mirror `CF_LIFECYCLE` sites). Sixth Policy-B state-root fold section (sorted job_id,
   length-prefixed borsh). `batch_map_deltas`/`commit_map_mirrors` + `persisted_escalation_keys`
   mirror; reset/is_empty/debug paths.
6. **Node actor (S8).** `verifier_loop::build_verifier_views` gains an escalation arm: surface
   panels this node is on (`PanelPhase` → `VerifierPhase` mapping, reuse the durable `SaltStore`
   path). Planner emits panel Commit/Reveal exactly like round-1 commitments.

## Protected surface — exactly two one-line hunks in `event_loop.rs`
Presented for founder approval at apply time (like the 2026-07-18 FetchChunk hunk):
- **P1 (C7 ingress):** the known-job check adds `|| state.escalation_rounds.contains_key(job_id)`
  — else panel txs are rejected as "unknown job" once the primary lifecycle drains.
- **P2 (snapshot push):** `push_verifier_snapshot` passes `&self.state.escalation_rounds` as a new
  arg to `build_verifier_views`.
Nothing else protected. Frozen `src/staging/pouw/` byte-identical (standing gate).

## Consensus / determinism invariants (build-time checklist)
Plan §4 items 1–7 all apply verbatim: seed/candidate determinism (sorted iteration, no wall-clock),
rollback safety, sorted settle sweep with pinned oracle, canonical state-root fold, C1 params
re-injection, single authoritative draw (never `escalation::resolve` on-chain), idempotent settle +
at-most-once drain. PLUS (new): **G1 — the F2 gate is consensus state**: the gate reads only the
drawn panel size and genesis `k_escalate`; gate-taken vs gate-refunded must be identical on every
node and across restart replay. **G2 — no double-settle**: a job is never live in both maps with
an undrained pot (`escrowed_for_job` asserts at hand-off and final settle — plan R5).

## Consensus-semantics note
Validity-widening + settlement-changing (NoQuorum pots move differently; new state-root section;
new CF). Rides the SAME not-yet-executed alpha genesis reset as the 2026-07-18 claim-race change
(`4d4e17a`) — MUST land pre-reset (plan R4). Chain-id bump at the public reset already recommended.

## Test plan
1. **Unit (keep):** `escalation_round.rs` 11/11 on `EscrowLedger` post-S1.
2. **Unit (new, state.rs):** panel-draw determinism (two perturbed nodes → identical panel +
   root); open-on-Escalate transition; **F2 gate both sides** (viable → round opens; short →
   fallback refund, byte-identical to today); rollback leaves `escalation_rounds` untouched;
   restart round-trip + `set_consensus_params` rebuild settles identically.
3. **Golden oracle:** all-participate inputs → on-chain settle outcome ≡ frozen
   `escalation::resolve(Trigger::NoQuorum)` field-for-field.
4. **B10 equivalence:** extend `run_on_both` with an escalation leg (staging `EscrowLedger` ≡
   `ChainState`, per-participant balances + conservation on every terminal).
5. **e2e (`pouw_payout_e2e.rs`):** update the NoQuorum negative-control: compute the harness's
   eligible candidates minus committee minus executor vs `quorum(k_escalate)`; if short, assert
   the F2 fallback refund (byte-identical to today's outcome), else assert the round OPENS. New
   scenarios (harness sized so the gate passes): (a) round-1 NoQuorum → panel Confirms →
   executor + panel + vindicated paid; (b) panel also-NoQuorums → bounded terminal (bond burn +
   committee slash). Conservation asserted every block.
6. **Live smoke:** extend/param the payout smoke for an escalation scenario (needs ≥ quorum(7)+
   committee+executor bonded nodes, or a genesis-lowered `k_escalate` for the smoke net — smoke
   may set `k_escalate=4` via consensus params to fit ~8 nodes; decide at build time by smoke
   capacity).
7. **Standing gates:** full `cargo test --workspace` green (NOT just lib/bins — 2026-07-18
   lesson); frozen `pouw/` byte-identical; 3-lens adversarial review workflow before commit.

## Build order (each step compiles + full suite green before the next)
S1 ledger generalisation → S2 DTO + `candidates()` accessor → S3 rocks CF → S4 ChainState
plumbing (map/root/rollback/persist) → S5+S6 open-draw-gate + settle routing (THE FLIP, includes
F2 gate) → S7 admission (C3 windows in `select_applicable_txs`; P1 protected hunk) → S8 verifier
loop arm (P2 protected hunk) → tests §5/§6 → live smoke.
