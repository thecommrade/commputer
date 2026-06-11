//! Real WASM execution runtime behind the unchanged `ExecutionOracle` seam.
//! What: deterministic, fuel-metered, sandboxed wasmi oracle (WasmOracle).
//! Wired in: src/staging/pouw/src/lib.rs (`pub mod wasm`, cfg-gated).
//! Existing files changed: lib.rs (one line) + Cargo.toml (feature/deps) ONLY.
//! Spec: src/staging/docs/2026-06-11-wasm-execution-runtime-design.md

pub mod error;
pub use error::{ExecError, ExecOutcome};
pub mod limits;
pub use limits::WasmLimits;

#[cfg(test)]
mod smoke {
    /// The pinned engine + parser + assembler link and run a trivial module.
    #[test]
    fn pinned_toolchain_links_and_runs() {
        let wasm = wat::parse_str(r#"(module (func (export "f") (result i32) (i32.const 7)))"#)
            .expect("wat assembles");
        let mut v = wasmparser::Validator::new();
        v.validate_all(&wasm).expect("module validates");
        let engine = wasmi::Engine::default();
        let module = wasmi::Module::new(&engine, &wasm[..]).expect("wasmi translates");
        let mut store = wasmi::Store::new(&engine, ());
        let instance = wasmi::Linker::<()>::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .expect("instantiates");
        let f = instance.get_typed_func::<(), i32>(&store, "f").expect("typed func");
        assert_eq!(f.call(&mut store, ()).expect("runs"), 7);
    }
}
