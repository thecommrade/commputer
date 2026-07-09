# Commputer Track-2 — PoUW Actor Wire-In: FINAL APPLICATION PLAN (Phases 2–4)

**Date:** 2026-07-09
**Branch context:** `agent-testnet-20260707` (post-merge of THE POUW FLIP; the on-chain PoUW surface is consensus-active + merged). Phase 0–1 inert substrate is **built + committed at `c37e379`** (`da_store`, `da_publisher`, `salt_store`, `executor_planner`, `verifier_planner` in `src/node/src`; `da_protocol` in `src/network/src`; `DaCommand`/`BridgeTransport` in `commputer_pouw_onchain::da_transport`).
**Purpose:** the apply-ready plan that wires the inert Phase 0–1 DA + planner substrate into the running node so PoUW pots **PAY OUT** (Confirmed → worker 85% / verifiers 10% / burn 5%) instead of the current REFUND default. This is the executable successor to the scoping plan `2026-07-09-track2-pouw-actors-plan.md`.
**Provenance:** the five region-maps in `p234_maps.json` (map workflow `wf_f01df178-5ca`: R1 DA activation, R2 executor loop, R3 verifier loop, R4 submit_job RPC, R5 meta) + the 3-lens review in `p234_reviews.json` (deadlock/liveness · determinism/nonce/funds · protected-minimality/additive) + **this finalize pass**, which re-read the real tree at `c37e379` to verify every load-bearing anchor. Where a raw hunk conflicts with a review blocker/major, **§0 overrides the raw hunk.**
**Additive-safety envelope:** no consensus rule, state-root layout, borsh schema, or genesis param changes; DA output is **never hashed into consensus**; all three actors ship dark behind off-by-default config flags. **NO genesis reset. NO atomic coordinated flip.** A node with the flags off is byte-identical to today on-chain (one wire-surface caveat: the always-on `/commputer/da/1` protocol advertise — see §0/P8-note and §5).
**PROTECTED (founder-only):** `src/node/src/{main,event_loop,config}.rs`, root `genesis.json`/`*.toml`, `src/core/src/token.rs`. The frozen `src/staging/pouw/` and `commputer_pouw` are NEVER modified.

**STATUS: reviewed, awaiting founder approval; nothing applied yet.** This document modifies no source. It creates only itself.

**HOW TO EXECUTE:** Apply **§1 (Phase A, non-protected)** first — an agent may build + test these on `agent-*` now; they compile standalone and are inert. Then the founder approves and applies **§2 (Phase B, protected)** region-by-region in the order of **§3**. Every §0 correction is *already folded* into the code shown in §1/§2 — the raw `p234_maps.json` hunks are superseded wherever they differ. Do not apply raw hunks mechanically; apply the reconciled sets here.

---

## §0 — BINDING CORRECTIONS (override the raw hunks)

Every review blocker and major is turned into a numbered correction below. The code in §1/§2 already reflects these; this section is the authority for *why* the reconciled code differs from `p234_maps.json`.

### P1 — [BLOCKER] R2 executor loop won't compile: moves non-Copy `WasmLimits` out of `&mut self` (E0507)

**Verified at `c37e379`:** `WasmLimits` derives `#[derive(Clone, Debug, PartialEq, Eq)]` — **NOT Copy** (`src/staging/pouw/src/wasm/limits.rs:22`). `reexecute(program_hash, input_hash, program_bytes, input, limits: WasmLimits)` takes `limits` **by value** (`src/node/src/executor_planner.rs:228–236`). The mapped `ExecutorLoop::process(&mut self, …)` calls `reexecute(c.program_hash, c.input_hash, program, input, self.wasm_limits)` (executor_loop.rs new-file line ~276) — moving `self.wasm_limits` out of a `&mut self` borrow → **E0507**, whole crate fails to build. The `NoDa` unit test never reaches this line (fetcher returns `None`), so tests pass while `cargo build` fails.

**Resolution (binding):** clone at the call site — `reexecute(c.program_hash, c.input_hash, program, input, self.wasm_limits.clone())`. `WasmLimits` is a ~6-scalar POD-ish struct; the clone is per-claim and cheap, and is the **same compiled value** either way (no determinism effect). R3's verifier loop already does `wasm_limits.clone()` (verifier_loop.rs line ~212) — make R2 match.

### P2 — [BLOCKER] `emit_*` uses `kind` after moving it into the `Transaction` literal (E0382)

**Verified:** `TxKind` derives `#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]` — **NOT Copy** (`src/core/src/transaction.rs:15–16`). In the mapped `emit_executor_action`, `kind` is moved into `Transaction { …, kind, … }` and then read again by `debug!("Emitted executor {:?} …", kind, …)` → **E0382**. (`tx` is also already moved by `pending_txs.push(tx)`.)

**Resolution (binding):** compute a static tag from `&kind` **before** the move and log the tag. `ExecutorAction` *is* Copy (`executor_planner.rs:43`), so the original raw-hunk alternative "log `action`" is valid for the executor-only path — but P4 unifies both loops onto one `emit_actor_tx(kind: TxKind)`, so the canonical fix is the tag-before-move shown in §2 (`emit_actor_tx`). Never read `kind` or `tx` after the struct-literal move.

### P3 — [BLOCKER] R3 salt not durable before `Commit` on fsync failure → honest bond burned

**Verified:** `SaltStore::insert` mutates the **in-memory map first**, then persists — `self.entries.insert(job_id, (result_hash, salt)); self.persist()` (`src/node/src/salt_store.rs:79–80`). `persist()` fsyncs and propagates the error (correctly, as of `c37e379`). The mapped verifier loop does `if let Err(e) = salts.insert(job_id, rh, salt) { tracing::error!("…NOT committing…") }` and then **falls through** to `plan_verifier_actions(now_height, &snap, &salts)` (verifier_loop.rs line ~263), which reads `salts.get(&job_id)` — the **in-memory entry `insert` already set** even though `persist()` returned `Err`. So on any fsync failure (ENOSPC / EIO / read-only fs) the loop STILL emits the `Commit`; if the node then crashes the never-persisted salt is gone on restart → no `Reveal` → bond **burned** as commit-no-reveal forfeiture (`lifecycle.rs:625–631`). The log says "NOT committing" but the code commits. This is exactly the fund loss the two-step contract exists to prevent.

**Resolution (binding):** on `insert` error, **clear the phantom in-memory entry before the planner reads it** — inside the `Err` branch add `let _ = salts.remove(&job_id);` (`SaltStore::remove` drops the in-memory entry at `salt_store.rs:93` before its own persist, which is what `plan_verifier_actions` reads). The verifier MUST NOT emit a `Commit` whose salt is not durably fsynced. (`salt_store.rs` already fsyncs-before-returning; the loop must *honor* the `Err` — this correction makes it do so.) See §1 verifier_loop.rs step (3).

### P4 — [MAJOR] Two divergent actor-tx return channels → ONE shared sink

**Verified conflict:** R2 wires `executor_action_rx: Option<UnboundedReceiver<ExecutorAction>>` + `exec_recv` future + `Some(action) = exec_recv => self.emit_executor_action(action)`; R3 instead assumes a **shared** `self.actor_tx_rx.recv()` arm carrying `TxKind`, drained by `emit_actor_tx`, plus `event_loop.actor_tx_tx` — fields/methods R2 never creates. Applying both yields two parallel unbounded channels, two `select!` arms, and two copies of the single-nonce-owner build/sign/gossip logic that must stay in lockstep.

**Resolution (binding):** **ONE** shared channel. Both off-thread loops emit a nonce-free `commputer_core::transaction::TxKind` over one `tokio::sync::mpsc::UnboundedSender<TxKind>` (`actor_tx_tx`, held at `run_node` scope, cloned into each loop). The event loop owns the single `actor_tx_rx: Option<UnboundedReceiver<TxKind>>` and drains it in ONE arm → ONE `emit_actor_tx(kind)` (the sole nonce owner + sign/gossip/admit path). **Delete** `executor_action_rx`, `emit_executor_action`, `exec_recv`; the executor loop's `run` maps `ExecutorAction → TxKind::ClaimJob/CompleteJob` locally before `send` (non-protected change, §1). The verifier already emits `TxKind::Commit/Reveal` over the same sender. This is defined once in §2 (event_loop `emit_actor_tx` + the `actor_recv` arm + `attach_actor_tx`).

### P5 — [MAJOR] The R1/R2/R3/R4 `main.rs` hunks do not compose → ONE reconciled `run_node` section

**Verified conflicts:** (a) R1 builds `da_cmd_tx`/`da_store` **inside** `if da_enabled { … let _ = (&da_cmd_tx,&da_store); }` — both drop at block-end (and the `da-cmd-pump` thread's `recv()` immediately errs → pump exits). (b) R2's spawn references `da_command_tx` (different name) + `attestations` (never constructed). (c) R3's spawn references `da_cmd_tx` (third name), `_node_config.verifier` (but R2 renames `_node_config → node_config` at line 890, so R3 then references an undefined binding), and `event_loop.actor_tx_tx` (never created). (d) R4 independently builds its own `(da_store, da_command_tx, da_command_rx)` and calls `event_loop.attach_da(da_store, da_command_rx)` with a **std** receiver in **reversed arg order** vs R1's `attach_da(da_command_rx: tokio::UnboundedReceiver, da_store: Arc)`.

**Resolution (binding):** ONE reconciled `run_node` section (§2 main.rs) that, at `run_node` scope (not block-scoped):
1. renames `let _node_config` → `let node_config` **once** (config is read for the enable flags);
2. computes `da_enabled` from config (§0/P12 config) — DA is needed by *either* loop, so `da_enabled = node_config.da.enabled || run_executor || run_verifier`;
3. when `da_enabled`: opens ONE `da_store: Arc<DaStore>`, creates ONE `std::sync::mpsc` `(da_cmd_tx, da_cmd_std_rx)` **and** ONE `tokio::sync::mpsc::unbounded` `(da_cmd_tok_tx, da_cmd_tok_rx)`, and spawns the **`da-cmd-pump`** thread forwarding std→tokio (the event loop's `select!` awaits the tokio receiver; `BridgeTransport` speaks std::sync::mpsc). `da_cmd_tx` (the **std** Sender) is the shared handle;
4. clones `da_store` + `da_cmd_tx` into `RpcState` (R4);
5. calls `event_loop.attach_da(da_cmd_tok_rx, da_store.clone())` — **R1 arg order, tokio receiver** (R4's std-receiver call is superseded, §0/P13);
6. creates the shared actor-tx channel `(actor_tx_tx, actor_tx_rx)` and `event_loop.attach_actor_tx(actor_tx_rx)`, holding `actor_tx_tx` for both loops;
7. conditionally spawns the executor and/or verifier loops on **dedicated OS threads**, each cloning `da_cmd_tx` (into a `BridgeTransport::with_timeout`) + `actor_tx_tx`, sharing the ONE DA backend (Q14).

`AttestationSource`: no real `da_root → DaAttestation` resolver exists yet (open-Q15), so **both** loops ship `NoAttestationSource` → `verify_available` Abstains → jobs refund. This is honest and inert: label R2/R3 **production-INERT** until attestation distribution lands (§6, §5). The reconciled section wires the mechanism now so only the resolver remains.

### P6 — [MAJOR] Deconflict identical PROTECTED anchors → ONE merged set per anchor

**Verified collisions (all against the pristine tree, so a verbatim sequential apply breaks after the first insert):**
- `event_loop.rs` struct tail `pub health_monitor: ChainHealthMonitor,\n}` (line 224) — R1 adds 5 DA fields, R3 adds `verifier_snapshot_tx`; R2 adds its fields at a *different* anchor (after `epoch_finalize_rx`, line 132).
- `EventLoop::new` tail `health_monitor: ChainHealthMonitor::new(),` (line ~297) — R1 + R3 both init here; R2 inits at line 252.
- After `attach_rpc` (line 310) — R1 `attach_da` + R2 `attach_executor` + R3 `push_verifier_snapshot` all anchor here.
- `run()` before `tokio::select! {` (line 724) — R1 `da_recv` + R2 `exec_recv` both add a local future.
- `select!` after the `rpc_recv`/`epoch_finalize_rx` arms — R1 `da_recv` arm + R2 `exec_recv` arm + R3 shared-actor arm.
- Both apply Ok-arms — `try_apply_finalized` after the mempool-prune loop (line 3307–3311) and `apply_synced_block` after `self.process_orphans(hash);` (line 3409) — R2 `push_executor_snapshot` + R3 `push_verifier_snapshot` both insert.
- `config.rs` NodeConfig field tail (`pub cors_origins: String,\n}`, line 27) + Default tail (`cors_origins: "*".to_string(),`, line 42) — R2 adds `executor`, R3 adds `verifier`, R5 adds all three (`executor`/`verifier`/`da`).

**Resolution (binding):** present the PROTECTED edits as **ONE merged block per anchor** (§2), not colliding per-region hunks. Concretely: **one** consolidated Track-2 struct-field block after `health_monitor` holding all 8 fields (moving R2's `executor_snapshot_tx` from the line-132 anchor to the tail so there is a single field anchor); **one** `EventLoop::new` init block; **one** appended methods block after `attach_rpc` (and one after `auto_register_validator`); **one** `run()` local-futures block (`da_recv` + `actor_recv`); **one** merged `select!` arm insertion; **one** two-line insertion at *each* of the two apply Ok-arms (`self.push_executor_snapshot(); self.push_verifier_snapshot();`); and the R5 canonical **three-table** config (P12). Re-anchor each region against the *post-previous-region* state, never the base commit.

### P7 — [MAJOR] Verifier loop must coalesce backlogged snapshots to the latest

**Verified:** the executor loop drains to newest — `while let Ok(view) = snapshot_rx.recv() { let mut view = view; while let Ok(newer) = snapshot_rx.try_recv() { view = newer; } … }` (executor_loop.rs lines ~345–349) — but the mapped verifier loop processes **every** tick with no drain: `while let Ok(tick) = snapshot_rx.recv() { … }` (verifier_loop.rs line ~178). Each tick can block the loop thread ~one DA retry window (30 s) plus WASM re-exec; a verifier on several committees falls progressively behind, the unbounded `std::sync::mpsc` grows, and it acts on an ever-staler `now_height`, emitting `Commit`/`Reveal` for closed windows (wasted `MINIMUM_FEE`, rejected at apply). Each tick is a full re-scan, so coalescing to the latest loses no membership and is strictly safer.

**Resolution (binding):** mirror the executor — bind `while let Ok(mut tick) = snapshot_rx.recv() { while let Ok(newer) = snapshot_rx.try_recv() { tick = newer; } … }` at the top of `run_verifier_loop`, before any work. See §1 verifier_loop.rs.

### P8 — [MAJOR] Rate-limit the inbound DA `GetChunk` serve path

**Verified:** the mapped `CommpBehaviourEvent::Da` `RrMessage::Request` arm serves every `GetChunk` via `self.da_store.get(chunk_hash)` = an `fs::read` of up to `MAX_ENCODED_CHUNK` (~68 KiB, `da_store.rs:131`) **inline on the swarm-owner event_loop task, with no rate limiter and no auth** on `/commputer/da/1`. The two existing inbound request-response handlers gate per-peer for exactly this reason — sync at `event_loop.rs:1531/1542` (`self.sync_rate_limiter.check(commputer::peer_hash::peer_bucket_tagged(&peer, N))`) and consensus at `:1625/:1672`. On a DA-enabled node any connected peer can stream `GetChunk` to pin the swarm thread in serial disk reads, delaying gossip/consensus/finalization — the same liveness/DoS the other handlers already guard.

**Resolution (binding):** rate-limit the DA `Request` the same way as sync. Capture `peer` from `RrEvent::Message { peer, message, .. }` (the raw hunk drops it) and gate with the **existing** `self.sync_rate_limiter` under a **distinct bucket tag `2`** (`peer_bucket_tagged(&peer, 2)` — tags `0`/`1` are sync GetBlock/GetBlocks; a distinct tag means DA serve can never starve block sync). Over-limit → immediate `send_response(channel, DaResponse::Chunk(None))` (cheap, never a ban). Reusing `sync_rate_limiter` (vs a new `da_rate_limiter` field) keeps the PROTECTED struct footprint minimal. See §2 event_loop Da arm.

*Note (wire-surface caveat, folds a minor):* the `da` behaviour is registered in `transport.rs` **unconditionally**, so even a DA-disabled node advertises `/commputer/da/1` in `identify` and answers `GetChunk` with `Chunk(None)` (`da_store` is `None`). This is inert and consistent with sync/consensus always-on, but do **not** claim byte-identical *on the wire* — only on-chain/on-state. Residual founder decision in §5.

### P9 — [MAJOR] Dynamic validator-enable: re-evaluate as the node becomes bonded after startup

**Verified:** R3's spawn gate is evaluated **once at startup** — `let verifier_enabled = _node_config.verifier.enabled && accounts.get(addr).map(|a| a.is_validator).unwrap_or(false);` — and only then is the loop spawned + `verifier_snapshot_tx` set. But a fresh node is **not** `is_validator` until its `ValidatorRegister` lands on-chain, which `auto_register_validator` broadcasts *after* startup (`main.rs:1203`) and takes several blocks to apply. So for the normal validator-join flow `verifier_enabled` is false at boot and the loop is **never spawned**, even though the node later becomes a bonded validator drawn onto committees. R2's executor is correct: it spawns whenever `executor.enabled` and gates **per-block** inside `push_executor_snapshot` (`is_validator && is_eligible`).

**Resolution (binding):** **drop the startup `is_validator` conjunct** for both loops. Spawn whenever the config says to (`cfg.enabled || cfg.auto_enable_when_bonded`, P12); the per-block snapshot hook is the runtime gate — `push_executor_snapshot` no-ops unless `is_validator && self.state.is_eligible(&me)`, and `push_verifier_snapshot` no-ops unless `build_verifier_views` is non-empty (i.e. the node is actually drawn onto a committee). This auto-activates the moment the node's `ValidatorRegister` applies, with no thread re-spawn — satisfying the founder's "auto-enable when bonded" decision *and* this major. (Byte-identical caveat: with `auto_enable_when_bonded=true` a node spawns two idle loop threads + opens the DA store; it emits no tx and makes no consensus/state change until bonded. Strict byte-identical requires all flags false — see §5.)

### P10 — [minor, folded] Unify actor-tx admission through `validate_tx_for_mempool`

The mapped `emit_executor_action` bypasses `validate_tx_for_mempool` (skips the F-3 per-account quota + the C7 unknown-job ingress filter) while `emit_actor_tx` routes through it — an asymmetry. **Resolution:** the single `emit_actor_tx` (P4) routes through `validate_tx_for_mempool` exactly like `handle_rpc_transaction` (`event_loop.rs:2182`): `validate → seen_tx_hashes.insert → gossipsub.publish → mempool_added_at.insert → pending_txs.push → enforce_mempool_limit`. This also subsumes the manual dedup guard (validate rejects `seen_tx_hashes` duplicates) and applies the same fee floor. Actor txs pay normal `MINIMUM_FEE` (Q11); they target known jobs so the C7 filter passes.

### P11 — [minor, folded] Per-call DA fetch timeout must be well below the retry window

The mapped loops set `BridgeTransport::with_timeout(30s)` equal to `DA_RETRY_WINDOW_MS = 30_000`. Because `verify_available` checks `now_tick() > deadline` between calls (`facade.rs:44,88`), a single slow bridge call that consumes the full 30 s per-call timeout also exhausts the entire per-job DA budget → effectively **one** fetch attempt before Abstain. **Resolution:** set the per-call bound well below the retry window — `DaConfig.fetch_timeout_ms` default **5_000** (§0/P12), retry window 30 s — so multiple providers/chunks are tried within one job's budget. Degrades to Abstain either way (safe); this only improves DA liveness under a partially-slow backend.

### P12 — [minor, folded] Reconcile the divergent config shapes → adopt R5's canonical three tables

R2 defines `ExecutorConfig { enabled, max_concurrent, min_reserve }`; R5 defines `ExecutorConfig { enabled, auto_enable_when_bonded, max_concurrent_claims, min_balance_reserve }` plus `VerifierConfig` and `DaConfig`. R2's main.rs reads `node_config.executor.max_concurrent`/`.min_reserve`. **Resolution:** adopt **R5's canonical three tables** (`ExecutorConfig`/`VerifierConfig`/`DaConfig`, each `#[serde(default)]`, all `enabled=false`, executor/verifier `auto_enable_when_bonded=true`, `DaConfig { enabled=false, fetch_timeout_ms=5_000 }`) as the single config surface (§2 config.rs), and update the main.rs spawns to the canonical field names (`max_concurrent_claims`, `min_balance_reserve`, `fetch_timeout_ms`). This deconflicts the colliding config anchors of P6 too.

### P13 — [minor, folded] `attach_da` signature + R4/R1 DA-construction reconciliation

R1's `attach_da(da_command_rx: tokio::UnboundedReceiver<DaCommand>, da_store: Arc<DaStore>)` awaits a **tokio** receiver in the `da_recv` arm; R4 calls `attach_da(da_store, da_command_rx)` with a **std** receiver in reversed order. **Resolution:** keep R1's signature/arm (tokio receiver, fed by the `da-cmd-pump`); the reconciled main.rs (P5) calls `attach_da(da_cmd_tok_rx, da_store.clone())`. R4 keeps only its RpcState field additions + the `submit_job` handler (both non-protected) and the RpcState-literal field init; R4's own DA-construction + `attach_da` call are superseded by the P5 section.

### P14 — [minor, folded] Verifier restart-liveness: repopulate `results` from a stored salt

The verifier's step 1 skips re-execution when `salts.get(&v.job_id).is_some()`, but step 2 reads `my_result_hash` from the in-memory `results` cache, which is empty after a restart. A node that persisted a salt but crashed before its `Commit` applied would then never commit that job (it abstains) — a liveness gap (not fund loss). **Resolution:** in step 1, when a salt exists but `results` lacks the job, repopulate `results.insert(job_id, stored_result_hash)` from the salt store's `(result_hash, salt)` pair, so a resumed node can still commit within the window. See §1 verifier_loop.rs step (1).

*Deferred (not folded; risk-registered in §6):* loop-thread panic supervision (a panic silently kills PoUW participation with no restart); first-result-only Kademlia `GetProviders` reply (v1 simplification); `da_provider_ids`/`pending_*` pruning on `ConnectionClosed` (bounded, alpha-acceptable); the Q3 tension between "v1 connected-peers discovery" and the mapped Kademlia `start_providing`/`get_providers` path (the reconciled code ships the Kademlia path; a connected-peers `FindProviders` is a documented alternative).

---

## §1 — PHASE A: NON-PROTECTED PRE-STAGES (agent-buildable + testable now)

These are the only genuinely non-protected surfaces. Each compiles standalone and is inert (does nothing until a PROTECTED §2 spawn/field feeds it). The PROTECTED companions (the `da-cmd-pump`, `da_provider_tag`, the `emit_actor_tx` sink, the channel plumbing) live in §2 because they touch `main.rs`/`event_loop.rs`.

### A1 — `src/node/src/executor_loop.rs` (NEW) + `pub mod executor_loop;` in `lib.rs` (after `pub mod da_publisher;`, line 26)

Off-thread driver: `run(cfg, wasm_limits, fetcher, snapshot_rx, action_tx)` blocks on a `std::sync::mpsc::Receiver<ExecutorChainView>`, coalesces to the newest view, DA-fetches + re-executes each claim, and emits nonce-free `TxKind` over the shared sender. Compiles standalone (all deps are committed at `c37e379`); inert because nothing constructs/spawns it until §2/B2. **Two corrections vs the map:**

**(P1) clone the limits at the re-exec call:**
```rust
// executor_loop.rs, in ExecutorLoop::process(&mut self, view, action_tx):
match reexecute(c.program_hash, c.input_hash, program, input, self.wasm_limits.clone()) {
    Ok(result_hash) => { self.results.insert(c.job_id, result_hash); }
    Err(_e) => { /* garbled bytes → retry next block (linchpin sha256==program_hash rejects) */ }
}
```

**(P4) emit `TxKind` (not `ExecutorAction`) over the shared sender** — change `run`'s `action_tx` to `tokio::sync::mpsc::UnboundedSender<commputer_core::transaction::TxKind>` and map inside `process`, still tracking in-flight by `job_id` (both variants Copy):
```rust
for a in plan_executor_actions(view.height, &snap) {
    match a {
        ExecutorAction::Claim { job_id }    => { self.in_flight_claims.insert(job_id); }
        ExecutorAction::Complete { job_id, .. } => { self.in_flight_completes.insert(job_id); }
    }
    let kind = match a {
        ExecutorAction::Claim { job_id } => commputer_core::transaction::TxKind::ClaimJob { job_id },
        ExecutorAction::Complete { job_id, result_hash } =>
            commputer_core::transaction::TxKind::CompleteJob { job_id, result_hash },
    };
    if action_tx.send(kind).is_err() { return Err(LoopGone); } // event loop dropped the receiver
}
```
The snapshot-coalescing loop (`while let Ok(view) = snapshot_rx.recv() { let mut view = view; while let Ok(newer) = snapshot_rx.try_recv() { view = newer; } … }`, lines ~345–353) is already correct — keep it. `AttestationSource`/`BridgeBlobFetcher`/`ExecutorChainView`/`build_chain_view` are as mapped. WASM runs on **this** dedicated thread, never the event loop.

### A2 — `src/node/src/verifier_loop.rs` (NEW) + `pub mod verifier_loop;` in `lib.rs` (after `pub mod verifier_planner;`, line 23)

Off-thread commit/reveal driver: `run_verifier_loop(snapshot_rx, actor_tx_tx, bridge, salts, wasm_limits, cfg, attestations)`. Emits `TxKind::Commit/Reveal` over the shared `actor_tx_tx` (P4-aligned already). Compiles standalone; inert. **Three corrections vs the map:**

**(P7) coalesce backlogged ticks to the newest at loop top:**
```rust
while let Ok(mut tick) = snapshot_rx.recv() {
    while let Ok(newer) = snapshot_rx.try_recv() { tick = newer; } // work only the latest applied state
    let me = tick.my_address.0;
    // …
```

**(P14) restart-liveness — repopulate `results` from a persisted salt in step (1):**
```rust
if results.contains_key(&v.job_id) { continue; }
if let Some((stored_rh, _salt)) = salts.get(&v.job_id) {
    results.insert(v.job_id, stored_rh); // resumed node: recover my result_hash so it can still Reveal/Commit
    continue;
}
// …else re-execute via DA + reexecute(…, wasm_limits.clone())…
```

**(P3) salt durability — clear the phantom in-memory entry on fsync failure, step (3):**
```rust
for job_id in jobs_needing_salt(tick.now_height, &snap, &salts) {
    let Some(rh) = results.get(&job_id).copied() else { continue };
    let salt: [u8; 32] = rand::random(); // node-local CSPRNG; the ONLY randomness (never a consensus input)
    if let Err(e) = salts.insert(job_id, rh, salt) {
        // P3: insert() set the in-memory entry BEFORE persist() failed — remove it so the planner
        // cannot see a salt that isn't on disk, else we'd emit a Commit we can never Reveal → burned bond.
        let _ = salts.remove(&job_id);
        tracing::error!("verifier: salt fsync failed for job {} — NOT committing: {}", hex8(&job_id), e);
    }
}
```
Step (2) already `reexecute(…, wasm_limits.clone())` (correct); the per-`(job,kind)` re-emit cooldown and the `phase == Settled` salt GC are as mapped.

### A3 — `src/node/src/rpc.rs` submit_job handler (NON-protected, R4 hunks 0/1/2)

Three non-protected edits to `rpc.rs`:
1. Two `RpcState` fields after `faucet_next_nonce` (~line 139): `pub da_store: Option<std::sync::Arc<commputer::da_store::DaStore>>` and `pub da_command_tx: Option<std::sync::mpsc::Sender<DaCommand>>` (`std::sync::mpsc::Sender` is `Sync` on rustc 1.94 — no `Mutex`).
2. The `POST /submit_job` handler + `SubmitJobRequest`/`SubmitJobResponse` + `MAX_JOB_BLOB_BYTES` + `submit_job_err`: hex-decode + size-cap (`128 * DEFAULT_CHUNK_SIZE - 4`) + `budget >= MIN_JOB_BUDGET`; bind `program_hash`/`input_hash`; derive the **submitter** wallet from `submitter_seed` and **zeroize** it; `spawn_blocking(publish_job_blob)` to persist the 2N coded chunks; `Advertise` every `live_chunk_hashes(&att)` over `da_command_tx`; build+sign `SubmitJobV2 { da_root, … }` with the submitter key; `state.tx_sender.try_send(tx)`.
3. Route registration in the PUBLIC tier with `DefaultBodyLimit::max(32 MiB)`.

**Inert/standalone:** when `da_store`/`da_command_tx` are `None` (DA off) the handler returns an honest `503` and touches no disk/swarm. It compiles as soon as the two `Option` fields exist; it does not *function* until §2/main.rs threads `Some(...)` (compile-coupling, §3). **Founder decisions surfaced (not resolved here):** the submitter's seed transits to the node it targets (loopback / own host behind D3 TLS only); the route is PUBLIC for parity with `POST /tx` but the founder may gate it behind the admin key or a loopback-only bind (§5).

### A4 — `src/network/src/transport.rs` `da` behaviour field (NON-protected, founder-gated edit to existing; R1 hunks 0/1/2)

Add `pub da: libp2p::request_response::Behaviour<crate::da_protocol::DaCodec>` to `CommpBehaviour` (after `consensus`), construct `let da = crate::da_protocol::da_behaviour();` in the `with_behaviour` closure, and add `da,` to the struct literal. The `CommpBehaviourEvent::Da` variant is auto-derived by `NetworkBehaviour`. `da_protocol.rs` (the length-prefixed, 10 MiB-capped, bomb-safe `DaCodec`) is already committed at `c37e379`. **Wire-surface caveat (P8-note):** registered unconditionally → the node advertises `/commputer/da/1` even when DA is disabled and answers `GetChunk` with `Chunk(None)`. Inert, but not byte-identical *on the wire*.

**Phase A build gate:** `cargo build -p commputer-network` and `-p commputer` clean; `cargo test -p commputer-network` (DA codec round-trip / oversized / truncated / dropped-stream) and `-p commputer` (executor_loop/verifier_loop unit + idempotency/restart, salt store fsync/remove, submit_job shape) green. No PROTECTED file touched.

---

## §2 — PHASE B: PROTECTED HUNKS (founder-gated), reconciled per file

Presented as ONE deconflicted set per file, corrections folded. Grouped for **region-by-region approval**: **B1 config flags** · **B2 DA activation** · **B3 executor wire** · **B4 verifier wire**. (B2–B4 all live in `event_loop.rs` + `main.rs`; the founder approves the grouping, then the merged file edits land together per §3.)

### B1 — `src/node/src/config.rs` (canonical three tables, P12)

One merged struct-field block after `pub cors_origins: String,` (line 27):
```rust
    pub cors_origins: String,
    /// Track-2 PoUW executor auto-claim loop (OFF by default).
    pub executor: ExecutorConfig,
    /// Track-2 PoUW verifier commit/reveal loop (OFF by default).
    pub verifier: VerifierConfig,
    /// Track-2 DA backend — serve/publish coded chunks over /commputer/da/1 (OFF by default).
    pub da: DaConfig,
}
```
One merged `Default` block after `cors_origins: "*".to_string(),` (line 42): `executor: ExecutorConfig::default(), verifier: VerifierConfig::default(), da: DaConfig::default(),`. One appended impls block between `impl Default for NodeConfig` and `impl NodeConfig` (line 45):
```rust
#[derive(Debug, Clone, Deserialize)] #[serde(default)]
pub struct ExecutorConfig { pub enabled: bool, pub auto_enable_when_bonded: bool,
    pub max_concurrent_claims: usize, pub min_balance_reserve: u64 }
impl Default for ExecutorConfig { fn default() -> Self {
    Self { enabled: false, auto_enable_when_bonded: true, max_concurrent_claims: 4, min_balance_reserve: 0 } } }

#[derive(Debug, Clone, Deserialize)] #[serde(default)]
pub struct VerifierConfig { pub enabled: bool, pub auto_enable_when_bonded: bool, pub min_balance_reserve: u64 }
impl Default for VerifierConfig { fn default() -> Self {
    Self { enabled: false, auto_enable_when_bonded: true, min_balance_reserve: 0 } } }

#[derive(Debug, Clone, Deserialize)] #[serde(default)]
pub struct DaConfig { pub enabled: bool, pub fetch_timeout_ms: u64 } // fetch_timeout_ms: BridgeTransport per-call bound (P11)
impl Default for DaConfig { fn default() -> Self { Self { enabled: false, fetch_timeout_ms: 5_000 } } }
```

### B2 — `src/node/src/event_loop.rs` — merged Track-2 surface

**(a) Struct fields — ONE block after `pub health_monitor: ChainHealthMonitor,` (line 224):**
```rust
    // ── Track-2 (Phases 2–4): all OFF/parked unless main.rs attaches them → byte-identical on-chain when off ──
    pub da_command_rx: Option<tokio::sync::mpsc::UnboundedReceiver<commputer_pouw_onchain::da_transport::DaCommand>>,
    pub da_store: Option<Arc<commputer::da_store::DaStore>>,
    pub pending_find: HashMap<libp2p::kad::QueryId, std::sync::mpsc::Sender<Vec<commputer_da::params::ProviderId>>>,
    pub pending_fetch: HashMap<libp2p::request_response::OutboundRequestId,
        std::sync::mpsc::Sender<Option<(Vec<u8>, commputer_da::transport::MerklePath)>>>,
    pub da_provider_ids: HashMap<[u8; 32], libp2p::PeerId>,       // reversible ProviderId(tag) → PeerId
    pub executor_snapshot_tx: Option<std::sync::mpsc::Sender<commputer::executor_loop::ExecutorChainView>>,
    pub verifier_snapshot_tx: Option<std::sync::mpsc::Sender<commputer::verifier_loop::VerifierTick>>,
    pub actor_tx_rx: Option<tokio::sync::mpsc::UnboundedReceiver<commputer_core::transaction::TxKind>>, // P4: ONE sink
}
```
**(b) `EventLoop::new` — ONE init block after `health_monitor: ChainHealthMonitor::new(),` (line ~297):** all `None` / `HashMap::new()` for the 8 fields above.

**(c) Methods after `attach_rpc` (line 310):** append `attach_da`, `attach_actor_tx`, `push_verifier_snapshot`:
```rust
    pub fn attach_da(&mut self,
        da_command_rx: tokio::sync::mpsc::UnboundedReceiver<commputer_pouw_onchain::da_transport::DaCommand>,
        da_store: Arc<commputer::da_store::DaStore>) {
        self.da_command_rx = Some(da_command_rx); self.da_store = Some(da_store);
    }
    pub fn attach_actor_tx(&mut self, rx: tokio::sync::mpsc::UnboundedReceiver<commputer_core::transaction::TxKind>) {
        self.actor_tx_rx = Some(rx); // P4: the single shared executor+verifier tx receiver
    }
    pub fn attach_executor(&mut self, snapshot_tx: std::sync::mpsc::Sender<commputer::executor_loop::ExecutorChainView>) {
        self.executor_snapshot_tx = Some(snapshot_tx); // action_rx unified into attach_actor_tx (P4)
    }
    fn push_verifier_snapshot(&self) { /* R3: no-op unless verifier_snapshot_tx is Some AND build_verifier_views non-empty (P9 gate) */ }
```
(`push_executor_snapshot` + `emit_actor_tx` are appended after `auto_register_validator`, item (g).)

**(d) `run()` local futures before `tokio::select! {` (line 724):**
```rust
    let da_recv = async {
        if let Some(ref mut rx) = self.da_command_rx { rx.recv().await }
        else { std::future::pending::<Option<commputer_pouw_onchain::da_transport::DaCommand>>().await }
    };
    let actor_recv = async {
        if let Some(ref mut rx) = self.actor_tx_rx { rx.recv().await }
        else { std::future::pending::<Option<commputer_core::transaction::TxKind>>().await }
    };
```
**(e) `select!` arms (after the `rpc_recv` and `epoch_finalize_rx` arms):**
```rust
                Some(kind) = actor_recv => { self.emit_actor_tx(kind); }          // P4: single nonce owner
                Some(cmd)  = da_recv    => { self.handle_da_command(cmd); }        // R1: DA backend
```
**(f) Both post-apply Ok-arms — merged two-line insertion (open-Q12, §5):**
- `try_apply_finalized` after the mempool-prune loop (`event_loop.rs:3307–3311`):
- `apply_synced_block` after `self.process_orphans(hash);` (`event_loop.rs:3409`):
```rust
                    self.push_executor_snapshot();   // R2
                    self.push_verifier_snapshot();   // R3
```
**(g) Methods after `auto_register_validator` (line 2550) — `emit_actor_tx` (P2+P4+P10) + `push_executor_snapshot`:**
```rust
    /// P4/P10 (PROTECTED): the SINGLE actor-tx sink. Both loops emit nonce-free TxKind here; this — the
    /// sole wallet-nonce owner — assigns nonce, signs, and admits via the SAME path as an RPC tx.
    fn emit_actor_tx(&mut self, kind: commputer_core::transaction::TxKind) {
        use commputer_core::transaction::{Transaction, TxKind, MINIMUM_FEE};
        let me = *self.wallet.address();
        // P2: capture a static tag from &kind BEFORE it moves into the literal (TxKind is not Copy → E0382).
        let tag: &'static str = match &kind {
            TxKind::ClaimJob { .. } => "ClaimJob", TxKind::CompleteJob { .. } => "CompleteJob",
            TxKind::Commit { .. } => "Commit", TxKind::Reveal { .. } => "Reveal", _ => "actor-tx",
        };
        let base = self.state.accounts.get(&me).map(|a| a.nonce).unwrap_or(0);
        let pending = self.pending_txs.iter().filter(|t| t.from == me).count() as u64;
        let nonce = base.saturating_add(pending);
        let mut tx = Transaction { from: me, nonce, kind, fee: MINIMUM_FEE,
            signature: vec![], public_key: vec![], memo: None, timelock: None };
        commputer_core::signing::sign_transaction(&mut tx, &self.wallet);
        if let Err(reason) = self.validate_tx_for_mempool(&tx) {   // P10: F-3 quota + C7 ingress + dedup
            debug!("actor tx {} rejected pre-mempool: {}", tag, reason); return;
        }
        let tx_hash = tx.hash();
        self.seen_tx_hashes.insert(tx_hash);
        if let Ok(data) = serde_json::to_vec(&tx) {
            let _ = self.network.swarm.behaviour_mut().gossipsub
                .publish(topics::tx_topic(), commputer_network::compress(&data));
        }
        self.mempool_added_at.insert(tx_hash, std::time::Instant::now());
        self.pending_txs.push(tx);
        self.enforce_mempool_limit();
        debug!("Emitted actor {} (nonce {})", tag, nonce);
    }
    /// R2 (PROTECTED): push an executor snapshot post-apply. P9 gate — act ONLY as a bonded, eligible validator.
    fn push_executor_snapshot(&self) {
        let Some(ref tx) = self.executor_snapshot_tx else { return };
        let me = *self.wallet.address();
        let is_validator = self.state.accounts.get(&me).map(|a| a.is_validator).unwrap_or(false);
        if !is_validator || !self.state.is_eligible(&me) { return; }   // runtime gate (auto-enable when bonded)
        let my_balance = self.state.accounts.get(&me).map(|a| a.balance.raw()).unwrap_or(0);
        let view = commputer::executor_loop::build_chain_view(self.state.blocks.height(),
            self.state.current_epoch, me, my_balance, &self.state.pending_jobs, &self.state.job_lifecycles);
        let _ = tx.send(view);
    }
```
**(h) Methods before `handle_rpc_transaction` (line ~2182) — R1 `handle_da_command` + `da_provider_tag`** (as mapped): `Advertise → kademlia.start_providing`; `HasChunk →` local `da_store.has`; `FindProviders → kademlia.get_providers` + stash `reply` in `pending_find`; `FetchChunk → da.send_request` (resolve `PeerId` from `da_provider_ids`) + stash in `pending_fetch`, else drop `reply` → Abstain.

**(i) Kademlia arm (R1 hunks 9/10):** capture `id` in `OutboundQueryProgressed { id, result, .. }`; add a `QueryResult::GetProviders` branch that, on the first result, tags each `PeerId` (`da_provider_tag`), records `da_provider_ids[tag] = peer`, pushes `ProviderId(tag)`, and `reply.send(out)`.

**(j) `CommpBehaviourEvent::Da` swarm arm (R1 hunk 11) WITH the P8 rate-limit:**
```rust
            SwarmEvent::Behaviour(CommpBehaviourEvent::Da(event)) => {
                use libp2p::request_response::{Event as RrEvent, Message as RrMessage};
                use commputer_network::da_protocol::{DaRequest, DaResponse};
                match event {
                    RrEvent::Message { peer, message } => match message {          // P8: bind `peer`
                        RrMessage::Request { request, channel, .. } => {
                            let DaRequest::GetChunk { chunk_hash } = request;
                            // P8: gate inbound serve on the existing sync limiter, distinct bucket tag 2,
                            // so a GetChunk flood can't pin the swarm thread / starve block sync.
                            let chunk = if self.sync_rate_limiter
                                .check(commputer::peer_hash::peer_bucket_tagged(&peer, 2)) {
                                self.da_store.as_ref().and_then(|s| s.get(chunk_hash).ok().flatten())
                            } else { None };
                            let _ = self.network.swarm.behaviour_mut().da
                                .send_response(channel, DaResponse::Chunk(chunk));
                        }
                        RrMessage::Response { request_id, response } => {
                            if let Some(reply) = self.pending_fetch.remove(&request_id) {
                                let DaResponse::Chunk(opt) = response;
                                let out = opt.and_then(|c| commputer::da_publisher::deserialize_merkle_path(&c.merkle_path)
                                    .map(|path| (c.bytes, path)));
                                let _ = reply.send(out);
                            }
                        }
                    },
                    RrEvent::OutboundFailure { request_id, .. } => { self.pending_fetch.remove(&request_id); } // → Abstain
                    RrEvent::InboundFailure { .. } | RrEvent::ResponseSent { .. } => {}
                }
            }
```

### B3 + B4 — `src/node/src/main.rs` — ONE reconciled `run_node` section (P5/P9/P12/P13)

```rust
    // (line 890) P5: read config for the enable flags (renamed from `_node_config`).
    let node_config = config::NodeConfig::load();
    // …
    // ── after faucet provisioning (~line 1158), BEFORE `let rpc_state = Arc::new(RpcState { … })` ──
    let (faucet_wallet, faucet_next_nonce) = rpc::provision_faucet_from_env(&state)?;

    // P9/P12: spawn a loop when explicitly enabled OR auto-enable-when-bonded; the per-block snapshot hook
    // is the runtime gate (no-ops until this node is a bonded validator / drawn onto a committee).
    let run_executor = node_config.executor.enabled || node_config.executor.auto_enable_when_bonded;
    let run_verifier = node_config.verifier.enabled || node_config.verifier.auto_enable_when_bonded;
    // DA is needed by either loop; open ONE store + ONE command backend (Q14) and pump std→tokio (R1/P5/P13).
    let da_enabled = node_config.da.enabled || run_executor || run_verifier;
    let (da_store, da_cmd_tx, da_cmd_tok_rx) = if da_enabled {
        use commputer_pouw_onchain::da_transport::DaCommand;
        let store = std::sync::Arc::new(commputer::da_store::DaStore::open(data_dir(testnet).join("da_chunks"))?);
        let (std_tx, std_rx) = std::sync::mpsc::channel::<DaCommand>();
        let (tok_tx, tok_rx) = tokio::sync::mpsc::unbounded_channel::<DaCommand>();
        std::thread::Builder::new().name("da-cmd-pump".into())
            .spawn(move || { while let Ok(c) = std_rx.recv() { if tok_tx.send(c).is_err() { break; } } })
            .expect("spawn da-cmd-pump");
        (Some(store), Some(std_tx), Some(tok_rx))
    } else { (None, None, None) };

    let rpc_state = std::sync::Arc::new(rpc::RpcState {
        // … existing fields …
        faucet_wallet,
        faucet_next_nonce: tokio::sync::Mutex::new(faucet_next_nonce),
        da_store: da_store.clone(),          // R4: /submit_job publisher + inbound serve share ONE store
        da_command_tx: da_cmd_tx.clone(),    // R4
    });

    let mut event_loop = EventLoop::new(state, wallet, network, hardware);
    event_loop.attach_rpc(tx_receiver, rpc_state.clone());
    // P5/P13: hand the tokio receiver (from the pump) + the store to the swarm-owner. R1 arg order.
    if let (Some(rx), Some(store)) = (da_cmd_tok_rx, da_store.clone()) { event_loop.attach_da(rx, store); }
    event_loop.auto_register_validator(contribution_percent);

    // P4: the ONE shared actor-tx channel; the event loop owns the receiver, both loops clone the sender.
    let (actor_tx_tx, actor_tx_rx) =
        tokio::sync::mpsc::unbounded_channel::<commputer_core::transaction::TxKind>();
    event_loop.attach_actor_tx(actor_tx_rx);

    // ── B3: executor loop (dedicated OS thread — BridgeTransport blocks on replies the event loop makes) ──
    if run_executor {
        // DETERMINISM (P1): WasmLimits stay at the COMPILED default; refuse_to_bind already asserts it
        // matches genesis network-wide (consensus_params.rs:43-46). The single authoritative source.
        let exec_wasm_limits = commputer_pouw::wasm::WasmLimits::default();
        let executor_bond = /* captured from state.game_params.executor_bond BEFORE `state` moved */ executor_bond;
        let (snap_tx, snap_rx) = std::sync::mpsc::channel();
        event_loop.attach_executor(snap_tx);
        let cfg = commputer::executor_planner::ExecutorCfg {
            max_concurrent_claims: node_config.executor.max_concurrent_claims,   // P12 names
            min_balance_reserve: node_config.executor.min_balance_reserve, executor_bond };
        let bridge = commputer_pouw_onchain::da_transport::BridgeTransport::with_timeout(
            da_cmd_tx.clone().expect("da enabled when a loop runs"),
            std::time::Duration::from_millis(node_config.da.fetch_timeout_ms));   // P11 per-call bound
        let fetcher = Box::new(commputer::executor_loop::BridgeBlobFetcher::new(
            bridge, exec_verifier_id,
            Box::new(commputer::executor_loop::NoAttestationSource))); // P5/open-Q15: inert until resolver lands
        let act = actor_tx_tx.clone();
        std::thread::Builder::new().name("pouw-executor".into())
            .spawn(move || { commputer::executor_loop::run(cfg, exec_wasm_limits, fetcher, snap_rx, act); })
            .expect("spawn pouw-executor");
    }

    // ── B4: verifier loop (dedicated OS thread) — P9: spawn on config, NOT a boot-time is_validator check ──
    if run_verifier {
        let (vsnap_tx, vsnap_rx) = std::sync::mpsc::channel();
        event_loop.verifier_snapshot_tx = Some(vsnap_tx);
        let salts = commputer::salt_store::SaltStore::open(data_dir(testnet)).expect("open verifier salt store");
        let bridge = commputer_pouw_onchain::da_transport::BridgeTransport::with_timeout(
            da_cmd_tx.clone().expect("da enabled when a loop runs"),
            std::time::Duration::from_millis(node_config.da.fetch_timeout_ms));   // P11
        let vcfg = commputer::verifier_planner::VerifierCfg {
            min_balance_reserve: node_config.verifier.min_balance_reserve };       // P12 names
        let wasm_limits = commputer_pouw::wasm::WasmLimits::default();             // MUST equal the executor's
        let attestations: Box<dyn commputer::verifier_loop::AttestationSource> =
            Box::new(commputer::verifier_loop::NoAttestationSource);               // open-Q15: inert until resolver
        let act = actor_tx_tx.clone();
        std::thread::Builder::new().name("pouw-verifier".into())
            .spawn(move || { commputer::verifier_loop::run_verifier_loop(
                vsnap_rx, act, bridge, salts, wasm_limits, vcfg, attestations); })
            .expect("spawn pouw-verifier");
    }
```
Notes: capture `executor_bond = state.game_params.executor_bond` and `exec_verifier_id = wallet.address().0` **before** `state`/`wallet` move into `EventLoop::new`. `NoAttestationSource` on both loops makes them Abstain (inert) until the `da_root → DaAttestation` resolver lands (open-Q15). `da-cmd-pump` holds no lock and exits cleanly when all `da_cmd_tx` senders drop or the tokio receiver is gone.

---

## §3 — COMPILE-COUPLING & APPLY ORDER

**Phase A first (non-protected, standalone-clean):**
1. `lib.rs` `pub mod executor_loop; pub mod verifier_loop;` + the two new files (A1, A2). Build `-p commputer`. *Coupling:* they reference `commputer::da_store`, `executor_planner`, `verifier_planner`, `salt_store` (all at `c37e379`) — clean now.
2. `network/src/transport.rs` `da` field (A4). Build `-p commputer-network`. Standalone (da_protocol committed).
3. `rpc.rs` submit_job + the two `RpcState` `Option` fields (A3). Build `-p commputer`. *Coupling:* the handler compiles once the two fields exist; it stays a `503` until main.rs supplies `Some(...)`.

**Phase B second (protected, per §2 groups), as coherent commits in this order — each names what will not build until its counterpart lands:**
- **B1 config** (config.rs) — standalone; unblocks the main.rs field reads in B3/B4.
- **B2 DA activation** (event_loop.rs items (a)(b)(c-`attach_da`)(d-`da_recv`)(e-`da_recv` arm)(h)(i)(j) + main.rs DA construction/`attach_da` + RpcState fields). *Coupling:* the event_loop `CommpBehaviourEvent::Da` arm (j) **will not build** until A4's `da` behaviour field exists (the `Da` variant is derive-generated from it); the RpcState DA fields (A3) **must be populated** by the main.rs constructor here or `submit_job` stays 503; the `da-cmd-pump`↔`attach_da` tokio receiver pairing (P13) must land together.
- **B3 executor wire** (event_loop.rs `executor_snapshot_tx`/`attach_executor`/`push_executor_snapshot` + the shared `actor_tx_rx`/`emit_actor_tx`/`actor_recv` arm/`attach_actor_tx` + both apply-hook pushes + main.rs executor spawn). *Coupling:* the main.rs `executor_loop::run(...)` spawn **will not build** until A1 exists; `emit_actor_tx`/`actor_tx_rx` are shared with B4 (land the shared sink once, in whichever of B3/B4 lands first).
- **B4 verifier wire** (event_loop.rs `verifier_snapshot_tx`/`push_verifier_snapshot` + main.rs verifier spawn + salt store). *Coupling:* the main.rs `verifier_loop::run_verifier_loop(...)` spawn **will not build** until A2 exists; reuses the shared `actor_tx_tx`/`emit_actor_tx` from B3.

**Whole-workspace build clean is the per-commit gate.** Because B2/B3/B4 co-edit `event_loop.rs` + `main.rs`, apply them as the merged sets in §2 (re-anchored against the previous set, P6), not as raw sequential hunks.

---

## §4 — VERIFICATION GATE

**Baselines stay green (confirm with `cargo test`):** network **92**, node-lib **125** (plus storage / core / pouw-onchain baselines from the last multinode gate). No PROTECTED behavior changes when all flags are off (on-chain byte-identical; wire delta = the always-on `/commputer/da/1` advertise only).

**Unit / in-process (Phase A, runnable now):**
- executor_loop / verifier_loop against an **in-process `DaTransport`** (`spawn_backend` over a shared `ChunkStore`, so `verify_available → Available`): executor emits exactly one `ClaimJob` for an affordable open job, `Complete` on resume, nothing for non-validator/expired/double-claim; verifier emits `Commit` only when `committee.contains(me) && Committing && !committed` with `bond == verifier_bond` and `commit == make_commitment(me, result_hash, salt)`, then a matching `Reveal`.
- **Corrections regression:** (P1) `cargo build -p commputer` compiles the re-exec call — a guard test that constructs `ExecutorLoop` and calls `process` through the DA path (not just `NoDa`); (P3) inject a `SaltStore` whose `persist` fails and assert **no `Commit` is emitted** (the `remove` clears the phantom); (P7) push N backlogged ticks and assert the verifier acts only on the latest `now_height`; (P14) reload a `SaltStore` with a stored salt and assert the resumed loop repopulates `results` and can still `Commit`/`Reveal`.
- DA codec round-trip + oversized/truncated/dropped-stream rejection (reuse `sync_protocol.rs` test template); `da_store` put/get/has/gc; `da_publisher` parity vs `LocalDiskTransport` golden; `da_transport` backend contract (dead→Abstain, parked→timeout→None, dropped-reply→None).

**pouw-e2e / `world.rs` parity:** drive `verify_available` through the real `BridgeTransport` against a 2-swarm (in-process) DA network → assert `Available` + exact reconstruction + the golden values (`world.rs:329–337`: worker_paid=3366, verifiers_paid=396, burned=198, conserved). Plug the actual node verifier closure (`reexecute + make_commitment`) into a `scenarios.rs` variant to prove the loop's re-execution equals the frozen `honest_reveal`. Reuse the B10 `run_on_both` equivalence to pin actor-driven settlement to the audited `EscrowLedger`.

**THE HEADLINE — LIVE loopback 3-node PAY-OUT (the true acceptance gate):** on a real/loopback multi-node testbed (roles split: 1 executor, ≥2 verifiers to reach quorum k=2-of-3), drive a real `POST /submit_job` → the submitter's `Bond` (bonded validator) → executor `ClaimJob` (opens lifecycle) → node A publishes+advertises the coded chunks → executor DA-fetches + `execute_job` → `CompleteJob` (committee drawn in the block tail) → each drawn verifier samples over `/commputer/da/1`, re-executes, `Commit`+`Reveal` → `settle_due_jobs`. **Assert it PAYS OUT, not refunds:** worker balance **+85%·budget**, submitter **NOT** refunded, `total_burned` **+5%·budget**, all bonds returned, `escrow_by_job` empty, `total_supply` unchanged, identical `compute_state_root` across all nodes. **Negative control (no verifier actors):** must show the NoQuorum→Escalate refund path (conserved).

**Environment caveat:** the live pay-out gate **cannot run in the current environment** (no live multi-node). It needs a real/loopback multi-node testbed and MUST pass before public alpha — shipping unverified networked code would violate "verify end-to-end." Until then R2/R3 are honestly labeled production-INERT (both loops Abstain via `NoAttestationSource`, open-Q15). **Prerequisite for a *paying* headline run:** the `da_root → DaAttestation` resolver (open-Q15) must land; with `NoAttestationSource` every job still refunds even fully wired.

---

## §5 — FOUNDER APPROVAL CHECKLIST + RESIDUAL OPEN QUESTIONS

**Region-by-region PROTECTED approvals (each a separate GO):**
- [ ] **B1 config.rs** — the three `#[serde(default)]` tables (`[executor]`/`[verifier]`/`[da]`), all `enabled=false`, executor/verifier `auto_enable_when_bonded=true`, `da.fetch_timeout_ms=5_000`. **Decision:** should `auto_enable_when_bonded` default `true` (auto-activate on bond; spawns idle loop threads + opens the DA store on every node) or `false` (strict byte-identical until explicitly enabled)? (P9 requires: whichever, the *runtime* gate is the per-block hook, not a boot-time `is_validator` check.)
- [ ] **B2 DA activation** — event_loop DA fields/arm/correlation-maps/Kademlia+Da swarm arms (incl. the P8 serve rate-limit) + main.rs `da_store`/`da-cmd-pump`/`da_cmd_tx` + `attach_da` (P13 tokio receiver) + RpcState DA fields. Confirm Q14 (ONE DaCommand backend for both loops) and Q3 (mapped Kademlia `get_providers`/`start_providing` path vs the founder's "v1 connected-peers discovery").
- [ ] **B3 executor wire** — the shared `emit_actor_tx`/`actor_tx_rx` sink (P4/P10), `push_executor_snapshot` (P9 gate), both apply-hook pushes, main.rs executor spawn (P1 WasmLimits default, P11 timeout). Confirm Q11 (actor txs pay `MINIMUM_FEE`).
- [ ] **B4 verifier wire** — `verifier_snapshot_tx`/`push_verifier_snapshot`, main.rs verifier spawn + salt store, P3/P7/P14 corrections. Confirm the salt-store location (`data_dir(testnet)`, node-local, fsync-before-broadcast) and the auto-enable-when-bonded eligibility strictness (`is_validator` vs bonded≥min_bond + Compliant).
- [ ] **A3/A4 non-protected but founder-gated edits** — `rpc.rs` `submit_job` route tier (PUBLIC vs admin-gated vs loopback-only; it accepts a submitter seed + consumes DA disk) and the `transport.rs` `da` behaviour field.

**Residual open questions (must be settled; several gate a *paying* run):**
1. **Q12 — exact post-apply snapshot hook location.** This plan places `push_executor_snapshot()` + `push_verifier_snapshot()` at **both** apply Ok-arms: `try_apply_finalized` after the mempool-prune loop (`event_loop.rs:3307–3311`) and `apply_synced_block` after `self.process_orphans(hash)` (`:3409`). R5's grep confirms **exactly two** `apply_block_validated` call sites cover every apply path — **founder confirm** these two sites, and treat "any new block-apply path also needs this call" as a review invariant.
2. **Q15 (blocking for payout) — attestation distribution.** No code resolves a full `DaAttestation` (`program_id`, `n_data`, `n_total`, `data_len`) from a bare on-chain 32-byte `da_root`. Until it lands, both loops ship `NoAttestationSource` → Abstain → refund. Options: advertise the ~90-byte attestation over `/commputer/da/1` keyed by `da_root`; extend `SubmitJobV2`; or a side table.
3. **The always-on `/commputer/da/1` advertise delta** (P8-note): keep the behaviour always-registered (consistent with sync/consensus; the plan's choice) or gate registration on `da_enabled`? Affects the "byte-identical on the wire" claim.
4. **DA store size/retention defaults.** `da_store` backstops (per-chunk `MAX_ENCODED_CHUNK` ~68 KiB, ~4 GiB total cap, `gc()` scoped to live jobs) exist, but **no shown region calls `gc()`** — the publisher/submit path persists chunks and retention leans on the hard cap until a periodic-gc caller lands. Confirm who pins (submitter / executor / responsible K-set) and until when (settlement vs a provider TTL), and the concrete size/retention knobs (Q13: non-protected `da_store` consts vs config).
5. **Q2 — WasmLimits source.** This pass threads `WasmLimits::default()` (compiled default, pinned network-wide by `refuse_to_bind`). Confirm this is acceptable vs threading `ConsensusParams.wasm_limits` off the applied `ChainState`. Determinism-critical: a divergence slashes the honest executor.
6. **Q4 — submit_job signer** (chosen: submitter seed in the request body; node-wallet signing rejected to avoid nonce contention with the actor loops) and its auth tier (Q above).

---

## §6 — RISK REGISTER

- **DA-loop DEADLOCK (highest).** Both loops hold a `BridgeTransport` that blocks on replies the event_loop produces; the swarm is single-owner in the event_loop `select!` (`event_loop.rs:725`). *Mitigation:* loops run on **dedicated OS threads**, never tokio workers; the std→tokio `da-cmd-pump` bridges without holding a lock; `with_timeout` (P11) bounds every call so a stalled swarm degrades to Abstain, never a hang.
- **Consensus stall from on-loop WASM.** `execute_job`/`reexecute` is CPU-heavy. *Mitigation:* it runs on the loop's own thread (off the event loop) — **not** `block_in_place` (repo memory: it doesn't unblock other `select!` arms). The event-loop arms only do cheap build/sign/admit (`emit_actor_tx`) and cheap projection (`push_*_snapshot`).
- **Determinism slash.** Divergent `WasmLimits` between executor and committee verifiers → divergent `result_hash` → the honest executor is slashed (spurious Disputed). *Mitigation:* both loops source the **identical** compiled `WasmLimits::default()` (P1 clone is value-identical); `refuse_to_bind` pins it to genesis network-wide (`consensus_params.rs:43–46`). Q2 residual.
- **Nonce-owner collision.** Actor txs share the node-wallet nonce with `ValidatorRegister`/RPC emitters; out-of-order Claim/Complete/Commit/Reveal reject. *Mitigation:* the **single** `emit_actor_tx` sink (P4) is the sole nonce owner (`base + pending count`, sequential across a burst); the C2/C3 speculative-apply discards any stale duplicate before block inclusion. Per-block ceiling ~1 self-tx/sender (a known throughput limit, not a fund risk).
- **Capital drain (executor).** Auto-claim escrows `max(budget, executor_bond)` per job (`state.rs:1878,:1903`). *Mitigation:* `max_concurrent_claims` + `min_balance_reserve` + the planner's cumulative per-tick affordability check (`executor_planner.rs:177–211`).
- **Lost-salt forfeiture (verifier funds).** A crash between Commit and Reveal burns an honest bond (`lifecycle.rs:625–631`). *Mitigation (P3):* `SaltStore::insert` fsyncs before returning; the loop clears the phantom in-memory entry on fsync failure and NEVER commits a non-durable salt; a lost salt → abstain + accept forfeiture, never a slashable garbage reveal.
- **DA-DoS / retention.** Serving arbitrary chunk_hashes turns nodes into free storage (~16 MiB/job × concurrent jobs). *Mitigation:* per-chunk + total store caps + `gc()` scoped to live jobs (residual: no gc caller wired yet, §5/Q4); the P8 per-peer serve rate-limit stops a `GetChunk` flood from starving sync/consensus; the `da_protocol` 10 MiB cap + bomb-safe reader bound each message.
- **Committee/verifier liveness.** Fewer than quorum (2-of-3) drawn members running the verifier loop → **every** job NoQuorum-refunds. Pay-out needs a critical mass of verifier-running bonded validators; a drawn-but-underfunded verifier skips-and-logs, never emits an unpayable tx. This makes Track-2 a *coordinated capability rollout*, not a hard fork.
- **ProviderId↔PeerId + Kademlia MemoryStore non-persistence.** `da_provider_tag = sha256(PeerId)` with the reverse map `da_provider_ids` recovers a dialable peer from the frozen facade's opaque `ProviderId`; Kademlia `MemoryStore` forgets provider records on restart (`transport.rs:146`) → re-advertise on startup / republish (or the Q3 v1 connected-peers path sidesteps DHT persistence). `da_provider_ids`/`pending_*` are bounded (peer population; self-drain on reply/failure); prune on `ConnectionClosed` deferred to alpha.
- **Loop-thread panic (no supervisor).** A panic in a loop thread (WASM or salt path) silently kills that thread; the event loop keeps running and its snapshot sends start returning `Err` (ignored) — no hang, but the node silently stops PoUW participation. *Deferred:* catch/log + optional restart, or a health flag.
- **Necessary-not-sufficient / production-INERT.** DA + wiring alone still yields refunds until the publisher path + a verifier-running committee + the Q15 attestation resolver all land. With all flags off the node is byte-identical on-chain (wire caveat: the always-on `/commputer/da/1` advertise). The money path is already merged and refund-safe on every path.
