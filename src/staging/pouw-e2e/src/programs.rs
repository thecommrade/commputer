//! Deterministic guest programs assembled from .wat, plus content-addressing helpers.
//! DOUBLER is copied verbatim from commputer-pouw's wasm/oracle.rs test fixture.

use sha2::{Digest, Sha256};

/// A canonical input used by every scenario (kept tiny + fixed for determinism).
pub const DEFAULT_INPUT: &[u8] = &[1u8, 2, 3, 40];

/// Doubles every input byte. Exercises the full ABI (alloc for input, write, run,
/// alloc for output, packed-i64 return, read).
pub const DOUBLER: &str = r#"(module
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

/// A distinct program (triples instead of doubles) — used as "program B" in the tampered
/// publish scenario so its bytes hash to a different program_id than DOUBLER.
pub fn tripler_src() -> String {
    DOUBLER.replace("(i32.const 2)", "(i32.const 3)")
}

/// A guest whose `run` traps immediately (valid alloc, then `unreachable`). Drives the
/// error-outcome scenario: real WASM execution yields the error sentinel.
pub const TRAPPER: &str = r#"(module
    (memory (export "memory") 1 1)
    (global $next (mut i32) (i32.const 1024))
    (func $alloc (export "alloc") (param $len i32) (result i32)
        (local $ptr i32)
        (local.set $ptr (global.get $next))
        (global.set $next (i32.add (global.get $next) (local.get $len)))
        (local.get $ptr))
    (func (export "run") (param $ptr i32) (param $len i32) (result i64)
        unreachable)
)"#;

/// Assemble .wat source into a .wasm module (panics on bad fixture — these are static).
pub fn assemble(wat_src: &str) -> Vec<u8> {
    wat::parse_str(wat_src).expect("guest .wat assembles")
}

/// program_id = sha256(raw .wasm bytes) — the identity shared by DA, the game, and the store.
pub fn program_id(wasm: &[u8]) -> [u8; 32] {
    Sha256::digest(wasm).into()
}

/// input_hash = sha256(input) — committed in JobSpec.input_hash.
pub fn input_hash(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_pouw::wasm::{ProgramStore, WasmLimits, WasmOracle};
    use commputer_pouw::job::JobSpec;

    #[test]
    fn doubler_assembles_and_doubles_under_real_wasm() {
        let wasm = assemble(DOUBLER);
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm.clone());
        // The linchpin: the store key IS sha256 of the raw bytes.
        assert_eq!(program_hash, program_id(&wasm));
        let input = DEFAULT_INPUT.to_vec();
        let spec = JobSpec { program_hash, input_hash: input_hash(&input) };
        let oracle = WasmOracle::new(store, WasmLimits::default());
        let out = oracle.execute(&spec, &input);
        assert_eq!(out.result, Ok(vec![2, 4, 6, 80]), "doubles each byte");
    }

    #[test]
    fn tripler_is_a_distinct_program() {
        assert_ne!(program_id(&assemble(DOUBLER)), program_id(&assemble(&tripler_src())));
    }

    #[test]
    fn trapper_run_errs_but_alloc_works() {
        let wasm = assemble(TRAPPER);
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm);
        let input = DEFAULT_INPUT.to_vec();
        let spec = JobSpec { program_hash, input_hash: input_hash(&input) };
        let oracle = WasmOracle::new(store, WasmLimits::default());
        let out = oracle.execute(&spec, &input);
        assert!(out.result.is_err(), "run() traps → ExecError");
        assert!(out.fuel_consumed > 0, "alloc + instantiation burned fuel before the trap");
    }
}
