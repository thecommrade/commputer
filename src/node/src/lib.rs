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
// PoUW executor kernel (already compiled into the binary via main.rs). Re-exported through the lib
// so the inert executor planner + re-execution shim can call `commputer::pouw_executor::execute_job`.
pub mod pouw_executor;
// Track-2 Phase 1: inert, pure executor auto-claim planner + re-execution shim + DA-blob codec.
pub mod executor_planner;
// Track-2 Phase 1: inert verifier commit/reveal planner + durable fsync-before-broadcast salt store.
pub mod salt_store;
pub mod verifier_planner;
// Track-2 Phase 0: inert DA publisher — build_attestation over the program‖input envelope,
// persist every coded chunk into the DaStore keyed by its transport chunk_hash.
pub mod da_publisher;
