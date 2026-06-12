# F-3 Blueprint: Race-Free Per-Account Mempool Quota at Event-Loop Admission

Task: `A2-mempool-quota` / `F-3`
Author: read-only overnight agent (`agent-overnight-20260610`)
Status: BLUEPRINT ONLY — `src/node/src/event_loop.rs` is PROTECTED. The founder applies the edits below by hand on `main`.

---

## 0. TL;DR for the founder

A single account can currently fill the entire 5000-slot mempool by submitting 5000
sequential-nonce transactions. The strict nonce check does NOT cap the count — it only
forces a sender's pending txs to be contiguous. Fix: cap per-`from` pending txs at
`MAX_MEMPOOL_TXS_PER_ACCOUNT = 64` inside the ONE function both real admission paths
already call — `validate_tx_for_mempool` (event_loop.rs:2109). The count is **derived**
from `self.pending_txs` (exactly like the existing nonce-count at :2143-2145), NOT stored
in a separate stateful HashMap — so it can never desync across the ~10 sites that mutate
`pending_txs`. Extract the decision into a pure free function so it is unit-testable
without constructing a full `EventLoop`. The staged unit test below exercises real
`Transaction` / `Wallet` / `Address` types.

---

## 1. Verification of the reported problem (every claim re-checked against current code)

### 1.1 RPC path has no per-from quota — CONFIRMED
`src/node/src/rpc.rs`:
- `submit_tx` at **:125**.
- `tx.validate_shape()` at **:131** (W5.7 F-1, body-bomb guard).
- `tx.verify()` at **:143** (signature).
- `state.tx_sender.try_send(tx)` at **:156**.

There is NO per-`from` accounting anywhere in `submit_tx`. The only backpressure is the
bounded channel (`TrySendError::Full` -> 503) which is global, not per-account. CONFIRMED.

### 1.2 The authoritative mempool + admission + eviction live in event_loop.rs — CONFIRMED
`src/node/src/event_loop.rs`:
- `pub pending_txs: Vec<Transaction>` field declared at **:118**, initialised `Vec::new()` at **:244**.
- `fn validate_tx_for_mempool(&self, tx: &Transaction) -> Result<(), &'static str>` at **:2109**, returns `Ok(())` at **:2150**.
- `fn handle_new_transaction(&mut self, tx, source)` (gossip path) at **:2153**; pushes `self.pending_txs.push(tx)` at **:2188**, then `self.enforce_mempool_limit()` at **:2189**.
- `fn handle_rpc_transaction(&mut self, tx)` (THE RPC path) at **:2070**; calls `validate_tx_for_mempool` at **:2071**, pushes `self.pending_txs.push(tx)` at **:2104**.
- `const MAX_MEMPOOL_SIZE: usize = 5000` at **:2250**.
- `fn enforce_mempool_limit(&mut self)` at **:2253**; global lowest-fee eviction loop, `self.pending_txs.remove(min_idx)` at **:2260**.

Note the task summary said admission ~:2182 / eviction ~:2261 — those land inside the
functions named above; the function entry points are :2109 / :2153 / :2070 / :2253.

### 1.3 `Transaction.from` is an `Address` — CONFIRMED
- `src/core/src/transaction.rs:170`: `pub from: Address,`.
- `src/core/src/identity.rs:9`: `pub struct Address(pub [u8; 32]);` with derives
  `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Borsh*`
  (identity.rs:7-8). It is `Copy + Hash + Eq + Ord` — usable as a HashMap key AND directly
  comparable, so the derived-count approach needs no wrapper.

### 1.4 The RPC-only snapshot approach is correctly REJECTED — CONFIRMED as the right call
A snapshot read in `rpc.rs::submit_tx` would (a) lag the authoritative mempool and (b)
require a lock on the hot submit path. More importantly it would miss the gossip path
(`handle_new_transaction`), through which a peer can relay another node's flood. The fix
MUST live at event-loop admission. CONFIRMED.

### 1.5 NEW finding — the strict nonce check does NOT already bound per-account count
`validate_tx_for_mempool` at :2137-2149 computes:
```
let on_chain_nonce = self.state.accounts.get(&tx.from).map(|a| a.nonce).unwrap_or(0);
let pending_from_sender = self.pending_txs.iter().filter(|ptx| ptx.from == tx.from).count() as u64;
let expected_nonce = on_chain_nonce + pending_from_sender;
if tx.nonce != expected_nonce { return Err("invalid nonce"); }
```
Each accepted tx must carry nonce `= on_chain_nonce + (current pending count for sender)`.
That forces a sender's pending txs to be **contiguous** but places **no cap on how many**:
submit nonce N, then N+1 (pending count is now 1, so expected = N+1, accepted), then N+2,
... up to 5000. So one signer fills the global mempool and `enforce_mempool_limit` (which
evicts the GLOBAL lowest-fee tx, not the flooder's) cannot protect honest higher-nonce or
other-sender txs once the flooder pads fees. **The griefing vector is real.** F-3 stands.

### 1.6 ALL sites that mutate `pending_txs` (why a separate counter map is the wrong design)
From `grep -n pending_txs event_loop.rs`:
- :244 init; :342 `clear()`; :393 push (re-queue ValidatorRegister after resync);
- :472 snapshot read-only map; :593 `extend(txs)` (snapshot/bulk rebuild);
- :983/:986/:991 persist to disk (read-only); :1276 read-only iterate;
- :2104 push (RPC path); :2188 push (gossip path); :2260 `remove` (eviction);
- :2322 push (`auto_register_validator`, self, fee-exempt);
- :2568-2573 `retain` (expiry sweep); :2707 `std::mem::take` + :2722 rebuild + :2729
  `extend(overflow)` (block-building drain/rebuild); :3028 `retain` (prune finalized).

A separate `HashMap<Address, usize>` would have to be incremented/decremented correctly at
EVERY one of these ~10 write sites, including `std::mem::take` at :2707 and two `retain`
sweeps. Miss one and the counter silently desyncs and either permanently locks out a sender
or stops enforcing. The DERIVED count (recomputed from `pending_txs` at admission) is
immune to this entire class of bug and costs one extra O(pending) pass that is already
being paid for the nonce check directly above it. **Chosen design: derived count.**

---

## 2. The fix — exact edits to `src/node/src/event_loop.rs` (PROTECTED; founder applies)

### Edit A — add the constant next to `MAX_MEMPOOL_SIZE` (after line 2250)
Locate (event_loop.rs:2249-2250):
```rust
    /// Maximum number of transactions in the mempool.
    const MAX_MEMPOOL_SIZE: usize = 5000;
```
Insert immediately after it:
```rust
    /// F-3: Maximum number of pending (unconfirmed) transactions a single
    /// `from` address may occupy in the mempool at once.
    ///
    /// Rationale for 64:
    ///   * MAX_MEMPOOL_SIZE is 5000. A cap of 64 lets ~78 distinct senders fully
    ///     share the pool before any global pressure, so no single signer can
    ///     monopolise it (was: 1 signer could take all 5000 via contiguous nonces).
    ///   * 64 is comfortably above any honest burst: a normal wallet rarely has
    ///     more than a handful of unconfirmed txs, and a batched/automated sender
    ///     gets 64 in-flight nonces before being asked to wait one block.
    ///   * 64 * MINIMUM_FEE (100_000 raw = 1e-4 COMME) = 6_400_000 raw the
    ///     attacker must commit per address per flood window, so the spam also
    ///     carries a real (burned) fee cost.
    ///   * Power of two; cheap to reason about. Tune via this single constant.
    const MAX_MEMPOOL_TXS_PER_ACCOUNT: usize = 64;
```

### Edit B — add the pure decision helper (free function, NOT a method)
The quota decision is extracted into a pure `fn` so it is unit-testable with no `EventLoop`,
no tokio, no network. Place it at module scope (e.g. directly above the `impl EventLoop`
block, or anywhere in the file at top level). Suggested location: just after the `use`
block (the file already has top-level items), or immediately before `impl EventLoop`.

```rust
/// F-3: Pure per-account mempool-quota decision.
///
/// Given how many txs `from` already has pending and the cap, decide whether a
/// newly arriving tx from that account is admissible. Pure + total so it can be
/// unit-tested in isolation (see tests::f3_*).
///
/// Returns:
///   * `Ok(())`               — admit (account is below the cap).
///   * `Err("...")`          — reject (account is at or above the cap).
#[inline]
fn account_quota_ok(pending_for_sender: usize, max_per_account: usize) -> Result<(), &'static str> {
    if pending_for_sender >= max_per_account {
        Err("per-account mempool quota exceeded")
    } else {
        Ok(())
    }
}
```

Note: `&'static str` matches the existing `validate_tx_for_mempool` error type exactly
(:2109 returns `Result<(), &'static str>`), so the call site stays type-clean.

### Edit C — call the helper inside `validate_tx_for_mempool` (the single choke point)
This function is called by BOTH real admission paths (`handle_rpc_transaction` at :2071 and
`handle_new_transaction` at :2154), so one check here covers both. The nonce block already
computes the per-sender pending count — REUSE it; do not add a second O(n) scan.

Locate the existing nonce block (event_loop.rs:2137-2149):
```rust
        // Nonce validation: must match expected next nonce for sender.
        // Account for pending txs already in mempool from the same sender.
        let on_chain_nonce = self.state.accounts
            .get(&tx.from)
            .map(|a| a.nonce)
            .unwrap_or(0);
        let pending_from_sender = self.pending_txs.iter()
            .filter(|ptx| ptx.from == tx.from)
            .count() as u64;
        let expected_nonce = on_chain_nonce + pending_from_sender;
        if tx.nonce != expected_nonce {
            return Err("invalid nonce");
        }
        Ok(())
```
Replace it with (adds the quota check; reuses `pending_from_sender`):
```rust
        // Nonce validation: must match expected next nonce for sender.
        // Account for pending txs already in mempool from the same sender.
        let on_chain_nonce = self.state.accounts
            .get(&tx.from)
            .map(|a| a.nonce)
            .unwrap_or(0);
        let pending_from_sender = self.pending_txs.iter()
            .filter(|ptx| ptx.from == tx.from)
            .count();

        // F-3: per-account mempool quota. Reuses the per-sender pending count
        // already computed for the nonce check (no extra scan). Derived from the
        // authoritative `pending_txs` so it can never desync. Self-issued,
        // fee-exempt ValidatorRegister txs (auto_register_validator) bypass this
        // path entirely (they push directly to pending_txs), so genuine
        // registration is unaffected.
        account_quota_ok(pending_from_sender, Self::MAX_MEMPOOL_TXS_PER_ACCOUNT)?;

        let expected_nonce = on_chain_nonce + pending_from_sender as u64;
        if tx.nonce != expected_nonce {
            return Err("invalid nonce");
        }
        Ok(())
```
Changes in this block:
1. `pending_from_sender` is now `usize` (drop the `as u64`) so it feeds the helper directly;
   re-cast to `u64` only where it is added to `on_chain_nonce`.
2. New `account_quota_ok(...)?` line between the count and the nonce comparison. Placing it
   BEFORE the nonce comparison means an over-quota sender is rejected with the clearer
   "per-account mempool quota exceeded" rather than a confusing "invalid nonce".

That is the entire functional change. No new struct field, no new state, nothing to keep
in sync across the ~10 `pending_txs` mutation sites.

### Why "reject" and not "evict the submitter's own oldest" (decision recorded)
The task offered either reject OR evict-own-oldest. REJECT is correct here because:
* Nonces must stay contiguous (see 1.5). Evicting the submitter's OLDEST pending tx
  (lowest nonce) would orphan every higher-nonce tx already in the pool — they would become
  un-includable (their predecessor nonce is gone) and would be dropped as strictly-stale at
  block build (:2715). Evicting oldest is actively harmful under contiguous-nonce semantics.
* Reject is O(1) after the count, returns a clear error to the RPC caller, and the global
  `enforce_mempool_limit` (:2253) still handles aggregate pressure independently.
If a future design wanted bounded replacement it should evict the submitter's HIGHEST nonce
(newest) to preserve the contiguous prefix — but plain reject is simpler and sufficient for F-3.

---

## 3. Behavioural consequences / edge cases (founder review checklist)

1. **Block-build drain (:2707) frees the quota.** `std::mem::take` empties `pending_txs`;
   included txs are pruned at :3028 and never return, future-nonce txs are re-queued at
   :2722. After a block, the per-from derived count drops to the sender's still-pending
   future-nonce txs, so quota naturally refreshes each block. Correct, no extra code.
2. **`auto_register_validator` (:2322) and resync re-queue (:393) bypass the check.** They
   push to `pending_txs` directly without calling `validate_tx_for_mempool`. These are
   self-issued, fee-exempt, single-tx-per-event paths — intentionally exempt. No change.
3. **Expiry sweep (:2569) frees the quota** by retaining-out expired txs. Derived count
   follows automatically.
4. **No RPC contract change required for correctness**, but see Section 5 for an OPTIONAL
   rpc.rs nicety so the caller learns *why* it was rejected.
5. **Off-by-one:** with cap 64 a sender holding 0..=63 pending is admitted; the 65th
   arrival (pending==64) is rejected. `>=` in the helper makes 64 the hard ceiling.

---

## 4. Staged unit test (COMPLETE, ready to paste into event_loop.rs's test module)

There is currently NO `#[cfg(test)] mod tests` in event_loop.rs (verified: `grep` for
`mod tests` / `#[cfg(test)]` returns nothing in that file), which is exactly why the
decision was extracted into the pure `account_quota_ok` free function. The founder adds the
module below at the BOTTOM of event_loop.rs. The pure-helper tests need no `EventLoop`. Two
bonus tests build REAL signed transactions and assert the count-derivation semantics that
the helper consumes, so the test is not a tautology — it exercises real `Wallet`,
`sign_transaction`, `tx.verify()`, and `Address` equality.

```rust
#[cfg(test)]
mod f3_quota_tests {
    // The pure helper lives at module scope in event_loop.rs.
    use super::account_quota_ok;

    use commputer_core::identity::Address;
    use commputer_core::transaction::{Transaction, TxKind};
    use commputer_core::token::Amount;
    use commputer_core::wallet::Wallet;
    use commputer_core::signing::sign_transaction;

    const CAP: usize = 64; // mirror EventLoop::MAX_MEMPOOL_TXS_PER_ACCOUNT

    /// Build a real, signed, verifiable Transfer tx from `wallet` with `nonce`.
    fn signed_transfer(wallet: &Wallet, nonce: u64) -> Transaction {
        let mut tx = Transaction {
            from: *wallet.address(),
            nonce,
            kind: TxKind::Transfer {
                to: Address([0x11u8; 32]),
                amount: Amount::from_comme(1),
            },
            fee: 100_000, // == MINIMUM_FEE
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        };
        sign_transaction(&mut tx, wallet);
        tx
    }

    // ---- pure-helper behaviour (the actual F-3 decision) ----

    #[test]
    fn quota_admits_when_below_cap() {
        assert!(account_quota_ok(0, CAP).is_ok());
        assert!(account_quota_ok(1, CAP).is_ok());
        assert!(account_quota_ok(CAP - 1, CAP).is_ok());
    }

    #[test]
    fn quota_rejects_at_cap() {
        // pending == cap: the (cap+1)th tx must be rejected.
        assert_eq!(
            account_quota_ok(CAP, CAP),
            Err("per-account mempool quota exceeded"),
        );
    }

    #[test]
    fn quota_rejects_above_cap() {
        assert_eq!(
            account_quota_ok(CAP + 100, CAP),
            Err("per-account mempool quota exceeded"),
        );
    }

    #[test]
    fn quota_boundary_is_exact() {
        // Exhaustively confirm the >= boundary around the cap.
        for pending in 0..CAP {
            assert!(
                account_quota_ok(pending, CAP).is_ok(),
                "pending {pending} should be admitted",
            );
        }
        for pending in CAP..(CAP + 5) {
            assert!(
                account_quota_ok(pending, CAP).is_err(),
                "pending {pending} should be rejected",
            );
        }
    }

    // ---- realism guard: prove the count the helper consumes is computed the
    //      same way the admission path computes it, against REAL signed txs ----

    /// Mirrors the per-sender count in validate_tx_for_mempool (event_loop.rs:2143-2145):
    /// `pending.iter().filter(|p| p.from == from).count()`.
    fn pending_for(pending: &[Transaction], from: &Address) -> usize {
        pending.iter().filter(|p| &p.from == from).count()
    }

    #[test]
    fn flooder_is_capped_but_other_sender_is_not() {
        // Attacker fills exactly CAP contiguous-nonce slots; a victim has 1.
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

        // Attacker is exactly at the cap -> the next attacker tx is rejected.
        let attacker_pending = pending_for(&pool, attacker.address());
        assert_eq!(attacker_pending, CAP);
        assert!(account_quota_ok(attacker_pending, CAP).is_err());

        // Victim is far below the cap -> still admitted (no collateral lockout).
        let victim_pending = pending_for(&pool, victim.address());
        assert_eq!(victim_pending, 1);
        assert!(account_quota_ok(victim_pending, CAP).is_ok());
    }

    #[test]
    fn freeing_a_slot_reopens_the_quota() {
        // Simulates a block draining one of the flooder's txs: count drops below
        // cap and admission reopens. (Mirrors the post-block drain at :2707/:3028.)
        let attacker = Wallet::generate();
        let mut pool: Vec<Transaction> = (0..(CAP as u64))
            .map(|n| signed_transfer(&attacker, n))
            .collect();
        assert_eq!(pending_for(&pool, attacker.address()), CAP);
        assert!(account_quota_ok(pending_for(&pool, attacker.address()), CAP).is_err());

        pool.remove(0); // a block included/pruned the oldest
        assert_eq!(pending_for(&pool, attacker.address()), CAP - 1);
        assert!(account_quota_ok(pending_for(&pool, attacker.address()), CAP).is_ok());
    }
}
```

Why these are real (not tautological):
* `quota_*` tests pin the exact `>=` boundary semantics of the shipped helper.
* `flooder_is_capped_but_other_sender_is_not` builds CAP+1 REAL ed25519-signed txs via
  `Wallet::generate()` + `sign_transaction`, asserts `tx.verify()` is true, and proves that
  the per-sender count (computed identically to event_loop.rs:2143-2145) trips the cap for
  the flooder while leaving a second distinct `Address` untouched — the actual F-3 property.
* `freeing_a_slot_reopens_the_quota` proves the derived-count design self-heals after the
  post-block drain, which is the whole reason a separate stateful counter map was rejected.

Verified the test's API surface exists in the current tree:
* `Wallet::generate()` — wallet.rs:19; `wallet.address() -> &Address` — wallet.rs:62.
* `sign_transaction(&mut Transaction, &Wallet)` — signing.rs:30.
* `Transaction.verify() -> bool` — transaction.rs:213.
* `TxKind::Transfer { to: Address, amount: Amount }` — transaction.rs:18-21.
* `Amount::from_comme(u64)` — token.rs:105.
* `Address([u8;32])` is a tuple struct, `Copy + Eq` — identity.rs:7-9.
* `MINIMUM_FEE = 100_000` — transaction.rs:148 (so `fee: 100_000` passes the min-fee gate).

---

## 5. OPTIONAL companion (non-protected) — clearer RPC error surfacing

This is NOT required for the fix (event_loop rejection already drops the tx). But because
`rpc.rs::submit_tx` (:156) forwards over a channel and returns before the event loop runs,
the HTTP caller currently only ever sees the channel-level 503 ("queue full") and never the
per-account reason. If the founder wants the caller to learn about the quota, that requires
a response-channel round-trip from the event loop back to the RPC handler, which is a larger
change than F-3 needs. Recommendation: ship Edits A-C only; treat RPC error surfacing as a
separate ticket. (rpc.rs is non-protected and could be staged later as a full patch.)

---

## 6. Founder apply checklist

1. Add `MAX_MEMPOOL_TXS_PER_ACCOUNT` constant after event_loop.rs:2250 (Edit A).
2. Add the `account_quota_ok` free fn at module scope (Edit B).
3. In `validate_tx_for_mempool` (:2137-2149): make `pending_from_sender` a `usize`, insert
   `account_quota_ok(pending_from_sender, Self::MAX_MEMPOOL_TXS_PER_ACCOUNT)?;` before the
   nonce comparison, and re-cast to `u64` in the `expected_nonce` sum (Edit C).
4. Append the `f3_quota_tests` module to the bottom of event_loop.rs.
5. `cargo test -p commputer --lib f3_quota` (adjust crate name to the node crate) and
   `cargo test` for the workspace; confirm no existing nonce/mempool tests regress.
6. Manual sanity: spin a node, fire 65 sequential-nonce signed txs from one key via /tx,
   confirm the 65th is dropped at admission (log: "per-account mempool quota exceeded") and
   a tx from a second key is still accepted.
