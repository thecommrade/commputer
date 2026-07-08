# Alpha-Reset Enforcement Batch — Apply-Ready Spec

**Date:** 2026-07-07
**Branch of record:** `agent-testnet-20260707` @ `ea7f24a`
**Run id:** `wf_e6f90776-f2a`
**STATUS: reviewed plan, NOT yet applied.**

## Purpose

At the alpha genesis reset the Commputer testnet discards all chain state and every node
restarts from a fresh, deterministically-derived genesis on one new binary. Because that reset
is a clean break, it is the one safe window to flip the safety mechanisms that were built INERT
(compiled but unenforced) during batches A (RPC) and B (consensus/network) to LIVE — producer-
signature enforcement, per-peer Snowball vote de-duplication, the F-3 mempool quota and sync-
serving rate limiter, and a real funded faucet. This document is the single, self-contained
source of truth a founder executes by hand at that reset: it folds every finding from the
adversarial review into binding amendments that override the raw hunks, then lays out the exact
apply order, verification gate, reset procedure, and rollback path.

## Provenance

Synthesized from a 5-slice apply-ready mapping workflow (`wf1_maps` — 47 hunks across
producer-signature enforcement, vote-aggregator wiring, F-3 quota + sync limiter, faucet D6 +
genesis accounts, and a batch-shape meta-layer) that was then put through a 3-lens adversarial
review (`wf1_reviews` — consensus-safety, integration-liveness, protected-minimality; all three
returned APPROVE_WITH_FIXES). Every load-bearing code citation in the maps was checked against
the working tree at `ea7f24a`; the reviews' file:line references are reproduced here so the
founder can locate code quickly. Line numbers anchor on exact code text — any intervening commit
shifts them, so match on the quoted `current_code`, not the line number.

## How to read this doc

1. **§0 (BINDING AMENDMENTS) wins over everything else.** It converts each review blocker and
   major — plus the load-bearing minors — into a numbered amendment `E1…E16` that OVERRIDES the
   raw hunks. Read §0 in full before touching a single hunk. Where §0 deletes or corrects a hunk,
   §1 annotates that hunk inline with `⚠ SEE AMENDMENT En`.
2. **§1** reproduces the raw slice hunks (copy-able `current_code → new_code`) so you can apply
   the untouched ones directly and see exactly what each amendment changes.
3. **§2** is the ordered apply checklist (what pre-stages on the agent branch vs. what the founder
   applies to protected files, and the compile-coupling that forces certain hunks to land together).
4. **§3–§5** are the verification gate, the genesis-reset procedure, and the rollback path.
5. **§6** is the crisp list of decisions the batch cannot proceed without.
6. **§7** records verified-good items so they are not "re-fixed," plus deferred residuals.

**Protected files (founder-only, per CLAUDE.md):** `src/node/src/main.rs`,
`src/node/src/event_loop.rs`, `src/node/src/config.rs`, root `genesis.json`, root
`commputer.toml`/`testnet.toml`, `src/core/src/token.rs`, plus `CLAUDE.md`/`RESUME.md`/whitepaper/
website. This batch touches four of them (`main.rs`, `event_loop.rs`, `config.rs`, root
`genesis.json`) and NO others. Everything else it touches (`block.rs`, `genesis.rs`,
`consensus_manager.rs`, `rpc.rs`, `vote_aggregator.rs`, `mempool_quota.rs` [new], `lib.rs`,
`testnet_genesis.rs`, `sync_rate_limiter.rs`, `scripts/*`) is NON-protected and pre-stageable.

---

## §0 — BINDING AMENDMENTS (read before any hunk)

> These amendments are normative. Where an amendment contradicts a raw hunk in §1, the amendment
> is correct and the hunk is wrong. Blockers and majors are `E1…E11`; folded load-bearing minors
> are `E12…E16`.

### E1 — ONE faucet-allocation mechanism only (BLOCKER, all three lenses)

**Problem.** Slice 4 and Slice 5 ship two contradictory, compiled faucet-allocation mechanisms.
Slice 4 sources the allocation from a fail-safe `Option<&str>` constant in non-protected
`testnet_genesis.rs` (`ALPHA_FAUCET_ADDRESS_HEX`, 100,000 COMME = `1e13` raw) applied **before**
`apply_block(&genesis)`. Slice 5 hunk 4 instead compiles the allocation into
`core/genesis.rs::default_genesis().accounts` as `vec![("<FAUCET_ADDRESS_HEX_64>", 1_000_000_000_000)]`
(10,000 COMME = `1e12` raw) and its prose instructs a main.rs call **after** `apply_block`.
Executed as written: if BOTH `apply_genesis_accounts` calls land, the second hits the
`total_emitted != 0` guard (`storage/state.rs:774-779`) → `Err` → `?` bails `run_node` → **every
node fails to start**; if slice 5's variant ships with its placeholder unfilled,
`Address::from_hex("<FAUCET_ADDRESS_HEX_64>")` fails (`state.rs:785-789`) → all-or-nothing
validation `Err` → **every node refuses to boot**; and because the allocation is not bound into
the genesis hash (genesis `state_root` is fixed `[0;32]`, `main.rs:394-412`), two builds differing
only in the allocation share a genesis hash, peer happily, and **fork on state root at height 1**.

**RESOLUTION (binding).**
- Adopt **Slice 4's mechanism ONLY**: the `Option<&str>` fail-safe constant + `alpha_genesis_accounts()`
  in `testnet_genesis.rs`, applied **BEFORE** `apply_block(&genesis)` in **both** the run path and
  `open_chain_state` (verified crash-safe: `persist_applied_block`, `state.rs:2637-2677`, sweeps
  dirty accounts + `META_TOTAL_EMITTED` into block 0's single atomic `WriteBatch`).
- **DELETE Slice 5 hunk 4** (`core/genesis.rs` accounts) **and Slice 5 hunk 5** (the paired
  `default_genesis().accounts.len()==1` test change). Do NOT compile any allocation into
  `core/genesis.rs`; leave `accounts: Vec::new()` and the existing `..._deserializes_to_empty` test
  untouched (they stay green).
- There must be **exactly ONE `apply_genesis_accounts` call per genesis path** in `main.rs` (the run
  path + the `open_chain_state` parity call), each sourced from the one compiled constant.
- The founder decides the **single COMME amount** (see §6); it is set once in `testnet_genesis.rs`
  (`ALPHA_FAUCET_ALLOCATION`).
- If root `genesis.json`'s `accounts`/`chain_id`/`timestamp` reference is kept (Slice 5 hunk 7), it
  MUST **byte-match** the compiled `testnet_genesis.rs` values. It has zero runtime effect; it is a
  published reference only, so it may also be dropped (§7). Do not let it drift half-way.

### E2 — Legacy gossipsub vote arms defeat the anti-Sybil dedup (MAJOR — consensus, integration, minimality)

**Problem.** Slice 2 attributes `SnowballResponse`/`VoteResponse` votes to `message.source` under
gossipsub `MessageAuthenticity::Signed` + Strict. Signed+Strict only proves the message signature
matches the embedded source key — it does NOT require that source to be a connected/noise-
authenticated peer. One attacker on a single gossipsub connection can mint unlimited ed25519
keypairs, publish one signed `SnowballResponse` per fabricated identity (distinct `round` nonces
defeat gossipsub dedup and the Item-18 sha256 suppression at `event_loop.rs:1199-1211`), and each
is counted as a distinct voter — quorum fabrication survives the batch on exactly the path the
batch exists to close. It also makes the vote-aggregator `HashSet<PeerId>` memory (minor) free to
inflate.

**RESOLUTION (binding — FOUNDER DECISION on delete-vs-gate).** In the SAME stage-1a event_loop
commit, do ONE of:
- **(a) DELETE** the legacy gossipsub `SnowballResponse`/`VoteResponse` vote arms entirely
  (request-response is the primary vote path per the comment at `event_loop.rs:1578`); or
- **(b) GATE** them on the originator being a currently-connected peer, before `record_peer_response`:
  ```rust
  let Some(o) = originator.filter(|o| self.peer_ips.contains_key(o)) else { return; };
  self.consensus.record_peer_response(height, preference, o);
  ```
Option (b) keeps the dual-path liveness fallback while binding vote identity to a real connection.
Fixing E2 also closes the aggregator-memory-inflation vector (minor) and the third-party
stale-vote replay surface (§7). This is a **FOUNDER DECISION**; both options are one-liners in the
two arms.

### E3 — Faucet nonce desync × F-3 quota collision + claim-check race (BLOCKER-class MAJOR — all three lenses)

**Problem.** Slice 4's dispenser consumes `faucet_next_nonce` on `tx_sender.try_send` = Ok, but
that only queues to the 256-slot channel; admission runs later in `validate_tx_for_mempool`, where
Slice 3's **own** new F-3 per-account quota (64 pending) applies to the faucet account itself. Once
the faucet has 64 pending txs, dispense N is rejected at admission while its nonce N is already
consumed; chain nonce reaches N via the 64 applied txs, and every later dispense is a future-nonce
tx that never confirms — **faucet dead until node restart**. Trigger is cheap and adversarial:
>64 successful claims inside one block-drain window, reachable via 65 distinct IPs/addresses, or
via a claim-check/consume race (Slice 4 does a read-only claim check, drops the lock, then awaits
the nonce lock — N concurrent requests from one IP with distinct addresses all pass the check
before any insert lands, bypassing the per-IP-per-epoch limit under the 100 req/s cap).

**RESOLUTION (binding).**
1. **Exempt the compiled faucet address from the F-3 quota** at the Slice-3 `validate_tx_for_mempool`
   call site (the address is a compile-time constant in the same crate):
   ```rust
   let faucet_exempt = crate::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX
       .and_then(|h| commputer_core::identity::Address::from_hex(h).ok())
       .is_some_and(|fa| fa == tx.from);
   if !faucet_exempt {
       commputer::mempool_quota::account_quota_ok(
           pending_from_sender,
           commputer::mempool_quota::MAX_MEMPOOL_TXS_PER_ACCOUNT,
       )?;
   }
   ```
2. **Serialize the dispense critical section** in the Slice-4 handler: acquire `faucet_next_nonce`
   FIRST; do the claim-check **and** provisional insert INSIDE that critical section; enforce an
   in-flight bound **well below** the 64 cap (`FAUCET_MAX_INFLIGHT = 32`), returning a retryable
   503 **without consuming the nonce** when over the bound; roll back the provisional claim inserts
   on `Full`/`Closed`. The corrected inner block:
   ```rust
   // E3: acquire the nonce lock FIRST — serialize the whole dispense so
   // concurrent claims cannot each pass a stale read-only claim check.
   let mut next_nonce = state.faucet_next_nonce.lock().await;

   // In-flight bound well below the F-3 cap (64). Derived from the live
   // mempool snapshot; retryable 503, nonce NOT consumed.
   const FAUCET_MAX_INFLIGHT: usize = 32;
   let in_flight = state.mempool.lock().await.iter()
       .filter(|t| t.from == *faucet_wallet.address()).count();
   if in_flight >= FAUCET_MAX_INFLIGHT {
       return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
           "error": "faucet busy (too many unconfirmed dispenses), try again shortly",
       })));
   }

   // Claim-check + PROVISIONAL insert inside the critical section.
   {
       let mut claims = state.faucet_claims.lock().await;
       let claimed = |k: &String| claims.get(k).is_some_and(|&e| e >= current_epoch);
       if claimed(&addr_key) || claimed(&ip_key) {
           return (StatusCode::TOO_MANY_REQUESTS, Json(serde_json::json!({
               "error": "faucet already claimed this epoch",
               "next_available_epoch": current_epoch + 1,
           })));
       }
       if claims.len() >= MAX_FAUCET_CLAIM_ENTRIES { claims.retain(|_, e| *e >= current_epoch); }
       claims.insert(addr_key.clone(), current_epoch);
       claims.insert(ip_key.clone(), current_epoch);
   }

   let tx = build_faucet_transfer(faucet_wallet, to, *next_nonce);
   let tx_hash = hex::encode(tx.hash().0);
   match state.tx_sender.try_send(tx) {
       Ok(()) => { *next_nonce = next_nonce.saturating_add(1); drop(next_nonce); /* 200 */ }
       Err(mpsc::error::TrySendError::Full(_)) => {
           let mut c = state.faucet_claims.lock().await; c.remove(&addr_key); c.remove(&ip_key);
           /* retryable 503, nonce NOT consumed */
       }
       Err(mpsc::error::TrySendError::Closed(_)) => {
           let mut c = state.faucet_claims.lock().await; c.remove(&addr_key); c.remove(&ip_key);
           /* 500 */
       }
   }
   ```
   (`state.mempool` is the already-maintained `RpcState.mempool` snapshot the reviews cite. If that
   field is not present on `RpcState`, add it in the same atomic pair — it is non-protected.)
3. **Add a concurrency test** (slice 4 test module): two concurrent claims, same IP, distinct
   addresses → **exactly one** 200 and exactly one tx queued.
4. Record in the ops runbook either way: "restart the faucet node to reseed its nonce from chain
   state." Holding a tokio `Mutex` across `.await` is intended here and is safe.

### E4 — Unsigned height-0 peer blocks stay valid post-flip (MAJOR — consensus)

**Problem.** `verify_producer_signature_strict` (`block.rs:219-225`) admits ANY height-0 block with
empty sig+key, and `validate_block_from_peer` has no height-based rejection (`:1876-1978`; the
orphan branch at `:2028` is skipped for height 0 via `height > 0 &&`). Post-flip an attacker can
gossip arbitrary unsigned "genesis" blocks that pass Stage 1c, enter `add_candidate` at height 0,
and — combined with E2's fabricated-originator hole — be Snowball-finalized at height 0, driving
`fork_detector.record_mismatch` → `initiate_chain_resync`. Unsigned-block injection survives at
exactly one height after the batch whose whole point is eliminating unsigned-block injection.

**RESOLUTION (binding).** In the Stage 1c hunk in `validate_block_from_peer`, reject any
peer-received block at height 0 **before** the producer-signature check (a node always has its own
genesis before it has peers, so no legitimate peer sends one):
```rust
if block.height() == 0 {
    self.adjust_peer_score(source, -20);
    return false;
}
```
This keeps the strict-verifier genesis carve-out for the CLI/local genesis while removing it from
the network ingestion path. See the corrected Slice-1 Stage-1c hunk in §1.

### E5 — checkpoint_hash signature malleability is MANDATORY, not optional (MAJOR — consensus)

**Problem.** `signable_bytes` (`block.rs:77-91`) omits `checkpoint_hash` (set every 1000 blocks,
`event_loop.rs:2883-2889`). A relay can strip/alter `checkpoint_hash` on a legitimately signed
block; the signature stays valid but the block **hash** changes → two distinct validly-signed
blocks from the same producer at the same height → false equivocation attribution + same-height
candidate split. Slice 1 hunk 3 carries the fix but marks it "optional."

**RESOLUTION (binding).** Apply Slice 1 hunk 3 (append `checkpoint_hash` to `signable_bytes`). It is
consensus-affecting (the signature domain changes), which is precisely why it must ride this reset
— it is only cheap now. Verified self-consistent: sign and verify both go through this one function
(`signing.rs:38-43`, `block.rs:117`); `checkpoint_hash` is already set before `sign_block`
(`event_loop.rs:2884-2892`); no golden-vector test pins the byte layout (grep: `signable_bytes` used
only in `block.rs` + `signing.rs`). Do not down-rank to "residual."

### E6 — Sync limiter × unbounded gap-request flood → permanent peer exhaustion for late joiners (MAJOR — integration)

**Problem.** `apply_synced_block` fires `for h in expected..height { self.request_block(h) }`
(`event_loop.rs:3203-3205`) — unbounded, re-fired on every out-of-order block; `BlockAnnounce`
gossip also fires `request_block`. After Slice 3, all `GetBlock` **and** `GetBlocks` share ONE 10/s
token bucket per peer. A joiner a few hundred blocks behind self-drains its bucket at the single
seed continuously, so its legitimate `GetBlocks` batch (one per 5s tick, `SYNC_BATCH_SIZE=10`) gets
empty replies → `batch_timed_out` → `record_batch_failure` → at `MAX_PEER_FAILURES=10` the seed
enters `exhausted_peers` **permanently** → `select_peer` returns `None` → sync stalls forever with
one seed. The gate's 30s/~15-block late-join smoke cannot reach this regime; a real joiner an hour
in (~1800 blocks) will.

**RESOLUTION (binding).** Apply BOTH:
1. **Bound the gap-request loop** in `apply_synced_block` to ~one sync batch (new protected hunk in
   the stage-1a event_loop commit):
   ```rust
   // current:  for h in expected..height { self.request_block(h); }
   let gap_end = height.min(expected + SYNC_BATCH_SIZE as u64);
   for h in expected..gap_end {
       self.request_block(h);
   }
   ```
   (`SYNC_BATCH_SIZE` is already in scope.)
2. **Give `GetBlocks` its own token bucket** separate from `GetBlock` so batch sync cannot be
   starved by `GetBlock` noise (either a second `SyncRateLimiter` field, or a two-bucket limiter in
   the non-protected `sync_rate_limiter.rs`). At minimum, keep `GetBlock`+`GetBlocks` in one bucket
   only as a **deliberate** decision, not accidental (see E-minor note in §1 Slice 3), and prefer
   the split.
3. **Extend the late-join smoke** to a longer backlog (`SMOKE_LATE_DELAY` high enough to exceed 10
   batches of pre-join chain) so the regression is actually exercised (§3).

### E7 — Reconcile Slice 2 vs Slice 5 into ONE consensus_manager pre-stage shape (MAJOR — integration, minimality)

**Problem.** Slice 2 renames to `record_peer_response` AND makes `record_response` `#[cfg(test)]` —
which does NOT compile against an unmodified `event_loop` (4 live call sites: `event_loop.rs:1666,
1779, 1867, 3010`). Slice 5 stage 0b instead prescribes an additive `record_response_from_peer`
with the old method kept as a **live** delegating shim. Different name, different cfg policy —
there is no green-at-every-commit path through the plan as written.

**RESOLUTION (binding — ONE reconciled shape).**
- Pre-stage (stage 0b, non-protected): add `record_peer_response(height, preference, peer) -> bool`
  and the `aggregator` field; keep `record_response(height, preference)` as a **LIVE (non-cfg)**
  delegating shim that attributes each call to `PeerId::random()` (exact old semantics; `PeerId::random`
  precedent in-crate at `sync_machine.rs:290`). The tree compiles with an unmodified event_loop.
- In the founder's **stage-1a** event_loop commit: switch the 4 feed sites to `record_peer_response`
  **and add `#[cfg(test)]` to the shim** — in the SAME commit. This is the intended loud failure
  mode (once the shim is `cfg(test)`, an unmodified event_loop no longer compiles, guaranteeing no
  feed site is missed).
- The method name is `record_peer_response` (Slice 2's name), not Slice 5's `record_response_from_peer`.
  Slice 5's stage-0b prose is superseded by this amendment.

### E8 — Per-peer dedup tightens small-net finalization; the gate is a decision point, not a rubber stamp (MAJOR — integration, consensus)

**Problem.** `record_round`'s no-quorum arm resets `consecutive_count` to 0 (`snowball.rs:153-156`),
and Slice 2's `try_finalize_round` consumes (replaces) the aggregator on ANY non-empty tally.
Post-batch, quorum needs ≥2 **distinct** peers' votes landing within the same consensus-tick window
for `decision_threshold` (3–5) consecutive windows — exactly the regime of the 3-node smoke and the
2-node alpha bootstrap. A straggler vote alone in a window is both wasted and streak-resetting.

**RESOLUTION (binding).**
- Make BOTH a **2-node** (`SMOKE_NODES=2`) AND a **3-node** (`SMOKE_NODES=3`) `multinode_assert.sh`
  run **hard PASS/FAIL preconditions** of the reset (§3). Do not launch with
  `GATE_ALLOW_BELOW_BASELINE=1`.
- **Pre-agree the fallback** if either flakes: do NOT consume the aggregator on below-quorum
  tallies — only replace it when `record_round` actually saw a quorum (i.e. returned finalized), or
  age rounds out after N ticks — so votes accumulate across tick boundaries within a bounded round
  window. Keep this fallback ready as a one-hunk change to `try_finalize_round`.

### E9 — peer_hash has ~16 bits of entropy → sync-limiter eclipse-assist (MAJOR — minimality/security)

**Problem.** Slice 3's sync-limiter hunk reuses `peer.to_bytes()[..8].iter().fold(...)` "byte-for-
byte identical" to the consensus limiter (`event_loop.rs:1590, 1635, 1668`). For ed25519 peers,
`PeerId::to_bytes()` is `[0x00,0x24,0x08,0x01,0x12,0x20, key[0], key[1], ...]` — the first 6 bytes
are constant, so `bytes[..8]` contains only **2 bytes of key entropy** (≤65,536 buckets). An
attacker grinds ~65k keys (sub-second) to collide a victim's bucket and drains the shared 10/s
`SyncRateLimiter` bucket at every serving node → victim's `GetBlocks` return empty → a late-joining
victim cannot sync from anyone.

**RESOLUTION (binding).**
- In the new sync-limiter hunk, key the bucket on the **FULL** `to_bytes()` via `DefaultHasher`
  (same one-line footprint):
  ```rust
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  peer.to_bytes().hash(&mut h);
  let peer_hash = h.finish();
  ```
- **FLAG** the identical weak fold in `consensus_rate_limiter` (`event_loop.rs:1590/1635`) for the
  founder: since the stage-1a event_loop commit already exists, fixing it there is one line in the
  same pass. Whether to also fix it now is a **FOUNDER DECISION** (§6) — flag it explicitly rather
  than silently re-blessing the weak fold as "consistent." The vote aggregator itself is safe (it
  keys on the full `PeerId`, not `peer_hash`).

### E10 — Shrink the protected main.rs faucet block into a non-protected helper (MAJOR — minimality)

**Problem.** Slice 4's ~30-line faucet-provisioning block in PROTECTED `main.rs` (env-var read,
`Wallet::from_seed_phrase` derivation, address cross-check/warn, info log, nonce seeding) contains
no logic that requires `main.rs`.

**RESOLUTION (binding).** Move derivation + nonce seeding into a **non-protected, pre-stageable,
unit-testable** helper — `rpc::provision_faucet_from_env` (or a new module) returning
`(Option<Wallet>, u64)`:
```rust
/// E10 + D6: derive the faucet wallet from COMMPUTER_FAUCET_SEED and seed its
/// next nonce from on-chain state. Non-protected, pre-stageable, unit-testable.
/// Fail-closed: a seed that is SET but invalid returns Err (aborts boot).
pub fn provision_faucet_from_env(
    state: &commputer_storage::state::ChainState,
) -> anyhow::Result<(Option<commputer_core::wallet::Wallet>, u64)> {
    let mut phrase = match std::env::var("COMMPUTER_FAUCET_SEED") {
        Ok(p) => p,
        Err(_) => return Ok((None, 0)),
    };
    let wallet = commputer_core::wallet::Wallet::from_seed_phrase(phrase.trim())
        .map_err(|e| anyhow::anyhow!("COMMPUTER_FAUCET_SEED is set but invalid: {e}"))?;
    // E11: scrub the secret from memory and the process environment ASAP.
    use zeroize::Zeroize;
    phrase.zeroize();
    std::env::remove_var("COMMPUTER_FAUCET_SEED");
    let addr_hex = hex::encode(wallet.address().0);
    if crate::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX != Some(addr_hex.as_str()) {
        tracing::warn!(
            "Faucet wallet {} does not match compiled genesis allocation {:?} — \
             dispenses will queue but can never confirm",
            addr_hex, crate::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX);
    }
    tracing::info!("Faucet provisioned: {}", addr_hex);
    let next_nonce = state.accounts.get(wallet.address()).map(|a| a.nonce).unwrap_or(0);
    Ok((Some(wallet), next_nonce))
}
```
The PROTECTED `main.rs` hunk then shrinks to **one destructuring line** plus the two `RpcState`
literal fields:
```rust
let (faucet_wallet, faucet_next_nonce) = rpc::provision_faucet_from_env(&state)?;
// ... later in the RpcState literal:
//     faucet_wallet,
//     faucet_next_nonce: tokio::sync::Mutex::new(faucet_next_nonce),
```
Keep the fail-closed bail-on-invalid-seed semantics inside the helper. Confirm at pre-stage that
`rpc.rs` can name `ChainState` and read `state.accounts.get(..)` (it is in the same crate). Same
trimming applies to the long comment blocks proposed inside protected hunks — keep one-line
pointers to this doc instead of paragraphs.

### E11 — COMMPUTER_FAUCET_SEED secrets hygiene (MAJOR — minimality/security)

**Problem.** Slice 4's founder procedure says "`commputer wallet create` — seed never committed,"
but `cmd_wallet_create` (`main.rs:468-509`) SAVES the keystore to `~/.commputer/wallet/…` before
printing the 24 words — the faucet private key lands on disk. The plan also never says where
`COMMPUTER_FAUCET_SEED` is set: inline `export` leaks to shell history; a systemd unit leaks via
`systemctl show`/ops repos; it stays readable in `/proc/PID/environ`. The env-var `String` is never
zeroized.

**RESOLUTION (binding).**
- **Generate offline** on a throwaway HOME and shred the temp keystore:
  `HOME=$(mktemp -d) commputer wallet create` on an offline box → record the printed 24 words →
  `shred`/`rm -rf` the temp dir. (Or record only the printed words and never persist the keystore.)
- **Deliver** the seed via a **root-owned `0600` `EnvironmentFile`** outside any repo — never inline
  `export`, never in a committed systemd unit.
- **Scrub** in the provisioning helper (E10): `zeroize` the phrase after derivation and
  `std::env::remove_var("COMMPUTER_FAUCET_SEED")` once the wallet is built.
- Verified-good (keep as-is): the code never logs the phrase (only the derived address);
  `Wallet::from_seed_phrase`'s error reports word indices/counts, not phrase content, so the
  fail-closed message is safe; nothing in the plan commits the seed.

### E12 — Duplicate block.rs:338-346 test hunk → keep the const-aware form only (folded minor)

Slice 1 (hunk 2) and Slice 5 (hunk 1) both rewrite the SAME test (`block.rs:338-346`) with
different text: Slice 1 hardcodes `assert!(!b.verify_producer_signature())`; Slice 5 uses the
const-aware `assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES)`. **Keep Slice
5's const-aware form** (meaningful in both const states) and **DELETE Slice 1's** so the hunk sets
are disjoint. It rides the const flip in the same stage-0a commit.

### E13 — Sync-limiter warn-log throttling (folded minor)

`SyncRateLimiter` logs a `warn!` on EVERY rejected request (`sync_rate_limiter.rs:96-101`); under
the (now E6-bounded) gap-request flood that is still a flood of `warn` lines/sec on the seed. Add
per-peer log throttling (e.g. warn once per window) in the **non-protected** `sync_rate_limiter.rs`
before the wire-in. Pre-stageable in stage 0.

### E14 — CF-proxy collapses per-IP faucet buckets to one (folded minor — FOUNDER DECISION)

With any NON-loopback fronting proxy (the roadmap's Cloudflare plan), `rate_limit_client_ip`
collapses all claimants into one `ip:<proxy>` slot = 1 claim per epoch **network-wide**. This is
fail-closed but user-hostile, and CF-fronted RPC is the stated alpha topology. **FOUNDER DECISION**
(§6): it must be a decided precondition of the reset, not left open — either add the proxy to the
trusted set so `X-Forwarded-For`/`CF-Connecting-IP` is peeled (`rpc.rs:1078` is the single trust-set
source line), or accept per-address-only claim limiting and drop the per-IP key.

### E15 — Pin the test-count baselines by RUNNING the summing command (folded minor)

The gate's node-crate baseline (294) disagrees with `fab7e10`'s commit message (291). Do NOT trust
commit messages. **Before applying the batch**, pin every baseline by running the exact summing
command the gate uses:
```bash
cd src && cargo test -p commputer 2>&1 \
  | grep -oE 'test result: ok\. [0-9]+ passed' | grep -oE '[0-9]+' \
  | awk '{s+=$1} END {print s}'
```
Record the true pre-batch number for `commputer` (and re-confirm the other five crates the same
way). Use that recorded number in `enforcement_gate.sh`, not the disputed 294.

### E16 — Record founder blessing for pre-staging edits to existing non-protected files (folded minor)

Slices 1–3 pre-stage edits to existing non-protected files (`block.rs`, `consensus_manager.rs`,
`rpc.rs`, `lib.rs`, `vote_aggregator.rs`, `sync_rate_limiter.rs`) outside `src/staging/`. The repo's
"new files only" agent rule forbids that without founder prompting. This matches the established,
founder-supervised batch-A/B precedent on this same branch, but the blessing must be **explicit and
recorded BEFORE stage 0** (a FOUNDER DECISION, §6), not assumed. The one-line `lib.rs` registration
(Slice 3) and the corrected genesis-identity edits are included in that blessing.

---

## §1 — Slice-by-slice hunk tables

> Raw hunks reproduced from `wf1_maps`. Amendment annotations (`⚠ SEE AMENDMENT En`) mark every
> hunk that §0 overrides or deletes. Apply the un-annotated hunks as written; apply the annotated
> ones per the cited amendment.

### Slice 1 — Producer-signature enforcement

Signing is already fully wired (`handle_block_tick` signs every produced block at
`event_loop.rs:2892` via `sign_block(&mut block, &self.wallet)`). Verification is entirely missing:
`validate_block_from_peer` never checks producer signatures, and two candidate-entry paths bypass
validation. This slice flips `ENFORCE_PRODUCER_SIGNATURES`, adds the Stage-1c verify call, closes
both bypasses, and (per E5) covers `checkpoint_hash` in `signable_bytes`.

**Hunk 1.1 — `src/core/src/block.rs`, `ENFORCE_PRODUCER_SIGNATURES` const (lines 121-127) — NON-protected**
```rust
// current:
pub const ENFORCE_PRODUCER_SIGNATURES: bool = false;
// new:
pub const ENFORCE_PRODUCER_SIGNATURES: bool = true;
```
The core flip. `verify_producer_signature()` (`block.rs:198-207`) branches on this const; `true`
routes every call to `verify_producer_signature_strict()` (admits unsigned genesis, rejects any
other empty sig/key, then delegates to `BlockHeader::verify_signature`). Consensus-affecting; rides
the reset only.

**Hunk 1.2 — `src/core/src/block.rs`, `strict_rejects_unsigned_nongenesis` test (lines 338-346) — NON-protected**
`⚠ SEE AMENDMENT E12 — [deleted]`. Slice 1's version of this test is dropped; use Slice 5 hunk 1
(the const-aware `assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES)`) instead.

**Hunk 1.3 — `src/core/src/block.rs`, `BlockHeader::signable_bytes` (lines 77-91) — NON-protected**
`⚠ SEE AMENDMENT E5 — [mandatory, apply]`. Append `checkpoint_hash` to the signed bytes:
```rust
// new (tail of signable_bytes, after chain_id):
        borsh::BorshSerialize::serialize(&self.chain_id, &mut bytes).unwrap();
        // E5: cover checkpoint_hash so a relay cannot strip/alter it on a signed
        // block (block hash would change while the signature stays valid).
        borsh::BorshSerialize::serialize(&self.checkpoint_hash, &mut bytes).unwrap();
        bytes
```

**Hunk 1.4 — `src/node/src/event_loop.rs`, `validate_block_from_peer` doc comment (lines 1872-1875) — PROTECTED**
Doc-only: add `/// Stage 1c: Producer-signature enforcement (ENFORCE_PRODUCER_SIGNATURES)` to the
stage list so it stays truthful.

**Hunk 1.5 — `src/node/src/event_loop.rs`, Stage 2 boundary (lines 1946-1952) — PROTECTED**
`⚠ SEE AMENDMENT E4 — [corrected: add height-0 rejection before the sig check]`. Corrected new_code:
```rust
        // === Stage 1c: Producer-signature enforcement ===
        // E4: reject any peer-received block at height 0 outright — a node has
        // its own genesis before it has peers, so no honest peer sends one, and
        // the strict-verifier genesis carve-out would otherwise leave height 0
        // as the one unsigned-block injection point post-flip.
        if block.height() == 0 {
            self.adjust_peer_score(source, -20);
            return false;
        }
        // Post-reset every non-genesis block must carry a valid ed25519 signature
        // whose embedded public key hashes to the declared producer address.
        if !block.verify_producer_signature() {
            self.ban_peer(source, "sent block with missing/invalid producer signature");
            return false;
        }

        // === Stage 2: Merkle root verification ===

        // Check merkle roots.
        if !block.verify_roots() {
            self.ban_peer(source, "sent block with invalid merkle roots");
            return false;
        }
```
This is THE enforcement call — one ed25519 verify (~50µs) is cheaper than hashing up to 500 txs,
so forged blocks are rejected before expensive work. Uses `verify_producer_signature()` (not
`_strict`) so behavior stays keyed to the single const. Ban-vs-score for relayed forgeries is a
FOUNDER DECISION (§6); `ban_peer` matches the merkle-root/tx-signature precedents. **State the exact
rejection log line verbatim** — the rogue-binary drill (§3) greps for it.

**Hunk 1.6 — `src/node/src/event_loop.rs`, `BlockResponse` arm (lines 1790-1802) — PROTECTED**
Bypass #1: the gossip `BlockResponse` arm adds a consensus candidate with ZERO validation. Add the
validate-before-add guard:
```rust
                if height == requested_height && !self.state.blocks.contains(&block.hash()) {
                    if !self.validate_block_from_peer(&block, source) {
                        return;
                    }
                    self.consensus.add_candidate(block);
                    self.consensus.try_finalize_round(height, self.peer_ips.len());
                    self.try_apply_finalized(height);
                }
```

**Hunk 1.7 — `src/node/src/event_loop.rs`, `handle_received_block` orphan-pool branch (lines 2027-2047) — PROTECTED**
Bypass #2: validate BEFORE orphan-buffering (move the `validate_block_from_peer` call above the
orphan branch; delete the old call). `process_orphans` re-injects pooled blocks without
re-validation, so an orphan buffered pre-validation bypasses all checks. Verified safe to validate
parentless blocks (parent-timestamp/leader checks are parent-conditional). Behavior change is a
strict improvement (invalid orphans dropped immediately). Residual noted in §7 (order-independence
of the parent-timestamp check).

### Slice 2 — Vote-aggregator wiring (anti-Sybil dedup by PeerId)

Wires the inert `VoteAggregator` into the live vote path inside `ConsensusManager`
(non-protected), instantiated as `VoteAggregator<libp2p::PeerId>`, one per `HeightState`.

**Hunk 2.1 — `src/consensus/src/vote_aggregator.rs`, after `new()` (~line 46) — NON-protected**
Add `set_sample_size`:
```rust
    /// Update k when network size changes (mirrors `SnowballParams::sample_size`).
    /// Recorded votes are kept — only the sampling bound changes.
    pub fn set_sample_size(&mut self, sample_size: usize) {
        self.sample_size = sample_size;
    }
```
Required so live aggregators track the (1→20) `sample_size` re-scaling; a stale small `k` caps every
tally below the production quorum of 14 and deadlocks finalization.

**Hunk 2.2 — `src/node/src/consensus_manager.rs`, header imports (lines 7-9) — NON-protected**
Add `use commputer_consensus::VoteAggregator;` and `use libp2p::PeerId;` (both dependencies already
present; no Cargo.toml change).

**Hunk 2.3 — `src/node/src/consensus_manager.rs`, `struct HeightState` (lines 139-147) — NON-protected**
Replace the raw `round_responses: HashMap<BlockHash, usize>` counter with
`aggregator: VoteAggregator<PeerId>`. Per-HeightState ownership gives free lifecycle cleanup (dies
with its height via `take_finalized`/`cleanup_below`/`clear`). Replacing (not augmenting) keeps
exactly one counting path so the Sybil-vulnerable path is unreachable.

**Hunk 2.4 — `src/node/src/consensus_manager.rs`, lifecycle doc comment (~line 154) — NON-protected**
Doc-only: point step 3 at `record_peer_response()` keyed by authenticated PeerId.

**Hunk 2.5 — `src/node/src/consensus_manager.rs`, `update_params_for_network_size` loop (lines 215-221) — NON-protected**
In the per-height loop, add `state.aggregator.set_sample_size(sample);` alongside
`state.voter.set_params(...)` (uses `set_sample_size` to preserve in-flight votes mid-round).

**Hunk 2.6 — `src/node/src/consensus_manager.rs`, `add_candidate_inner` closure (lines 275-280) — NON-protected**
In the sole `HeightState` construction site, replace `round_responses: HashMap::new()` with
`aggregator: VoteAggregator::new(self.params.sample_size)`.

**Hunk 2.7 — `src/node/src/consensus_manager.rs`, `record_response` (lines 317-324) — NON-protected**
`⚠ SEE AMENDMENT E7 — [corrected: shim stays LIVE (non-cfg) during pre-stage]`. Add
`record_peer_response(height, preference, peer) -> bool` (routes through `VoteAggregator::record_vote`;
same-peer repeat is a no-op returning false; debug-log on dedup, no peer penalty). Keep
`record_response(height, preference)` as a delegating shim to `record_peer_response(.., PeerId::random())`
— **without** the `#[cfg(test)]` attribute while pre-staged in stage 0 (so an unmodified event_loop
compiles). The founder adds `#[cfg(test)]` to the shim in the stage-1a commit that switches the feed
sites. Corrected shim:
```rust
    pub fn record_peer_response(&mut self, height: u64, preference: BlockHash, peer: PeerId) -> bool {
        if let Some(state) = self.heights.get_mut(&height)
            && !state.voter.is_finalized() {
                let newly = state.aggregator.record_vote(height, preference, peer);
                if !newly {
                    debug!("Deduped duplicate Snowball vote from {} at height {} for {}",
                           peer, height, preference);
                }
                return newly;
            }
        false
    }

    /// E7: LIVE delegating shim during pre-stage; founder adds #[cfg(test)] in stage 1a.
    pub fn record_response(&mut self, height: u64, preference: BlockHash) {
        self.record_peer_response(height, preference, PeerId::random());
    }
```

**Hunk 2.8 — `src/node/src/consensus_manager.rs`, `try_finalize_round` vote block (lines 335-347) — NON-protected**
`⚠ SEE AMENDMENT E8 — [gate is a decision point; pre-agree the below-quorum fallback]`. Consume the
tally via `state.aggregator.tally(height, &mut rand::thread_rng())`; on a non-empty tally, reset the
round by replacing the aggregator with a fresh `VoteAggregator::new(self.params.sample_size)`, then
`record_round(&tally)`. Preserves Snowball β-consecutive-round semantics. **If the 2-node/3-node
gate flakes (E8), switch to the fallback: replace the aggregator only when `record_round` saw a
quorum** (do not consume on below-quorum tallies).

**Hunk 2.9 — `src/node/src/event_loop.rs`, TOPIC_CONSENSUS dispatch (lines 1235-1238) — PROTECTED**
Thread `message.source` (the SIGNED originator) into `handle_consensus_message(msg, propagation_source, message.source)`.
`propagation_source` is the relaying neighbour; attribution must use the signed author.

**Hunk 2.10 — `src/node/src/event_loop.rs`, `handle_consensus_message` signature (lines 1728-1730) — PROTECTED**
Add `originator: Option<libp2p::PeerId>` param (single caller; contained).

**Hunk 2.11 — `src/node/src/event_loop.rs`, `SnowballResponse` arm (lines 1778-1780) — PROTECTED**
`⚠ SEE AMENDMENT E2 — [delete this arm OR gate on connected peer — FOUNDER DECISION]`. Raw hunk
would call `record_peer_response(height, preference, originator.unwrap_or(source))`. Per E2, either
delete the arm or prepend the connected-peer gate before recording.

**Hunk 2.12 — `src/node/src/event_loop.rs`, `VoteResponse` arm (lines 1866-1868) — PROTECTED**
`⚠ SEE AMENDMENT E2 — [delete this arm OR gate on connected peer — FOUNDER DECISION]`. Identical
treatment to 2.11.

**Hunk 2.13 — `src/node/src/event_loop.rs`, `ConsensusResponse::Vote` arm (lines 1663-1671) — PROTECTED**
Primary rr path (unaffected by E2 — `peer` is noise-authenticated). Switch
`record_response(height, BlockHash(preference))` → `record_peer_response(height, BlockHash(preference), peer)`.
`voted_peers`/`health_monitor` bookkeeping left untouched.

**Hunk 2.14 — `src/node/src/event_loop.rs`, solo self-vote (lines 3007-3015) — PROTECTED**
Feed site 4: attribute to `self.network.local_peer_id` (a `pub PeerId` field). At `peer_count == 0`
params are (1,1,1); per-round aggregator reset lets each round's self-vote count fresh.

**Hunk 2.15 — `src/consensus/src/vote_aggregator.rs`, module header WIRING comment (lines 15-19) — NON-protected**
Doc-only: flip the "INERT" banner to "LIVE since the alpha-reset enforcement batch." Apply WITH the
batch, not before.

### Slice 3 — F-3 mempool quota + sync-serving rate-limiter wire-in

Two independent DoS gates. F-3 has no existing code — a new non-protected module supplies the pure
decision, leaving the protected `event_loop.rs` change at ~3 lines. The `SyncRateLimiter` component
already landed in the network crate (`ea7f24a`); only the event_loop wire-in is new.

**Hunk 3.1 — `src/node/src/mempool_quota.rs` — NEW FILE — NON-protected**
Full contents (pre-stageable; verify with `cargo test -p commputer mempool_quota` before stage 1):
```rust
// mempool_quota.rs — F-3: pure per-account mempool-quota decision.
//
// WHAT: One account can currently fill the whole mempool with contiguous-nonce
// txs (the nonce check forces contiguity but caps no count). This module holds
// the cap constant and the pure admit/reject decision, extracted for unit tests.
//
// WIRING (INERT until the enforcement batch): event_loop.rs (PROTECTED) calls
// commputer::mempool_quota::account_quota_ok(...) inside validate_tx_for_mempool,
// reusing the per-sender pending count the nonce check already computes. The
// count is DERIVED from pending_txs at admission (never a stateful counter map),
// so it cannot desync. FILES NEEDING CHANGES: event_loop.rs (protected) + the
// one-line `pub mod mempool_quota;` in lib.rs.

/// F-3: max pending (unconfirmed) txs a single `from` address may occupy.
/// 64 lets ~78 distinct senders fully share the 5000-slot pool; 64 * MINIMUM_FEE
/// (100_000 raw) = 6.4M raw committed per address per flood window.
pub const MAX_MEMPOOL_TXS_PER_ACCOUNT: usize = 64;

/// F-3: pure per-account mempool-quota decision.
/// Ok(()) = admit (below cap); Err(..) = reject (at/above cap).
#[inline]
pub fn account_quota_ok(pending_for_sender: usize, max_per_account: usize) -> Result<(), &'static str> {
    if pending_for_sender >= max_per_account {
        Err("per-account mempool quota exceeded")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{account_quota_ok, MAX_MEMPOOL_TXS_PER_ACCOUNT as CAP};
    use commputer_core::identity::Address;
    use commputer_core::transaction::{Transaction, TxKind};
    use commputer_core::token::Amount;
    use commputer_core::wallet::Wallet;
    use commputer_core::signing::sign_transaction;

    fn signed_transfer(wallet: &Wallet, nonce: u64) -> Transaction {
        let mut tx = Transaction {
            from: *wallet.address(), nonce,
            kind: TxKind::Transfer { to: Address([0x11u8; 32]), amount: Amount::from_comme(1) },
            fee: 100_000, signature: vec![], public_key: vec![], memo: None, timelock: None,
        };
        sign_transaction(&mut tx, wallet);
        tx
    }

    #[test] fn quota_admits_when_below_cap() {
        assert!(account_quota_ok(0, CAP).is_ok());
        assert!(account_quota_ok(CAP - 1, CAP).is_ok());
    }
    #[test] fn quota_rejects_at_cap() {
        assert_eq!(account_quota_ok(CAP, CAP), Err("per-account mempool quota exceeded"));
    }
    #[test] fn quota_rejects_above_cap() {
        assert_eq!(account_quota_ok(CAP + 100, CAP), Err("per-account mempool quota exceeded"));
    }
    #[test] fn quota_boundary_is_exact() {
        for p in 0..CAP { assert!(account_quota_ok(p, CAP).is_ok(), "pending {p} admitted"); }
        for p in CAP..(CAP + 5) { assert!(account_quota_ok(p, CAP).is_err(), "pending {p} rejected"); }
    }

    fn pending_for(pending: &[Transaction], from: &Address) -> usize {
        pending.iter().filter(|p| &p.from == from).count()
    }

    #[test] fn flooder_is_capped_but_other_sender_is_not() {
        let attacker = Wallet::generate();
        let victim = Wallet::generate();
        assert_ne!(attacker.address(), victim.address());
        let mut pool: Vec<Transaction> = Vec::new();
        for n in 0..(CAP as u64) {
            let tx = signed_transfer(&attacker, n);
            assert!(tx.verify(), "test tx must be a valid signed tx");
            pool.push(tx);
        }
        pool.push(signed_transfer(&victim, 0));
        assert_eq!(pending_for(&pool, attacker.address()), CAP);
        assert!(account_quota_ok(pending_for(&pool, attacker.address()), CAP).is_err());
        assert_eq!(pending_for(&pool, victim.address()), 1);
        assert!(account_quota_ok(pending_for(&pool, victim.address()), CAP).is_ok());
    }

    #[test] fn freeing_a_slot_reopens_the_quota() {
        let attacker = Wallet::generate();
        let mut pool: Vec<Transaction> = (0..(CAP as u64)).map(|n| signed_transfer(&attacker, n)).collect();
        assert!(account_quota_ok(pending_for(&pool, attacker.address()), CAP).is_err());
        pool.remove(0);
        assert!(account_quota_ok(pending_for(&pool, attacker.address()), CAP).is_ok());
    }
}
```

**Hunk 3.2 — `src/node/src/lib.rs`, end of module declarations (~line 11) — NON-protected**
Add `pub mod mempool_quota;`. (One-line edit to an existing file — covered by the E16 blessing.)

**Hunk 3.3 — `src/node/src/event_loop.rs`, `validate_tx_for_mempool` nonce block (lines 2176-2189) — PROTECTED**
`⚠ SEE AMENDMENT E3 — [corrected: exempt the compiled faucet address from the quota]`. Compute
`pending_from_sender` as before; then, per E3, apply the quota only when the sender is NOT the
faucet:
```rust
        let pending_from_sender = self.pending_txs.iter()
            .filter(|ptx| ptx.from == tx.from)
            .count();

        // F-3 per-account mempool quota (after the C7 ingress filter; reuses the
        // per-sender count from the nonce check). E3: exempt the compiled faucet
        // address — a trusted internal issuer whose nonce is serialized in rpc.rs.
        let faucet_exempt = crate::testnet_genesis::ALPHA_FAUCET_ADDRESS_HEX
            .and_then(|h| commputer_core::identity::Address::from_hex(h).ok())
            .is_some_and(|fa| fa == tx.from);
        if !faucet_exempt {
            commputer::mempool_quota::account_quota_ok(
                pending_from_sender,
                commputer::mempool_quota::MAX_MEMPOOL_TXS_PER_ACCOUNT,
            )?;
        }

        let expected_nonce = on_chain_nonce + pending_from_sender as u64;
        if tx.nonce != expected_nonce {
            return Err("invalid nonce");
        }
        Ok(())
```
One choke point covers both admission paths (RPC `:2091`, gossip `:2193`). REJECT (never evict —
eviction orphans higher contiguous nonces). Self-issued fee-exempt direct `pending_txs` pushes
(`auto_register_validator`, resync re-queue) intentionally bypass this.

**Hunk 3.4 — `src/node/src/event_loop.rs`, `EventLoop` field list (lines 209-212) — PROTECTED**
Add `pub sync_rate_limiter: commputer_network::sync_rate_limiter::SyncRateLimiter,` after
`consensus_rate_limiter` (same ownership precedent).

**Hunk 3.5 — `src/node/src/event_loop.rs`, `EventLoop::new` init (lines 287-288) — PROTECTED**
Add `sync_rate_limiter: commputer_network::sync_rate_limiter::SyncRateLimiter::new(),`.

**Hunk 3.6 — `src/node/src/event_loop.rs`, sync serve handler (lines 1501-1530) — PROTECTED**
`⚠ SEE AMENDMENT E9 (peer_hash full-bytes) + E6 (GetBlocks own bucket) + E13 (warn throttle)`. Gate
`GetBlock`/`GetBlocks` with `self.sync_rate_limiter.check(peer_hash)`; over-limit → cheap EMPTY
response (`Block(None)` / `Blocks(vec![])`), no ban. `GetHeight` stays ungated. Corrected
`peer_hash` derivation (E9):
```rust
                                use std::hash::{Hash, Hasher};
                                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                peer.to_bytes().hash(&mut hasher);
                                let peer_hash = hasher.finish();
```
Per E6, prefer a **separate** token bucket for `GetBlocks` vs `GetBlock`; if kept in one bucket, do
so deliberately and note it in the slice risk list. The empty-response path is the requester-
tolerated `Block(None)`/empty-`Blocks` code path (`event_loop.rs:1545-1563`).

### Slice 4 — Faucet D6 + genesis accounts

Enables a real funded faucet at the reset. Per E1, the allocation lives ONLY in `testnet_genesis.rs`
and is applied BEFORE `apply_block`. Per E3, the dispenser is serialized + faucet-exempt from F-3.
Per E10, provisioning moves into a non-protected helper; per E11, the seed is scrubbed.

**Hunk 4.1 — `src/node/src/testnet_genesis.rs`, end of `generate_testnet_genesis()` (~lines 74-79) — NON-protected**
The compiled allocation constants + `alpha_genesis_accounts()`. `Option<&str> = None` is fail-safe
(empty allocation → byte-identical genesis). Set `ALPHA_FAUCET_ALLOCATION` to the founder's single
COMME amount (E1, §6):
```rust
/// D6: hex address of the alpha-reset faucet account. Founder fills at reset.
pub const ALPHA_FAUCET_ADDRESS_HEX: Option<&str> = None;

/// D6: faucet funding at the alpha reset — founder-chosen COMME amount in raw units.
/// (Single source of truth per Amendment E1; root genesis.json must byte-match.)
pub const ALPHA_FAUCET_ALLOCATION: u64 = 100_000 * UNITS_PER_COMME;

/// Compiled height-0 allocation list, in the `(address_hex, raw_units)` shape
/// `ChainState::apply_genesis_accounts` takes.
pub fn alpha_genesis_accounts() -> Vec<(String, u64)> {
    match ALPHA_FAUCET_ADDRESS_HEX {
        Some(addr) => vec![(addr.to_string(), ALPHA_FAUCET_ALLOCATION)],
        None => Vec::new(),
    }
}
```

**Hunk 4.2 — `src/node/src/rpc.rs`, `RpcState` struct (lines 89-94) — NON-protected (compile-paired to main.rs)**
Add `faucet_wallet: Option<Wallet>` and `faucet_next_nonce: Mutex<u64>` (and, per E3, ensure a
`mempool` snapshot field exists for the in-flight count). Namespace the `faucet_claims` keys
`"addr:"`/`"ip:"`. **Build coupling:** adding fields breaks the `RpcState` literal in PROTECTED
`main.rs:1138` — this hunk lands only in the stage-1b atomic pair.

**Hunk 4.3 — `src/node/src/rpc.rs`, `build_faucet_transfer` doc + `#[allow(dead_code)]` (lines 724-734) — NON-protected**
Remove `#[allow(dead_code)]` and the INERT paragraph; the builder body is untouched.

**Hunk 4.4 — `src/node/src/rpc.rs`, `async fn faucet()` full replacement (lines 759-806) — NON-protected**
`⚠ SEE AMENDMENT E3 — [corrected: serialize the dispense; acquire the nonce lock first; in-flight
bound; provisional claim insert; rollback on send failure]` and `⚠ SEE AMENDMENT E14 — [per-IP key
collapses under a non-loopback proxy — FOUNDER DECISION]`. The raw hunk's ordering (testnet gate →
per-IP derivation via `rate_limit_client_ip` → 4 KiB body parse → address validate → read-only
claim check → honest 503 → nonce-lock across build+send → consume on Ok) is the right shape but the
claim check must move INSIDE the nonce critical section per E3 (see the E3 corrected block). Keep
`MAX_FAUCET_CLAIM_ENTRIES = 100_000` bounding, the honest 503 for unprovisioned wallet, and
`Full → retryable 503 (no consume)`, `Closed → 500`.

**Hunk 4.5 — `src/node/src/rpc.rs`, `make_rpc_state` test helper (line 1531) — NON-protected**
Factor into `make_rpc_state_with_faucet(Option<Wallet>)`; keep `make_rpc_state()` source-compatible.

**Hunk 4.6 — `src/node/src/rpc.rs`, test `RpcState` literal (lines 1561-1563) — NON-protected**
Init the new fields (`faucet_wallet`, `faucet_next_nonce: Mutex::new(0)`, and the `mempool` field if
added per E3).

**Hunk 4.7 — `src/node/src/rpc.rs`, dispense test (lines 1915-1919) — NON-protected**
Keep `faucet_dispenses_signed_transfer_when_provisioned` (200 + valid signed 1-COMME Transfer +
epoch re-claim refusal). **Add the E3 concurrency test** (two concurrent claims, same IP, distinct
addresses → exactly one 200).

**Hunk 4.8 — `src/node/src/main.rs`, run-path genesis application (lines 908-915) — PROTECTED**
Apply the compiled allocation BEFORE the genesis block (E1). Exactly ONE `apply_genesis_accounts`
call on this path:
```rust
    if state.blocks.is_empty() {
        // D6/E1: credit compiled height-0 allocations BEFORE apply_block so the
        // credit + total_emitted ride block 0's atomic RocksDB batch (crash-safe).
        let alloc = testnet_genesis::alpha_genesis_accounts();
        state.apply_genesis_accounts(&alloc)?;
        if !alloc.is_empty() { info!("Applied {} genesis account allocation(s)", alloc.len()); }
        let genesis = create_genesis();
        info!("Genesis block hash: {}", genesis.hash());
        state.apply_block(&genesis)?;
    } else {
```

**Hunk 4.9 — `src/node/src/main.rs`, `open_chain_state` (lines 452-462) — PROTECTED**
Same BEFORE-`apply_block` allocation on the CLI genesis path (fork-prevention parity). Exactly ONE
call here.

**Hunk 4.10 — `src/node/src/main.rs`, faucet provisioning (lines 1135-1140) — PROTECTED**
`⚠ SEE AMENDMENT E10 + E11 — [corrected: shrink to one call into the non-protected helper]`. Replace
the ~30-line block with:
```rust
    let (faucet_wallet, faucet_next_nonce) = rpc::provision_faucet_from_env(&state)?;
```
(Helper defined in E10; it also performs the E11 scrub.)

**Hunk 4.11 — `src/node/src/main.rs`, `RpcState` literal (lines 1162-1164) — PROTECTED**
Add the two literal fields:
```rust
        faucet_wallet,
        faucet_next_nonce: tokio::sync::Mutex::new(faucet_next_nonce),
```
(plus the `mempool` field if E3 adds one). Second and last `RpcState` literal in the workspace.

### Slice 5 — Batch shape, ordering, verification gate, genesis reset, rollback (META layer)

**Hunk 5.1 — `src/core/src/block.rs`, `strict_rejects_unsigned_nongenesis` (lines 338-346) — NON-protected**
`⚠ SEE AMENDMENT E12 — [KEEP this const-aware form; DELETE Slice 1 hunk 1.2]`. This is the surviving
test hunk:
```rust
        assert!(!b.verify_producer_signature_strict());
        // Const-aware: pre-flip accepts, post-flip rejects unsigned non-genesis.
        assert_eq!(b.verify_producer_signature(), !ENFORCE_PRODUCER_SIGNATURES);
```

**Hunk 5.2 — `src/core/src/genesis.rs`, `TESTNET_CHAIN_ID` (line 6) — NON-protected**
Bump `"commputer-testnet-1"` → founder's new chain-id string (proposal `"commputer-testnet-2"`).
Severs the old chain's identity (stamped into every header, `event_loop.rs:2869`).

**Hunk 5.3 — `src/core/src/genesis.rs`, `default_genesis().genesis_timestamp` (line 268) — NON-protected**
Bump the fixed `1774656000` → founder's reset-epoch timestamp (proposal `1783468800` = 2026-07-08
00:00:00 UTC). Changes the genesis hash so old, already-producer-signed blocks orphan at height 1 —
the actual defense against stale-chain re-infection (there is NO inbound chain_id check).

**Hunk 5.4 — `src/core/src/genesis.rs`, `default_genesis().accounts` (lines 270-272) — NON-protected**
`⚠ SEE AMENDMENT E1 — [DELETED]`. Do NOT compile any allocation into core. Leave `accounts: Vec::new()`.

**Hunk 5.5 — `src/core/src/genesis.rs`, `..._deserializes_to_empty` test (lines 340-343) — NON-protected**
`⚠ SEE AMENDMENT E1 — [DELETED]`. Leave the existing `assert!(default_genesis().accounts.is_empty())`
untouched (stays green because 5.4 is deleted).

**Hunk 5.6 — `src/node/src/config.rs`, `DEFAULT_TESTNET_CHAIN_ID` (line 12) — PROTECTED**
Lockstep copy of 5.2 (identical string). Founder applies in stage 1c. No compile coupling; keeps the
version banner/status honest.

**Hunk 5.7 — root `genesis.json` (lines 1-3) — PROTECTED**
`⚠ SEE AMENDMENT E1 — [if kept, byte-match the ONE mechanism: testnet_genesis.rs address+amount, the
new chain_id, the new timestamp; else drop entirely — zero runtime effect]`. Published reference
only. Its `accounts` tuple-array must equal `[["<faucet_addr_hex>", <ALPHA_FAUCET_ALLOCATION>]]`.

**Hunk 5.8 — `scripts/enforcement_gate.sh` — NEW FILE — NON-protected**
`⚠ SEE AMENDMENT E15 — [replace the hard-coded node baseline 294 with the RUN-and-summed value]`.
Otherwise apply as written; it encodes the whole gate (per-crate baselines, frozen-crate diff,
const canary, 3-node live gate, strict verify-chain, late-join sync). See §3 for the full command
set. Mark executable (`chmod +x`). Reconcile the node baseline per E15 before hard-gating; keep
`GATE_ALLOW_BELOW_BASELINE` as a documented escape, not the default. Per E6, extend the late-join
phase's `SMOKE_LATE_DELAY` to exceed 10 batches of backlog.

---

## §2 — Apply order & compile-coupling

**Precondition (E16):** the founder records explicit blessing to pre-stage edits to existing
non-protected files (`block.rs`, `genesis.rs`, `consensus_manager.rs`, `rpc.rs`, `lib.rs`,
`vote_aggregator.rs`, `sync_rate_limiter.rs`) outside `src/staging/`, and (E15) pins the true
pre-batch test baselines by running the summing command. Then:

### Stage 0 — pre-stage on `agent-testnet-20260707` (all NON-protected; green at every commit)
- **0a** `core/block.rs`: const flip (1.1) + const-aware test (5.1/E12) + `signable_bytes`
  `checkpoint_hash` (1.3/E5) — **one commit**. Behavior-inert except the offline `verify-chain` CLI
  goes strict (no live inbound caller).
- **0b** `consensus_manager.rs` + `vote_aggregator.rs`: additive `record_peer_response` +
  `aggregator` field (2.2–2.8) with the **LIVE (non-cfg)** `record_response` shim (E7) +
  `set_sample_size` (2.1) + module-header doc (2.15) — **one commit**. Unmodified event_loop still
  compiles.
- **0c** `mempool_quota.rs` new module (3.1) + `lib.rs` registration (3.2) — **one commit**.
  `cargo test -p commputer mempool_quota` must pass.
- **0d** `core/genesis.rs`: chain-id bump (5.2) + timestamp bump (5.3) — **one commit**. (5.4/5.5
  DELETED per E1; do not touch `accounts` or its test.)
- **0e** `testnet_genesis.rs`: `alpha_genesis_accounts()` + constants (4.1) — **one commit**.
- **0f** `rpc.rs`: `provision_faucet_from_env` helper (E10) + `MAX_FAUCET_CLAIM_ENTRIES` const +
  `build_faucet_transfer` un-`dead_code` (4.3) — **one commit**. NOTE: the `RpcState` field
  additions + corrected handler + tests (4.2, 4.4–4.7) are **NOT** pre-stageable (they break
  `main.rs`'s literal); keep them as a `.patch` for stage 1b.
- **0g** `sync_rate_limiter.rs` warn-throttle (E13) + `scripts/enforcement_gate.sh` (5.8, `chmod +x`,
  baseline per E15) — **one commit**.

### Stage 1 — founder applies to PROTECTED files, one sitting
- **1a — "event_loop enforcement" (one commit):**
  - Stage-1c producer-sig call + **height-0 rejection (E4)** + doc (1.4, 1.5).
  - Close both bypasses: `BlockResponse` validate (1.6), orphan-reorder (1.7).
  - Vote wiring: TOPIC_CONSENSUS dispatch (2.9), `handle_consensus_message` signature (2.10), rr Vote
    arm (2.13), solo self-vote (2.14); legacy arms **delete-or-gate (E2)** (2.11, 2.12); **add
    `#[cfg(test)]` to the `record_response` shim in `consensus_manager.rs` (E7) in this same commit**.
  - Sync limiter: field (3.4) + init (3.5) + serve gate with **full-bytes `peer_hash` (E9)** +
    **`GetBlocks` own bucket (E6)** (3.6); **bound the gap-request loop (E6)** in `apply_synced_block`.
  - F-3 quota wire-in with **faucet exemption (E3)** (3.3).
  - Optional in the same commit (FOUNDER DECISION, §6): fix the `consensus_rate_limiter` weak fold
    (E9); add a `header.chain_id` inbound check as defense-in-depth.
- **1b — ATOMIC PAIR (one commit): `rpc.rs` + `main.rs`.** Apply the `rpc.rs` `RpcState` fields +
  corrected E3 handler + tests (4.2, 4.4–4.7) AND the `main.rs` literal fields (4.11), the one-line
  provisioning call (4.10/E10), and the two `apply_genesis_accounts` calls (4.8, 4.9). The tree is
  RED between the two files (E0063) — apply both, then `cargo build --workspace`, then commit as ONE.
- **1c — `config.rs` (5.6) + root `genesis.json` (5.7/E1).** No build coupling; strings/values must
  byte-match the compiled truth.

### Stage 2 — run the verification gate (§3).

### Compile-coupling map
- `record_response` shim non-cfg in 0b → compiles; `#[cfg(test)]` added in 1a **same commit** as the
  feed-site switch (E7). Applying 0b's `#[cfg(test)]` early would break event_loop before stage 1.
- `rpc.rs` fields ↔ `main.rs` literal: bidirectional E0063 — **atomic pair 1b** (also breaks
  `multinode_smoke.sh`'s on-demand build, so even smoke testing is red between the two files).
- `block.rs` const flip ↔ `block.rs` test (5.1): same commit 0a, else core suite drops one.
- `mempool_quota.rs` ↔ `lib.rs`: same commit 0c; the `event_loop` call (3.3) lands in 1a.
- `SyncRateLimiter` type: already satisfied by the network crate at `ea7f24a`; only the event_loop
  side (3.4–3.6) + gap-bound (E6) are new.
- Producer-sig call ↔ core `verify_producer_signature`: no compile coupling (method exists);
  SEMANTIC coupling with the const flip — either alone is safe, both give enforcement.
- `apply_genesis_accounts` ↔ storage impl: already satisfied (`fab7e10`).

---

## §3 — Verification gate

Run from `src/`. **Pin baselines FIRST (E15)**, before applying anything:
```bash
cd src && cargo test -p commputer 2>&1 \
  | grep -oE 'test result: ok\. [0-9]+ passed' | grep -oE '[0-9]+' \
  | awk '{s+=$1} END {print s}'
```
Record that number for `commputer` and re-run per-crate for the other five. The disputed node
baseline is 294 (gate) vs 291 (`fab7e10` message) — the summed run is authoritative; do NOT launch
with `GATE_ALLOW_BELOW_BASELINE=1`.

**Per-crate suites (expect ≥ recorded baseline, 0 failures):** consensus 146 / network 77 /
core 216 / node 294* / storage 225 / pouw-onchain 88 (`*` reconcile 294-vs-291 per E15). Increments
come from slices 1–4 (mempool_quota tests, faucet tests, etc.) — sum the new tests at apply time.

**`scripts/enforcement_gate.sh`** rolls up: per-crate baselines → frozen-crate diff
(`git diff --quiet -- src/staging/pouw`) → const canary
(`grep 'ENFORCE_PRODUCER_SIGNATURES: bool = true'`) → live 3-node gate → strict verify-chain →
late-join sync gate.

**Multinode gate — HARD PASS/FAIL preconditions (E8):**
```bash
SMOKE_NODES=2 SMOKE_DURATION=90 FORCE_BUILD=1 bash scripts/multinode_assert.sh   # 2-node bootstrap
SMOKE_NODES=3 SMOKE_DURATION=90 FORCE_BUILD=1 bash scripts/multinode_assert.sh   # 3-node consensus
```
Both must PASS (height ≥ 8/node, spread ≤ 2, ≥ 12 "Snowball finalized", 0 panics). If either flakes,
apply the E8 below-quorum fallback and re-run.

**Strict verify-chain** over node1's smoke data (now meaningful — const is strict):
```bash
HOME=/tmp/multinode-smoke-1 src/target/debug/commputer verify-chain   # must print "0 errors"
```

**Extended late-join sync gate (E6)** — longer backlog than the default so the exhaustion regime is
exercised:
```bash
SMOKE_NODES=3 SMOKE_LATE_NODES=1 SMOKE_LATE_DELAY=<high enough to exceed 10 batches> \
  SMOKE_DURATION=120 bash scripts/multinode_assert.sh
```
The late node must catch up (never park in `exhausted_peers`).

**NEW targeted checks the units cannot provide:**
- **Unsigned-block REJECTED (2-node / rogue-binary drill).** FOUNDER-ONLY (it hand-edits a COPY of a
  protected file in a throwaway worktree): `git worktree add /tmp/rogue-drill HEAD`; in the worktree
  comment out `event_loop.rs:2892` (`sign_block`); `cargo build -p commputer`; run the rogue as a
  seedless bootstrap leader + one REAL node seeded to it. PASS = the real node logs the Slice-1
  rejection line, never logs "Snowball finalized" for rogue blocks, and its `/status` height stays 0;
  then `git worktree remove --force /tmp/rogue-drill`. (State the exact rejection log string in the
  Slice-1 hunk so the drill can grep it — §6.)
- **Concurrent faucet claim (E3).** The added rpc test: two concurrent claims, same IP, distinct
  addresses → exactly one 200, exactly one tx queued.
- **Faucet live check.** Before provisioning: `POST /faucet` → honest 503. After the reset: 200, and
  the recipient balance becomes the dispense amount within a few blocks (request shape per Slice 4).

---

## §4 — Genesis reset procedure

"Alpha genesis reset" = discard ALL persistent chain state everywhere; every node re-derives genesis
from the compiled `default_genesis()` in the NEW binary (deterministic: the bumped fixed timestamp +
chain id ⇒ identical genesis hash on all nodes; the compiled `alpha_genesis_accounts()` credits the
faucet at height 0 via the new `apply_genesis_accounts` call, so state roots agree iff binaries agree).

**Data locations** (per `config.rs`/`main.rs`; verify these paths against the actual `config.rs` at
apply time — if the layout has shifted, correct here before wiping):
- base `~/.commputer/`
- chain data `data_dir(testnet)` = `~/.commputer/testnet/` (RocksDB + `mempool.json`) — **WIPE**
- wallet `~/.commputer/wallet/` — **KEEP** (keys persist; balances are chain state and reset anyway)
- peer key `~/.commputer/peer_id` — **KEEP** (unless a fresh P2P identity is wanted)
- config `~/.commputer/config.toml` — **KEEP**
- local smoke dirs `/tmp/multinode-smoke-N` — wiped by the harness on each run

(If any of these paths cannot be confirmed from `config.rs`/`main.rs` at apply time, treat the exact
wipe target as a **TODO to resolve before the wipe** — do not guess.)

**Order:**
1. Apply the batch + pass the FULL gate (§3) on the build box.
2. Build release: `cd src && cargo build --release -p commputer --bin commputer`.
3. STOP every node (seed box: `systemctl stop commputer-seed`).
4. On EVERY node: `cp <old-binary> commputer.pre-enforcement` (rollback artifact), then
   `rm -rf ~/.commputer/testnet` — nothing else.
5. Install the new binary.
6. Start the SEED FIRST with NO `--seeds` (the bootstrap-leader gate in `handle_block_tick` lets only
   a seedless node produce the first block).
7. Start all other nodes with `--seeds` pointing at the seed.
8. Confirm `/status` heights converge and the faucet dispenses.

**Chain-id / genesis-hash implication.** The allocation is NOT bound into the genesis hash (genesis
`state_root` is fixed `[0;32]`, `apply_block` performs no state-root check), so two binaries differing
only in the allocation share a genesis hash and fork silently at the first state-root comparison.
Therefore: **one release build, publish its checksum, and forbid operators from self-building during
alpha.** The timestamp + chain-id bump (5.2/5.3) is what actually severs old history — any straggler
that skipped step 4 self-orphans instead of re-injecting the old chain.

---

## §5 — Rollback

**Pre-launch, nothing to preserve — revert = wipe + re-genesis on the OLD binary.**

If the reset network fails the gate (no finalization, panics, mass rejection):
1. Stop all nodes.
2. Reinstall `commputer.pre-enforcement`, OR rebuild from the tag `ea7f24a` — build from the TAG, not
   a partially-reverted tree: the `core/genesis.rs` identity hunks (5.2/5.3) change genesis identity,
   so a half-revert forks.
3. `rm -rf ~/.commputer/testnet` again.
4. Restart seed-first (step 6–7 of §4).
5. Git: restore protected files with
   `git checkout ea7f24a -- src/node/src/main.rs src/node/src/event_loop.rs src/node/src/config.rs genesis.json`
   (or revert the stage-1 commits). Pre-staged stage-0 commits may stay on the agent branch — they are
   inert without the protected wiring EXCEPT the `core/genesis.rs` identity hunks, which is precisely
   why the rollback binary must come from the pre-batch commit.

---

## §6 — FOUNDER DECISIONS REQUIRED

The batch cannot proceed without each of these:

- **Single faucet COMME amount** (E1) — set once in `testnet_genesis.rs::ALPHA_FAUCET_ALLOCATION`;
  Slice 4 proposes 100,000 COMME (`1e13` raw), Slice 5 proposed 10,000 COMME — pick one.
- **Faucet address** — fill `ALPHA_FAUCET_ADDRESS_HEX` from the offline-generated wallet (E11).
- **Delete vs. gate the legacy gossipsub vote arms** (E2) — delete entirely, or gate on
  `peer_ips.contains_key(originator)`.
- **CF-proxy per-IP faucet collapse policy** (E14) — trust the fronting proxy and peel
  `X-Forwarded-For`/`CF-Connecting-IP` (single source `rpc.rs:1078`), or accept per-address-only
  claim limiting.
- **Which reverse proxy fronts RPC** during alpha (drives E14 and the ConnectInfo/per-IP behavior).
- **Fix the `consensus_rate_limiter` weak fold now?** (E9) — one line in the stage-1a commit, or defer.
- **Explicit blessing to pre-stage edits to existing non-protected files** (E16) — record BEFORE
  stage 0.
- **Chain-id string + genesis timestamp** for the reset (5.2/5.3/5.6/5.7) — all four locations must
  carry identical values; proposals `"commputer-testnet-2"` / `1783468800`.
- **COMMPUTER_FAUCET_SEED format** — 24-word BIP39 phrase (the only public deterministic `Wallet`
  constructor) vs. raw 32-byte hex (would need `from_secret_bytes` made `pub` in core).
- **Dispense amount per claim** — `build_faucet_transfer` uses 1 COMME; the dead `faucet.rs` says 10.
- **Ban vs. score-penalty for relayed forgeries** (Slice 1 Stage-1c) — `ban_peer` (matches
  merkle-root precedent) vs. `adjust_peer_score(source, -20)`.
- **Add a `header.chain_id` inbound check** in `validate_block_from_peer` as defense-in-depth? (One
  protected line; recommended, outside the mapped slice mandate.)
- **Authorize the rogue-binary drill** (§3) — the only live proof the producer-sig call is wired; it
  hand-edits a COPY of a protected file in a throwaway worktree (founder-only).
- **Exact rejection log line** emitted by the Stage-1c hunk — must be stated verbatim so the drill
  can grep it.
- **Second allocation?** — `alpha_genesis_accounts()` takes a list; only the faucet entry is
  specified. Add a treasury/ops account at the same reset, or not.
- **`GetHeight` rate-limiting** — the plan leaves it ungated (8-byte liveness poll); confirm.

---

## §7 — Non-blocking residuals / notes (verified-good; do NOT "re-fix")

**Verified-good — leave as-is:**
- Gossipsub is `MessageAuthenticity::Signed` with default `ValidationMode::Strict`
  (`transport.rs:219-227`; no override anywhere in `network/src`). (Necessary but not sufficient for
  vote attribution — see E2.)
- `ConnectInfo` IS injected (`rpc.rs:1511-1512`), so per-IP faucet keys work in production behind a
  direct client.
- `/faucet` sits on the PUBLIC rate-limited tier (`rpc.rs:1415-1418`).
- Single-node bootstrap survives enforcement: own blocks enter via `add_local_candidate`
  (`event_loop.rs:2932`), never through `validate_block_from_peer`, and are signed at `:2892`; the
  solo (1,1,1) self-vote path finalizes.
- Historical sync blocks pass Stage 1c: `validate_block_from_peer` has no block-age/height-window
  rejection (timestamp check is future-only at `:1902-1906`; parent checks are conditional at `:1913`).
- The orphan-reorder hunk (1.7) is safe on the same grounds (parent-conditional checks).
- Requester-side sync load (1 batch per 5s tick, `SYNC_BATCH_SIZE=10`) never approaches the 10/s
  serve cap absent the gap-request flood (which E6 bounds).
- `checkpoint_hash` is set before `sign_block` (`event_loop.rs:2884-2892`), so the E5 fix is
  self-consistent; `signable_bytes` has no other callers.
- The `apply_genesis_accounts` guards pass in the BEFORE-`apply_block` ordering; the empty list is a
  verified no-op (`state.rs:758-761`).
- Exactly 4 live `record_response` feed sites and 2 `RpcState` literals, as slices 2/4 claim.

**Deferred (fast-follow, NOT part of this batch):**
- Defense-in-depth producer-sig check at apply time (`apply_block_validated`, `storage/state.rs:1043`
  checks roots + tx sigs but NOT producer sigs). Non-protected but breaks ~30 storage tests whose
  `validated_block()` helper builds unsigned blocks — needs a Wallet-based refactor. Recommend a
  separate non-blocking ticket, not the reset batch.
- Orphan-reorder order-independence: blocks validated while their parent is absent skip the
  parent-timestamp check (candidate acceptance becomes arrival-order dependent). Strictly better than
  today (orphans currently get NO validation); a cheap parent-timestamp re-check in `process_orphans`
  would restore order-independence.
- Third-party replay of legacy vote arms: a victim's signed `VoteResponse` can be re-published after
  the gossipsub duplicate cache and app-level `seen_message_ids` clear (`event_loop.rs:1208-1210`),
  re-attributing a stale vote to the current round. Bounded (only votes the victim actually cast);
  fixing E2 (delete arms, or key votes by the existing `round` field) closes it.
- `VerifyChain` CLI run against a PRE-reset DB with the flipped binary reports "invalid producer
  signature" for every legacy unsigned block. Acceptable because the reset wipes chain data; if old
  DBs must remain verifiable, add a `--legacy` flag (protected `main.rs`).
- Nonce self-heal: no loop detects "ahead vs pending-not-yet-mined" from `RpcState` alone; the heal is
  a faucet-node restart (counter reseeds from chain state). Single-provisioner invariant: exactly ONE
  node may carry `COMMPUTER_FAUCET_SEED` (two = colliding nonce counters).
- Per-epoch faucet cadence is tied to `ChainStatus.epoch`; run-path `epoch_duration` default is 3600s,
  so the effective rate is 1 claim per address AND per IP per hour. In-memory claims clear on restart
  (map now bounded at 100k with a past-epoch sweep).
- `SyncRateLimiter` buckets are never pruned (grows one entry per unique peer ever seen; bounded in
  practice by `MAX_PEERS≈50`). Acceptable for alpha; add an eviction sweep in the network crate later.
- `src/core/src/state/mod.rs` (legacy/parallel genesis at line 75) has no producer-signature calls;
  assumed dead/test-only but not fully traced.
