//! §1.12 (2026-07-19 go-live batch, Task E): cross-crate chain-id equality guard.
//!
//! Asserts that the node's DISPLAY-only chain-id constant (`commputer::config::
//! DEFAULT_TESTNET_CHAIN_ID`) and the CONSENSUS chain-id constant (`commputer_core::
//! genesis::TESTNET_CHAIN_ID`, enforced per-block in `commputer_storage::state`) never
//! silently diverge. Divergence history: config.rs drifted to "-2" while genesis.rs/
//! genesis.json were bumped in a prior pass (see
//! `src/staging/docs/2026-07-08-protected-batch-application-plan.md` §1.12/§2.6) — this
//! test is the guard that was supposed to prevent exactly that and was never actually
//! landed. This pass (2026-07-19) bumps the CONSENSUS const to "commputer-testnet-3"
//! (`src/core/src/genesis.rs`, non-protected, founder-approved) but the DISPLAY const
//! lives in the PROTECTED `src/node/src/config.rs` and cannot be touched here.
//!
//! BLOCKER (found while writing this test, TDD-first): `commputer::config` is not
//! reachable from node integration tests at all today. `config.rs` is compiled only via
//! `mod config;` (private) in the PROTECTED `src/node/src/main.rs` binary target — it is
//! never `pub mod`-declared in `src/node/src/lib.rs`, so no `commputer::config` path
//! exists for the `commputer` LIBRARY crate that integration tests link against (unlike
//! e.g. `sync_machine`/`da_attestation`, which lib.rs re-exports for exactly this reason).
//! Verified: `use commputer::config::DEFAULT_TESTNET_CHAIN_ID;` fails with
//! `error[E0432]: unresolved import` / `could not find 'config' in the crate root`,
//! NOT a value-mismatch — i.e. it cannot even compile in the current tree, one layer
//! below what this pass was scoped to fix.
//!
//! Two protected-adjacent hunks must land before this activates (see
//! `src/staging/docs/2026-07-19-protected-chainid-hunks.md`):
//!   1. `src/node/src/lib.rs`: add `pub mod config;` (mirrors the existing dual bin+lib
//!      declaration pattern already used for `validation`/`faucet`/`testnet_genesis`/
//!      `wizard` — NOT in this task's assigned file list, so left unapplied pending a
//!      quick founder nod alongside item 2; lib.rs itself is not on the protected list,
//!      but touching the module boundary of a protected file's sibling is out of this
//!      task's scope).
//!   2. `src/node/src/config.rs:12`: `DEFAULT_TESTNET_CHAIN_ID` → `"commputer-testnet-3"`
//!      (PROTECTED, prepared-only per the go-live batch plan).
//!
//! Until both land, this test is `#[ignore]`d — that is the honest state marker. The
//! batch-end gate un-ignores it after founder approval. The commented-out import below
//! is what activates once (1) lands; uncomment it and delete this comment.
// use commputer::config::DEFAULT_TESTNET_CHAIN_ID;
use commputer_core::genesis::TESTNET_CHAIN_ID;

#[test]
#[ignore = "activate when the founder applies (a) lib.rs `pub mod config;` and (b) the \
            config.rs -3 hunk — see src/staging/docs/2026-07-19-protected-chainid-hunks.md; \
            until then `commputer::config` does not even compile from node integration tests"]
fn display_chain_id_matches_consensus_chain_id() {
    // Placeholder equality — uncomment the import above and this line, delete the
    // `assert!(true, ...)` fallback, once lib.rs exposes `pub mod config;`.
    // assert_eq!(DEFAULT_TESTNET_CHAIN_ID, TESTNET_CHAIN_ID);
    assert_eq!(TESTNET_CHAIN_ID, TESTNET_CHAIN_ID, "placeholder — see module doc comment");
}
