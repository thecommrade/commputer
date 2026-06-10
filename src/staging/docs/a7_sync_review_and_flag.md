# A7 — Hash-aware SyncMachine: real logic review + founder flag

**Status:** FLAG FOR FOUNDER (touches protected `event_loop.rs`; cannot be wired by an agent)
**Verdict:** Bug is **real and serious**. The v2 design is the **right shape** but is
**`partial` (sound core, real caveats)** — do **not** wire as-is. Address the 5 caveats below first.
**Reviewed:** 2026-06-10 on `agent-wire-testnet-20260610`, against the live tree (not the blueprint text).

> ⚠️ Corrects the overnight swarm's verdict. The swarm reviewer marked A7 "fabricated"
> because it checked the *filesystem* (empty — read-only agents never write to disk) and
> never evaluated the logic. That was a **false negative**. The logic was re-reviewed here.

---

## The bug is real (pre-mainnet correctness hole)

In the live `sync_machine.rs`, `complete_verification` (~line 219) collapses two distinct
outcomes at line ~231 (`if our_height >= new_target`): "we have genuinely caught up to the
canonical chain" and "we are sitting on an orphan/minority fork at the same height." Because
the sync wire carries only **bare heights, never block hashes** (`SyncResponse::Height(u64)`,
`sync_protocol.rs:32`), a node **cannot tell two peers at the same height apart when their
tip hashes differ**. Consequence: a node can finish sync **on a minority/dead fork and go
`Active`**, producing on a chain nobody else follows.

The v2 design is the first one here that can even *express* "two peers, same height,
different hash" — via `PeerTip{height,hash,peer}`, `record_tip(...)`, a Snowball-weighted
`ForkChoice`, and a typed `VerifyOutcome` (`Complete | KeepDownloading | Rollback | WipeAndResync`).
That is the correct direction.

## What's sound

- **Compiles / types check.** `PeerId: Copy` (confirmed by existing `for &peer in available`),
  `BlockHash` tuple-constructs and derives `Ord` (block.rs:13) so the fork-choice tie-break
  `a.0.1.0.cmp(b.0.1.0)` is valid. Tests use real `PeerId::random()` / `BlockHash([n;32])`,
  not tautologies.
- **Conservatism is safe.** When in doubt it over-reverts + re-downloads rather than accepting
  a bad tip. `revert_to` is bounded by `FINALITY_DEPTH = 10`.

## The 5 caveats — fix before wiring

1. **`record_height` shim is a TRAP (ordering hazard).** The compatibility shim maps
   `record_height(h) → record_tip(h, BlockHash::GENESIS, peer)`. If the **wire** (Step 2,
   non-protected `sync_protocol.rs`) is not upgraded to carry the hash **before** the
   protected `event_loop.rs` caller is switched to `record_tip` (Step 4), then **every peer
   reports GENESIS**, fork-choice "agrees" on a fake hash, and orphan detection is silently
   defeated — the old bug wearing a hash costume. **The non-protected wire bump must land
   first; the protected caller swap second.** Never ship Step 4 without Step 2.

2. **`fork_point` is conservative-only, not a true common-ancestor walk.** The wire carries
   only the *tip* hash, never historical hashes, so `fork_point` is always `c_height - 1` and
   `depth = our_height - (c_height-1)`. A deep reorg with a shallow true divergence gets
   **over-reverted**; the `FINALITY_DEPTH` escalation keys off `our_height`, not the real fork
   depth. Safe (over-revert + redownload) but imprecise — acceptable for bring-up, document it.

3. **Race the blueprint under-sells.** `our_tip` and `tip_reports` are sampled at different
   instants in the async loop; `apply_synced_block` advances the local tip on other
   `select!` arms between height responses and `complete_verification_v2`. A `Rollback` can
   fire against a **stale `c_height`**. Outcome is bounded (a refused revert falls through)
   but there is **no post-revert re-validation** — add one.

4. **Quorum clamp weakens lying-peer defense when peers are few.**
   `self.quorum.min(observed).max(observed/2 + 1)` ⇒ with `observed = 1`, effective quorum
   = 1, so a **single peer dictates the canonical tip** and can force a `Rollback` on a small
   testnet. Exactly the wrong behavior when peers are scarce. Add a floor (e.g. require ≥2
   independent tips before honoring a rollback), or gate rollback behind `peer_count ≥ k`.

5. **Integration is NOT additive — it requires protected edits.** Wiring touches
   **protected `event_loop.rs` at 3 sites**: responder fill (~1505), `record_tip` swap
   (~1541), and the `Verifying`-branch match (~847–853). Plus a **breaking wire change** in
   non-protected `sync_protocol.rs` (bump `/commputer/sync/1 → /2`) and a full-file patch of
   non-protected `sync_machine.rs`. The staged file itself correctly touches nothing.

## Recommended sequencing (founder, main session)

1. Land the non-protected pieces first, behind the new wire version:
   `sync_protocol.rs` (add hash to `SyncResponse`, bump to `/sync/2`) + `sync_machine_v2.rs`.
2. Add the post-revert re-validation (caveat 3) and the rollback quorum floor (caveat 4).
3. Only then switch the protected `event_loop.rs` callers to `record_tip` + the typed
   `VerifyOutcome` match (caveats 1 & 5). Build + test in the main session.
4. Add a fork/reorg/lying-peer integration test (the W5.10 scenarios this design unlocks)
   before trusting it on a real multi-host network.

**Bottom line:** keep A7 staged. It fixes a genuine pre-mainnet hole and is the right design,
but it is founder-only (protected files) and needs caveats 1–4 closed before it is safe.
