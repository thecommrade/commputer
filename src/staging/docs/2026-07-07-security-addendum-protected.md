# Security Addendum — PROTECTED-File Launch Blockers (Founder Punch-List)

**Date:** 2026-07-07
**Branch of record:** `agent-testnet-20260707` @ `ea7f24a`
**Provenance:** 8-dimension adversarial security sweep + per-finding adversarial verification, workflow run `wf_c4fe8af2-b93`. **35 findings CONFIRMED of 38 raised; 3 refuted** (recorded in §5). Severities in this doc are the post-verification `verdict.corrected_severity`, not the finder's initial guess.
**Companion:** `src/staging/docs/2026-07-07-enforcement-batch-spec.md` (binding amendments `E1…E16`). This addendum cross-references that spec but does not supersede it.
**Purpose:** The enforcement-batch spec fixes the *inert-by-design* safety flips (producer-sig, vote dedup, F-3 quota, faucet). THIS doc is the disjoint set the sweep surfaced that the **founder** must fix by hand because the fix lands in a PROTECTED file (`src/node/src/event_loop.rs`) — plus the `node_state.rs` root-cause partner and two cross-cutting deployment decisions. The companion workflow is separately auto-fixing the ~21 non-protected findings (indexed in §4 for completeness).

---

## ⚠ STATUS / SEVERITY BANNER — LAUNCH BLOCKERS

> **These are the security findings the founder MUST fix in a PROTECTED batch before the alpha accepts a single untrusted peer.** Every CRITICAL below is a remote, unauthenticated, single-message, network-wide chain-halt or Sybil-finalization primitive against live (non-INERT) node code. They are not the tracked PoUW-inert work; they are live DoS / consensus-integrity holes.

**PROTECTED `event_loop.rs` findings (13):**

| Severity | Count | Indices |
|----------|-------|---------|
| CRITICAL | 4 | [0] [1] [4] [7] |
| HIGH     | 2 | [12] [13] |
| MEDIUM   | 4 | [16] [18] [20] [24] |
| LOW      | 3 | [27] [28] [30] |

**Plus the root-cause partner in non-protected `node_state.rs`:** [2] CRITICAL (same `network_height`-poisoning cluster as [0]/[7]; its band-aid is non-protected but the *durable* fix is protected — see §0).

**Cluster view:** counting the `network_height` cluster ([0]+[2]+[7]) together, the founder faces **5 CRITICAL, 2 HIGH, 4 MEDIUM, 3 LOW** launch blockers in this batch. **Do not open the alpha to untrusted peers until §0 and the CRITICAL/HIGH rows of §1 are applied and the multinode gate re-passes.**

---

## §0 — LEAD BLOCKER: the `network_height` poisoning chain-halt

> Its own section because it is the single most dangerous finding in the sweep and it is **NOT covered by any enforcement E-amendment**. Findings **[0], [2], [7]** are one root cause seen from three ingress points.

### The bug

Every consensus-message handler raises `self.network_height` from an **attacker-supplied, unvalidated `height` field BEFORE any validation**, and `NodeStateMachine::set_network_height` is **strictly monotonic** — it silently ignores any lower value (`node_state.rs:76-80`, comment "can never decrease"). So a single gossip message with `height = u64::MAX` **permanently pins the sync target**, and every receiving node transitions `Active → Stale → Syncing` forever. Because block production and consensus voting are both gated on `is_active()`, the node stops producing and stops voting until it is restarted — and gossipsub auto-forwards the poison to the whole mesh, so one message halts the network.

### Exact call sites (evidence from the finders)

- **`event_loop.rs:1764-1768` — gossip `SnowballQuery{height}`**: `if height > self.network_height { self.network_height = height; }` — **no block required, no validation at all**. This is the cheapest vector: one tiny gossip frame on `TOPIC_CONSENSUS`.
- **`event_loop.rs:1737` — gossip `BlockCandidate`**: raises `network_height` at 1737, *then* calls `validate_block_from_peer` at 1746 — poison precedes validation.
- **`event_loop.rs:1598-1600` — request-response `BlockProposal`**: `if height > self.network_height { self.network_height = height; self.node_state.set_network_height(height); }` — sets the state machine **directly**, before the 1602 validation; needs only a serde-parseable block.
- **`event_loop.rs:1537, 1549, 1557-1558` — `SyncResponse::Block/Blocks/Height`**: a peer answering our `GetHeight` probe with `Height(u64::MAX)` pins it too ([7]).
- **`node_state.rs:76-80`** — `set_network_height`: monotonic gate (`if height <= self.network_height { return; }`). Value is sticky once poisoned.
- **`event_loop.rs:781`** — every sync tick unconditionally feeds `self.network_height` into `node_state.set_network_height(...)`.
- **`node_state.rs:126-129`** — `check_transitions`: `network_height - our_height > STALE_THRESHOLD` (STALE_THRESHOLD = 10) → `Stale → Syncing`, so `is_active()` becomes false.
- **`event_loop.rs:2677`** (block production) and **`event_loop.rs:2943`** / **1618** (consensus voting / finalization) both early-return on `!is_active()`.
- **`transport.rs:219-222`** — gossipsub is configured with only `heartbeat_interval`, no publisher gating beyond Signed/Strict, so the poison SnowballQuery fans out to the whole mesh.

### Why there is no recovery path

Every `force_active()` call site (`event_loop.rs:792/817/823/835/868/883`) sits inside the `if !self.sync_complete` block (`event_loop.rs:783`). Once a node has finished initial sync, `sync_complete = true`, so that block is skipped forever. `Syncing → Active` otherwise requires `our_height >= network_height = u64::MAX`, which can never happen. The consensus-stall timer that would trigger `initiate_chain_resync` is itself behind the `is_active()` gate at 2943, so it never fires. **The node is stuck until restart — and can be re-poisoned the instant it comes back.** (One secondary claim from finder [0] — that the stall self-triggers `reset_to_genesis` and wipes state — does NOT hold, which only means the node hangs rather than self-wiping; it does not help a defender.)

### Recommended fix (PROTECTED — founder)

1. **Never raise `network_height` from an unvalidated gossip/RR `height` field.** Move every `self.network_height = height` update to **after** `validate_block_from_peer` succeeds. Delete the pre-validation `node_state.set_network_height(height)` at `event_loop.rs:1598-1600`.
2. **Clamp every single advance** to `self.state.blocks.height() + MAX_SYNC_WINDOW` (e.g. a few thousand), so no one message can jump the target to an unreachable value. Legitimate far-behind nodes still converge via successive bounded sync batches.
3. **Only trust authenticated evidence:** advance from (a) blocks that passed full validation and whose height is within the window, and (b) `SyncResponse::Height` replies to our *own* `GetHeight` probes.
4. **Consider making `network_height` self-healing** (recompute from the max of recently-validated block heights / live peer height responses) so a transient bad reading cannot permanently pin the node out of `Active`.

**Note on the non-protected band-aid:** finding [2]'s `non_protected_fix` puts a `SANE_MAX_GAP` clamp inside `node_state.rs::set_network_height`, and finding [17] adds a median-clamp in `sync_machine.rs` — both are being applied by the companion workflow as defense-in-depth. **They are not sufficient alone:** `event_loop.rs:1737/1766` write `self.network_height` directly (bypassing `node_state`), so the durable fix (steps 1–3 above) is unavoidably PROTECTED and must ride this batch.

---

## §1 — PROTECTED findings table (by corrected severity)

> Cross-ref column: **NEW** = no enforcement E-amendment covers it; **E-n / Slice-n** = overlaps or is closed by the cited enforcement work (justified against the companion spec). "Partial" means the E-work mitigates but does not fully close the specific hole.

### CRITICAL

| # | Title | Site | Attack (short) | Fix | Cross-ref |
|---|-------|------|----------------|-----|-----------|
| **[0]** | `network_height` poisoning → permanent chain-halt | `event_loop.rs:1737` (+1766, 1598-1600) | One unvalidated gossip `SnowballQuery{height:u64::MAX}` pins the monotonic sync target; every node goes `Active→Syncing` forever; production + voting stop; gossip fans it out network-wide. | Raise `network_height` only after validation, clamped to `tip+WINDOW`; drop the pre-validation `set_network_height`. | **NEW** — see §0. |
| **[7]** | Unauthenticated `network_height` poisoning halts production chain-wide | `event_loop.rs:1557` | Same cluster via `SyncResponse::Height(u64::MAX)` answering our `GetHeight`, or any gossip block with `header.height=u64::MAX`; monotonic + `sync_complete=true` ⇒ no recovery. | Same as [0] (bounded, post-validation advance). | **NEW** — see §0. |
| **[2]** | Monotonic `network_height` permanently demotes nodes to Syncing | `node_state.rs:76` (non-protected file; durable fix protected) | `set_network_height` is strictly monotonic; a single self-reported `u64::MAX` from any peer pins it; `Syncing→Active` needs `our_height ≥ u64::MAX`. | Reject implausible jumps (`> our_height + SANE_MAX_GAP`) / make it self-healing; **plus** the protected [0]/[7] fix (direct writes bypass this file). | **NEW** — §0 partner. Band-aid auto-fixed; durable fix protected. |
| **[1]** | Unbounded gap-request loop on attacker-controlled height (self-DoS + amplification) | `event_loop.rs:3203` (twin at `3055`) | A synced block with `header.height=u64::MAX` (empty block ⇒ zero merkle roots pass `verify_roots`; no producer-sig check pre-flip) hits `for h in expected..height { self.request_block(h); }` — ~1.8e19 synchronous iterations that never return (permanent single-node freeze) + enqueues that many outbound `GetBlock`s (OOM + peer flood). | Clamp: `for h in expected..height.min(expected + SYNC_BATCH_SIZE)`. Also reject blocks whose height exceeds tip by more than a sane bound. | **E6 (partial).** E6 bounds exactly this loop in `apply_synced_block`. ⚠ **E6 does NOT bound the twin loop at `try_apply_finalized:3055`** — founder must apply the same clamp there. |
| **[4]** | Snowball votes counted with zero source accounting → single peer fabricates a quorum | `event_loop.rs:1867` (+1779) → `consensus_manager.rs:317` | `record_response(height, preference)` does `round_responses[preference] += 1` with no PeerId, no per-peer dedup, no validator/candidate check; the `round` nonce busts gossip dedup; low small-net thresholds (quorum 2, decision 5) let one unauthenticated peer drive finalization → censorship / fork / liveness stall. No producer-sig gate on the applied block. | Source-attribute votes: `record_peer_response(height, preference, peer)` with a per-height/round `HashSet<PeerId>`, ideally validator-gated. | **E2 / E7 / Slice 2 (covers, with a caveat).** Slice 2 wires the `VoteAggregator` (per-`PeerId` dedup) and E7 reconciles the shim; **E2 is the load-bearing part** — Signed+Strict alone lets one connection mint unlimited keypairs, so E2's delete-or-gate of the legacy gossip vote arms is what actually closes [4]. Verify E2 option (a)/(b) is applied, not just the aggregator. |

### HIGH

| # | Title | Site | Attack (short) | Fix | Cross-ref |
|---|-------|------|----------------|-----|-----------|
| **[12]** | `handle_rpc_transaction` never calls `enforce_mempool_limit` → RPC bypasses the 5000-tx cap | `event_loop.rs:2124` | Gossip path calls `enforce_mempool_limit()` (2228); RPC path pushes to `pending_txs` at 2124 and returns without it. With no balance check, one fresh keypair streams nonce 0,1,2,… to `/tx` (100 req/s/IP) → unbounded `pending_txs` → OOM. | One line: `self.enforce_mempool_limit();` at the end of `handle_rpc_transaction` (the fn already exists at 2322). | **Partial (E3 / Slice-3 F-3).** The F-3 per-account quota (64) at `validate_tx_for_mempool` is a single choke point covering both RPC (:2091) and gossip (:2193), so it bounds the *single-key* flood on the RPC path. The specific global-cap one-liner is **NEW** and still wanted (many-key flood). |
| **[13]** | Orphan pool per-parent `Vec` is unbounded → remote OOM | `event_loop.rs:2033` (+`3197-3201`) | The `orphan_pool.len() < 100` cap counts DISTINCT parent-hash keys, not blocks per key. Attacker sends many distinct ~2MB blocks all sharing one fake `parent_hash` + non-connecting height → one `Vec` grows unbounded. Buffered BEFORE `validate_block_from_peer` (2044), so each bypasses size/merkle/tx-sig checks. | Cap per-parent `Vec` length (≤20) AND total buffered orphans (≤200), evicting oldest. A non-protected `bounded_orphan_insert` helper can hold the logic; the two push sites are protected. | **Partial (Slice 1 Hunk 1.7 + producer-sig flip).** Hunk 1.7 moves validation *before* orphan-buffering, and producer-sig enforcement (Slice 1) removes the cheap unsigned-block supply — together these gut the ~2MB-bypass sub-attack. The explicit per-parent/total **count cap is NEW** and still needed (a validator can flood signed orphans). |

### MEDIUM

| # | Title | Site | Attack (short) | Fix | Cross-ref |
|---|-------|------|----------------|-----|-----------|
| **[16]** | Inbound `PeerExchangeMessage` has no entry cap → CPU amp + Kademlia pollution (eclipse aid) | `event_loop.rs:1247` | Send side caps at `MAX_PEERS_PER_EXCHANGE=20` (3391); the inbound handler iterates the entire attacker `msg.peers` map (up to 2MiB decompressed) doing base58/multihash PeerId parses + Multiaddr parses + `kademlia.add_address` per entry — thousands of costly parses/msg at 50 msg/s, flooding the routing table with attacker addresses. | Reject if `msg.peers.len() > MAX_PEERS_PER_EXCHANGE`, or `.take(MAX_PEERS_PER_EXCHANGE)`; prefer diverse /16 subnets. | **NEW** |
| **[18]** | No balance/fee-payability check at mempool admission + lowest-fee eviction → free flooding + fee censorship | `event_loop.rs:2129` | `validate_tx_for_mempool` never checks the sender can pay. Fresh keypairs each sign one nonce-0 tx with `fee=u64::MAX` (never paid); `enforce_mempool_limit` evicts the LOWEST fee, so honest lower-fee txs are pushed out and unpayable max-fee txs survive → cheap mempool capture / censorship. | Add `ChainState::can_cover_fee(&tx, pending_debits)` (non-protected `state.rs`) and call it from `validate_tx_for_mempool`; make eviction prefer un-affordable txs. Wiring is the protected one-liner. | **NEW.** (F-3 caps *count*, not *payability* — orthogonal; both wanted.) |
| **[20]** | Consensus rate-limiter bucket key has ~16 bits of entropy → grindable collision starves a validator | `event_loop.rs:1590` (+1635, 1668) | `peer.to_bytes()[..8]` folded to a bucket: ed25519 PeerId's first 6 bytes are constant, so only 2 key bytes vary (~65k buckets). Attacker grinds a keypair into victim V's bucket (~sub-second) and floods the shared 10/s RR bucket → V's `BlockProposal`/`VoteRequest` answered `NotReady` and dropped. | Key on a hash of the FULL `peer.to_bytes()` (`DefaultHasher`) or key the internal map by `PeerId` directly. Sites 1590/1635/1668 are protected. | **E9 (FOUNDER DECISION).** E9 fixes the *sync* limiter with full-bytes hashing and **explicitly flags this consensus fold at 1590/1635** for a same-pass one-line fix — but leaves "fix it now?" a §6 founder decision. Fold it in. |
| **[24]** | `producer_blocks` / `block_seen_times` grow unbounded from pre-validation blocks | `event_loop.rs:2024` (+2000) | Both maps are inserted BEFORE the orphan return (2041) and BEFORE `validate_block_from_peer` (2044), and are never pruned. Attacker streams distinct blocks (arbitrary producer/height) → both maps grow forever → OOM. | Insert only AFTER `validate_block_from_peer` passes; prune on finalization (drop `producer_blocks` ≤ applied tip; LRU/evict old `block_seen_times`). | **NEW.** (Same pre-validation-insert anti-pattern as [13]/[24]; fits the same 1a pass.) |

### LOW

| # | Title | Site | Attack (short) | Fix | Cross-ref |
|---|-------|------|----------------|-----|-----------|
| **[27]** | Sync `GetBlocks` amplification — unbounded per-peer serve rate as committed | `event_loop.rs:1511` | The built `SyncRateLimiter` is INERT/unwired; one peer streams `GetBlocks{start,end}`, each serving up to 100 blocks `serde_json`-encoded → multi-MB reply per tiny request. `max_concurrent_streams(8)` caps concurrency, not rate. | Wire `SyncRateLimiter` into the `GetBlock`/`GetBlocks` handler (per-peer token bucket). | **Slice 3 Hunk 3.6 (+E6/E9) covers.** The sync-limiter wire-in is exactly this handler; ensure the E9 full-bytes `peer_hash` and E6 own-bucket are applied. |
| **[28]** | Integer overflow in `GetBlocks` serve range (`start + 100`) — debug-panic, no overflow-checks profile | `event_loop.rs:1513` | Attacker sends `start = u64::MAX`; `start + 100` overflows. Root `Cargo.toml` sets no `[profile]`, so a debug build panics (remote crash); release wraps to empty (benign). | `start.saturating_add(100)`. | **NEW (adjacent to Slice 3 Hunk 3.6).** Same handler the sync-limiter edits — fold the saturating_add into the 1a serve-gate commit. |
| **[30]** | `ChainHealthMonitor` voter set inflatable + same weak peer hash → `/health` `active_voters` poisonable | `event_loop.rs:1669` | Every accepted Vote calls `health_monitor.record_vote(peer_hash)` with the same ~16-bit fold as [20]; an attacker answering `BlockProposals` with `Vote{accept:true}` inflates `active_voters`; distinct identities collide. Display/observability integrity only (gates no consensus). | Key `record_vote` on the full-PeerId hash and/or count only confirmed validators; expire `voter_activity`. | **NEW (E9-adjacent).** E9 fixes the sync limiter and flags the consensus limiter, but **does not cover this third use of the weak fold at :1669** — extend the same full-bytes hashing here. |

---

## §2 — Cross-cutting / deferred non-protected items (founder decisions)

These are non-protected findings whose real resolution is a founder judgement call, not a mechanical patch — so they are deliberately NOT in the auto-fix stream.

### [25] `state.rs:1070` — block `state_root` is never validated on apply (MEDIUM)

`verify_roots` (`core/block.rs:243-246`) compares `tx_root`/`proof_root` only and deliberately omits `state_root`; `apply_block_validated` (`state.rs:1070`) gates solely on `verify_roots` and never recomputes/compares `header.state_root` against post-apply state. A malicious producer can set `header.state_root` to any 32 bytes; honest nodes recompute their own balances (so funds aren't corrupted) but **store and propagate the forged root verbatim** — defeating the state commitment for light clients, checkpoints, and any future state-proof consumer; genuine cross-node divergence would go undetected here (`fork_detector` only compares `parent_hash`).

**Why deferred:** enabling strict state-root validation risks bricking sync if root computation has *any* asymmetry (non-determinism, ordering) — and it is the natural partner of producer-signature enforcement (both make a forged/divergent block a clean reject). **Recommendation:** the founder enables it **together with** the enforcement batch's producer-sig flip and validates via the multinode + strict `verify-chain` gate (§3 of the enforcement spec). The companion workflow is leaving a `// SECURITY(F25)` TODO marker at the apply site so it is not lost. (Note: the enforcement spec §7 already defers the sibling apply-time *producer-sig* check for the same "breaks ~30 storage tests" reason; treat [25] as the same fast-follow, gated on the reset.)

### [26] `rpc.rs:918` — keyless ADMIN tier exposed behind a keyless proxy (LOW)

`auth_middleware` is a pure pass-through when `state.api_key` is `None` (`rpc.rs:918`). Under the documented D3 topology (node on loopback, no api_key, fronted by a public TLS reverse proxy — caveat at `rpc.rs:1452`), the **entire ADMIN tier becomes internet-reachable**, including `/peers/full` (`get_peers_full`, `rpc.rs:1361`) which returns the **un-redacted validator IPs** that public `/peers` strips (F-5), plus `/metrics`, `/storage/metrics`, `/traffic`, `/network/quality`. `rpc_bind_guard` (`rpc.rs:1458`) only sees the node's own bind address and cannot detect the proxy.

**Ties to E14 (CF-proxy).** This is a **deployment-policy decision**, coupled to E14 and §6's "which reverse proxy fronts RPC." **Options:** (a) only register the admin sub-router when `state.api_key.is_some()` (404 admin paths otherwise); or (b) in `auth_middleware`, reject admin-tier requests carrying `X-Forwarded-For`/`CF-Connecting-IP` while no api_key is set. Decide alongside E14 before the alpha proxy goes live, or the F-5 IP redaction is silently defeated.

### Other founder-judgement items already surfaced in the enforcement spec §6

Not new findings, but the sweep reinforces them: E2 delete-vs-gate of the legacy vote arms (closes [4]); E9 "fix the consensus rate-limiter weak fold now?" (closes [20], and should be extended to [30]); E14 CF-proxy per-IP collapse (couples to [26]).

---

## §3 — Recommended sequencing

**Plainly: the protected security fixes here — above all §0 `network_height` — should fold into the SAME founder protected batch as the enforcement work and ride the SAME alpha genesis reset.** Several overlap the E-amendments (E2/E6/E9/Slice-3), all are pre-launch, and the batch already opens `event_loop.rs`, `config.rs`, `main.rs`, and `genesis.json` for editing in one sitting. Doing the security fixes in the same `event_loop.rs` commit avoids a second protected-file touch and lets the ONE multinode + strict-`verify-chain` gate validate everything.

**Ordered checklist, keyed to the enforcement spec's §2 apply order:**

1. **Stage 0 (pre-stage, non-protected, agent branch):** unchanged from the enforcement spec (0a–0g). Add here the non-protected band-aids the companion workflow produces for the `network_height` cluster ([2] `node_state.rs` `SANE_MAX_GAP`, [17] `sync_machine.rs` median clamp) and the non-protected helpers for [13] (`bounded_orphan_insert`), [18] (`can_cover_fee` in `state.rs`). Keep green at every commit.
2. **Stage 1a — the "event_loop enforcement" commit (PROTECTED, founder):** apply the enforcement 1a hunks (producer-sig + E4 height-0 reject, bypass closes 1.6/1.7, vote wiring + E2, sync-limiter + E6 gap-bound + E9 full-bytes hash, F-3 + E3) **and in the same commit** the security fixes that live in the same file:
   - **§0 `network_height`** — post-validation, clamped advances at 1737/1766/1598-1600/1557; drop the pre-validation `set_network_height`. **[0][7]** (and completes [2]).
   - **[1]** clamp the gap-request loop at **both** `3203` and `3055` (E6 only does 3203).
   - **[12]** add `enforce_mempool_limit()` to `handle_rpc_transaction`.
   - **[13]/[24]** move `orphan_pool` / `producer_blocks` / `block_seen_times` inserts to AFTER validation; add per-parent + total orphan caps; prune on finalization.
   - **[16]** cap inbound `PeerExchangeMessage` entries.
   - **[18]** wire `can_cover_fee` into `validate_tx_for_mempool`; bias eviction to un-affordable txs.
   - **[20]** (E9 §6 decision — recommend YES) full-bytes hash the consensus rate-limiter at 1590/1635/1668.
   - **[28]** `start.saturating_add(100)` in the `GetBlocks` serve range.
   - **[30]** full-bytes hash `health_monitor.record_vote` at 1669.
   - **[27]** ensure the sync-limiter wire-in (Slice 3 Hunk 3.6) actually gates `GetBlocks`.
3. **Stage 1b — atomic `rpc.rs` + `main.rs` pair:** unchanged (faucet). Add **[26]** admin-tier fail-closed here (non-protected `rpc.rs`), decided with E14.
4. **Stage 1c — `config.rs` + root `genesis.json`:** unchanged.
5. **[25] state-root validation:** enable in `state.rs` (non-protected) **in the same batch** as the producer-sig flip; it is the state-commitment partner. Gate on the multinode + strict `verify-chain` run — if any root asymmetry surfaces, that is exactly what the gate is for.
6. **Stage 2 — verification gate (enforcement spec §3):** the 2-node AND 3-node `multinode_assert.sh` HARD PASS, strict `verify-chain` "0 errors", and the extended late-join gate all now also exercise these security fixes. **Additionally re-run with a poison probe** (a scripted peer sending `SnowballQuery{height:u64::MAX}`) and confirm the node stays `Active` and keeps producing — the direct regression test for §0.

**Do not launch with `GATE_ALLOW_BELOW_BASELINE=1`.** Do not open to untrusted peers until §0 + all CRITICAL/HIGH rows pass the gate.

---

## §4 — Non-protected fixes (auto-fix in progress on branch — index only)

The companion workflow is fixing the ~21 non-protected findings below on the agent branch (files: `state.rs`, `consensus_manager.rs`, `rpc.rs`, `sync_machine.rs`, `consensus_protocol.rs`, `transport.rs`, `wallet.rs`, `block.rs`). Listed for a complete index of the sweep. Several are duplicate reports of one root cause (grouped).

**CRITICAL**
- **[3] / [5] / [6]** `state.rs:1349` — unbounded MultiSig `signers × signatures` ed25519 verify loop (no size guard; `validate_shape`'s `MAX_MULTISIG_SIGNERS=16` is called only at `rpc.rs:131`, not on the gossip/apply paths) → single-threaded event-loop freeze / chain halt. *Fix:* size guard at the top of the MultiSig apply arm + `validate_shape()` in `validate_tx_for_mempool`. (Three independent finders, same bug.)

**HIGH**
- **[10]** (+ dup **[21]** MEDIUM) `state.rs:1273` — forgeable `MilestoneBurn` / `CharitableDonation` inflate consensus `total_burned` with no balance debit / no nonce → replayable circulating-supply corruption + emergency-access flip.
- **[11]** (+ dup **[22]** MEDIUM) `state.rs:1281` — `StorageWill.contact_hashes` unbounded at ingress and apply → permanent on-chain state bloat + mempool RAM bloat (bypasses even the RPC `validate_shape`).
- **[9]** (+ dup **[14]** HIGH) `consensus_manager.rs:275` — attacker candidate/arbitrary-height flooding grows `ConsensusManager.heights` without bound → OOM.

**MEDIUM**
- **[15]** `rpc.rs:455` — unbounded concurrent WebSocket connections on public `/ws` (fd/memory exhaustion).
- **[17]** `sync_machine.rs:127` — sync `target_height` poisoning via unvalidated median peer height → permanent Downloading stall (partner mitigation for §0).
- **[19]** (+ dup **[23]** MEDIUM) `consensus_manager.rs:538` — `checkpoint_votes` map never pruned, keyed by attacker-controlled `(height, validator)` → unbounded growth.
- **[8]** `rpc.rs:1207` — lock-order inversion across public RPC handlers → permanent unauthenticated RPC deadlock (single-node DoS).

**LOW**
- **[29]** `consensus_protocol.rs:55` — codec eagerly allocates the full declared length (≤10MB) before reading (down-graded from MEDIUM in verification; timeout- and stream-bounded).
- **[31]** `transport.rs:182` — libp2p peer key written world-readable then `chmod`'d (TOCTOU) and follows symlinks.
- **[32]** `wallet.rs:37` — `Wallet::seed_phrase()` leaves raw key entropy + BIP39 `Mnemonic` unzeroized on the stack.
- **[33]** `state.rs:1199` — zero-address can hold and be drained: `apply_genesis_accounts` doesn't reject it and the `Transfer` arm doesn't guard zero-from unsigned spends.
- **[34]** `block.rs:78` — `checkpoint_hash` excluded from `signable_bytes` → signature malleability / two valid blocks at one height. **This IS enforcement amendment E5** (Slice 1 Hunk 1.3, mandatory) — applied in the enforcement batch, not the auto-fix stream.

*(Not auto-fixed — founder decisions in §2: [25] state-root validation, [26] keyless admin tier.)*

---

## §5 — Refuted (recorded so they are not re-litigated)

Three findings from the 38 raised were REFUTED under verification (`sec_refuted.json`):

1. **Request-response eager `vec![0u8; len]` pre-allocation (10 MiB slowloris memory pin).** Refuted as the claimed mechanism: `request_response::Config::default()` gives a 10s `request_timeout` and the read future sits in a `FuturesMap` (aborted after ~10s), so no *indefinite* pin; `alloc_zeroed` is demand-zero (mmap) so RSS impact is ~0 until pages are faulted; faulting them in costs real bandwidth and is stream-capped (8 sync + 4 consensus, yamux 64). Worst case is ~120 MiB *virtual* mapping per connection for ≤10s. Real severity LOW (mild anti-pattern), not MEDIUM. `with_capacity(min(len, 64KiB))` remains reasonable hardening.
2. **Gossip decompression CPU amplification within the 2 MiB cap.** Refuted at the claimed magnitude: the per-peer 50 msg/s limit (`event_loop.rs:1172-1189`) runs BEFORE `decompress` (1214) and bounds the rate; gossipsub delivers full payloads only from ~12 mesh peers (not 50); a 2 MiB zero-fill bomb fails `serde_json` on byte 0 (no 2 MiB traversal); real cost is a bounded ~ms deflate decode. Reasonable defense-in-depth, not a live exposure.
3. **ed25519 non-strict `verify()` malleability / equivocation.** Refuted against the pinned dependency: ed25519-dalek 2.2.0 without `legacy_compatibility` enforces canonical `S` (rejects the S+L transform) AND rejects non-canonical `R` encodings, so no third-party transform of a valid signature exists — manufactured equivocation without the producer's key is impossible, not merely improbable. Adopting `verify_strict` is fine hygiene against future dependency drift, but there is no live vulnerability. (Pre-flip the point is moot anyway: `ENFORCE_PRODUCER_SIGNATURES=false` lets a relay strip the signature outright — the already-tracked unsigned-block item the enforcement batch closes.)

---

*End of addendum. This is the founder's protected-file security punch-list for the alpha; apply §0 first, then the CRITICAL/HIGH rows of §1, in the same protected batch and genesis reset as the enforcement spec.*
