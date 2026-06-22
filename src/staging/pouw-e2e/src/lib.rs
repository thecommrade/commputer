//! End-to-end PoUW job-lifecycle harness.
//! Spec: src/staging/docs/2026-06-12-pouw-e2e-harness-design.md
//! Composes commputer-pouw (WASM runtime + fuel economics) and commputer-da (data
//! availability) into one job lifecycle and asserts conservation across 8 scenarios.

pub mod programs;
pub mod world;
pub mod scenarios;

#[cfg(test)]
mod smoke {
    #[test]
    fn both_siblings_link_and_wasm_feature_is_on() {
        // Proves commputer-da links:
        let _ = commputer_da::params::ChunkingParams::default();
        // Proves commputer-pouw links:
        let _ = commputer_pouw::ids::ParticipantId([0u8; 32]);
        // Proves the `wasm-runtime` feature is compiled in (the wasm module exists):
        assert_eq!(commputer_pouw::wasm::WasmLimits::default().fuel, 100_000_000);
    }
}
