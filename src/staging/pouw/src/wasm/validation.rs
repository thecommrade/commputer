//! The determinism gate (spec §5): allow-list feature validation + targeted
//! scans. THE authoritative guarantee behind the a==b equivalence oracle —
//! every reject is deterministic (same verdict on every node).
//! New file; wired via wasm/mod.rs. No existing-file changes.

use crate::wasm::error::ExecError;
use crate::wasm::limits::WasmLimits;
use wasmparser::{ExternalKind, Operator, Parser, Payload, Validator, WasmFeatures};

/// The allow-list (spec §5): integer WASM1 + the deterministic extensions Rust
/// toolchains emit by default. FLOATS is explicitly subtracted — WASM1 includes
/// it (verified empirically against wasmparser 0.228). Everything else (SIMD,
/// relaxed-SIMD, threads, reference-types, multi-value, tail-call, GC,
/// memory64, ...) is OFF and rejects in layer 1.
/// CONSENSUS-COUPLING: any change to these bits is a validation-policy change —
/// bump `VALIDATION_VERSION` in limits.rs in the same commit, or drift will not
/// fail loud (the fingerprint folds the version, not these bits).
pub const GATE_FEATURES: WasmFeatures = WasmFeatures::WASM1
    .union(WasmFeatures::BULK_MEMORY)
    .union(WasmFeatures::SIGN_EXTENSION)
    .difference(WasmFeatures::FLOATS);

const WASM_PAGE_BYTES: u64 = 65_536;

fn reject<T>(why: impl Into<String>) -> Result<T, ExecError> {
    Err(ExecError::Rejected(why.into()))
}

/// Run the full gate over raw module bytes. Both executor and every verifier
/// call this before instantiation.
pub fn validate_module(bytes: &[u8], limits: &WasmLimits) -> Result<(), ExecError> {
    // Layer 1: spec-level validation under the locked feature set.
    let mut validator = Validator::new_with_features(GATE_FEATURES);
    if let Err(e) = validator.validate_all(bytes) {
        return reject(format!("feature gate: {e}"));
    }

    // Layer 2: targeted scans for constructs feature flags cannot express.
    let mut saw_own_memory = false;
    let (mut export_memory, mut export_alloc, mut export_run) = (false, false, false);

    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
        match payload {
            // Rule 7: zero imports — the entire host nondeterminism surface is
            // structurally absent, not denied-by-config.
            Payload::ImportSection(_) => return reject("import section present (zero-import ABI)"),
            // Rule 9: no start section — the ABI calls are the only execution path.
            Payload::StartSection { .. } => return reject("start section present"),
            // Rules 5/6: fixed memory within the shared cap.
            Payload::MemorySection(reader) => {
                for mem in reader {
                    let mem = mem.map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
                    saw_own_memory = true;
                    match mem.maximum {
                        Some(max) if max == mem.initial => {}
                        _ => return reject("memory min==max required (growth impossible by construction)"),
                    }
                    if mem.initial.saturating_mul(WASM_PAGE_BYTES) > limits.max_memory_bytes {
                        return reject(format!(
                            "memory of {} pages exceeds the shared cap of {} bytes",
                            mem.initial, limits.max_memory_bytes
                        ));
                    }
                }
            }
            // Rule 5 (tables): fixed size, same reasoning.
            Payload::TableSection(reader) => {
                for table in reader {
                    let table = table.map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
                    match table.ty.maximum {
                        Some(max) if max == table.ty.initial => {}
                        _ => return reject("table min==max required"),
                    }
                }
            }
            // Rule 8: required export names + kinds. Exact SIGNATURES are
            // enforced at typed binding (abi.rs) and fold to Rejected there.
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
                    match (export.name, export.kind) {
                        ("memory", ExternalKind::Memory) => export_memory = true,
                        ("alloc", ExternalKind::Func) => export_alloc = true,
                        ("run", ExternalKind::Func) => export_run = true,
                        _ => {} // extra exports are harmless (e.g. __heap_base)
                    }
                }
            }
            // Rule 4: no grow opcodes anywhere.
            Payload::CodeSectionEntry(body) => {
                let mut ops = body
                    .get_operators_reader()
                    .map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
                while !ops.eof() {
                    let op = ops.read().map_err(|e| ExecError::Rejected(format!("parse: {e}")))?;
                    match op {
                        Operator::MemoryGrow { .. } => return reject("memory.grow is forbidden"),
                        Operator::TableGrow { .. } => return reject("table.grow is forbidden"),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_own_memory {
        return reject("module must define its own memory (export `memory` required)");
    }
    if !(export_memory && export_alloc && export_run) {
        let mut missing = Vec::new();
        if !export_memory { missing.push("memory"); }
        if !export_alloc { missing.push("alloc"); }
        if !export_run { missing.push("run"); }
        return reject(format!("required exports missing: {}", missing.join(", ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::limits::WasmLimits;

    fn wat(s: &str) -> Vec<u8> {
        wat::parse_str(s).expect("fixture wat must assemble")
    }
    fn ok(s: &str) {
        validate_module(&wat(s), &WasmLimits::default()).expect("must pass the gate");
    }
    fn rejected(s: &str, needle: &str) {
        match validate_module(&wat(s), &WasmLimits::default()) {
            Err(crate::wasm::error::ExecError::Rejected(why)) => {
                assert!(why.contains(needle), "want reject reason containing {needle:?}, got {why:?}")
            }
            other => panic!("expected Rejected({needle}), got {other:?}"),
        }
    }

    /// The minimal valid ABI module every accept-test builds on.
    const VALID: &str = r#"(module
        (memory (export "memory") 1 1)
        (func (export "alloc") (param i32) (result i32) (i32.const 1024))
        (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#;

    #[test]
    fn minimal_abi_module_passes() { ok(VALID); }

    #[test]
    fn bulk_memory_and_sign_ext_pass() {
        // Deterministic extensions Rust toolchains emit by default — allowed.
        ok(r#"(module
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32)
                (memory.fill (i32.const 0) (i32.const 0) (i32.const 8))
                (i32.const 1024))
            (func (export "run") (param i32 i32) (result i64)
                (i64.extend32_s (i64.const 5))))"#);
    }

    #[test]
    fn floats_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0))
            (func (result f32) (f32.const 1.5)))"#, "feature gate");
    }

    #[test]
    fn simd_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0))
            (func (result i32) (i32x4.extract_lane 0 (v128.load (i32.const 0)))))"#, "feature gate");
    }

    #[test]
    fn atomics_rejected() {
        rejected(r#"(module (memory 1 1 shared)
            (func (export "f") (result i32) (i32.atomic.load (i32.const 0))))"#, "feature gate");
    }

    #[test]
    fn memory_grow_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32) (memory.grow (i32.const 1)))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "memory.grow");
    }

    #[test]
    fn unbounded_memory_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "min==max");
    }

    #[test]
    fn growable_memory_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1 2)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "min==max");
    }

    #[test]
    fn oversized_memory_rejected() {
        // 64 MiB cap = 1024 pages; 1025 must reject.
        rejected(r#"(module
            (memory (export "memory") 1025 1025)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "cap");
    }

    #[test]
    fn any_import_rejected() {
        rejected(r#"(module
            (import "env" "tick" (func))
            (memory (export "memory") 1 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "import");
    }

    #[test]
    fn start_section_rejected() {
        rejected(r#"(module
            (memory (export "memory") 1 1)
            (func $s)
            (start $s)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "start");
    }

    #[test]
    fn missing_exports_rejected() {
        rejected(r#"(module (memory (export "memory") 1 1))"#, "export");
        rejected(r#"(module
            (memory 1 1)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#, "export");
    }
}
