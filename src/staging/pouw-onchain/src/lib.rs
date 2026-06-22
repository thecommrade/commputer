//! P0 on-chain wiring adapters (decision-independent) — realizes the on-chain
//! wiring blueprint §6, phase P0. NEW staging crate: ready-to-wire code, not
//! signatures, for the founder to fold into the protected node/transaction path.
//!
//! Three modules:
//! - `exec_adapter` — the deterministic, fuel-metered `WasmOracle` replacing the
//!   wall-clock `node/src/wasm_executor.rs` stub.
//! - `jobspec_map` — bridges the post-G3 on-chain `SubmitJob` format to the
//!   staging `JobSpec`/`Job`.
//! - `escrow_ledger` — a reference `ChainHooks` impl with per-job escrow pots
//!   (what `storage/state.rs` must do under adopt-escrow, P1).
//!
//! Spec: src/staging/docs/2026-06-13-p0-onchain-adapters-spec.md

pub mod exec_adapter;
pub mod jobspec_map;
pub mod escrow_ledger;
pub mod settlement_resolution;
pub mod lifecycle;
pub mod escalation_round;
pub mod capacity;
pub mod consensus_params;
pub mod da_transport;
pub mod bonded_stake;
