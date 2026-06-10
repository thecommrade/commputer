# Staging Wire-In Roadmap (A8-wirein-audit)

> Generated read-only on branch `agent-overnight-20260610`. No files were
> modified. Every file:line claim below was verified against the **current**
> code on the working tree (post-cbd4e75), not the context summary.
>
> **Audience:** the founder, draining the staging backlog after launch.
> **This file's final home:** `docs/integration/wirein_roadmap.md` (reference
> only, no runtime value — delete once the backlog is empty).
>
> **Supersedes** the older partial `wire_in_plan.md`
> (`worktree-agent-a6a9b43004222b606:src/staging/docs/wire_in_plan.md`), which
> only covered the 12 wave-1/2 deliverables and predates several landings.

---

## 0. How this map was built

```
git branch | grep worktree-agent      # 28 worktree-agent-* branches
# Each branch carries the SAME 8 common staging test files (the pre-existing
# main:src/staging base) PLUS exactly one distinct deliverable. The set-diff
# of each branch against that common base is the deliverable.
```

The 8 common files present in `main:src/staging` AND every branch (already
staged on main, NOT the subject of this roadmap):
`chain_health_monitor_tests.rs`, `config_validator_tests.rs`,
`consensus_protocol_tests.rs`, `consensus_rate_limiter_tests.rs`,
`eclipse_detector_tests.rs`, `leader_comprehensive_tests.rs`,
`node_state_comprehensive_tests.rs`, `sync_machine_comprehensive_tests.rs`.

---

## 1. Cross-cutting facts (verified this session)

| Fact | Verified at | Consequence |
|---|---|---|
| `proof_manager` is declared ONLY in the binary crate (`src/node/src/main.rs:15` `mod proof_manager;`); it is **NOT** in `src/node/src/lib.rs` (which stops at line 11). | `grep proof_manager src/node/src/lib.rs` → no hit | **Hard prereq** for T1.3 bench (and any external bench/test reaching `ProofManager`). Add `pub mod proof_manager;` to lib.rs. `main.rs` is PROTECTED but the lib.rs add is non-protected. |
| The hand-rolled Prometheus handler `get_prometheus_metrics` lives at `src/node/src/rpc.rs:474-516` (route bound at `rpc.rs:1177`). It renders only 7 gauges/counters via `format!`. | read rpc.rs:473-516 | **T3.2 REPLACE-not-ADD trap.** The metrics deliverable SUPERSEDES this handler — do not add a duplicate `/metrics/prometheus` route. |
| No `StorageError` type exists yet (only `StateError` at `src/storage/src/state.rs:1690`). `rocks.rs` has the panic accessor `fn cf` at `src/storage/src/rocks.rs:125-129` (`panic!("BUG: column family ...")`) with **21** `self.cf(...)` call sites + the definition. `thiserror` is already a storage dep (`src/storage/Cargo.toml:13`). | grep + read | T1.1 storage_error blueprint is accurate. |
| `SnowballVoter::record_round(&HashMap<BlockHash, usize>)` exists (`src/consensus/src/snowball.rs:108`); there is no per-peer ingest API. | read snowball.rs:42-108 | T1.4 edge tests + snowball_api_proposals are correctly scoped; PSEUDO tests are `#[ignore]`d pending the new API. |
| `CommpBehaviour` (`src/network/src/transport.rs:143-155`) already has `relay_client`, `dcutr`, `upnp`. **No `autonat`.** Workspace libp2p features (`src/Cargo.toml:41`) lack `"autonat"`. | read transport.rs + src/Cargo.toml | T2.1 autonat genuinely missing; T2.2 relay-SERVER missing (only client present); T2.3 upnp behaviour present but external-addr propagation missing. |
| The `event_loop.rs` UPnP arm (`src/node/src/event_loop.rs:1458-1473`) only `info!`-logs `NewExternalAddr` — it never calls `swarm.add_external_address(addr)`. | read event_loop.rs:1458 | T2.3 gap is REAL; wire-in target is PROTECTED. |
| `compute_epoch_verdicts` is a `pub fn` static helper at `src/node/src/proof_manager.rs:257`; `ProofVerifier::verify` at `src/proofs/src/verifier.rs:13`. | read | T1.2 proptests + T1.3 bench targets exist. |

---

## 2. ALREADY LANDED / SUPERSEDED — do NOT re-land (10 deliverables)

These staging deliverables are already in `main` (or their fix is). Verified.
Mark the branches for deletion after a glance.

| Deliverable | Source branch | Landed as | Evidence |
|---|---|---|---|
| W5.7 F-2 rate-limit TTL eviction (`rpc_rate_limit_eviction.rs`) | `worktree-agent-ae8724c4b6edd1653` | commit 984f08c | `rpc.rs:916` `MAX_RATE_LIMIT_ENTRIES`, sweep at `:937`, regression test `:1570` |
| W5.7 F-1 body-bomb blueprint (`docs/rpc_body_bomb_patch_blueprint.md`) | `worktree-agent-aaf8c8771610ad60d` | commit afec7df | `rpc.rs:1166` `DefaultBodyLimit::max(64*1024)`, test `:1508` |
| config_doctor crate | `worktree-agent-a47dd1f30197f8754` | `src/doctor/` (commputer-doctor) | identical check set: cloud_ip/genesis/ntp/port_reachability + mod |
| seed_node_kit | `worktree-agent-af0c84ee972d897d1` | `src/seed-node-kit/` | commit cf5a34c, binaries present |
| ADRs 0000-0005 (`docs/adrs/*`) | `worktree-agent-acc63a93a9bbd60ee` | `docs/adrs/` | same 7 files in main:docs/adrs |
| ops/ systemd kit (`ops/commputer.service` etc) | `worktree-agent-a00109aa97e21df99` | `deploy/commputer.service` | landed in deploy/ |
| docker/ (Dockerfile, compose) | `worktree-agent-ac99080151660b302` | top-level `Dockerfile` + `docker-compose.yml` | landed at repo root |
| Snowball scaling blueprint (`docs/snowball_scaling_patch_blueprint.md`) | `worktree-agent-a6ac448112efd18f9` | commit 4ae26ed | `consensus_manager.rs:200` `update_params_for_network_size`, (20,14,20) at peer_count>=21 |
| IPv6 anti-scale blueprint (`docs/ipv6_anti_scale_patch_blueprint.md`) | `worktree-agent-a1d23dc8ac5711a87` | landed | `compliance_check.rs:323` `is_datacenter_ipv6` + `IPV6_DC_PREFIXES` exist |
| Partial wire_in_plan.md (the wave-1/2 version) | `worktree-agent-a6a9b43004222b606` | superseded by THIS file | covered only 12 items, several now landed |

> **Caveat:** confirm each by a 30-second `git diff` against main before
> deleting the branch — landings were verified by symbol presence, not a
> byte-for-byte diff. The doctor/seed-kit/ADR/ops/docker landings are clearly
> the same content; the four `feat:` commits (984f08c, afec7df, 4ae26ed,
> IPv6) are confirmed by the named symbols.

---

## 3. STILL-OPEN BACKLOG — prioritized (18 deliverables)

Effort key: **S** ≤30 min (move file + maybe 1 Cargo line); **M** 30 min–2 h
(new module + router/Cargo wiring + a compile loop); **L** ≥2 h (touches a
protected file, or a multi-site refactor, or needs new API design).

Protected-file key: 🔒 = wire-in requires editing a protected file (founder
main-session only); ⚪ = no protected file touched.

### TIER A — Docs & ADRs (zero code risk, land first)

#### A-1. RPC validation audit — ⚪ S
- **Branch:** `worktree-agent-a75c8e3558ae5051b` → `docs/rpc_validation_audit.md`
- **What:** Audit of every `rpc.rs` handler vs `transaction.rs` limits
  (TxKind, MAX_MEMO_LENGTH, MAX_BATCH_SIZE) + middleware. Documentation only;
  proposes remediations, lands no code.
- **Destination:** `docs/SECURITY_RPC.md` (or `security/rpc_validation_audit.md`).
- **Prereq/conflict:** None. Some F-1/F-2/F-5 items it references are now
  fixed — re-read before acting on its proposals so you don't chase closed bugs.
- **Order rationale:** Pure markdown; informs Tier C RPC work.

#### A-2. RPC API inventory + Grafana dashboard + smoke scripts bundle — ⚪ S
- **Branch:** `worktree-agent-a6a9b43004222b606` (the "wave-4 D5" multi-file branch)
- **What:** `docs/rpc_api_inventory.md` (33 endpoints, 16 gaps),
  `dashboards/grafana_commputer.json` + README, `scripts/byzantine_smoke.sh`,
  `scripts/multi_machine_testnet/*` (bootstrap_runbook, precheck_node.sh,
  tail_chain_health.sh). Also carries `consensus/snowball_api_proposals.rs`
  (see B-5) and the obsolete `wire_in_plan.md`.
- **Destination:** docs → `docs/`, dashboards → `deploy/grafana/`,
  scripts → `scripts/ops/`.
- **Prereq/conflict:** The Grafana JSON expects the **T3.2 Prometheus metric
  names** (`commputer_chain_height`, `commputer_peer_count`, …). Land the
  dashboard AFTER C-2 (metrics) or its panels read empty. The `snowball_api_proposals.rs`
  inside this branch pairs with B-5/B-6 — keep them together.
- **Order rationale:** Docs/scripts are safe; dashboard waits on metrics.

### TIER B — Tests & benches (new files, mostly ⚪, no runtime behaviour change)

These exercise REAL public APIs (verified the helpers exist). Most are
integration-test files: drop in `<crate>/tests/`, no `mod` declaration needed.
The only friction is one Cargo `[dev-dependencies]` line each.

#### B-1. proof verifier proptests — ⚪ M
- **Branch:** `worktree-agent-a55e4019233bf2d12` → `proof_verifier_proptests.rs`
- **What:** Property tests for the 5 verifiers (Cpu/Gpu/Ram/Storage/Bandwidth):
  round-trip prove→verify==true, single-bit-flip rejects, mismatched-challenge
  rejects. Exercises real `ProofVerifier::verify` (`src/proofs/src/verifier.rs:13`)
  and per-channel provers.
- **Destination:** `src/proofs/tests/verifier_proptests.rs`.
- **Prereq:** Add `proptest = "1.4"` to `src/proofs/Cargo.toml [dev-dependencies]`.
  **proptest is not yet a dep anywhere** in the workspace — this is the first.
- **Order:** Lands independently; high value (covers the core proof-of-useful-work surface).

#### B-2. tx validation proptests — ⚪ M
- **Branch:** `worktree-agent-ae0d0edc0fc27408f` → `tx_validation_proptests.rs`
- **What:** Property tests for transaction validation.
- **Destination:** `src/core/tests/tx_validation_proptests.rs`.
- **Prereq:** `proptest = "1.4"` in `src/core/Cargo.toml [dev-dependencies]`.
- **Order:** Independent.

#### B-3. consensus edge tests (T1.4) — ⚪ M
- **Branch:** `worktree-agent-a7f8d94adf7f24edd` → `consensus_edge_tests.rs`
- **What:** Snowball edge cases (competing blocks same height, late votes
  post-finality, stalled rounds, unknown-hash votes, idempotent counting,
  threshold cap) against the REAL `SnowballVoter` API with genesis params
  (sample=3, quorum=2, threshold=5). 3 scenarios it CANNOT express on today's
  API are `#[ignore]`d PSEUDO tests.
- **Destination:** `src/consensus/tests/edge_cases.rs` (no tests/ dir yet — create it).
- **Prereq:** None for the active tests. The `#[ignore]`d ones unlock only
  after B-5/B-6 (snowball API additions) land.
- **Order:** Land the active tests now; revisit the ignored ones after B-6.

#### B-4. sync robustness tests — ⚪ M
- **Branch:** `worktree-agent-add58e0494f90931a` → `sync_robustness_tests.rs`
- **What:** Adversarial/pathological peer behaviour against
  `src/node/src/sync_machine.rs`.
- **Destination:** `src/node/tests/sync_robustness.rs` (the `src/node/tests/`
  dir already exists — analytics_e2e.rs, integration.rs, etc.).
- **Prereq:** A `[dev-dependencies]` line per the file header (read it).
- **Order:** Independent.

#### B-5. compliance_check deep tests — ⚪ S/M
- **Branch:** `worktree-agent-aef11689c24b759b3` → `compliance_check_tests.rs`
- **What:** End-to-end exercise of `src/validator/src/compliance_check.rs`
  (cloud-IP detection etc.). Now extra-relevant: IPv6 detection landed, so
  these tests guard a live code path.
- **Destination:** `src/validator/tests/compliance_deep.rs` (or append to the
  `#[cfg(test)]` block below `feature_219_*`).
- **Prereq:** None (uses existing types).
- **Order:** Independent; low effort.

#### B-6. wallet test harness — ⚪ S/M
- **Branch:** `worktree-agent-a8bcc42a59fa085fc` → `wallet_test_harness.rs`
- **What:** Exhaustive coverage of `Wallet` (`src/core/src/wallet.rs`, 7 pub fns — verified exists).
- **Destination:** `src/core/tests/wallet_test_harness.rs`.
- **Prereq:** None.
- **Order:** Independent; low effort.

#### B-7. compute_epoch_verdicts bench (T1.3) — ⚪→needs lib.rs prereq M
- **Branch:** `worktree-agent-a1f5742a039c1c1e5` → `benches/compute_epoch_verdicts_bench.rs`
- **What:** Criterion microbench of `ProofManager::compute_epoch_verdicts`
  (`proof_manager.rs:257`) across validator counts 1/10/50/100 and valid vs
  50%-invalid scenarios. Quantifies the bbbed4f rayon parallelisation.
- **Destination:** `src/node/benches/compute_epoch_verdicts.rs` (no
  `src/node/benches/` dir yet — create it).
- **Prereq (HARD):** **`pub mod proof_manager;` must be added to
  `src/node/src/lib.rs`** (currently only `mod proof_manager;` in the binary
  `main.rs:15`). A bench links the LIBRARY target, so it cannot see
  binary-only symbols. This lib.rs add is ⚪ (lib.rs is NOT protected) but is a
  load-bearing one-liner.
- **Prereq (Cargo):** `src/node/Cargo.toml`: add `criterion = { version =
  "0.5", features = ["html_reports"] }` to `[dev-dependencies]` and a
  `[[bench]] name = "compute_epoch_verdicts" harness = false` table. criterion
  is not a workspace dep — pin locally.
- **Order:** Do the lib.rs add FIRST (it also unblocks any future external
  proof_manager test), then the bench.

### TIER C — Non-protected runtime code (real behaviour, no protected file)

#### C-1. StorageError + safe column-family accessor (T1.1) — ⚪ L
- **Branch:** `worktree-agent-af41c7c40b4078a30` → `storage_error.rs`
- **What:** Replaces the panic at `rocks.rs:125-129` (`fn cf` →
  `panic!("BUG: column family ...")`) with a `StorageError` enum + a
  `cf_handle_safe`/`cf_safe` accessor returning `Result`. `From<rocksdb::Error>`
  makes `?`-propagation drop-in. Defense-in-depth: a missing CF becomes a
  recoverable error, not an OOM-of-the-node panic.
- **Destination:** new file `src/storage/src/error.rs`; add `pub mod error;` +
  `pub use error::StorageError;` to `src/storage/src/lib.rs` (after line 12).
- **Migration scope (verified):** **21** `self.cf(...)` call sites in
  `rocks.rs` (+ the definition). The blueprint enumerates each with its
  function (put_block @149/152, get_block @166, get_block_by_height @179,
  put_account @194, get_account @202, all_accounts @215, etc.) and the
  required return-type change to `Result<_, StorageError>`. Several methods
  currently return non-Result (all_accounts, archived_account_count,
  estimate_db_size) — the blueprint recommends returning Result over
  silently masking schema corruption.
- **Prereq:** None new — `thiserror` already a storage dep.
- **Conflict:** This is a signature-changing sweep; callers of these
  `RocksStore` methods (across storage + node) must adopt `?` or `.unwrap()`.
  Do it in one branch, run `cargo build -p commputer-storage` then the whole
  workspace.
- **Order:** High-value safety fix but L effort. Land after the cheap test/doc
  tiers so a long compile loop doesn't block them.

#### C-2. Prometheus /metrics (T3.2) — ⚪ code, but wire-in touches 🔒 L
- **Branch:** `worktree-agent-a7ea4e3501887e527` → `rpc/metrics.rs`
- **What:** A `Metrics` struct (prometheus crate: gauges/counters/histograms,
  atomic, lock-free) + an axum handler rendering Prometheus text format. The
  first real metrics surface.
- **⚠️ REPLACE-NOT-ADD TRAP:** This **SUPERSEDES** the hand-rolled
  `get_prometheus_metrics` at `rpc.rs:474-516` (route at `rpc.rs:1177`). Do
  NOT add a second `/metrics/prometheus` route. Recommended routing per the
  file header: `/metrics` → new text handler, `/metrics/json` → the old JSON
  `get_metrics`, `/metrics/prometheus` → alias to the new handler for
  back-compat. Delete the old `get_prometheus_metrics` body.
- **Destination:** `src/node/src/rpc_metrics.rs` (flat, avoids the
  rpc.rs→rpc/mod.rs rename) + `pub mod rpc_metrics;` in lib.rs. Embed
  `pub prom: Arc<Metrics>` on `RpcState` (or have the handler take
  `State<Arc<RpcState>>` and pull `state.prom.clone()` — 1 line, matches the
  file).
- **Prereq (Cargo):** `prometheus = { version = "0.13", default-features =
  false, features = ["process"] }` in `src/node/Cargo.toml`.
- **🔒 PROTECTED dependency:** Every metric has a known update call site in
  `src/node/src/event_loop.rs` (e.g. `set_chain_height` near the
  `update_rpc_status` ~415; `set_peer_count` ~507/515; `record_block_finalized`
  ~2984; `record_consensus_stalled` ~2885). **event_loop.rs is PROTECTED** —
  the founder applies these in the main session. The struct + handler + router
  swap are non-protected; only the metric-emission call sites are 🔒. The
  file header gives line-precise call sites (against commit bbbed4f — re-verify
  the lines, they will have drifted).
- **Conflict:** Land BEFORE A-2's Grafana dashboard (which reads these names).
- **Order:** After Tier A/B. It is the highest-value operator feature but the
  protected-file emission wiring makes it a founder-driven M/L.

#### C-3. AutoNAT v2 behaviour (T2.1) — code ⚪, wire-in 🔒 M/L
- **Branch:** `worktree-agent-a6bcb8b91a904fa15` → `network/autonat_behaviour.rs`
- **What:** Adds AutoNAT v2 (client + server roles) so NATted home nodes can
  learn whether their dial-back addrs are reachable and seed nodes can probe.
  GENUINELY MISSING — `CommpBehaviour` has no autonat (verified).
- **Destination:** `src/network/src/autonat.rs` + `pub mod autonat;` in
  network lib.rs.
- **Prereq (Cargo, 🔒-adjacent):** add `"autonat"` to the libp2p feature list
  in `src/Cargo.toml:41`. (Workspace Cargo.toml — founder edits; agents may
  not touch any Cargo.toml.)
- **🔒 PROTECTED dependency:** Two new fields on `CommpBehaviour`
  (`autonat_client`, `autonat_server`) in `src/network/src/transport.rs`
  (NOT protected) BUT the swarm event handling for
  `CommpBehaviourEvent::AutonatClient/Server` goes in
  `src/node/src/event_loop.rs` (PROTECTED). Founder wires the event arms.
- **Order:** Part of the NAT trio (C-3/C-4/C-5). Land autonat first — relay
  and dcutr are far more useful once autonat confirms reachability. M for the
  behaviour, L overall with the protected event wiring.

#### C-4. Relay SERVER role (T2.2) — code ⚪, wire-in 🔒 M
- **Branch:** `worktree-agent-a7bfb2124c17639bd` → `network/relay_server.rs`
- **What:** Adds `relay::Behaviour` (Circuit Relay v2 SERVER) wrapped in a
  `Toggle` so public seed nodes can relay for NATted operators. Only the
  relay CLIENT exists today (`transport.rs:148 relay_client`).
- **Destination:** `src/network/src/relay_server.rs` + lib.rs export.
- **🔒 PROTECTED dependency:** New `relay_server: Toggle<relay::Behaviour>`
  field on `CommpBehaviour` (transport.rs, non-protected) toggled by a config
  flag. The config flag (`relay_server.enabled`) lives in the protected
  config struct (`src/node/src/config.rs`) and the protected `commputer.toml`/
  `testnet.toml`. Founder adds the config field + wiring.
- **Order:** After C-3. Only matters once you operate public seed nodes that
  should double as relays.

#### C-5. UPnP external-address propagation (T2.3) — code ⚪, wire-in 🔒 S/M
- **Branch:** `worktree-agent-ae2af2c7cd217a224` → `network/upnp_listen_propagation.rs`
- **What:** A free fn translating `upnp::Event::NewExternalAddr/ExpiredExternalAddr`
  into `Swarm::add_external_address`/`remove_external_address`, so `identify`
  advertises the router-mapped address and remote peers can dial back.
- **GAP IS REAL (verified):** `event_loop.rs:1458-1473` UPnP arm only
  `info!`-logs `NewExternalAddr` — it never calls `add_external_address`.
  Without this, UPnP discovers an address that nobody is ever told about.
- **Destination:** `src/network/src/upnp_listen.rs` + `pub use
  upnp_listen::handle_upnp_event;` in network lib.rs.
- **🔒 PROTECTED dependency:** The single call site is INSIDE the
  `SwarmEvent::Behaviour(CommpBehaviourEvent::Upnp(event))` arm in
  `src/node/src/event_loop.rs:1458` (PROTECTED). Founder replaces the
  log-only body with a `handle_upnp_event(&mut self.network.swarm, event)`
  call (still keeping the log).
- **Order:** Cheapest of the NAT trio; high marginal value for home operators.
  Land alongside or right after C-3.

#### C-6. Peer scoring — code ⚪, wire-in 🔒 M
- **Branch:** `worktree-agent-adaf2b1b3d3f071d5` → `network/peer_score.rs`
- **What:** Peer reputation/scoring module (the header gives file:line-precise
  wire-in points against event_loop.rs @ bbbed4f).
- **Destination:** `src/network/src/peer_score.rs` + lib.rs export.
- **🔒 PROTECTED dependency:** Wire-in is in `src/node/src/event_loop.rs`
  (PROTECTED). Founder chooses how to integrate scoring into peer handling.
- **Order:** After the NAT trio (depends on a healthy peer set to be meaningful).

### TIER D — Operator tooling & SDKs (new crates / tools, ⚪)

#### D-1. Backup / restore tool — ⚪ M
- **Branch:** `worktree-agent-a9e89189efb51965d` → `backup/{README, commputer-backup.rs, commputer-restore.sh, cron_backup_example.sh}`
- **What:** RocksDB checkpoint-based backup binary + restore script + cron
  example. NOT landed (verified: no `commputer-backup` in tree).
- **Destination:** `src/tools/backup/` + `scripts/ops/`.
- **Prereq (code):** Needs `RocksStore::create_checkpoint` wired in
  `src/storage/src/rocks.rs` (the header notes the insertion point ~rocks.rs:55).
  That is a non-protected storage addition.
- **Order:** Independent operator convenience; land post-launch when you have
  data worth backing up. Pairs naturally with C-1 (both touch rocks.rs).

#### D-2. Rust client SDK — ⚪ M
- **Branch:** `worktree-agent-a212ee6c62d5b4b45` → `client_sdk/*` (lib.rs,
  client.rs, types.rs, error.rs, 3 examples, `Cargo.toml.proposed`)
- **What:** A standalone HTTP client crate against the node RPC (status,
  send_transaction, monitor_chain_height examples). NOT landed (verified).
- **Destination:** new workspace crate `src/client-sdk/` (add to
  `src/Cargo.toml` members — founder, since it is the workspace Cargo.toml).
- **Prereq/conflict:** SDK types must match current RPC response shapes — A-1
  (rpc_validation_audit) and A-2 (rpc_api_inventory) are the reference. Verify
  endpoint shapes haven't drifted before publishing.
- **Order:** Lowest urgency (external-facing convenience). Land once the RPC
  surface is frozen.

### TIER E — Docs/scripts already useful, low effort, no deps

#### E-1. Operator runbook — ⚪ S
- **Branch:** `worktree-agent-a6696e5eb2f6ebf35` → `docs/operator_runbook.md`
- **Destination:** `docs/operator/` (already exists with
  multi_machine_bootstrap.md, runbook.md). Reconcile/merge with the existing
  runbook.md to avoid duplication.
- **Order:** Anytime; verify it doesn't contradict the landed operator docs.

#### E-2. event_loop topology doc — ⚪ S
- **Branch:** `worktree-agent-a06c32c91e52dbdf9` → `docs/event_loop_topology.md`
- **Destination:** `docs/architecture/`. Pure reference describing the
  tokio::select! arms. **Re-verify against current event_loop.rs** before
  landing — the line refs will have drifted and the spawn_blocking/select
  pattern (per MEMORY feedback note) must be described correctly.
- **Order:** Anytime.

#### E-3. Network chaos smoke script — ⚪ S
- **Branch:** `worktree-agent-aed29e868e955cfc1` → `scripts/network_chaos_smoke.sh` + README
- **Destination:** `scripts/ops/`. A toxiproxy-style partition/latency smoke.
- **Order:** Anytime; useful for pre-launch resilience validation.

---

## 4. Recommended landing ORDER (single ordered list)

Docs/tests first (zero runtime risk), then non-protected code, then
protected-file wiring last (founder main-session, batched to minimize
protected-file churn).

```
# Phase 1 — docs (no risk, informs later work)
 1. A-1  rpc_validation_audit.md        (re-read; some bugs already fixed)
 2. A-2  rpc_api_inventory + scripts    (HOLD the Grafana JSON until step 12)
 3. E-1  operator_runbook.md            (reconcile with landed runbook)
 4. E-2  event_loop_topology.md         (re-verify line refs)
 5. E-3  network_chaos_smoke.sh

# Phase 2 — tests/benches (new files, no behaviour change)
 6. B-5  compliance_check_tests         (guards landed IPv6 path)
 7. B-6  wallet_test_harness
 8. B-1  proof_verifier_proptests       (first proptest dep)
 9. B-2  tx_validation_proptests
10. B-4  sync_robustness_tests
11. B-3  consensus_edge_tests (active tests only)
#   --> add `pub mod proof_manager;` to lib.rs HERE (unblocks bench + future)
11b. B-7 compute_epoch_verdicts bench   (needs the lib.rs add above)

# Phase 3 — non-protected runtime code
12. C-2  metrics struct+handler (REPLACE old handler) + router swap
         --> then land A-2's Grafana dashboard (names now exist)
13. C-1  StorageError + cf_safe sweep (21 sites; long compile loop)
14. D-1  backup tool (+ RocksStore::create_checkpoint)
15. C-3  autonat behaviour (struct/lib only)
16. C-4  relay_server behaviour (struct/lib only)
17. C-5  upnp propagation fn (network module only)
18. C-6  peer_score module (network module only)
19. D-2  client SDK crate

# Phase 4 — PROTECTED-file wiring (founder, main session, batched)
P-a. C-2 event_loop.rs metric-emission call sites (~10 sites)
P-b. C-3 event_loop.rs AutonatClient/Server event arms + src/Cargo.toml "autonat" feature
P-c. C-5 event_loop.rs Upnp arm: swap log-only for handle_upnp_event(...)
P-d. C-4 config.rs + commputer.toml/testnet.toml relay_server.enabled flag
P-e. C-6 event_loop.rs peer-scoring integration
P-f. B-3 revisit #[ignore]d PSEUDO tests after B-6 snowball API lands (see §5)
```

Batching all protected-file edits into Phase 4 means the founder opens
`event_loop.rs` once, applies C-2/C-3/C-5/C-6 emission + event-arm changes
together, and runs one workspace build — rather than five separate
protected-file sessions.

---

## 5. SnowballVoter API extension (paired B-5/B-6, deferred)

`snowball_api_proposals.rs` (inside branch `worktree-agent-a6a9b43004222b606`,
file `src/staging/consensus/snowball_api_proposals.rs`) is a SPEC, not code:
`// PROPOSED:` fn stubs to add to `impl SnowballVoter` in
`src/consensus/src/snowball.rs`. It closes 3 gaps the T1.4 edge tests (B-3)
could not express on the current aggregate-only `record_round(&HashMap)` API:

1. peer-disconnect mid-round (no timeout/`RoundOutcome::Timeout` today),
2. cross-round conflicting vote from one peer (no per-peer history),
3. duplicate vote same peer same round (same root cause).

Proposed fix: a `record_peer_vote(peer_id, hash, round)` ingest that
aggregates internally and keys on `(peer_id, round)` for idempotency, while
leaving the existing aggregate `record_round` path intact for replay/sim
callers (minimum-disruption — verified the existing call site stays valid).

**This is a design decision, not a mechanical wire-in** — the founder reviews
each proposed signature, accepts/rejects, writes the real impl, then enables
the matching `#[ignore]`d test in B-3. Treat as a post-launch consensus task,
NOT part of the mechanical backlog drain.

---

## 6. Branch cleanup checklist

After draining: the 10 SUPERSEDED branches in §2 can be deleted immediately
(after a confirming `git diff`). The 18 open branches in §3 are deleted as
each deliverable lands on main. The 8 common test files were never the
subject of this backlog (already on main:src/staging).
