# Commputer Track-2 — PoUW Off-Chain Actors BUILD PLAN (executor · verifier · libp2p DA)

**Date:** 2026-07-09
**Branch context:** `agent-testnet-20260707` (post-merge of THE POUW FLIP; on-chain PoUW surface is consensus-active + merged).
**Purpose:** the scoping plan for Track 2 — the three off-chain node actor loops that make PoUW pots **PAY OUT** (Confirmed → worker 85% / verifiers 10% / burn 5%) instead of the current **REFUND** default (every pot conserves-and-returns because no actor acts). The on-chain machinery (apply arms, committee draw, `settle_due_jobs`, the five terminal resolvers) is DONE and tested; Track 2 is pure client plumbing that drives the already-live arms.
**Provenance:** four area-map passes — (1) on-chain PoUW surface + the actor contract; (2) executor auto-claim loop; (3) verifier commit/reveal loop; (4) real libp2p DA backend — folded here into a single sequenced build plan. Every load-bearing claim is cited to `file:line` from those maps.
**Cross-references:** `src/staging/docs/2026-07-06-phase12-wiring-spec.md` (the C5 production-INERT note @ :384–385; "no executor auto-claim loop" @ :199, :287), `src/staging/docs/2026-07-05-production-plan.md` (D2 zero-comp refund; multinode gate), `src/staging/docs/2026-06-23-pouw-readiness-assessment.md` (THE MAP).

**STATUS: SCOPING — awaiting founder GO to build. Nothing implemented. This document modifies no source.**

**Hard constraints (from CLAUDE.md + the maps):**
- The frozen `src/staging/pouw/` crate (`commputer_pouw`: `select_committee`, `make_commitment`, `reveal_matches`, `compute_verdict`, `GameParams`, `WasmOracle`) is **NEVER modified** — the actors consume it, byte-identical.
- PROTECTED (founder-only): `src/node/src/{main,event_loop,config}.rs`, root `genesis.json`/`*.toml`, `src/core/src/token.rs`. Every actor's substance lives in **NEW non-protected modules**; the PROTECTED surface is a minimal, founder-reviewed set of channel fields + one `select!` arm + task spawns.
- Agents create NEW files. The only edits to existing files are the founder-gated PROTECTED wire-ins and one founder-gated edit to the non-protected-but-existing `src/network/src/transport.rs` (`CommpBehaviour`).

---

## §1 — THE PAY-OUT GAP (why every pot refunds today)

**The entire pay-out machinery is deployed and consensus-active.** `apply_transaction` dispatch is live for the full sequence: `SubmitJobV2` (`state.rs:1531`), `ClaimJob` (:1577), `CompleteJob` (:1584), `Commit` (:1617), `Reveal` (:1627), `Bond`/`RequestUnbond`/`WithdrawUnbonded` (:1662/:1677/:1686). Committee draw runs in-apply in the block tail (`draw_committees_for_completed_jobs` `state.rs:3568`, called at :869, seed = `hash(block.hash()‖job_id)`). Settlement runs every block inside the rollback envelope (`settle_due_jobs(height)` `state.rs:937`, inside `apply_txs_with_rollback` :874). The five resolvers (`settlement_resolution.rs`) all conserve supply and drain the pot to 0: `resolve_confirmed:154` (the **only** executor payout — worker 85% / verifiers 10% / burn 5% + all bonds returned), `resolve_disputed:181`, `resolve_timeout:68`, `resolve_cancel:49`, `resolve_unavailable:94`, plus `resolve_escalation_fallback:130` (the D2 zero-comp NoQuorum refund stand-in).

**Nothing is broken. The pay-out condition is simply structurally unreachable because no client emits the driving tx sequence.** Concretely, every pot refunds today via one of three drains (all conserved, all safe):

1. **No executor → job never claimed.** `pending_jobs` (`state.rs:230`) fills at `SubmitJobV2`, but nothing scans it and emits `ClaimJob`. At `height > claim_by`, `expire_pending_job` (`state.rs:2009`) refunds the full budget. **This is where ~100% of pots refund now.**
2. **Claimed but no `CompleteJob`** → `executor_hash == None` → verdict routes to `TimedOut` (executor bond slashed, submitter refunded).
3. **Completed but no verifier `Commit`/`Reveal`** → zero reveals → `compute_verdict(&[])` = `NoQuorum` → `Terminal::Escalate` → `resolve_escalation_fallback` refunds (D2 zero-comp).

**What each actor must do to flip a job to `Confirmed` (`resolve_confirmed`, the paying terminal):**

- **Executor** must (a) emit a `ClaimJob{job_id}` that passes `apply_claim_job` (`state.rs:1843`): `from` is a bonded validator (:1857), `job_id ∈ pending_jobs` (:1869), `height ≤ claim_by` (:1873), pot == budget (:1880), and the wallet can fund `e_bond = max(budget, game_params.executor_bond)` (:1878, escrowed :1903); then (b) fetch program+input bytes via DA, run `execute_job` (`pouw_executor.rs:41`) to get `result_hash`, and emit `CompleteJob{job_id,result_hash}` accepted by `post_result` (`lifecycle.rs:494`) at `height ≤ result_by`.
- **Verifier(s)** — a committee drawn from the snapshotted candidate set, `k = game_params.k` (default 3), quorum `= ceil(2/3·3) = 2` (`params.rs:34–37,76`) — each must (a) detect membership (`to_record().committee` contains me at `phase == Committing`), independently re-execute via `execute_job` to get **its own** `result_hash`, and emit a `Commit{job_id,commit,bond}` accepted by `record_commit` (`lifecycle.rs:539`): in committee (:546), `bond == verifier_bond` (:549), no double-commit (:552), where `commit = make_commitment(result_hash‖salt‖verifier)`; then (b) emit a `Reveal{job_id,result_hash,salt}` accepted by `record_reveal` (`lifecycle.rs:562`) at `phase == Revealing`, `height ≤ reveal_by`, matching the prior commitment via frozen `reveal_matches` (:573).
- **DA** must deliver the program (and input) bytes cross-node so both the executor and each verifier can actually compute. Without it `verify_available` → `Abstain` → no `Commit` → NoQuorum → refund.

**`Confirmed` requires ALL THREE together:** `ClaimJob` + `CompleteJob` (executor) AND ≥ quorum(k)=2-of-3 committee members each `Commit`+`Reveal` a hash **equal to the executor's** `result_hash` within its phase window (verifier), AND DA delivering the bytes both sides re-execute. Settlement itself is 100% on-chain and needs **no actor** — the loops only produce txs; the chain does the rest deterministically inside the rollback envelope.

---

## §2 — THE 3 ACTORS

### §2.1 EXECUTOR auto-claim loop — emits `ClaimJob` + `CompleteJob`

**Exists (merged/live/tested):**
- The execution kernel is real + tested but **uncalled**: `pouw_executor::execute_job` (`node/src/pouw_executor.rs:40–58`) — deterministic, fuel-metered WASM via `commputer_pouw` `WasmOracle`; enforces the linchpin `sha256(bytes)==program_hash` (:49–52); returns `hash_parts(outcome)` == the value the committee/`settle` compares. File is `#![allow(dead_code)]` (:17) with **zero callers**. It IS compiled in (`node/Cargo.toml:17` links `commputer-pouw` with `wasm-runtime`; `main.rs:14` declares `mod pouw_executor`). Oracle build path real+tested: `exec_adapter.rs build_oracle:17` / `populate_from_da:24`.
- Claim/complete on-chain surface merged: `apply_claim_job` (`state.rs:1843–1929`) rejects double-claim (:1853), non-validator (:1857), unknown/expired job (:1869–1875); `CompleteJob` arm (:1584–1605) → `lifecycle_post_result` records at parent height, committee drawn in tail from `block.hash()`.
- Claimable-job source real + persisted: `pending_jobs` map (`state.rs:230`), `PendingJobRecord = {submitter,budget,program_hash,input_hash,da_root,submitted_height,claim_by}` (`state.rs:3254–3267`), state-root folded.
- Tx-submission machinery is real but duplicated inline (no helper): build `Transaction{...}` → `signing::sign_transaction(&mut tx,&wallet)` → `gossipsub.publish(topics::tx_topic(), compress(json))` → `self.pending_txs.push(tx)`. Three call sites: `event_loop.rs:369–396`, :2200–2215, :2510–2543. `wallet` @ :112, `pending_txs` @ :118.
- Timer-arm template real: the `select!` at `event_loop.rs:724`; the `job_timeout_interval` arm (:905–920) reads `self.state.blocks.height()` at a quiescent between-blocks point — the exact template for an executor tick. Off-runtime CPU pattern exists: `spawn_blocking` → result over mpsc → dedicated `select!` arm (epoch/proof channels :121–128, spawn sites :2593, :3461).
- DA bridge real + tested: `da_transport.rs BridgeTransport` (sync-over-async mpsc, degrades to `Abstain`/`None`, never hangs; `with_timeout` :82) + `MonotonicClock`.

**Mocked / missing:**
- **The entire loop.** `execute_job` has zero callers (`phase12-wiring-spec.md:199,:287`; C5 INERT note :384–385).
- Real DA network backend (hard dep — see §2.3). Without it the executor cannot fetch bytes → job Timeouts.
- **Input-bytes distribution is undefined** (shared blocker): only the PROGRAM is published to DA (`world.rs:93–101`); `PendingJobRecord` carries `input_hash` + a single `da_root` but no code publishes/fetches input. `execute_job` needs BOTH `program_bytes` AND `input`.
- No `SubmitJobV2` producer path (jobs are test-injected today). `job_rpc.rs`/`job_spec.rs` are legacy V1 String-hash substrate. Do NOT reuse `compute_handler.rs`/`compute_session.rs` (V1 `#![allow(dead_code)]`) or `proof_manager.rs` (that is PoW resource-proof, not the PoUW result proof). The node-local `job_pool` (`event_loop.rs:912`) is an OBSERVE-ONLY V1 mirror — must NOT settle PoUW.
- Consensus-anchored `WasmLimits` not threaded to any executor (`consensus_params.rs:48` + `min_fuel_cap:55` exist but unsourced; `execute_job` takes bare `WasmLimits`, tests pass `default()`).

**Wiring point:** a NEW non-protected module (e.g. `src/staging/.../pouw_executor_loop.rs`) exposing a **pure planner** `fn plan_executor_actions(state: &ChainState, me: Address, policy: &ExecutorPolicy) -> Vec<ExecutorAction>`. PROTECTED `event_loop.rs` gets only: a new `executor_tick_interval` alongside :654–697, a `handle_executor_tick()` that (1) scans `self.state.pending_jobs` for claimable jobs and `self.state.job_lifecycles` for my own `AwaitingResult`/unset-hash jobs to resume, (2) builds+signs+gossips a `ClaimJob` via the :369–396 pattern, (3) hands `(program_hash,input_hash,program_bytes,input,WasmLimits)` to `spawn_blocking` (WASM is CPU-heavy — MUST NOT run on the loop thread) returning `result_hash` over a new mpsc consumed by a dedicated arm that emits `CompleteJob`.

**Build steps:**
1. **[non-protected]** New module + `ExecutorPolicy` + `ExecutorAction` types + `plan_executor_actions` planner (pure, sync, no async). Encodes the affordability + eligibility + resume logic mirroring the live gates.
2. **[non-protected]** `spawn_blocking`-friendly execution shim that calls `pouw_executor::execute_job` and returns `result_hash` (thin wrapper; keeps WASM off the loop thread).
3. **[non-protected]** Consensus-`WasmLimits` sourcing: thread `ConsensusParams.wasm_limits` onto the planner input (single authoritative source, never `default()`).
4. **[PROTECTED, founder]** `event_loop.rs`: `executor_tick_interval` field + arm; `result_hash` mpsc channel pair + dedicated drain arm; 2 tx-emit call sites reusing the :369–396 pattern.
5. **[PROTECTED, founder]** `main.rs`: construct the channel, spawn the executor task with the wallet + `[executor] enabled` config role + a `BridgeTransport` DA handle.
6. **[PROTECTED, founder]** `config.rs`: `[executor] enabled=false` (default OFF), concurrency cap, min-balance reserve.

### §2.2 VERIFIER commit/reveal loop — emits `Commit` + `Reveal`

**Exists (merged/live/tested):** the on-chain machinery is complete — the loop only PRODUCES txs; the chain does everything else in-apply.
- Commit path: arm `state.rs:1617–1625` → `apply_commit :1934–1966` (zero-addr + validators-only gate + balance pre-check) → `lifecycle_record_commit :3504` → `record_commit lifecycle.rs:539–559` (phase/window/membership/`bond==verifier_bond`/no-double-commit, escrows bond).
- Reveal path: arm `state.rs:1627–1632` → `apply_reveal :1970–2002` (self-advances `Committing→Revealing` first, :1993) → `record_reveal lifecycle.rs:562–581` (phase==Revealing / `height≤reveal_by` / matching commitment via frozen `reveal_matches` / no replay). No money moves on reveal.
- Committee draw is 100% in-apply, deterministic (`state.rs:3568`, called :869; candidates snapshotted+SORTED at ClaimJob :1888–1900).
- Settlement 100% in-apply (`settle_due_jobs :937` → `settle lifecycle.rs:599–678`): burns commit-no-reveal forfeitures (:625–631) before the verdict branch. The loop does NOTHING at settlement.
- Frozen primitives (DO NOT MODIFY): `commit_reveal.rs:8 make_commitment`, `:12 reveal_matches`, `committee.rs:5 select_committee`, `job.rs:36–48` Commitment/Reveal.
- `Commit`/`Reveal` TxKinds exist, borsh-append-safe (`transaction.rs:173–186`); verifier is ALWAYS `tx.from` (no spoof surface).
- Re-execution unit = the SAME `pouw_executor.rs:40 execute_job` the executor uses (shared runtime via `exec_adapter.rs`).
- Node reads everything it needs lock-free: `EventLoop` owns `pub state: ChainState` single-threaded (`event_loop.rs:111`); `job_lifecycles` is pub (`state.rs:224`); `to_record()` (`lifecycle.rs:391`) exposes phase/committee/commitments/reveals/deadlines/hashes/`verifier_bond`/`executor_hash` — everything for membership detection AND on-chain idempotency.
- Self-origination template: `auto_register_validator` (`event_loop.rs:2489–2550`). Mempool ingress already tolerates Commit/Reveal for KNOWN jobs (C7 filter :2268–2280); `select_applicable_txs` drops any doomed PoUW tx from a producer's block.
- Choreography reference (frozen): `pouw-e2e/world.rs gate_pool:120 → run_lifecycle:206` + `scenarios.rs` honest/dishonest reveal closures.

**Mocked / missing:**
- **The verifier loop itself** — nothing detects "I am on committee for job X". (`proof_manager.rs` is the epoch PoW/storage system, unrelated; zero commit/reveal substrate in `compute_session.rs`/`compute_handler.rs`/`job_rpc.rs`.)
- Real DA backend (C5). No backend → `verify_available` → `Abstain` → no Commit → NoQuorum → refund (today's inert behavior).
- **Input-bytes sourcing missing** (shared with executor): lifecycle stores only `input_hash` (`lifecycle.rs:293`); staging DA publishes program only (`world.rs:93`), holding input out-of-band (:239). Re-execution cannot run without input bytes.
- **Salt persistence missing (fund-safety critical):** `commit = H(result_hash‖salt‖verifier)`; the salt is secret until reveal and is NEVER on-chain. There is no durable local salt store → a verifier that commits then restarts before revealing loses its bond (burned as commit-no-reveal forfeiture, `lifecycle.rs:625–631`) despite being honest.
- No executor auto-claim/complete loop (sibling) → committees only form at `CompleteJob` (:869), so with no autonomous emitter the verifier has nothing to react to.

**Wiring point:** a NEW non-protected module (e.g. `src/node/src/pouw_verifier.rs` — needs `self.wallet`/`self.network`/`self.state`, all node-crate types) wired by ONE new `select!` interval arm in PROTECTED `event_loop.rs` mirroring `job_timeout_interval` (declared :670, fired :905), calling `self.drive_pouw_verifier()`. That method: `me = ParticipantId(wallet.address().0)`; iterate `state.job_lifecycles` calling `.to_record()`; COMMIT branch — for `committee.contains(me) && phase==Committing && height≤commit_by && !committed(me)`: DA-fetch program(+input) → `execute_job` → draw salt → **durably persist `(job_id → {salt,result_hash})` and fsync BEFORE broadcasting** → build+sign a `Commit` (bond = `record.verifier_bond`) via the :2489–2550 template; REVEAL branch — for a commitment-without-reveal at `phase==Revealing && height≤reveal_by`: load salt+hash, emit `Reveal`. Needs a `BridgeTransport` DA handle + `DaCommand` backend threaded at construction (main.rs, PROTECTED).

**Build steps:**
1. **[non-protected]** New `pouw_verifier` module: membership-detection + phase-gate logic as a pure planner over `Vec<JobLifecycleRecord>` + `me` + `height`, returning `Vec<VerifierAction>` (Commit / Reveal / Abstain).
2. **[non-protected]** **Durable salt store** (new): node-local, `fsync-before-broadcast`. Location TBD (§5 open Q) — a file under `data_dir` (like `mempool.json`) or a RocksDB CF. Keyed `job_id → {salt,result_hash}`. This is the single most fund-safety-critical new artifact in Track 2.
3. **[non-protected]** Re-execution shim (shared with §2.1 step 2): `spawn_blocking` → `execute_job` → own `result_hash`. Verifier commits to its OWN hash, never copies on-chain `executor_hash` (honesty seam).
4. **[PROTECTED, founder]** `event_loop.rs`: one `select!` arm calling `drive_pouw_verifier()`.
5. **[PROTECTED, founder]** `main.rs`: thread the DA handle + spawn; `config.rs`: `[verifier] enabled=false`.

### §2.3 REAL libp2p DA backend — the linchpin both loops need

**Exists (complete + unit-tested):**
- `commputer-da` crate is complete and deterministic: `params.rs` (`DaAttestation`, `ChunkingParams` DEFAULT_CHUNK_SIZE=64KiB, SAMPLES_PER_VERIFIER=16, REPLICATION_FACTOR_K=20), `chunk.rs`, `code.rs` (systematic rate-1/2 RS over GF(2^8), N≤128 data → ≤256 coded), `merkle.rs`+`commit.rs`, `sampling.rs` (per-verifier Fisher-Yates, seed binds verifier_id), `facade.rs DataAvailability::verify_available → Available(bytes)|Abstain` with sha256 re-bind, `adapter.rs resolve_and_populate`. Node links it transitively.
- The DA interface is the SYNC `DaTransport` trait (`da/src/transport.rs:16–21`): `advertise`, `find_providers→Vec<ProviderId>`, `fetch_chunk→Option<(Vec<u8>,MerklePath)>`, `has_chunk`. Comment (:4–8) says a real libp2p adapter implements the same shapes; the four methods map 1:1 to Kademlia `start_providing`/`get_providers` + Bitswap want-block/want-have.
- The sync→async bridge exists + tested: `BridgeTransport` (`pouw-onchain/da_transport.rs:66–124`) turns each call into a `DaCommand` (:53–58) over `std::sync::mpsc` and blocks on the reply. Failure contract (:44–52,60–65): gone/silent/parked backend degrades to unavailable default → facade `Abstain`, never panic/hang; `with_timeout` :82 bounds the wait. `MonotonicClock` never hashed into consensus.
- On-chain DA anchor already carried: `SubmitJobV2.da_root` (`transaction.rs:122–125`); `JobLifecycleRecord` persists program_hash/input_hash/da_root (`lifecycle.rs:170–173`, RocksDB per B1b).
- Choreography fully specified + tested in the frozen `world.rs`: `publish()` (`build_attestation` + `transport.put` all 2N coded chunks with Merkle paths, :93–101) → `gate_pool()` (each candidate runs `verify_available`; Available reconstructs+rebinds+populates `ProgramStore`, :120–154) → `run_lifecycle()`. `happy_path_confirms_and_conserves` (:312–337) is the golden assertion a real backend must reproduce over the wire.
- The libp2p request-response PATTERN to mirror is present + hardened: `CommpBehaviour` (`network/src/transport.rs:143–156`) already has TWO request-response protocols (`sync`, `consensus`) + kademlia(MemoryStore) + gossipsub + identify. `sync_protocol.rs` is a complete template: length-prefixed JSON codec, decompression-bomb-safe incremental reader (MAX_SYNC_MESSAGE=10MiB :15, READ_CHUNK=64KiB :22, `read_length_prefixed` :32–58), `sync_behaviour()` :157–164, non-vacuous tests :211–319. Handled at `SwarmEvent::Behaviour(...::Sync)` (`event_loop.rs:1514–1611`); Kademlia events at :1451.
- Storage substrate trivially extensible: RocksDB named CFs (`rocks.rs:12–29`, `create_missing_column_families=true` :18) — a `CF_DA_CHUNKS` keyed by chunk_hash is additive + off-consensus. `LocalDiskTransport` (`da/transport.rs:99–191`) shows the on-disk `(bytes‖MerklePath)` encoding.

**Mocked / missing:**
- The only concrete `DaTransport` impls are single-node: `InMemoryTransport` (`da/transport.rs:41–75`) + `LocalDiskTransport` (:99–191), test-only. **No cross-node transport exists.**
- The only `DaCommand` consumer is the TEST thread `spawn_backend(store)` (`da_transport.rs:147–172`). `BridgeTransport` is wired **NOWHERE** in `src/node` — no production `DaCommand` producer OR consumer.
- **#1** a libp2p DA fetch protocol: new request-response behaviour (mirror `SyncCodec`) `DaRequest::GetChunk{chunk_hash} → DaResponse::Chunk(Option<(Vec<u8>,MerklePath)>)`, added to `CommpBehaviour`, handled in the swarm match.
- **#2** provider discovery: nothing maps `chunk_hash → holder`. Options: (a) Kademlia `start_providing`/`get_providers` (matches `providers.rs` xor_distance K=20); or (b) a simpler v1 returning connected peers / the validator set, relying on Merkle+sha256-rebind to reject wrong holders.
- **#3** a local blob store: nowhere stores coded chunks to serve `GetChunk`/`has_chunk`. Needs `CF_DA_CHUNKS` (or a filesystem dir) + retention/GC + size caps.
- **#4 the PUBLISHER path (biggest gap):** no node code runs `build_attestation` over program bytes, persists the 2N coded chunks, and advertises them. `SubmitJobV2` carries only `da_root`, not the bytes. No publisher → zero providers → Abstain → NoQuorum → refund → **nothing pays out**.
- **#5** the `DaCommand` backend actor: because the swarm is owned exclusively by the event_loop `select!` (`event_loop.rs:725 self.network.swarm.select_next_some()`), the backend cannot independently drive the swarm — it must inject commands into the event_loop and correlate async replies via a pending-request map (`QueryId`/`OutboundRequestId → reply Sender`).
- **#6** `ProviderId([u8;32])` (`params.rs:51`) is not a libp2p `PeerId` (multihash of pubkey) — `find_providers` returns `ProviderId` but `fetch_chunk(from)` must resolve a dialable `PeerId`. A real impedance mismatch to design.

**Wiring point:** the backend must live where the swarm lives (the event_loop) because `BridgeTransport` is sync+blocking and the swarm is single-owner. (1) Add a `da` request-response behaviour to `CommpBehaviour` (`transport.rs:143–156`) via a NEW `network/src/da_protocol.rs` mirroring `sync_protocol.rs`; register in the `with_behaviour` closure (:290–299). (2) In `EventLoop::run` add a NEW `select!` arm (mirror `solver_response_rx`/`epoch_finalize_rx` :758/:764) draining `da_command_rx: mpsc::Receiver<DaCommand>`: `Advertise → kademlia.start_providing`; `HasChunk →` sync local blob-store lookup + immediate reply; `FindProviders → kademlia.get_providers` returns a `QueryId`, stash the reply `Sender` in `pending_find`, fulfil in the Kademlia arm (:1451); `FetchChunk → da.send_request` returns an `OutboundRequestId`, stash in `pending_fetch`, fulfil in a NEW `...::Da` swarm arm (mirror Sync :1514), which also serves inbound `GetChunk` from the local blob store. (3) In `main.rs` construct the blob store + mpsc, and spawn the verifier+executor loops on **dedicated OS threads / `spawn_blocking`** holding `BridgeTransport::with_timeout(da_command_tx, window)` — they MUST NOT run on the event_loop thread (the bridge blocks on replies the event_loop produces → same-thread = **deadlock**).

**Build steps (this DA deliverable lands before/with the two loops):**
1. **[non-protected, NEW]** blob-store module (`CF_DA_CHUNKS` RocksDB CF or filesystem dir) — put/get/has/GC, size-capped, scoped to known/active jobs. Inert, unit-testable.
2. **[non-protected, NEW]** `network/src/da_protocol.rs` codec (mirror `sync_protocol.rs`: length-prefixed, 10MiB cap, bomb-safe reader). Additive.
3. **[non-protected, NEW]** publisher module: `build_attestation` over program(+input) bytes → persist 2N coded chunks → advertise. Single-node testable against `LocalDiskTransport` golden.
4. **[non-protected, founder-gated edit to existing]** `network/src/transport.rs`: add the `da` behaviour field to `CommpBehaviour` + register in `with_behaviour`.
5. **[PROTECTED, founder]** `event_loop.rs`: `da_command_rx` drain arm + `pending_find`/`pending_fetch` correlation maps + a new `CommpBehaviourEvent::Da` swarm arm + provider-reply fulfilment in the Kademlia arm.
6. **[PROTECTED, founder]** `main.rs`: construct blob store + channel + spawn the DA-backed loops off-thread; `config.rs` (if a size/retention knob is added).

---

## §3 — DEPENDENCY ORDER & SEQUENCING

**DA is the linchpin — both loops are downstream-blocked by it.** The executor needs DA to fetch program bytes before `execute_job` can run (`pouw_executor.rs:12–16` header: live trigger is P4 "when the DA transport actually delivers bytes"); the verifier needs DA to sample+reconstruct before it can Commit/Reveal. Nothing pays out until real cross-node DA works. **DA is necessary but not sufficient** — it makes the other two possible; it does not by itself make jobs pay out (still NoQuorum until publisher + executor + verifier all land).

**Build order (maps agree):**
1. **DA substrate (inert):** blob store → `da_protocol.rs` codec → publisher. All new/additive, single-node/in-process testable against `BridgeTransport`+`spawn_backend`.
2. **DA activation (PROTECTED):** event_loop correlation maps + `Da` swarm arm + `CommpBehaviour` field; main.rs spawns. This makes cross-node fetch real.
3. **Executor loop:** planner (inert) → PROTECTED tick + `spawn_blocking` + `result_hash` arm. Testable against in-process DA before the real backend exists.
4. **Verifier loop:** planner + durable salt store (inert) → PROTECTED `select!` arm. Downstream of the executor (needs `CompleteJob` to draw committees).

**INERT / additive vs coordinated flip — and the genesis-reset question:**

- **NO coordinated flip, NO genesis reset, NO atomic enablement is required for pay-out to switch on.** This is the key difference from THE POUW FLIP (which had to land B2–B4 + PROTECTED Phase B atomically because any subset created reachable non-conserved state). Here the entire on-chain surface is ALREADY consensus-active and merged; the actors are **pure clients** submitting txs the live arms already accept. Adding them changes **NO consensus rule, state-root layout, borsh schema, or genesis param** (`phase12-wiring-spec.md:287` "do not change consensus"). A buggy actor tx is a **deterministic apply-error**: dropped from a producer's block by `select_applicable_txs` (1.2-MEMPOOL) and rejected by peers if it slips in → wastes a fee, **cannot fork**, no state smear (P1 rollback).
- The DA outcome is **never hashed into consensus** (`da_transport.rs:9,20`; facade never feeds a consensus value) → no fork risk from the DA layer either. `CF_DA_CHUNKS` is auto-created + off-consensus. The `/commputer/da/1` protocol negotiates support like sync/consensus already do — old nodes simply don't speak it and are treated as non-providers. It deploys as an **ordinary node-software upgrade**.
- **All three actors ship dark behind off-by-default config flags** (`[executor] enabled=false`, `[verifier] enabled=false`, DA opt-in). A node that never enables them is **byte-identical** to today. Enabling a loop with no DA backend / no committee simply yields the current conserved default (Abstain → NoQuorum → D2 zero-comp refund).
- **The only non-additive friction is the PROTECTED footprint** — the substance is all in new modules, but the DA wire-in touches PROTECTED files (`event_loop.rs` correlation maps + new arms, `main.rs` spawns) **more heavily than the rest of the flip** (whose PROTECTED footprint was deliberately minimized, `wiring-spec.md:68`). This is the founder-gated cost, not a consensus flip.

**Net:** functionally this is a **coordinated capability rollout** (pay-out needs a critical mass of committee members running the verifier loop + a publisher + DA together), not a hard fork. The money path is already merged and refund-safe either way.

---

## §4 — TEST STRATEGY

**Per-loop UNIT (pure planners, no async):**
- **Executor:** `plan_executor_actions(state,me,policy)` table tests — open affordable pending job → `ClaimJob`; job already in `job_lifecycles` → none (double-claim guard mirrors `state.rs:1853`); non-validator `me` → none; `height>claim_by` → none; my lifecycle `AwaitingResult` + unset `executor_hash` + `height≤result_by` → resume `CompleteJob`; `e_bond>balance` → skip. `execute_job` determinism + linchpin already covered (`pouw_executor.rs:94–114`).
- **Verifier:** back DA with `spawn_backend` over a shared `ChunkStore` so `verify_available → Available`; emit a `Commit` ONLY when `committee.contains(me) && Committing && !committed`, `bond==verifier_bond`, `commit==make_commitment(me,result_hash,salt)`; then a matching `Reveal` that `reveal_matches`. Negative guards: nothing when off-committee / wrong phase / past window (each an apply-Err); must wait one block after `CompleteJob` (same-block Commit sees empty committee, `wiring-spec` line 155).
- **DA:** `DaCodec` round-trip + oversized/truncated/dropped-stream rejection (reuse `sync_protocol.rs` test template :211–319 + the 10MiB cap + bounded reader). Blob-store put/get/has/GC. Publisher parity: `build_attestation` → persist → `has_chunk`/`fetch_chunk` agree with `LocalDiskTransport` golden (`da/transport.rs:224–248`). Backend contract re-run: dead-backend → Abstain (:316–332), parked → timeout → None (:294–313), dropped-reply → None (:272–291).

**IDEMPOTENCY / RESTART (safety-critical — must not double-claim / double-reveal):**
- Executor: kill+reload `ChainState` mid-lifecycle (claimed, `CompleteJob` not landed) → reloaded planner re-scans `job_lifecycles` (`executor==me && phase==AwaitingResult && executor_hash` unset) and resumes `CompleteJob` **exactly once**; a claimed job absent from `pending_jobs` is never re-claimed.
- Verifier: after committing, re-run ticks → no duplicate `Commit` (reads on-chain commitments via `to_record`); restart → reload salt → still reveals; **LOST salt → loop must NOT emit a garbage reveal** (it abstains, accepting the forfeiture rather than a slashable mismatch).

**pouw-e2e / `world.rs` harness (the synchronous reference for the choreography):**
- The existing `world.rs run_lifecycle:206` (`gate_pool → claim → commit/reveal`) and the manual full-Confirmed drive at `state.rs:8918+` (b8 committee-draw) + P8-driver settle tests at :7942+ already prove the on-chain sequence pays out. The actor test asserts **the ACTORS PRODUCE that same tx sequence across blocks** (claim@h1, complete@h2, commits@h3, advance, reveals@h4, settle@reveal_by+1) and that `Terminal==Confirmed`, `worker_paid==85%·budget`, `escrowed_for_job()==0`.
- Extend `world.rs` with a variant driving `verify_available` through the real `BridgeTransport` against a 2-swarm (in-process or 2-process) DA network → assert `Available` + exact reconstruction + the golden values (`world.rs:329–337`: worker_paid=3366, verifiers_paid=396, burned=198, conserved).
- Plug the ACTUAL node verifier closure (`execute_job + make_commitment`) into a `scenarios.rs` variant to prove the loop's re-execution == the frozen `honest_reveal`.
- Reuse the B10 cross-boundary `run_on_both` equivalence tests (storage `#[cfg(test)]`, `state.rs:4794+`, Escalate :4923) to pin actor-driven settlement to the audited `EscrowLedger` reference.

**THE HEADLINE GATE — LIVE MULTINODE PAY-OUT (the first end-to-end economic proof):**
Extend the §1.4 3-node testnet (the 56-finalization harness) / `scripts/multinode_smoke.sh` with a **shared in-process or loopback DA store**. Drive: `Bond` (register the actor wallet as a bonded validator) → `SubmitJobV2` (escrow) → executor loop emits `ClaimJob` (opens lifecycle) → node A **publishes the job's coded chunks + advertises** → executor loop fetches bytes + `execute_job` → `CompleteJob` (committee drawn in the block tail) → the drawn committee's verifier loops each sample over `/commputer/da/1`, re-execute, `Commit`+`Reveal` → `settle_due_jobs`. **Assert the pot PAYS OUT rather than refunds:** worker balance **+85%·budget**, submitter **NOT** refunded, `total_burned` **+5%·budget**, all bonds returned, `escrow_by_job` empty, and identical `compute_state_root` across all nodes. Split roles across nodes (one executor, ≥2 verifiers) to actually reach quorum(k)=2. **A negative control (no verifier actors) must show the NoQuorum→Escalate refund path.** Conservation on every path: `total_supply` unchanged + `escrowed_for_job()==0` after settle.

**Environment caveat:** the live multinode pay-out gate CANNOT be verified in the current environment (no live multi-node, `production-plan.md:282`) — it needs a real/loopback multi-node testbed. Shipping unverified networked code would violate "verify end-to-end"; the DA + verifier + executor loops must be proven on a loopback testbed before public alpha.

---

## §5 — RISKS & OPEN QUESTIONS

**Risks:**
- **Consensus stall from on-loop WASM.** `execute_job` is CPU-heavy; running it on the event-loop thread stalls consensus. MUST be `spawn_blocking` + mpsc + new `select!` arm — **NOT `block_in_place`** (repo memory: `block_in_place` doesn't unblock other `select!` arms).
- **DA-loop DEADLOCK.** The verifier/executor loop calls `BridgeTransport` which blocks on replies the event_loop produces; on the event_loop thread it deadlocks. Must run on a dedicated OS thread / `spawn_blocking` (single-swarm-owner at `event_loop.rs:725`).
- **Determinism slash.** Divergent `WasmLimits` between executor and committee verifiers → divergent `result_hash` → the HONEST executor is slashed (spurious Disputed). Source consensus-anchored limits (`consensus_params.rs:48`), never `default()`. All nodes must have byte-identical B8 genesis params (refuse_to_bind).
- **Lost-salt forfeiture** (verifier funds): crash between Commit and Reveal burns an honest bond (`lifecycle.rs:625–631`). **fsync the salt record BEFORE broadcasting the Commit.**
- **Capital drain** (executor): auto-claim escrows `max(budget,executor_bond)` per job (`state.rs:1878,:1903); a greedy/buggy policy locks the whole balance. Cap concurrent claims + min-balance reserve + affordability check before `ClaimJob`.
- **Nonce / duplicate-tx races.** PoUW txs share the node wallet nonce with `ValidatorRegister`/RPC emitters; out-of-order `ClaimJob`/`CompleteJob`/`Commit`/`Reveal` reject. A single nonce owner + a per-job in-flight set are required.
- **Claim race / job-id malleability.** First `ClaimJob` wins (:1853 guard); losers waste a fee. `job_id = tx.hash()` with memo/timelock outside the signed payload (`state.rs:1546–1551`) → a relayer can shift job_id pre-inclusion — **fund-safe** (refundable via `expire_pending_job`) but the executor must key off the on-chain `pending_jobs` entry, never a client-predicted id, and tolerate a claim race.
- **Committee/verifier liveness.** Fewer than quorum(k)=2-of-3 drawn members running the verifier loop → EVERY job NoQuorum-refunds. Payout needs a critical mass of verifier-running bonded validators. A drawn-but-underfunded verifier (`balance < verifier_bond` or `< fee`) should skip-and-log, not emit an unpayable tx.
- **Same-block race.** A Commit landing in the `CompleteJob` block sees an empty committee → apply-Err; `select_applicable_txs` self-corrects but a naive loop re-broadcasting every tick wastes fees / spams. Producer==verifier: its own Commit must be in a block where phase is already Committing and survive its own soundness filter.
- **DA DoS / retention.** Serving arbitrary chunk_hashes turns nodes into free storage. Blob store must be scoped to chunks for known/active jobs (`job_lifecycles`/`pending_jobs`), size-capped, TTL'd to settlement, with an explicit max-message cap (reuse sync's 10MiB + bomb-safe reader).
- **Kademlia MemoryStore non-persistence** (`transport.rs:146,263`): a node forgets provider records on restart → re-advertise on startup + republish (`providers.rs PROVIDER_REPUBLISH_TICKS=22h`), or use the v1 connected-peers approach to sidestep DHT provider records.
- **Blob size envelope:** 64KiB × ≤256 coded chunks ≈ 16MiB program data per job replicated across the responsible set; × concurrent jobs = real disk + bandwidth. Needs sizing + GC before public testnet.
- **PROTECTED-file weight.** The DA event_loop wire-in is NOT a thin call (new arm + 2 correlation maps + new swarm arm + main.rs spawns) — the highest protected footprint of the three actors. Keep all substance in new modules.
- **Necessary-not-sufficient.** DA alone still yields NoQuorum until publisher + executor + verifier all land.

**Open questions (FOUNDER DECISIONS needed before building):**
1. **INPUT distribution (the single biggest blocker to real re-execution, undefined anywhere in code):** does `da_root` cover the PROGRAM only (as in `world.rs`) or a `program‖input` blob? If program-only, where does the executor/verifier get input bytes — a second DA anchor, inline in the tx, or a separate fetch? Execution cannot run without input.
2. **`WasmLimits` source:** is `ConsensusParams.wasm_limits` threaded onto `ChainState` yet (as `game_params`/`resolution_params` are per B1b/B8), or must executor+verifier read genesis independently? Determinism-critical — needs a single authoritative source.
3. **Provider discovery:** Kademlia DHT provider records (start_providing/get_providers) vs a simpler v1 returning connected peers / the validator set (relies on Merkle+rebind to reject wrong holders — avoids DHT persistence + `ProviderId↔PeerId` complexity for a small testnet)?
4. **Canonical PUBLISHER:** submitter at `SubmitJobV2` (holds bytes + computed `da_root`), executor at Claim/Complete, or both (+ repair daemon over the responsible K-set)? How do program bytes first enter the DA network at all (SubmitJobV2 carries only `da_root`)? Does the executor also publish the RESULT, or only the program (verifiers re-execute)?
5. **Blob store backend + retention:** new `CF_DA_CHUNKS` RocksDB CF (transactional with the rest of storage) vs a filesystem dir (`LocalDiskTransport` encoding)? Who pins (submitter / executor / responsible K-set) and until when (settlement? `PROVIDER_RECORD_TTL_TICKS=48h`)?
6. **`ProviderId↔PeerId` mapping** (`params.rs:51` vs multihash): derive `ProviderId=hash(PeerId)`, carry PeerId in a side table, or redefine?
7. **Salt-store location/format** and its interaction with the protected boundary: a new file under `data_dir` (like `mempool.json`) or a RocksDB CF? Must be node-local, durable, fsync-before-broadcast.
8. **Role/config policy:** which nodes run executor, which run verifier, both? Config-flag names + inert-by-default policy; auto-enable only when the node is a bonded validator?
9. **Eligibility bootstrap:** does an actor auto-`Bond`+`ValidatorRegister` on startup, or assume the operator pre-bonded? (ClaimJob/Commit/Reveal all require `is_validator`; candidacy requires `is_validator + Compliant + bonded≥min_bond`, `state.rs:1888–1897`.)
10. **Claim policy:** claim anything affordable, only jobs we produced, or randomized backoff (to avoid the claim race + wasted fees)? `PendingJobRecord` drops resources/max_duration (`state.rs:3251–3252`), so the executor cannot resource-fit from on-chain data alone — is that acceptable, or is a side channel needed?
11. **Mempool policy:** should actor txs bypass the per-account quota / fee floor (protocol-driven) or pay normal fees?
12. **Exact post-apply hook** in `event_loop.rs` where the just-applied `ChainState` is available to snapshot — founder confirms the location.
13. **DA config knobs:** `config.rs` (PROTECTED) vs a new non-protected DA config module?
14. **Shared vs separate `DaCommand` backend** for executor + verifier (one swarm-DA task servicing both) — affects main.rs threading.
15. **`da_root → fetchable-chunks` facade signature** the executor/verifier call — confirm the interface the DA mapper exposes.

---

## §6 — EFFORT / PHASING (approve incrementally)

Modeled on Track 1's phasing (Phase 0 = additive substrate that builds+tests clean on its own; later phases = the PROTECTED wire-in). Each phase's non-protected work is agent-buildable now; each PROTECTED phase is a separate founder approval.

- **Phase 0 — DA additive substrate [non-protected, buildable now, INERT].** Blob-store module (`CF_DA_CHUNKS` or FS dir; put/get/has/GC + caps) · `network/src/da_protocol.rs` codec (mirror `sync_protocol.rs`) · publisher module (`build_attestation` → persist → advertise). Unit + single-node tests against `LocalDiskTransport`/`spawn_backend`. Tree builds clean; nothing is wired live. **Gate:** answers to open-Q 1 (input), 3 (discovery), 4 (publisher), 5 (store/retention), 6 (ProviderId).
- **Phase 1 — Executor + Verifier planners + salt store [non-protected, buildable now, INERT].** `plan_executor_actions` + `plan_verifier_actions` pure planners · the durable fsync-before-broadcast salt store · the `spawn_blocking` re-execution shim threading consensus `WasmLimits`. Full unit + idempotency/restart tests (no async). **Gate:** answers to open-Q 2 (WasmLimits), 7 (salt store), 9 (bootstrap), 10 (claim policy).
- **Phase 2 — DA activation [PROTECTED, founder].** `CommpBehaviour` `da` field (non-protected edit to existing `transport.rs`) + `event_loop.rs` `da_command_rx` arm + `pending_find`/`pending_fetch` correlation maps + `...::Da` swarm arm + main.rs blob store + channel + off-thread spawn. Re-run the `da_transport.rs` contract tests against the real backend. Makes cross-node fetch real.
- **Phase 3 — Executor loop wire-in [PROTECTED, founder].** `executor_tick_interval` + `handle_executor_tick` + `spawn_blocking` + `result_hash` mpsc arm + `ClaimJob`/`CompleteJob` emit; `main.rs` spawn + `config.rs` `[executor]` flag.
- **Phase 4 — Verifier loop wire-in [PROTECTED, founder].** One `drive_pouw_verifier()` `select!` arm + `main.rs` DA-handle thread/spawn + `config.rs` `[verifier]` flag.
- **Phase 5 — LIVE MULTINODE PAY-OUT gate [testbed].** The §4 headline test on a loopback 3-node testnet: a real `SubmitJobV2` reaches `Confirmed` and pays worker+verifiers, with the no-verifier negative control refunding. This is the first end-to-end economic proof and the true acceptance gate; it CANNOT run in the current environment (needs a real/loopback multi-node testbed).

**Phasing note:** Phases 0–1 are fully agent-stageable and testable now (in-process DA) and carry **zero** consensus/PROTECTED risk. Phases 2–4 are the founder-gated PROTECTED wire-ins; approve per phase. No genesis reset and no coordinated atomic flip is required at any phase — the money path is already merged; Track 2 only turns on the clients that drive it.
