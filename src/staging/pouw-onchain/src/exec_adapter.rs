//! Module 1 — the deterministic execution adapter (decision-independent).
//!
//! Replaces the wall-clock `node/src/wasm_executor.rs` stub with the deterministic,
//! fuel-metered `WasmOracle` from `commputer-pouw` (feature `wasm-runtime`).
//!
//! WIRE-IN (founder patch-spec): `node/src/main.rs` constructs the oracle via
//! `build_oracle`; the consensus path populates the store ONLY via `populate_from_da`
//! (DA-reconstructed + sha256-rebound bytes) — never a raw local insert of un-rebound
//! bytes. The program_hash returned is the linchpin: it == sha256(bytes) == the
//! `ProgramStore` key == `JobSpec.program_hash`.
//!
//! Existing file to delete: `node/src/wasm_executor.rs` (replaced by `WasmOracle`).

use commputer_pouw::wasm::{ProgramStore, WasmLimits, WasmOracle};

/// Construct the deterministic, fuel-metered oracle from a populated store.
pub fn build_oracle(programs: ProgramStore, limits: WasmLimits) -> WasmOracle {
    WasmOracle::new(programs, limits)
}

/// Insert DA-reconstructed+rebound bytes into the store; returns the program_hash
/// (== sha256(bytes) == the `ProgramStore` key == the linchpin). The ONLY way the store
/// is populated on the consensus path — never a raw local insert of un-rebound bytes.
pub fn populate_from_da(store: &mut ProgramStore, rebound_bytes: &[u8]) -> [u8; 32] {
    // `ProgramStore::insert` content-addresses by sha256 of the raw bytes — exactly the
    // linchpin identity shared by DA, the game, and the store. We pass the bytes through
    // it so there is one (and only one) hashing surface on the consensus path.
    store.insert(rebound_bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_pouw::job::JobSpec;
    use commputer_pouw::wasm::WasmLimits;
    use sha2::{Digest, Sha256};

    /// Doubles every input byte. Copied verbatim from `pouw-e2e`'s `programs::DOUBLER`
    /// (which itself mirrors the `commputer-pouw` wasm/oracle.rs fixture).
    const DOUBLER: &str = r#"(module
        (memory (export "memory") 1 1)
        (global $next (mut i32) (i32.const 1024))
        (func $alloc (export "alloc") (param $len i32) (result i32)
            (local $ptr i32)
            (local.set $ptr (global.get $next))
            (global.set $next (i32.add (global.get $next) (local.get $len)))
            (local.get $ptr))
        (func (export "run") (param $ptr i32) (param $len i32) (result i64)
            (local $out i32) (local $i i32)
            (local.set $out (call $alloc (local.get $len)))
            (block $done (loop $loop
                (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                (i32.store8
                    (i32.add (local.get $out) (local.get $i))
                    (i32.mul (i32.const 2)
                        (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)))
            (i64.or
                (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32))
                (i64.extend_i32_u (local.get $len))))
    )"#;

    fn assemble(src: &str) -> Vec<u8> {
        wat::parse_str(src).expect("guest .wat assembles")
    }

    /// `build_oracle` + a DOUBLER guest executes and doubles input bytes.
    #[test]
    fn build_oracle_runs_a_doubler_guest() {
        let wasm = assemble(DOUBLER);
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm.clone());
        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let spec = JobSpec { program_hash, input_hash };

        let oracle = build_oracle(store, WasmLimits::default());
        let out = oracle.execute(&spec, &input);
        assert_eq!(out.result, Ok(vec![2u8, 4, 6, 80]), "doubles each byte");
    }

    /// `populate_from_da` returns `sha256(bytes)` and the oracle then resolves that
    /// program_hash (the linchpin: DA bytes -> store key -> execution).
    #[test]
    fn populate_from_da_is_sha256_and_resolves() {
        let wasm = assemble(DOUBLER);
        let expected: [u8; 32] = Sha256::digest(&wasm).into();

        let mut store = ProgramStore::new();
        let program_hash = populate_from_da(&mut store, &wasm);
        assert_eq!(program_hash, expected, "program_hash == sha256(rebound bytes)");

        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let spec = JobSpec { program_hash, input_hash };
        let oracle = build_oracle(store, WasmLimits::default());
        let out = oracle.execute(&spec, &input);
        assert_eq!(out.result, Ok(vec![2u8, 4, 6, 80]), "DA-populated program resolves and runs");
    }

    /// A `WasmOracle` built from the store reports `fuel_consumed > 0` and is
    /// deterministic across two independent builds (digest + fuel equal) — the
    /// linchpin + determinism, end to end.
    #[test]
    fn two_independent_da_builds_agree_with_nonzero_fuel() {
        let wasm = assemble(DOUBLER);
        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();

        let build = || {
            let mut store = ProgramStore::new();
            let program_hash = populate_from_da(&mut store, &wasm);
            let oracle = build_oracle(store, WasmLimits::default());
            let spec = JobSpec { program_hash, input_hash };
            oracle.execute(&spec, &input)
        };

        let a = build();
        let b = build();
        assert!(a.fuel_consumed > 0, "real execution must consume fuel");
        assert_eq!(a.result, b.result, "two independent builds agree on the result digest");
        assert_eq!(a.fuel_consumed, b.fuel_consumed, "fuel is consensus-equal across builds");
    }
}
