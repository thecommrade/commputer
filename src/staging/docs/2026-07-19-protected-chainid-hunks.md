# Protected chain-id hunks — `commputer-testnet-3` (Task E, 2026-07-19 go-live batch)

**Status: PREPARED, NOT APPLIED.** These touch protected files (`src/node/src/config.rs`,
`commputer.toml`, root `genesis.json`) plus one companion file discovered while writing the
§1.12 equality test (`src/node/src/lib.rs` — not on the protected list, but out of this task's
assigned file scope, so it is bundled here for the same founder go/no-go rather than applied
unilaterally). Present all four for founder approval as one batch; apply together (a chain-id
bump is only coherent atomically).

Non-protected work already landed this pass (separate commit): `src/core/src/genesis.rs`
`TESTNET_CHAIN_ID` → `"commputer-testnet-3"` (consensus-enforced const, `state.rs:1161-1167`),
`genesis_timestamp` → `1784505600` (2026-07-20 00:00 UTC placeholder — founder sets the real
value at the reset ceremony), doctor crate literals, `scripts/seed.toml`.

---

## 1. `src/node/src/config.rs:12` (PROTECTED)

Display-only constant (`main.rs:438,1159,1485`, `rpc.rs:1702`) — not consensus-enforced, but
must track the real chain-id so operator-facing output isn't misleading.

```diff
- pub const DEFAULT_TESTNET_CHAIN_ID: &str = "commputer-testnet-2";
+ pub const DEFAULT_TESTNET_CHAIN_ID: &str = "commputer-testnet-3";
```

No visibility change needed — the const is already `pub` (checked before writing this doc).

## 2. `src/node/src/lib.rs` (not protected, but out of Task E's file scope — bundle with #1)

**Why this is needed:** while writing the §1.12 cross-crate equality test
(`src/node/tests/chain_id_equality.rs`), discovered that `commputer::config` is not reachable
from node integration tests at all today — `config.rs` is compiled only via the PROTECTED
`main.rs`'s private `mod config;` (binary-only module tree), never `pub mod`-declared in
`lib.rs`. Verified concretely: `use commputer::config::DEFAULT_TESTNET_CHAIN_ID;` in a probe
integration test fails to compile with
`error[E0432]: unresolved import 'commputer::config' — could not find 'config' in 'commputer'`
(confirmed by actually running it, then removing the probe file). This is one layer below a
value mismatch — the equality test cannot even build without this hunk, regardless of #1.

The fix mirrors an existing, already-established pattern in this exact file — several modules
(`validation`, `faucet`, `testnet_genesis`, `wizard`) are already declared in BOTH `main.rs`
(`mod X;`, binary-private) and `lib.rs` (`pub mod X;`, lib-public) so the same source file
compiles into both targets. Adding `config` to that list is additive and follows precedent;
it does not touch `main.rs` or `config.rs` themselves.

```diff
  pub mod fork_detector;
  pub mod validation;
  pub mod testnet_genesis;
  pub mod faucet;
  pub mod wizard;
  pub mod leader;
  pub mod node_state;
  pub mod sync_machine;
  pub mod chain_health_monitor;
  pub mod config_validator;
  pub mod kademlia_bootstrap_fix;
  pub mod block_maps;
  pub mod mempool_quota;
  pub mod peer_hash;
  pub mod da_store;
+ // §1.12 (2026-07-19): re-exported so node integration tests can reach
+ // `commputer::config::DEFAULT_TESTNET_CHAIN_ID` for the chain-id equality guard
+ // (`tests/chain_id_equality.rs`). Mirrors the existing dual bin+lib declaration
+ // pattern used above for validation/faucet/testnet_genesis/wizard.
+ pub mod config;
```

Compile-safety note: `config.rs` has a file-level `#![allow(dead_code)]` inner attribute
(applies to the module wherever it's mounted) and no `main.rs`-specific imports observed in a
skim of the file — re-mounting it as a second module instance under `lib.rs` should be a clean
duplicate compile, same as the four precedent modules. Re-verify with a full
`cargo build --workspace` after applying, before re-running the gate.

## 3. `commputer.toml:8` (PROTECTED)

```diff
- chain_id = "commputer-testnet-1"
+ chain_id = "commputer-testnet-3"
```

Note: this file was already stale at `-1` (one behind the current `-2` consensus const) before
this pass — not something this batch introduced, just carrying it forward correctly to `-3`.

## 4. Root `genesis.json:2` (PROTECTED)

```diff
  {
-   "chain_id": "commputer-testnet-2",
+   "chain_id": "commputer-testnet-3",
    "total_supply": 200000000000000000,
    ...
```

Changing this (or the `TESTNET_CHAIN_ID` const alone) changes the genesis hash — this MUST
land together with the reset ceremony's other genesis-affecting decisions (timestamp,
`genesis.rs` `genesis_timestamp` placeholder above), not standalone.

---

## Post-approval activation step

Once hunks #1 and #2 are applied (config.rs bumped + lib.rs exposes `pub mod config;`),
un-ignore the equality test:

```diff
  #[test]
- #[ignore = "activate when the founder applies (a) lib.rs `pub mod config;` and (b) the \
-             config.rs -3 hunk — see src/staging/docs/2026-07-19-protected-chainid-hunks.md; \
-             until then `commputer::config` does not even compile from node integration tests"]
  fn display_chain_id_matches_consensus_chain_id() {
-     // Placeholder equality — uncomment the import above and this line, delete the
-     // `assert!(true, ...)` fallback, once lib.rs exposes `pub mod config;`.
-     // assert_eq!(DEFAULT_TESTNET_CHAIN_ID, TESTNET_CHAIN_ID);
-     assert_eq!(TESTNET_CHAIN_ID, TESTNET_CHAIN_ID, "placeholder — see module doc comment");
+     assert_eq!(DEFAULT_TESTNET_CHAIN_ID, TESTNET_CHAIN_ID);
  }
```

And uncomment `use commputer::config::DEFAULT_TESTNET_CHAIN_ID;` at the top of
`src/node/tests/chain_id_equality.rs`. This is the batch-end gate mentioned in the go-live
plan's Task E: "fix any that hardcode `-2`" / un-ignore step.
