# Production Plan — THE MAP v2 (2026-07-05)

**What this is:** The founder-approved, ordered plan from today's state to a public testnet. Supersedes the
sequencing sections of `2026-06-23-pouw-readiness-assessment.md` (which remains the authoritative detail
reference for the B1–B10 items and the LIVE-ENABLEMENT GATE). Grounded in the 2026-07-05 verified
full-codebase survey (9-agent sweep; every load-bearing claim spot-checked against code).

**Where it wires in:** This is a docs-only file. No code changes. Each phase below names its own wire-in targets.

---

## Founder decisions locked 2026-07-05

| # | Decision | Choice |
|---|----------|--------|
| D1 | Merge `agent-persist-20260623` → `main` | **APPROVED & DONE** — ff to `a950fdf`, branch deleted after test verification |
| D2 | NoQuorum verdict handling in v1 | **Fallback terminal** — settle via an existing conserved resolver (timeout-style: partial executor comp, refund remainder, return bonds). On-chain EscalationRound becomes a post-flip fast-follow with its own DTO/persistence/settler work. |
| D3 | RPC posture for public testnet | **Public/keyed route split** — read-only routes (/status, /block, /peers, /tx) public + rate-limited; admin/operator routes API-key gated; TLS via reverse proxy in front. |
| D4 | Cloud-IP nerf policy | **Display-only for testnet** — flag stays visible in /peers + dashboard; reward enforcement + exemption mechanism designed properly before mainnet genesis. |

Still open (raise at the relevant step): D5 permissionless bonding confirm (validators-only override hook
exists, default permissionless — confirm at B5); D6 faucet funding amount + genesis account (at Phase-2
faucet step); D7 release/versioning convention (two divergent release scripts exist).

---

## Phase 0 — Integrate (DONE 2026-07-05)

- [x] Fast-forward `main` `edd053a` → `a950fdf` (B1a, N1, B1b, B10 — verified clean ff, no protected files in diff)
- [x] Full workspace test suite re-run on `main` (green — see session log)
- [x] Delete `agent-persist-20260623` after verification
- Everything stays **LOCAL**. Nothing is pushed from `~/Coin` — ever. Public artifacts go through the
  `~/commputer-clean` airlock only.

## Phase 1 — Track 1: the atomic PoUW flip (supervised main-session; per-change approval on PROTECTED files)

Ordering principle: fix the persistence substrate first, then land B2–B4 + B5–B9 as ONE coordinated
enablement (the LIVE-ENABLEMENT GATE: any subset alone = reachable non-conserved or non-persisted money).

**1.0 Per-block persistence fix (NEW, prerequisite — non-protected `storage/src/state.rs`)**
The 2026-07-05 survey found the live apply path (`apply_block_validated`) persists only block+meta;
accounts + the 4 PoUW maps persist only at clean-shutdown flush, which is PUT-only (no deletes → stale
bonded/unbonding rows resurrect across restarts = value duplication; crash after Bond txs = lost state).
`apply_block_atomic` exists + is tested but has zero production callers.
- Make `apply_block_validated` persist accounts + all 4 maps atomically per block (WriteBatch), or route it
  through `apply_block_atomic`. Internals live in non-protected state.rs — no event_loop change required.
- Make `flush_consensus_maps` delete stale CF rows for entries removed in-memory.
- Make `revert_block` roll back the 4 maps (StateDiff currently restores balances only → Bond revert would
  duplicate value).
- Fix stale in-code comments claiming the maps are "in-memory only".

**1.1 B2–B4 (non-protected state.rs, gate-bound — built together, committed together)**
- B2: SubmitJobV2 burn→escrow at the shared arm (~state.rs:1076-1093, also the Batch arm ~:1240); remove
  SubmitJobV2 from `is_burn`/`burn_amount`; `total_burned` must NOT move on escrow.
- B3: `JobLifecycle::open` at ClaimJob + escrow budget & executor bond.
- B4: route Commit/Reveal arms → `lifecycle_record_commit`/`lifecycle_record_reveal` (record_commit escrows
  the bond ITSELF — do not also escrow_into_job = double-escrow). Closes the inert-Commit spam window
  (arbitrary declared bonds accepted at fee-only cost today).
- D2 fallback: NoQuorum/Escalate terminals settle via the conserved fallback resolver — no job pot may
  strand. Cover the fallback terminal with a new B10-style equivalence case.

**1.2 B5–B8 (PROTECTED `event_loop.rs` / `main.rs` / genesis — founder approves each edit)**
- B5: committee draw at CompleteJob. Seed = post-result block hash (founder-locked v1). Candidate filter
  reads ONLY deterministic finalized on-chain state (`is_eligible`: bonded ≥ min_bond, on-chain compliance,
  exclude executor) — NEVER node-local `consensus.slashed_validators` (per-node ordering → forked
  committees). D5 (permissionless bonding) confirmed here.
- B6: lifecycle loop in `enforce_timeouts` tick — advance + should_settle + pot pre-validation + settle +
  drain terminals (Escalate → D2 fallback in v1).
- B7: G6 capacity admission in block assembly. Prerequisite non-protected glue first: `pending_job_from_tx`
  + `validator_churn_bps` helpers (NOT yet built — do these in 1.1's window).
- B8: genesis consensus_params (Game/Resolution/PhaseDeadlines/Stake/Capacity/WasmLimits) into ROOT
  genesis.json + GenesisConfig, AND populate the `game_params`/`resolution_params` ChainState fields B1b
  left defaulted (`TODO(B8)` at state.rs open() load path) — same change, or reloaded lifecycles get stale
  params.
- **1.2-DA: DA transport backend** (founder wire-in, PROTECTED): BridgeTransport's async libp2p backend
  (Kademlia + request-response) does not exist — without it every real verifier Abstains → all jobs
  NoQuorum. The flip is not "live" for multi-node until this lands; local single-process testing can use
  InMemoryTransport meanwhile.

**1.3 B9 + N2 (PROTECTED, small)**
- B9: delete stale `src/genesis.json` (schema-incompatible; a node launched from src/ silently falls back
  to default genesis and gets rejected by peers). Root genesis.json is canonical.
- N2: `#[cfg(unix)]` on the two `tokio::signal::unix` registrations (event_loop.rs ~:666, ~:681) — gates
  Windows binaries only; own commit per the distribution blueprint.

**1.4 Verification gate (flip is "landed" only when ALL green)**
- B10 golden-equivalence suite (now including the D2 fallback terminal) + full workspace tests.
- 3-node local multinode: `scripts/multinode_smoke.sh` + `multinode_assert.sh` — cross-node state-root
  agreement with PoUW txs flowing (Bond → SubmitJobV2 escrow → Claim → Commit/Reveal → settle), plus a
  kill-and-restart node to prove per-block persistence (1.0) under crash.
- Conservation audit across apply AND reorg. Soak before Phase 2.
- NOTE: the flip is consensus-affecting (old binaries can't borsh-decode the new TxKinds; state-root format
  changes) — irrelevant while no network is deployed, but from the first public node onward any change like
  this requires a coordinated upgrade.

## Phase 2 — Track 2: public testnet (independent of PoUW; some items can start now)

Ordered; (P) = PROTECTED file involved:
1. **ConnectInfo fix** (non-protected rpc.rs, 1 line): `.into_make_service_with_connect_info::<SocketAddr>()`
   — per-IP rate limiting currently collapses to ONE global bucket ("unknown" key).
2. **D3 RPC route split** (rpc.rs): public read-only + /tx with rate limits; keyed admin routes; keep F-4
   refuse-to-bind for the keyed tier.
3. **Faucet provisioning** (P: main.rs + root genesis.json): wire staged `src/staging/rpc_faucet_dispense.rs`,
   genesis-fund the faucet account (D6). Without it a public testnet has no token distribution.
4. **F-3 per-account mempool quota** (P: event_loop.rs, ~validate_tx_for_mempool): blueprint at
   `src/staging/docs/f3_mempool_quota_blueprint.md` — closes the single-account mempool-flood grief.
5. **A7 hash-aware sync** (P: event_loop.rs): staged `src/staging/sync_machine_v2.rs` ("sound core,
   5 caveats — do not wire as-is"); today sync is height-only → dead-fork undetectable.
6. **Consensus-safety hardening** (survey findings; consensus_manager.rs + event_loop.rs (P) + block.rs):
   per-peer dedup/sampling on Snowball vote ingestion (one peer can fabricate quorum today); close the
   unsigned-block "legacy compat" acceptance (block.rs verify_producer_signature empty-sig=true); fix the
   HashMap-order nondeterministic round winner; cap decompression output; rate-limit sync serving.
7. **Wallet/key-management security pass** — the one subsystem the 2026-07-05 sweep did not cover in depth;
   audit before real validators hold keys.
8. **D4 execution**: keep cloud-IP nerf display-only; document for operators; mainnet enforcement +
   exemption design tracked separately.
9. **Seed + API infrastructure**: real seed box; `seed.commputer.xyz` must be grey-cloud/DNS-only (CF HTTP
   proxy cannot carry raw P2P TCP — the current record is decorative); origin (or Worker SEED_RPC_URL) for
   `api.commputer.xyz` (currently CF 525 → dashboard shows zeros); TLS reverse proxy in front of RPC;
   compiled-in `SEED_NODES` is empty — decide whether to bake the seed in at release.
10. **Release engineering** (P: website): first tag + release CI (ci.yml only builds/tests today; two
    divergent release scripts, no version truth source — D7), Linux binaries + SHA-256 (macOS needs
    osxcross or CI runners; Windows needs N2 first), THEN un-gate install.sh + index.html download button +
    GitHub release assets — three coordinated edits.
11. **Publish**: everything public flows through `~/commputer-clean` (de-anon airlock; scrub scan before
    every push). Still open there: live repo main reset (un-scrubbed `492c5b1`), CF account identity.

## Standing invariants (unchanged)
- Agents on `agent-*` branches; founder on main; PROTECTED files only with founder per-change approval.
- Frozen game crate `src/staging/pouw/` stays byte-identical (DTO seam in pouw-onchain).
- NEVER push from `~/Coin`. Git identity `The Commrade <noreply@commputer.xyz>`.
- Every money-moving change lands with conservation tests; B10 equivalence is the permanent regression net.
