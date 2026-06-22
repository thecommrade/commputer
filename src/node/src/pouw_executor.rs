//! PoUW deterministic executor (P0) — replaces the deleted wall-clock `wasm_executor`/`native_executor`
//! stubs with the real fuel-metered `WasmOracle` (via `commputer-pouw-onchain::exec_adapter`).
//!
//! `execute_job` deterministically runs a job's program against its input and returns the canonical
//! `result_hash` the verification game compares on — exactly `hash_parts(run_output)` (mirrors
//! `commputer-pouw` `engine.rs` `result_hash`), so a validator's `CompleteJob{result_hash}` equals the
//! committee's comparison value. No wall-clock; fuel-metered + deterministic across nodes.
//!
//! P0 SCOPE: this is the executor *unit* (given the program bytes + input). Its LIVE trigger — a
//! validator-executor loop that fetches the program bytes and submits the `CompleteJob` — is wired at
//! **P4**, when the DA transport actually delivers bytes (pre-P4 there is no byte source). At P0 the
//! bytes are "locally present" (the patch-spec's P0 stance); this function + its test prove the
//! deterministic-execution + linchpin properties (the P0 patch-spec "done-when").

// P0: the live caller (a validator-executor loop that fetches the program bytes via DA and submits
// CompleteJob{result_hash}) is wired at P4; until then `execute_job` is the tested-but-uncalled unit.
#![allow(dead_code)]

use commputer_pouw::ids::hash_parts;
use commputer_pouw::job::JobSpec;
use commputer_pouw::oracle::ExecutionOracle;
use commputer_pouw::wasm::{ProgramStore, WasmLimits};
use commputer_pouw_onchain::exec_adapter::{build_oracle, populate_from_da};

/// Why a job could not be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// The provided program bytes do not hash to the job's committed `program_hash` (the linchpin:
    /// `sha256(program_bytes) == program_hash`). A validator handed wrong/garbled bytes MUST refuse
    /// rather than execute and attest a result for the wrong program.
    ProgramHashMismatch { expected: [u8; 32], got: [u8; 32] },
}

/// Deterministically execute the job `(program_hash, input_hash)` against `program_bytes` + `input`,
/// returning the canonical `result_hash`. Refuses (linchpin) if the bytes don't match `program_hash`.
///
/// `input_hash` is enforced inside the oracle (it folds `sha256(input) != input_hash` into a
/// deterministic error digest), so a wrong input yields a deterministic — and detectably-wrong —
/// `result_hash` rather than a silent mismatch.
pub fn execute_job(
    program_hash: [u8; 32],
    input_hash: [u8; 32],
    program_bytes: &[u8],
    input: &[u8],
    limits: WasmLimits,
) -> Result<[u8; 32], ExecError> {
    // Populate the content-addressed store; `populate_from_da` returns sha256(bytes) — the linchpin.
    let mut store = ProgramStore::new();
    let got = populate_from_da(&mut store, program_bytes);
    if got != program_hash {
        return Err(ExecError::ProgramHashMismatch { expected: program_hash, got });
    }
    let oracle = build_oracle(store, limits);
    let spec = JobSpec { program_hash, input_hash };
    // `run` is the game's canonical execution surface; `hash_parts` folds it to the comparison value.
    let outcome_digest = oracle.run(&spec, input);
    Ok(hash_parts(&[&outcome_digest]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Doubles every input byte (verbatim from `pouw-onchain` exec_adapter's test fixture).
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

    /// Real deterministic execution (no wall-clock): same job → identical `result_hash` across
    /// independent runs; and the linchpin refuses program bytes that don't match `program_hash`.
    /// This is the P0 patch-spec "done-when" at the executor-unit level.
    #[test]
    fn executes_deterministically_and_enforces_the_linchpin() {
        let wasm = wat::parse_str(DOUBLER).expect("guest assembles");
        let program_hash: [u8; 32] = Sha256::digest(&wasm).into();
        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();

        // Deterministic: two independent executions agree on the canonical result_hash.
        let r1 = execute_job(program_hash, input_hash, &wasm, &input, WasmLimits::default())
            .expect("valid program executes");
        let r2 = execute_job(program_hash, input_hash, &wasm, &input, WasmLimits::default())
            .expect("valid program executes");
        assert_eq!(r1, r2, "result_hash is deterministic across runs (consensus-equal)");

        // Linchpin: bytes that don't hash to program_hash are refused (not executed).
        let wrong_bytes = b"\x00asm\x01\x00\x00\x00 not the committed program".to_vec();
        let got = execute_job(program_hash, input_hash, &wrong_bytes, &input, WasmLimits::default());
        assert!(
            matches!(got, Err(ExecError::ProgramHashMismatch { .. })),
            "linchpin: wrong program bytes are refused, got {got:?}"
        );
    }
}
