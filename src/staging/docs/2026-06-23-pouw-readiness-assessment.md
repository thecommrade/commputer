# PoUW On-Chain Integration — Consolidated Readiness Assessment

**Branch:** `agent-overnight-20260622` · **Date:** 2026-06-23 · Synthesized from a 3-lens whole-branch
adversarial review (architecture-coherence · go-live-completeness · cross-module-correctness) + an
adversarial re-check of contested findings.

> ## ⛔ LIVE-ENABLEMENT GATE (the one cross-cutting constraint — read first)
> The PoUW money path is **inert today and conserved by construction** (no reachable money path). Turning
> it on is **ALL-OR-NOTHING**: these MUST land together as a single atomic enablement, or the chain breaks:
> - **B1** persistence (RocksDB + `compute_state_root` + reorg/revert recovery) for
>   `escrow_by_job`/`bonded_stake`/`unbonding_stake`/`job_lifecycles`
> - **B2** flip `SubmitJobV2` burn→escrow
> - **B3** lifecycle creation at `ClaimJob`
> - **B4** route `Commit`/`Reveal` to the lifecycle helpers
>
> Landing any SUBSET makes reachable money state that is **non-conserved** (B2 alone) or
> **non-persisted / not in the state root** (B3/B4 without B1) → guaranteed **fund loss or consensus
> fork**. No live PoUW tx until B1–B4 + the Phase-B protected wiring (B5–B8) all pass **B10** (the
> cross-boundary golden-equivalence test). The individual pieces are documented; THIS atomicity
> constraint is the artifact that was missing.

## 1. Overall verdict

**Coherent: yes. Safe to build on: yes. Safe to merge to `main` as foundation-not-live: yes** (with the
gate above noted). All three lenses returned `approved=false` *for go-live*, but they agree on the same
thing from different angles: the **Phase-A scaffolding is built + hardened** (the `Ledger` trait, the
`escrow_by_job`/`bonded_stake`/`job_lifecycles` stores, the `JobLifecycle` state machine, the settlement
resolvers, the `Commit`/`Reveal` TxKinds, the `ChainLedger` helpers with pot pre-validation), while the
**Phase-B live wiring is deliberately absent and documented as PROTECTED / to-do**. Inert, not broken:
`SubmitJobV2` burns at submit (`state.rs:946`), the `Commit`/`Reveal` arms only bump nonce and escrow
nothing (`state.rs:974-997`), and the three maps stay empty until the live txs exist (`state.rs:159`).
Conservation holds today because no money path is reachable.

## 2. DONE on this branch (verified)

- ✅ `Ledger` trait + `ChainLedger` impl over `&mut ChainState` (`state.rs:2124+`)
- ✅ `escrow_by_job` pot + escrow/pay/burn primitives (P1)
- ✅ `bonded_stake`/`unbonding_stake` + bond/unbond/withdraw/slash + `stake_of`/`is_eligible` (G4)
- ✅ `JobLifecycle` multi-block commit-reveal machine + `expected_escrow`/`should_settle`/`advance`/`settle`
- ✅ 5 settlement resolvers generic over `Ledger`; escalation 2nd-round handoff conserved
- ✅ `SubmitJobV2` (G3) + `Commit`/`Reveal` TxKinds (appended at enum end — borsh-safe)
- ✅ Capacity (G6) admission logic — pure, tested, awaiting wiring
- ✅ Pot pre-validation in `lifecycle_settle`/`record_commit` (panic→Err)
- ✅ On-chain integration tests: Confirmed + Disputed + Timeout + malformed-pot-rejection, all conserved

## 3. BLOCKS LIVE TESTNET (all documented; none implemented)

| # | Prerequisite | Protected? |
|---|---|---|
| B1 | Full persistence (RocksStore ser/load, `open`, `flush`/`apply_block_atomic`, `compute_state_root`+`snapshot`, `revert_block`/`try_reorg`/`reset_to_genesis`) for the 3 maps — **land all 5 together** | non-protected |
| B2 | Flip `SubmitJobV2` burn→escrow (`state.rs:946`) | non-protected |
| B3 | Lifecycle creation at `ClaimJob` (open AwaitingResult; escrow budget+Be) | non-protected |
| B4 | Route `Commit`/`Reveal` → `lifecycle_record_commit`/`record_reveal` | non-protected |
| B5 | Committee draw at `CompleteJob` (seed = post-result block hash; filter by FINALIZED on-chain state only, not `consensus.slashed_validators`) | **PROTECTED** event_loop.rs |
| B6 | `enforce_timeouts` lifecycle loop (`advance` + `should_settle` + `settle` + drain) | **PROTECTED** event_loop.rs |
| B7 | G6 capacity admission wired into block assembly | **PROTECTED** event_loop.rs |
| B8 | Genesis `consensus_params` (Game/Resolution/PhaseDeadlines/Stake/Capacity) → root `genesis.json` + `GenesisConfig` | **PROTECTED** genesis.json |
| B9 | Delete stale `src/genesis.json` (D-2 — canonical is the root file; the stale nested one → peer-handshake footgun) | **PROTECTED** genesis |
| B10 | Cross-boundary golden-equivalence test (a full live SubmitJobV2→…→settle through `ChainState` apply, conserved, vs the staging reference) | non-protected |

## 4. NEW gaps the system view surfaced (NOT previously scheduled)

- **N1 — No bond/unbond TxKinds.** `bond`/`request_unbond`/`withdraw_unbonded` exist on `ChainState`
  (`state.rs:1965-2068`) but **no `TxKind` triggers them**. Without them `bonded_stake` stays empty
  forever → the committee draw has zero stake-weighted, eligible candidates → **bonded-stake committee
  selection is impossible at go-live.** Needs `Bond`/`RequestUnbond`/`WithdrawUnbonded` TxKinds (appended
  at the enum end, borsh-safe) + apply arms + escrow-style accounting. **Add to the P2 roadmap.**
- **N2 — Windows compile failure.** `event_loop.rs:666,681` call `tokio::signal::unix::signal()` with no
  `#[cfg(unix)]` guard (the `select!` arms already `.pending()`-fallback). PROTECTED; already in the
  distribution blueprint §3.4 (D-3). Gates Windows binaries only — Linux/macOS Phase-1 unaffected.
- **N3 — Persistence (B1) has no assigned owner/task** — documented in code comments only. Process gap.

## 5. Genuine bugs / contradictions

**None beyond the documented inert-foundation state.** The reviewers' three "blocker" candidates were
adversarially re-checked and **downgraded to FALSE POSITIVES**:
- *"Double-burn"* (SubmitJobV2 burn + settlement burn): the `Commit`/`Reveal` arms are inert and never
  escrow; no settlement is reachable today. The real constraint is the B2 sequencing (flip burn→escrow
  **as** you wire settlement, never independently) — captured in the gate above.
- *"NoQuorum→Escalate forfeiture stranded"*: the primary round burns non-revealer bonds **before** the
  verdict branch (`lifecycle.rs:306-311`), so the pot holds exactly `budget + Be + revealers·Bv` on every
  path; the `EscalationHandoff` carries revealers + their bonds (`escalation_round.rs:3-4`). Conserved.
- *"Built backwards"*: Phase-A-before-Phase-B is intentional + documented (CLAUDE.md forbids agents
  touching the PROTECTED files). Incomplete, not inverted.

## 6. Merge-readiness recommendation

**MERGE to `main` as foundation-not-live — conditionally.** Additive, inert, conserved-by-construction,
matches patch-spec intent, per-module reviews folded. Before/at merge:
1. This doc IS the missing "live-enablement gate" artifact (§ gate + B1–B10).
2. Schedule the NEW gaps: **N1 (bond TxKinds — silently blocks committee selection)**, N2 (Windows cfg —
   Phase-2), N3 (assign the persistence owner).
3. Do NOT, on this agent branch: flip `SubmitJobV2` (B2), delete `src/genesis.json` (B9), or touch any
   PROTECTED file (B5–B9, N2) — those are founder/main-session actions per CLAUDE.md.

`approved=false` for go-live is correct; `approved=true` for merge-as-inert-foundation is warranted.
