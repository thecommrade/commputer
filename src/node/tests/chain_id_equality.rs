//! §1.12 (2026-07-19 go-live batch, Task E): cross-crate chain-id equality guard.
//!
//! Asserts that the node's DISPLAY-only chain-id constant (`commputer::config::
//! DEFAULT_TESTNET_CHAIN_ID`) and the CONSENSUS chain-id constant (`commputer_core::
//! genesis::TESTNET_CHAIN_ID`, enforced per-block in `commputer_storage::state`) never
//! silently diverge. Divergence history: config.rs drifted to "-2" while genesis.rs/
//! genesis.json were bumped in a prior pass (see
//! `src/staging/docs/2026-07-08-protected-batch-application-plan.md` §1.12/§2.6) — this
//! test is the guard that was supposed to prevent exactly that. ACTIVATED 2026-07-19:
//! the founder approved and applied the two enabling hunks (lib.rs `pub mod config;` +
//! config.rs `-3`) — see `src/staging/docs/2026-07-19-protected-chainid-hunks.md`.
use commputer::config::DEFAULT_TESTNET_CHAIN_ID;
use commputer_core::genesis::TESTNET_CHAIN_ID;

#[test]
fn display_chain_id_matches_consensus_chain_id() {
    assert_eq!(DEFAULT_TESTNET_CHAIN_ID, TESTNET_CHAIN_ID);
}
