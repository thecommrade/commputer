//! WasmOracle: the real ExecutionOracle (spec §3/§7). Deterministic wasmi
//! interpreter, fuel-metered, hard-capped, zero host imports.
//! New file; wired via wasm/mod.rs. No existing-file changes — implements the
//! UNCHANGED trait from src/staging/pouw/src/oracle.rs.

use crate::job::JobSpec;
use crate::oracle::ExecutionOracle;
use crate::wasm::abi;
use crate::wasm::error::{classify_wasmi_error, ExecError, ExecOutcome};
use crate::wasm::limits::WasmLimits;
use crate::wasm::store::ProgramStore;
use crate::wasm::validation::validate_module;
use sha2::{Digest, Sha256};
use wasmi::{CompilationMode, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

struct HostState {
    limits: StoreLimits,
}

pub struct WasmOracle {
    engine: Engine,
    programs: ProgramStore,
    limits: WasmLimits,
}

impl WasmOracle {
    pub fn new(programs: ProgramStore, limits: WasmLimits) -> Self {
        // The deterministic engine configuration (spec §5/§6). NOTE: wasmi's
        // `simd` cargo feature is NOT enabled, so SIMD is not even compiled in;
        // floats(false) is layer 2 of the float ban (the gate is layer 1).
        let mut config = Config::default();
        config.consume_fuel(true);
        config.floats(false);
        config.compilation_mode(CompilationMode::Eager);
        config.set_max_recursion_depth(limits.max_call_depth);
        config.set_max_stack_height(limits.max_stack_height);
        Self { engine: Engine::new(&config), programs, limits }
    }

    pub fn limits(&self) -> &WasmLimits {
        &self.limits
    }

    /// The rich, fallible interface (NOT on the trait — spec §3). Returns the
    /// error variant and consumed fuel for local logging/tests and the future
    /// cost-coupling cycle.
    pub fn execute(&self, spec: &JobSpec, input: &[u8]) -> ExecOutcome {
        let mut fuel_consumed = 0u64;
        let result = self.run_inner(spec, input, &mut fuel_consumed);
        ExecOutcome { result, fuel_consumed }
    }

    fn run_inner(
        &self,
        spec: &JobSpec,
        input: &[u8],
        fuel_consumed: &mut u64,
    ) -> Result<Vec<u8>, ExecError> {
        // 1. Content addressing (spec §7 step 1): resolve + verify BOTH hashes
        //    before doing anything else.
        let program = self.programs.get(&spec.program_hash).ok_or(ExecError::ProgramUnavailable)?;
        let program_digest: [u8; 32] = Sha256::digest(&program).into();
        if program_digest != spec.program_hash {
            return Err(ExecError::HashMismatch); // defense-in-depth re-check
        }
        let input_digest: [u8; 32] = Sha256::digest(input).into();
        if input_digest != spec.input_hash {
            return Err(ExecError::HashMismatch);
        }
        if input.len() as u64 > self.limits.max_input_bytes {
            return Err(ExecError::Rejected("input exceeds max_input_bytes".into()));
        }

        // 2. The determinism gate (spec §5).
        validate_module(&program, &self.limits)?;

        // 3. Translate + instantiate. Fuel is set BEFORE instantiation because
        //    instantiation (e.g. data-segment init) can charge fuel.
        let module = Module::new(&self.engine, &program[..])
            .map_err(|e| ExecError::Rejected(format!("translation: {e}")))?;
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes as usize)
            .memories(1)
            .tables(1)
            .instances(1)
            .build();
        let mut store = Store::new(&self.engine, HostState { limits: store_limits });
        store.limiter(|s| &mut s.limits);
        store.set_fuel(self.limits.fuel).expect("consume_fuel is enabled in Config");

        // Track consumed fuel after every wasmi call so every exit path reports it.
        macro_rules! track {
            () => {
                *fuel_consumed =
                    self.limits.fuel.saturating_sub(store.get_fuel().unwrap_or(0));
            };
        }

        let linker: Linker<HostState> = Linker::new(&self.engine);
        let instantiated = linker.instantiate_and_start(&mut store, &module);
        track!();
        let instance = instantiated.map_err(|e| classify_wasmi_error(&e))?;

        let handles = abi::bind(&store, &instance)?;
        let mem_len = handles.memory.data(&store).len();

        // 4. alloc + write input (spec §7 step 4).
        let alloc_res = handles.alloc.call(&mut store, input.len() as i32);
        track!();
        let in_ptr = alloc_res.map_err(|e| classify_wasmi_error(&e))? as u32;
        abi::check_bounds(mem_len, in_ptr, input.len() as u32, "alloc(input)")?;
        handles
            .memory
            .write(&mut store, in_ptr as usize, input)
            .map_err(|e| ExecError::AbiViolation(format!("input write: {e}")))?;

        // 5. run + read output (spec §7 steps 5-6).
        let run_res = handles.run.call(&mut store, (in_ptr as i32, input.len() as i32));
        track!();
        let packed = run_res.map_err(|e| classify_wasmi_error(&e))?;
        let (out_ptr, out_len) = abi::unpack(packed);
        if out_len as u64 > self.limits.max_output_bytes {
            return Err(ExecError::AbiViolation(format!(
                "declared output {out_len} exceeds max_output_bytes"
            )));
        }
        abi::check_bounds(mem_len, out_ptr, out_len, "run() output")?;
        let mut output = vec![0u8; out_len as usize];
        handles
            .memory
            .read(&store, out_ptr as usize, &mut output)
            .map_err(|e| ExecError::AbiViolation(format!("output read: {e}")))?;
        Ok(output)
    }
}

impl ExecutionOracle for WasmOracle {
    /// The consensus-facing fold (spec §8): the infallible trait returns the
    /// canonical outcome digest — success and every failure mode included —
    /// so the verification game is untouched.
    fn run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8> {
        self.execute(spec, input).outcome_digest(&self.limits).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobSpec;
    use crate::wasm::error::ExecError;
    use crate::wasm::limits::WasmLimits;
    use crate::wasm::store::ProgramStore;
    use sha2::{Digest, Sha256};

    /// Doubles every input byte into freshly alloc'd output. Exercises the whole
    /// ABI: alloc for input, write, run, alloc for output, packed i64, read.
    pub(crate) const DOUBLER: &str = r#"(module
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

    pub(crate) fn oracle_with(wat_src: &str) -> (WasmOracle, JobSpec, Vec<u8>) {
        let wasm = wat::parse_str(wat_src).expect("fixture assembles");
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm);
        let input = vec![1u8, 2, 3, 40];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let oracle = WasmOracle::new(store, WasmLimits::default());
        (oracle, JobSpec { program_hash, input_hash }, input)
    }

    #[test]
    fn happy_path_runs_and_meters() {
        let (oracle, spec, input) = oracle_with(DOUBLER);
        let out = oracle.execute(&spec, &input);
        assert_eq!(out.result, Ok(vec![2u8, 4, 6, 80]));
        assert!(out.fuel_consumed > 0, "real execution must consume fuel");
        assert!(out.fuel_consumed < WasmLimits::default().fuel);
    }

    #[test]
    fn two_independent_oracles_agree_exactly() {
        // Simulates executor vs verifier: fresh engine, fresh store, same bytes.
        let (a, spec, input) = oracle_with(DOUBLER);
        let (b, _, _) = oracle_with(DOUBLER);
        let (ra, rb) = (a.execute(&spec, &input), b.execute(&spec, &input));
        assert_eq!(ra.result, rb.result);
        assert_eq!(ra.fuel_consumed, rb.fuel_consumed, "fuel is consensus-equal (spec §6)");
    }

    #[test]
    fn same_instance_is_deterministic_across_calls() {
        // Spec §9.A row 1: one long-lived oracle (exactly how a verifier runs)
        // must not accumulate state across executions.
        let (oracle, spec, input) = oracle_with(DOUBLER);
        let r1 = oracle.execute(&spec, &input);
        let r2 = oracle.execute(&spec, &input);
        assert_eq!(r1, r2, "same instance, same job => identical outcome + fuel");
    }

    #[test]
    fn different_program_yields_different_digest() {
        use crate::oracle::ExecutionOracle as _;
        let (a, spec_a, input) = oracle_with(DOUBLER);
        // A distinct program: triples instead of doubles (one-constant change).
        let tripler = DOUBLER.replace("(i32.const 2)", "(i32.const 3)");
        let (b, spec_b, _) = oracle_with(&tripler);
        assert_ne!(a.run(&spec_a, &input), b.run(&spec_b, &input));
    }

    #[test]
    fn unknown_program_is_unavailable() {
        let (oracle, mut spec, input) = oracle_with(DOUBLER);
        spec.program_hash = [0xAB; 32];
        assert_eq!(oracle.execute(&spec, &input).result, Err(ExecError::ProgramUnavailable));
    }

    #[test]
    fn tampered_input_is_hash_mismatch() {
        let (oracle, spec, _input) = oracle_with(DOUBLER);
        let r = oracle.execute(&spec, b"not-the-committed-input");
        assert_eq!(r.result, Err(ExecError::HashMismatch));
        assert_eq!(r.fuel_consumed, 0, "nothing ran");
    }

    #[test]
    fn oversized_input_rejected_before_running() {
        let (_, _, _) = oracle_with(DOUBLER);
        let mut limits = WasmLimits::default();
        limits.max_input_bytes = 2;
        let wasm = wat::parse_str(DOUBLER).unwrap();
        let mut store = ProgramStore::new();
        let program_hash = store.insert(wasm);
        let oracle = WasmOracle::new(store, limits);
        let input = vec![9u8; 3];
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let spec = JobSpec { program_hash, input_hash };
        match oracle.execute(&spec, &input).result {
            Err(ExecError::Rejected(why)) => assert!(why.contains("input")),
            other => panic!("expected Rejected(input...), got {other:?}"),
        }
    }
}
