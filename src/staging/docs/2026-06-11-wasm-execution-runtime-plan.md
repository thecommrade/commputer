# WASM Execution Runtime (`WasmOracle`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the toy `IteratedHashVm` with a real deterministic, metered, sandboxed wasmi execution oracle behind the **unchanged** `ExecutionOracle` trait, per the approved spec `src/staging/docs/2026-06-11-wasm-execution-runtime-design.md`.

**Architecture:** A new `wasm/` module inside the `commputer-pouw` staging crate, gated behind a default-off `wasm-runtime` cargo feature. `WasmOracle` resolves `JobSpec.program_hash` from a content-addressed `ProgramStore`, runs a two-layer determinism gate (wasmparser feature-locked validator + targeted scans), executes via wasmi (interpreter; fuel-metered; zero host imports) through a tiny `alloc`/`run` guest ABI, and folds every outcome into a canonical 32-byte digest so the infallible trait and the byte-equality game are untouched.

**Tech Stack:** Rust (workspace at `/home/operator/Coin/src`, edition 2024) · `wasmi = "=1.0.9"` (pinned, consensus-critical) · `wasmparser = "=0.228.0"` (same copy wasmi pulls) · `wat = "=1.251.0"` (dev-only, fixture assembly) · `sha2` (already a dep) · `proptest` (already a dev-dep).

---

## Context an implementer must know (read once, before Task 1)

- **Branch:** `agent-wire-testnet-20260610`. Agent containment rules apply: NEW files only, except the two declared integration points — `src/staging/pouw/src/lib.rs` (one cfg-gated module line) and `src/staging/pouw/Cargo.toml` (feature + deps). NO other existing file changes. No protected files. Git identity `The Commrade <commrade@commputer.xyz>`. Never push.
- **Working directory for every cargo command:** `/home/operator/Coin/src` (the workspace root). Feature-gated test runs: `cargo test -p commputer-pouw --features wasm-runtime`.
- **The seam (do not change):** `src/staging/pouw/src/oracle.rs:12` — `pub trait ExecutionOracle { fn run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8>; }`. `JobSpec { program_hash: [u8;32], input_hash: [u8;32] }` in `job.rs`. The game engine re-hashes whatever `run` returns (`engine.rs` `result_hash`), and equivalence is literal `a == b` — which is why every code path here must be bit-deterministic.
- **Every API name below was verified by compiling and running a probe against wasmi 1.0.9 + wasmparser 0.228.0** on 2026-06-11. Empirical facts you can rely on:
  - `Config`: `consume_fuel(true)`, `floats(false)`, `compilation_mode(CompilationMode::Eager)`, `set_max_recursion_depth(usize)`, `set_max_stack_height(usize)`. There are NO `wasm_simd`/`wasm_relaxed_simd` methods unless wasmi's `simd` cargo feature is enabled — we do NOT enable it, so SIMD support is not even compiled into the engine.
  - `Store::new(&engine, state)`, `store.limiter(|s| &mut s.limits)`, `store.set_fuel(n)` / `store.get_fuel()` (both `Result`). **Set fuel BEFORE instantiation** — instantiation can charge fuel (`MemoryError::OutOfFuel` exists).
  - `StoreLimitsBuilder::new().memory_size(bytes).memories(1).tables(1).instances(1).build()` → `StoreLimits`.
  - `Linker::new(&engine)`, `linker.instantiate_and_start(&mut store, &module)` (there is no plain `instantiate` in 1.0; start sections are gate-rejected so nothing guest-side runs at instantiation).
  - `instance.get_memory(&store, "memory")`, `memory.read/write(&store, offset, buf)`, `memory.data(&store).len()`, `instance.get_typed_func::<i32, i32>(&store, "alloc")`, `TypedFunc<(i32, i32), i64>`.
  - Error classification: `wasmi::errors::{ErrorKind, FuelError, MemoryError, TableError}` + `wasmi::TrapCode`. wasmi's own `is_out_of_fuel` is `pub(crate)`; we replicate its public-matchable arms. (`ErrorKind::ResumableOutOfFuel` is doc(hidden) and can only arise from resumable calls, which we never use — omit it and say so in a comment.)
  - **Out-of-fuel leaves a nonzero remainder**: probe with budget 100,000 → trap → `get_fuel() == 1`, consumed 99,999. Assert cross-instance *equality* of consumed fuel, never `== budget`.
  - `wasmparser::WasmFeatures::WASM1` **includes FLOATS** — it must be explicitly subtracted. Verified: `WasmFeatures::WASM1.union(BULK_MEMORY).union(SIGN_EXTENSION).difference(FLOATS)` const-evaluates; under it the validator rejects float, SIMD, and atomics modules and accepts bulk-memory + sign-ext integer modules; wasmi `floats(false)` independently rejects float modules at translation.
  - Payload walk: `Parser::new(0).parse_all(bytes)` yields `Payload::{ImportSection, MemorySection, TableSection, StartSection{..}, ExportSection, CodeSectionEntry}`; memory entries expose `.initial`/`.maximum` (pages); export entries expose `.name`/`.kind` (`ExternalKind::{Func, Memory}`); `body.get_operators_reader()` + match `Operator::MemoryGrow{..} | Operator::TableGrow{..}`.
- **TDD:** every task = write failing test → see it fail → implement minimally → see it pass → commit. Run only the named test in the inner loop; run the full feature suite before each commit.
- **Pre-flight (already done 2026-06-11, do not repeat):** the `wasm32-unknown-unknown` rustup target is installed on this machine, so Task 10's `build-guest.sh` needs no network. If you find it missing anyway, STOP and report BLOCKED before writing any Task 10 code.

### File map (what exists at the end)

| File | Responsibility |
|---|---|
| `src/staging/pouw/Cargo.toml` (modify) | `wasm-runtime` feature; optional pinned deps; `wat` dev-dep |
| `src/staging/pouw/src/lib.rs` (modify) | one line: `#[cfg(feature = "wasm-runtime")] pub mod wasm;` |
| `src/staging/pouw/src/wasm/mod.rs` | module root + re-exports |
| `src/staging/pouw/src/wasm/limits.rs` | `WasmLimits`, engine identity consts, `config_fingerprint()` |
| `src/staging/pouw/src/wasm/error.rs` | `ExecError`, `ExecOutcome`, canonical digests, wasmi error classification |
| `src/staging/pouw/src/wasm/store.rs` | `ProgramStore` (content-addressed bytes) |
| `src/staging/pouw/src/wasm/validation.rs` | the determinism gate (`GATE_FEATURES` + scans) |
| `src/staging/pouw/src/wasm/abi.rs` | export binding, i64 unpacking, bounds checks |
| `src/staging/pouw/src/wasm/oracle.rs` | `WasmOracle`: `execute()` + `ExecutionOracle` impl |
| `src/staging/pouw/src/wasm/fixtures/guest_example.wasm` | checked-in compiled Rust guest |
| `src/staging/pouw/guest-example/{Cargo.toml,src/lib.rs,build-guest.sh}` | standalone guest crate (NOT a workspace member) |
| `src/staging/pouw/tests/wasm_runtime.rs` | determinism / adversarial / proptest / showcase / game-integration tests |
| `src/staging/pouw/README.md` (modify — Task 12 only, additive section) | WasmOracle docs |

---

### Task 1: Feature gate, pinned deps, module skeleton

**Files:**
- Modify: `src/staging/pouw/Cargo.toml`
- Modify: `src/staging/pouw/src/lib.rs`
- Create: `src/staging/pouw/src/wasm/mod.rs`
- Test: smoke test inside `mod.rs`

- [ ] **Step 1: Edit `src/staging/pouw/Cargo.toml`** — append the feature and deps (keep the existing content exactly as is):

```toml
[features]
# Real WASM execution runtime (spec: docs/2026-06-11-wasm-execution-runtime-design.md).
# Default-OFF so the default build/lockfile graph of sibling crates is untouched.
wasm-runtime = ["dep:wasmi", "dep:wasmparser"]
```

and in `[dependencies]` (exact `=` pins are consensus-critical — see spec §2/§6):

```toml
wasmi = { version = "=1.0.9", optional = true }       # CONSENSUS-CRITICAL pin (spec §2)
wasmparser = { version = "=0.228.0", optional = true } # same copy wasmi 1.0.9 pulls (one lockfile copy)
```

and in `[dev-dependencies]`:

```toml
wat = "=1.251.0"   # .wat -> .wasm fixture assembly; test-only (Cargo has no optional dev-deps; accepted, tiny)
```

- [ ] **Step 2: Add the module line to `src/staging/pouw/src/lib.rs`** (after the existing `pub mod` list):

```rust
#[cfg(feature = "wasm-runtime")]
pub mod wasm;
```

- [ ] **Step 3: Create `src/staging/pouw/src/wasm/mod.rs`** with a failing-to-compile-without-children skeleton — start with just the smoke test:

```rust
//! Real WASM execution runtime behind the unchanged `ExecutionOracle` seam.
//! What: deterministic, fuel-metered, sandboxed wasmi oracle (WasmOracle).
//! Wired in: src/staging/pouw/src/lib.rs (`pub mod wasm`, cfg-gated).
//! Existing files changed: lib.rs (one line) + Cargo.toml (feature/deps) ONLY.
//! Spec: src/staging/docs/2026-06-11-wasm-execution-runtime-design.md

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
```

- [ ] **Step 4: Verify the default build is untouched, then the feature build passes**

Run: `cargo test -p commputer-pouw` (from `/home/operator/Coin/src`)
Expected: PASS, exactly the pre-existing test count — wasmi must NOT compile here.

Run: `cargo test -p commputer-pouw --features wasm-runtime smoke`
Expected: PASS `pinned_toolchain_links_and_runs` (first run compiles wasmi — allow a few minutes).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/Cargo.toml src/staging/pouw/src/lib.rs src/staging/pouw/src/wasm/mod.rs
git commit -m "feat(pouw): wasm-runtime feature gate + pinned wasmi/wasmparser + module skeleton (Task 1)"
```

---

### Task 2: `limits.rs` — limits, engine identity, config fingerprint

**Files:**
- Create: `src/staging/pouw/src/wasm/limits.rs`
- Modify: `src/staging/pouw/src/wasm/mod.rs` (add `pub mod limits;` + re-export)

- [ ] **Step 1: Write the failing tests** (inside `limits.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_limit_sensitive() {
        let a = WasmLimits::default();
        let b = WasmLimits::default();
        assert_eq!(a.config_fingerprint(), b.config_fingerprint());
        let mut c = WasmLimits::default();
        c.fuel += 1;
        assert_ne!(a.config_fingerprint(), c.config_fingerprint(),
            "any limit change must change the consensus fingerprint");
    }

    #[test]
    fn defaults_are_the_spec_constants() {
        let l = WasmLimits::default();
        assert_eq!(l.fuel, 100_000_000);
        assert_eq!(l.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(l.max_call_depth, 1024);
        assert_eq!(l.max_stack_height, 1 << 20);
        assert_eq!(l.max_input_bytes, 10 * 1024 * 1024);
        assert_eq!(l.max_output_bytes, 10 * 1024 * 1024);
        assert_eq!(ENGINE_ID, "wasmi");
        assert_eq!(ENGINE_VERSION, "1.0.9");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p commputer-pouw --features wasm-runtime limits` → FAIL (module missing).

- [ ] **Step 3: Implement `limits.rs`:**

```rust
//! Consensus-critical limits + engine identity for the WASM runtime (spec §6).
//! New file; wired via wasm/mod.rs. No existing-file changes.
//! Every constant here is destined for chain consensus params (founder, cycle #3):
//! two nodes disagreeing on ANY of them diverge on every job by design (the
//! fingerprint is folded into every outcome digest).

use sha2::{Digest, Sha256};

pub const ENGINE_ID: &str = "wasmi";
/// MUST match the `=` pin in Cargo.toml. Upgrading the engine is a coordinated
/// protocol change, never a silent bump (spec §2).
pub const ENGINE_VERSION: &str = "1.0.9";
pub const ABI_VERSION: u32 = 1;
pub const VALIDATION_VERSION: u32 = 1;
/// Domain-separation tag for every digest this runtime produces (spec §8).
pub const DOMAIN: &[u8] = b"commputer-pouw-wasm-v1";

/// Hard caps, identical on every node (spec §6). Fuel is the ONLY compute meter
/// (wall-clock is forbidden as a meter — it is non-deterministic and would cause
/// false disputes; see the dead stub src/node/src/wasm_executor.rs for the
/// anti-pattern this replaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmLimits {
    pub fuel: u64,
    pub max_memory_bytes: u64,
    pub max_call_depth: usize,
    pub max_stack_height: usize,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_call_depth: 1024,
            max_stack_height: 1 << 20,
            max_input_bytes: 10 * 1024 * 1024,
            max_output_bytes: 10 * 1024 * 1024,
        }
    }
}

impl WasmLimits {
    /// SHA-256 over the full determinism identity: engine id/version, ABI and
    /// validation-policy versions, and every limit, in a fixed serialization.
    pub fn config_fingerprint(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update(ENGINE_ID.as_bytes());
        h.update([0u8]); // separator: ENGINE_ID is not fixed-width
        h.update(ENGINE_VERSION.as_bytes());
        h.update([0u8]);
        h.update(ABI_VERSION.to_le_bytes());
        h.update(VALIDATION_VERSION.to_le_bytes());
        h.update(self.fuel.to_le_bytes());
        h.update(self.max_memory_bytes.to_le_bytes());
        h.update((self.max_call_depth as u64).to_le_bytes());
        h.update((self.max_stack_height as u64).to_le_bytes());
        h.update(self.max_input_bytes.to_le_bytes());
        h.update(self.max_output_bytes.to_le_bytes());
        h.finalize().into()
    }
}
```

In `mod.rs` add: `pub mod limits;` and `pub use limits::WasmLimits;`.

- [ ] **Step 4: Run to verify pass** — `cargo test -p commputer-pouw --features wasm-runtime limits` → PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/wasm/
git commit -m "feat(pouw/wasm): WasmLimits + engine identity + consensus config fingerprint (Task 2)"
```

---

### Task 3: `error.rs` — ExecError, ExecOutcome, canonical digests

**Files:**
- Create: `src/staging/pouw/src/wasm/error.rs`
- Modify: `src/staging/pouw/src/wasm/mod.rs` (add `pub mod error;` + re-exports)

- [ ] **Step 1: Write the failing tests** (inside `error.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm::limits::WasmLimits;

    #[test]
    fn digests_are_deterministic_and_domain_separated() {
        let l = WasmLimits::default();
        assert_eq!(ok_digest(&l, b"out"), ok_digest(&l, b"out"));
        assert_ne!(ok_digest(&l, b"out"), ok_digest(&l, b"other"));
        assert_eq!(error_digest(&l), error_digest(&l));
        // The OK digest of ANY output can never equal the error sentinel
        // (0x00 vs 0x01 discriminant byte in the preimage).
        assert_ne!(ok_digest(&l, b""), error_digest(&l));
    }

    #[test]
    fn config_drift_diverges_every_digest() {
        let a = WasmLimits::default();
        let mut b = WasmLimits::default();
        b.fuel += 1; // a mis-configured node
        assert_ne!(ok_digest(&a, b"x"), ok_digest(&b, b"x"), "drift must fail loud (spec §8)");
        assert_ne!(error_digest(&a), error_digest(&b));
    }

    #[test]
    fn every_error_kind_folds_to_the_same_sentinel() {
        let l = WasmLimits::default();
        let kinds = [
            ExecError::ProgramUnavailable,
            ExecError::HashMismatch,
            ExecError::Rejected("x".into()),
            ExecError::OutOfFuel,
            ExecError::Trapped("y".into()),
            ExecError::AbiViolation("z".into()),
        ];
        for k in kinds {
            let o = ExecOutcome { result: Err(k), fuel_consumed: 0 };
            assert_eq!(o.outcome_digest(&l), error_digest(&l),
                "which-error must be indistinguishable in the digest (no covert channel, spec §8)");
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p commputer-pouw --features wasm-runtime error` → FAIL.

- [ ] **Step 3: Implement `error.rs`:**

```rust
//! Deterministic error model + the canonical outcome digest fold (spec §8).
//! New file; wired via wasm/mod.rs. No existing-file changes.

use crate::wasm::limits::{WasmLimits, DOMAIN};
use sha2::{Digest, Sha256};

/// Every way an execution can fail. All variants are deterministic given
/// identical (program, input, limits, engine version) — EXCEPT
/// `ProgramUnavailable`, which depends on local store state; safe this cycle
/// only because tests/sim populate every node's store (spec §8/§10.2).
/// Payload strings are for local logs/tests ONLY — they never reach the digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    ProgramUnavailable,
    HashMismatch,
    /// Failed the determinism gate (validation.rs) or wasmi translation.
    Rejected(String),
    OutOfFuel,
    /// Any other runtime trap (unreachable, OOB, div0, stack/recursion cap...).
    Trapped(String),
    /// The guest violated the ABI contract (bad alloc ptr, oversized/OOB output).
    AbiViolation(String),
}

/// The rich result of `WasmOracle::execute` — NOT on the trait. The future
/// cost-coupling cycle reads `fuel_consumed` from here (spec §3/§10.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub result: Result<Vec<u8>, ExecError>,
    /// budget − remaining. 0 when execution never started. NOTE: on an
    /// out-of-fuel trap wasmi leaves a small remainder, so this is generally
    /// < budget even for OutOfFuel; the consensus property is cross-node
    /// EQUALITY of this number, not any particular value.
    pub fuel_consumed: u64,
}

impl ExecOutcome {
    /// The consensus-facing 32-byte value (what `ExecutionOracle::run` returns).
    pub fn outcome_digest(&self, limits: &WasmLimits) -> [u8; 32] {
        match &self.result {
            Ok(out) => ok_digest(limits, out),
            Err(_) => error_digest(limits),
        }
    }
}

/// sha256(DOMAIN ‖ fingerprint ‖ 0x00 ‖ output)
pub fn ok_digest(limits: &WasmLimits, output: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(limits.config_fingerprint());
    h.update([0x00]);
    h.update(output);
    h.finalize().into()
}

/// sha256(DOMAIN ‖ fingerprint ‖ 0x01) — ONE sentinel for every error kind,
/// so "which trap" cannot be a covert channel (spec §8).
pub fn error_digest(limits: &WasmLimits) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(limits.config_fingerprint());
    h.update([0x01]);
    h.finalize().into()
}

/// Map a wasmi runtime error deterministically. Replicates the arms of wasmi's
/// pub(crate) `Error::is_out_of_fuel` (wasmi-1.0.9 src/error.rs:131-141).
/// `ErrorKind::ResumableOutOfFuel` is intentionally omitted: it is doc(hidden)
/// and can only arise from resumable calls, which this oracle never uses.
pub fn classify_wasmi_error(e: &wasmi::Error) -> ExecError {
    use wasmi::errors::{ErrorKind, FuelError, MemoryError, TableError};
    use wasmi::TrapCode;
    let out_of_fuel = matches!(
        e.kind(),
        ErrorKind::TrapCode(TrapCode::OutOfFuel)
            | ErrorKind::Memory(MemoryError::OutOfFuel { .. })
            | ErrorKind::Table(TableError::OutOfFuel { .. })
            | ErrorKind::Fuel(FuelError::OutOfFuel { .. })
    );
    if out_of_fuel {
        ExecError::OutOfFuel
    } else {
        ExecError::Trapped(e.to_string())
    }
}
```

In `mod.rs` add: `pub mod error;` and `pub use error::{ExecError, ExecOutcome};`.

- [ ] **Step 4: Run to verify pass** — `cargo test -p commputer-pouw --features wasm-runtime error` → PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/wasm/
git commit -m "feat(pouw/wasm): ExecError/ExecOutcome + canonical outcome digests + wasmi error classification (Task 3)"
```

---

### Task 4: `store.rs` — content-addressed ProgramStore

**Files:**
- Create: `src/staging/pouw/src/wasm/store.rs`
- Modify: `src/staging/pouw/src/wasm/mod.rs` (add `pub mod store;` + re-export)

- [ ] **Step 1: Write the failing tests** (inside `store.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn insert_addresses_by_content_and_get_roundtrips() {
        let mut s = ProgramStore::new();
        let bytes = b"fake-wasm".to_vec();
        let expected: [u8; 32] = Sha256::digest(&bytes).into();
        let hash = s.insert(bytes.clone());
        assert_eq!(hash, expected, "address must be sha256 of the RAW bytes (spec §7)");
        assert_eq!(s.get(&hash).as_deref(), Some(&bytes[..]));
        assert!(s.get(&[0u8; 32]).is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p commputer-pouw --features wasm-runtime store` → FAIL.

- [ ] **Step 3: Implement `store.rs`:**

```rust
//! Content-addressed program bytes: hash -> raw .wasm (spec §7). NO fetching,
//! NO eviction — data availability is deferred cycle #2 (spec §10.2).
//! New file; wired via wasm/mod.rs. No existing-file changes.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ProgramStore {
    programs: HashMap<[u8; 32], Arc<[u8]>>,
}

impl ProgramStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store raw .wasm bytes under their sha256. The RAW bytes are the canonical
    /// program identity — never a compiled/serialized artifact (spec §7).
    pub fn insert(&mut self, bytes: impl Into<Arc<[u8]>>) -> [u8; 32] {
        let bytes: Arc<[u8]> = bytes.into();
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        self.programs.insert(hash, bytes);
        hash
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<Arc<[u8]>> {
        self.programs.get(hash).cloned()
    }
}
```

In `mod.rs` add: `pub mod store;` and `pub use store::ProgramStore;`.

- [ ] **Step 4: Run to verify pass** — `cargo test -p commputer-pouw --features wasm-runtime store` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/wasm/
git commit -m "feat(pouw/wasm): content-addressed ProgramStore (Task 4)"
```

---

### Task 5: `validation.rs` — the determinism gate

**Files:**
- Create: `src/staging/pouw/src/wasm/validation.rs`
- Modify: `src/staging/pouw/src/wasm/mod.rs` (add `pub mod validation;`)

- [ ] **Step 1: Write the failing tests** (inside `validation.rs`; every reject row of spec §5 gets a test):

```rust
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
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p commputer-pouw --features wasm-runtime validation` → FAIL.

- [ ] **Step 3: Implement `validation.rs`:**

```rust
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
                        return reject("memory exceeds the shared cap");
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
        return reject("required exports missing: memory, alloc, run");
    }
    Ok(())
}
```

In `mod.rs` add: `pub mod validation;`.

**Implementer notes:** (a) If field/variant names differ slightly in wasmparser 0.228 (`table.ty.maximum` vs another shape), follow the compiler — the *semantics* above are the spec; record any rename in the commit message. (b) The atomics test module has no run/alloc exports because shared memory already fails layer 1 — that is the point of the test.

- [ ] **Step 4: Run to verify pass** — `cargo test -p commputer-pouw --features wasm-runtime validation` → PASS (12 tests).

- [ ] **Step 5: Commit**

```bash
git add src/staging/pouw/src/wasm/
git commit -m "feat(pouw/wasm): determinism gate — allow-list features + grow/min-max/import/start/export scans (Task 5)"
```

---

### Task 6: `abi.rs` + `oracle.rs` — WasmOracle::execute happy path

**Files:**
- Create: `src/staging/pouw/src/wasm/abi.rs`
- Create: `src/staging/pouw/src/wasm/oracle.rs`
- Modify: `src/staging/pouw/src/wasm/mod.rs` (add modules + `pub use oracle::WasmOracle;`)

- [ ] **Step 1: Write the failing tests** (inside `oracle.rs`):

```rust
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
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p commputer-pouw --features wasm-runtime wasm::oracle` → FAIL. (Use the `wasm::oracle` filter, NOT bare `oracle` — the bare substring also matches 4 pre-existing tests in `oracle.rs`/`verdict.rs` and would distort the expected counts.)

- [ ] **Step 3: Implement `abi.rs`:**

```rust
//! Host side of the zero-import guest ABI (spec §7): export binding with exact
//! signatures, packed-i64 splitting, and bounds checks against the fixed memory.
//! New file; wired via wasm/mod.rs. No existing-file changes.

use crate::wasm::error::ExecError;
use wasmi::{Instance, Memory, Store, TypedFunc};

pub struct AbiHandles {
    pub memory: Memory,
    pub alloc: TypedFunc<i32, i32>,
    pub run: TypedFunc<(i32, i32), i64>,
}

/// Bind the three required exports with EXACT signatures. Validation (rule 8)
/// already guaranteed presence+kind; a wrong signature deterministically folds
/// to Rejected here.
pub fn bind<T>(store: &Store<T>, instance: &Instance) -> Result<AbiHandles, ExecError> {
    let memory = instance
        .get_memory(store, "memory")
        .ok_or_else(|| ExecError::Rejected("export `memory` is not a memory".into()))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(store, "alloc")
        .map_err(|e| ExecError::Rejected(format!("export `alloc` signature: {e}")))?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(store, "run")
        .map_err(|e| ExecError::Rejected(format!("export `run` signature: {e}")))?;
    Ok(AbiHandles { memory, alloc, run })
}

/// Split run()'s packed i64 into (out_ptr, out_len). Both halves are decoded
/// as u32 unconditionally — a guest that mis-packs produces garbage that the
/// bounds check below rejects deterministically (spec §7).
pub fn unpack(packed: i64) -> (u32, u32) {
    (((packed as u64) >> 32) as u32, packed as u64 as u32)
}

/// Check [ptr, ptr+len) lies inside the fixed linear memory.
pub fn check_bounds(mem_len: usize, ptr: u32, len: u32, what: &str) -> Result<(), ExecError> {
    let end = ptr as u64 + len as u64; // cannot overflow: u32 + u32 fits in u64
    if end > mem_len as u64 {
        return Err(ExecError::AbiViolation(format!(
            "{what} [{ptr}, {end}) exceeds memory size {mem_len}"
        )));
    }
    Ok(())
}
```

- [ ] **Step 4: Implement `oracle.rs`:**

```rust
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

        // 4. alloc + write input (spec §7 steps 4).
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
```

In `mod.rs` add: `pub mod abi;`, `pub mod oracle;`, `pub use oracle::WasmOracle;`.

- [ ] **Step 5: Run to verify pass** — `cargo test -p commputer-pouw --features wasm-runtime wasm::oracle` → PASS (7 tests). Then full sweep: `cargo test -p commputer-pouw --features wasm-runtime` → all green.

- [ ] **Step 6: Commit**

```bash
git add src/staging/pouw/src/wasm/
git commit -m "feat(pouw/wasm): WasmOracle execute() + zero-import ABI host glue + ExecutionOracle fold (Task 6)"
```

---

### Task 7: Trait-fold digest tests

**Files:**
- Test: append to `src/staging/pouw/src/wasm/oracle.rs` tests

- [ ] **Step 1: Write the failing tests** (append inside `oracle.rs` `mod tests`):

```rust
    use crate::oracle::ExecutionOracle as _;
    use crate::wasm::error::{error_digest, ok_digest};

    #[test]
    fn trait_run_returns_ok_digest_for_success() {
        let (oracle, spec, input) = oracle_with(DOUBLER);
        let digest = oracle.run(&spec, &input);
        let expected = ok_digest(&WasmLimits::default(), &[2u8, 4, 6, 80]);
        assert_eq!(digest, expected.to_vec());
    }

    #[test]
    fn trait_run_returns_the_single_error_sentinel_for_failures() {
        let (oracle, mut spec, input) = oracle_with(DOUBLER);
        spec.program_hash = [0xAB; 32]; // ProgramUnavailable
        let digest = oracle.run(&spec, &input);
        assert_eq!(digest, error_digest(&WasmLimits::default()).to_vec());
    }
```

- [ ] **Step 2: Run to verify fail→pass** — these should pass immediately if Task 6 was implemented per plan; if either fails, the fold is wrong — fix `oracle.rs`, not the test. Run: `cargo test -p commputer-pouw --features wasm-runtime trait_run` → PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/src/wasm/oracle.rs
git commit -m "test(pouw/wasm): trait fold returns ok-digest / single error sentinel (Task 7)"
```

---

### Task 8: Adversarial suite — caps and sandbox hold

**Files:**
- Create: `src/staging/pouw/tests/wasm_runtime.rs` (integration test target; entire file feature-gated)

- [ ] **Step 1: Create the test file with the adversarial matrix:**

```rust
//! Integration tests for the WASM runtime (spec §9.B/§9.A). Entire file is
//! feature-gated: `cargo test -p commputer-pouw --features wasm-runtime`.
//! New file; no existing-file changes. (Roadmap: spec §9.)
#![cfg(feature = "wasm-runtime")]

use commputer_pouw::job::JobSpec;
use commputer_pouw::wasm::error::{error_digest, ExecError};
use commputer_pouw::wasm::{ExecOutcome, ProgramStore, WasmLimits, WasmOracle};
use sha2::{Digest, Sha256};

/// Build an oracle around one wat fixture and one input.
fn setup(wat_src: &str, input: &[u8]) -> (WasmOracle, JobSpec) {
    let wasm = wat::parse_str(wat_src).expect("fixture assembles");
    let mut store = ProgramStore::new();
    let program_hash = store.insert(wasm);
    let input_hash: [u8; 32] = Sha256::digest(input).into();
    (WasmOracle::new(store, WasmLimits::default()), JobSpec { program_hash, input_hash })
}

/// Run the same (program, input) on TWO independent oracles and assert the
/// outcomes agree exactly — the no-false-dispute property (spec §9.A).
fn assert_consensus(wat_src: &str, input: &[u8]) -> ExecOutcome {
    let (a, spec) = setup(wat_src, input);
    let (b, _) = setup(wat_src, input);
    let (ra, rb) = (a.execute(&spec, input), b.execute(&spec, input));
    assert_eq!(ra.result, rb.result, "honest nodes must agree on the result");
    assert_eq!(ra.fuel_consumed, rb.fuel_consumed, "and on the exact fuel");
    ra
}

const ABI_SHELL_TOP: &str = r#"(module
    (memory (export "memory") 1 1)
    (func (export "alloc") (param i32) (result i32) (i32.const 1024))
    (func (export "run") (param i32 i32) (result i64)"#;

#[test]
fn infinite_loop_is_deterministic_out_of_fuel() {
    let fixture = format!("{ABI_SHELL_TOP} (loop $l (br $l)) (i64.const 0)))");
    let outcome = assert_consensus(&fixture, b"in");
    assert_eq!(outcome.result, Err(ExecError::OutOfFuel));
    // wasmi leaves a remainder on the OOF trap (verified: budget 100_000 ->
    // remaining 1). NEVER assert == budget; cross-instance equality above is
    // the consensus property (spec §9.B).
    assert!(outcome.fuel_consumed <= WasmLimits::default().fuel);
    assert!(outcome.fuel_consumed > 0);
}

#[test]
fn unreachable_is_deterministic_trap() {
    let fixture = format!("{ABI_SHELL_TOP} (unreachable)))");
    let outcome = assert_consensus(&fixture, b"in");
    assert!(matches!(outcome.result, Err(ExecError::Trapped(_))));
}

#[test]
fn out_of_bounds_store_is_deterministic_trap() {
    // Writes 4 bytes at the very end of the 1-page memory -> OOB.
    let fixture = format!(
        "{ABI_SHELL_TOP} (i32.store (i32.const 65535) (i32.const 1)) (i64.const 0)))"
    );
    let outcome = assert_consensus(&fixture, b"in");
    assert!(matches!(outcome.result, Err(ExecError::Trapped(_))));
}

#[test]
fn deep_recursion_hits_the_recursion_cap_deterministically() {
    let fixture = r#"(module
        (memory (export "memory") 1 1)
        (func (export "alloc") (param i32) (result i32) (i32.const 1024))
        (func $rec (call $rec))
        (func (export "run") (param i32 i32) (result i64) (call $rec) (i64.const 0)))"#;
    let outcome = assert_consensus(fixture, b"in");
    // Stack/recursion exhaustion is a trap, not OOF (the cap is max_call_depth).
    assert!(matches!(outcome.result, Err(ExecError::Trapped(_))));
}

#[test]
fn out_of_bounds_output_pointer_is_abi_violation() {
    // Packs out_ptr = 65536 (one past the end), out_len = 8.
    let fixture = format!(
        "{ABI_SHELL_TOP} (i64.or (i64.shl (i64.const 65536) (i64.const 32)) (i64.const 8))))"
    );
    let outcome = assert_consensus(&fixture, b"in");
    assert!(matches!(outcome.result, Err(ExecError::AbiViolation(_))));
}

#[test]
fn oversized_declared_output_is_abi_violation() {
    // out_len u32::MAX overflows max_output_bytes long before any read.
    let fixture = format!(
        "{ABI_SHELL_TOP} (i64.or (i64.shl (i64.const 0) (i64.const 32)) (i64.const 4294967295))))"
    );
    let outcome = assert_consensus(&fixture, b"in");
    assert!(matches!(outcome.result, Err(ExecError::AbiViolation(_))));
}

#[test]
fn wrong_export_signature_is_rejected_not_trapped() {
    // Spec §9.C last row. This is the ONE gate rule enforced post-instantiation
    // (abi.rs typed binding) rather than in validation.rs: the module passes
    // validate_module (presence+kind are right) but `run` has the wrong type.
    // It must fold to Rejected — never Trapped — and deterministically so.
    let fixture = r#"(module
        (memory (export "memory") 1 1)
        (func (export "alloc") (param i32) (result i32) (i32.const 1024))
        (func (export "run") (param i32) (result i64) (i64.const 0)))"#;
    let outcome = assert_consensus(fixture, b"in");
    assert!(
        matches!(outcome.result, Err(ExecError::Rejected(_))),
        "wrong signature must be Rejected, got {:?}",
        outcome.result
    );
}

#[test]
fn every_adversarial_failure_folds_to_the_same_sentinel() {
    use commputer_pouw::oracle::ExecutionOracle as _;
    let sentinel = error_digest(&WasmLimits::default()).to_vec();
    let loops = format!("{ABI_SHELL_TOP} (loop $l (br $l)) (i64.const 0)))");
    let traps = format!("{ABI_SHELL_TOP} (unreachable)))");
    for fixture in [loops, traps] {
        let (oracle, spec) = setup(&fixture, b"in");
        assert_eq!(oracle.run(&spec, b"in"), sentinel, "no covert trap channel (spec §8)");
    }
}
```

- [ ] **Step 2: Run to verify** — `cargo test -p commputer-pouw --features wasm-runtime --test wasm_runtime` → PASS (8 tests). Any failure here is an implementation bug from Task 6 — fix `oracle.rs`/`abi.rs`, never weaken an assertion. (Timing note: the out-of-fuel tests each burn the full 100M-fuel budget in the interpreter, ~1.5s each in debug — a few seconds total is normal, not a hang.)

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/tests/wasm_runtime.rs
git commit -m "test(pouw/wasm): adversarial matrix — OOF/trap/OOB/recursion/ABI-violation all deterministic + single sentinel (Task 8)"
```

---

### Task 9: Property-based determinism harness

**Files:**
- Test: append to `src/staging/pouw/tests/wasm_runtime.rs`

- [ ] **Step 1: Append the proptest** (uses the DOUBLER-equivalent transform; mirrors spec §9.A):

```rust
mod determinism_properties {
    use super::*;
    use proptest::prelude::*;

    /// Same doubling transform as the oracle unit tests (kept in-sync by eye;
    /// it is 12 lines of wat). Output[i] = 2*input[i] mod 256.
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

    proptest! {
        /// For arbitrary inputs: two independent oracles agree bit-for-bit on
        /// output AND fuel; the output is the expected transform; different
        /// inputs yield different digests.
        #[test]
        fn independent_oracles_always_agree(input in proptest::collection::vec(any::<u8>(), 0..512)) {
            let (a, spec) = setup(DOUBLER, &input);
            let (b, _) = setup(DOUBLER, &input);
            let (ra, rb) = (a.execute(&spec, &input), b.execute(&spec, &input));
            prop_assert_eq!(&ra.result, &rb.result);
            prop_assert_eq!(ra.fuel_consumed, rb.fuel_consumed);
            let expected: Vec<u8> = input.iter().map(|b| b.wrapping_mul(2)).collect();
            prop_assert_eq!(ra.result.unwrap(), expected);
        }
    }
}
```

- [ ] **Step 2: Run to verify** — `cargo test -p commputer-pouw --features wasm-runtime --test wasm_runtime determinism_properties` → PASS (1 test; proptest runs 256 cases internally).

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/tests/wasm_runtime.rs
git commit -m "test(pouw/wasm): proptest — independent oracles agree on output+fuel for arbitrary inputs (Task 9)"
```

---

### Task 10: The compiled-Rust guest (realism showcase)

**Files:**
- Create: `src/staging/pouw/guest-example/Cargo.toml`
- Create: `src/staging/pouw/guest-example/src/lib.rs`
- Create: `src/staging/pouw/guest-example/build-guest.sh` (chmod +x)
- Create: `src/staging/pouw/src/wasm/fixtures/guest_example.wasm` (built artifact, checked in)
- Test: append to `src/staging/pouw/tests/wasm_runtime.rs`

- [ ] **Step 1: Create `guest-example/Cargo.toml`:**

```toml
# Standalone guest crate — NOT a workspace member. Built by build-guest.sh to
# wasm32-unknown-unknown; the artifact is CHECKED IN under src/wasm/fixtures/
# so `cargo test` never needs the wasm32 toolchain (spec §4).
[package]
name = "guest-example"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[profile.release]
opt-level = "s"
panic = "abort"
lto = true

# Empty table: detaches this crate from the enclosing workspace so cargo
# builds it standalone inside the tree (spec §4 guest constraint c).
[workspace]
```

- [ ] **Step 2: Create `guest-example/src/lib.rs`:**

```rust
//! Realism-showcase guest (spec §9.D): a real Rust program compiled to wasm32
//! that passes the determinism gate. Integer-only; static bump arena (NO
//! dlmalloc => no memory.grow — spec §4 guest constraint b).
//! Transform: 32-byte output = 4 u64 LE lanes of an xorshift64* stream seeded
//! by FNV-1a over the input, 1000 rounds per lane. Mirrored natively by
//! `native_reference()` in tests/wasm_runtime.rs — keep both in sync.
#![no_std]

const ARENA_SIZE: usize = 64 * 1024;
static mut ARENA: [u8; ARENA_SIZE] = [0; ARENA_SIZE];
static mut NEXT: usize = 0;

/// Bump allocator over the static arena. Never grows memory; exhaustion traps
/// (deterministically) via unreachable.
#[no_mangle]
pub extern "C" fn alloc(len: i32) -> i32 {
    unsafe {
        let base = core::ptr::addr_of_mut!(ARENA) as *mut u8;
        let start = NEXT;
        let len = len as usize;
        if len > ARENA_SIZE || start > ARENA_SIZE - len {
            core::arch::wasm32::unreachable()
        }
        NEXT = start + len;
        base.add(start) as i32
    }
}

#[no_mangle]
pub extern "C" fn run(ptr: i32, len: i32) -> i64 {
    let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };

    // FNV-1a 64 seed over the input.
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in input {
        seed ^= b as u64;
        seed = seed.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let mut state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };

    // 4 lanes x 1000 xorshift64 rounds — integer-only, loops enough to meter.
    let mut out = [0u8; 32];
    for lane in 0..4 {
        for _ in 0..1000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
        }
        out[lane * 8..lane * 8 + 8].copy_from_slice(&state.to_le_bytes());
    }

    let out_ptr = alloc(32);
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), out_ptr as *mut u8, 32) };
    // Canonical packing (spec §7): unsigned shift, never sign-extending.
    (((out_ptr as u32 as u64) << 32) | 32u64) as i64
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
```

- [ ] **Step 3: Create `guest-example/build-guest.sh`** and `chmod +x` it:

```bash
#!/usr/bin/env bash
# Rebuilds src/wasm/fixtures/guest_example.wasm (spec §4 guest constraints):
#   (a) --initial-memory == --max-memory  -> memory min==max (gate rule 5)
#   (b) static bump arena, no dlmalloc    -> no memory.grow  (gate rule 4)
#   target-cpu=mvp -> plain integer MVP output (no post-MVP surprises).
# NOT required for `cargo test` — the artifact is checked in.
set -euo pipefail
cd "$(dirname "$0")"
rustup target add wasm32-unknown-unknown
RUSTFLAGS="-C target-cpu=mvp -C link-arg=--initial-memory=1048576 -C link-arg=--max-memory=1048576 -C link-arg=-zstack-size=131072" \
    cargo build --release --target wasm32-unknown-unknown
mkdir -p ../src/wasm/fixtures
cp target/wasm32-unknown-unknown/release/guest_example.wasm ../src/wasm/fixtures/guest_example.wasm
echo "rebuilt:"
sha256sum ../src/wasm/fixtures/guest_example.wasm
```

- [ ] **Step 4: Build and check in the artifact**

Run: `bash src/staging/pouw/guest-example/build-guest.sh` (from `/home/operator/Coin`)
Expected: prints `rebuilt:` + a sha256. (The wasm32 target is pre-installed — see the pre-flight
note; the `rustup target add` in the script is an idempotent no-op here.)
If the build fails for toolchain reasons, STOP and report BLOCKED — do not substitute a wat-built fake.

- [ ] **Step 5: Append the showcase tests to `tests/wasm_runtime.rs`:**

```rust
mod guest_showcase {
    use super::*;
    use commputer_pouw::wasm::validation::validate_module;

    const GUEST: &[u8] = include_bytes!("../src/wasm/fixtures/guest_example.wasm");

    /// Native mirror of guest-example/src/lib.rs `run` — keep in sync BY HAND.
    fn native_reference(input: &[u8]) -> Vec<u8> {
        let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in input {
            seed ^= b as u64;
            seed = seed.wrapping_mul(0x0000_0100_0000_01B3);
        }
        let mut state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
        let mut out = vec![0u8; 32];
        for lane in 0..4 {
            for _ in 0..1000 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
            }
            out[lane * 8..lane * 8 + 8].copy_from_slice(&state.to_le_bytes());
        }
        out
    }

    #[test]
    fn checked_in_guest_passes_the_gate() {
        // Regression that build-guest.sh's constraints actually held (spec §9.D).
        validate_module(GUEST, &WasmLimits::default()).expect("compiled guest must pass the gate");
    }

    #[test]
    fn compiled_rust_guest_matches_native_reference() {
        let input = b"the people's compute".to_vec();
        let mut store = ProgramStore::new();
        let program_hash = store.insert(GUEST.to_vec());
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let oracle = WasmOracle::new(store, WasmLimits::default());
        let spec = JobSpec { program_hash, input_hash };
        let outcome = oracle.execute(&spec, &input);
        assert_eq!(outcome.result, Ok(native_reference(&input)));
        assert!(outcome.fuel_consumed > 4_000, "4000 xorshift rounds must meter visibly");
    }

    #[test]
    fn compiled_guest_is_deterministic_across_instances() {
        let input = b"verify me twice".to_vec();
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        let mk = || {
            let mut store = ProgramStore::new();
            let program_hash = store.insert(GUEST.to_vec());
            (WasmOracle::new(store, WasmLimits::default()), JobSpec { program_hash, input_hash })
        };
        let (a, spec) = mk();
        let (b, _) = mk();
        let (ra, rb) = (a.execute(&spec, &input), b.execute(&spec, &input));
        assert_eq!(ra.result, rb.result);
        assert_eq!(ra.fuel_consumed, rb.fuel_consumed);
    }
}
```

- [ ] **Step 6: Run to verify** — `cargo test -p commputer-pouw --features wasm-runtime --test wasm_runtime guest_showcase` → PASS (3 tests). If `checked_in_guest_passes_the_gate` fails, the build flags are wrong — fix `build-guest.sh` (gate stays as-is; spec §9 explicitly forbids weakening it).

- [ ] **Step 7: Commit** (include the binary artifact deliberately):

```bash
git add src/staging/pouw/guest-example/ src/staging/pouw/src/wasm/fixtures/guest_example.wasm src/staging/pouw/tests/wasm_runtime.rs
git commit -m "feat(pouw/wasm): compiled-Rust guest showcase — built artifact checked in + native-reference equivalence (Task 10)"
```

---

### Task 11: Game integration — the real oracle through the unchanged game

**Files:**
- Test: append to `src/staging/pouw/tests/wasm_runtime.rs`

- [ ] **Step 1: Append the integration tests** (mirrors `engine.rs`'s existing `all_honest_sampled_job_confirms_and_settles_85_10_5`, swapping in the real oracle — note `p_trap_bps = 0` so the only stochastic branch is sampling, forced to 100%):

```rust
mod game_integration {
    use super::*;
    use commputer_pouw::engine::{run_job, JobInputs};
    use commputer_pouw::ids::{JobId, ParticipantId};
    use commputer_pouw::job::{Job, Verdict};
    use commputer_pouw::oracle::{ByteEq, Ledger};
    use commputer_pouw::params::GameParams;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const GUEST: &[u8] = include_bytes!("../src/wasm/fixtures/guest_example.wasm");

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    fn setup_game() -> (WasmOracle, JobSpec, Vec<u8>) {
        let input = b"useful work, verified".to_vec();
        let mut store = ProgramStore::new();
        let program_hash = store.insert(GUEST.to_vec());
        let input_hash: [u8; 32] = Sha256::digest(&input).into();
        (WasmOracle::new(store, WasmLimits::default()), JobSpec { program_hash, input_hash }, input)
    }

    /// Spec §9.E / success criterion §12.3: a real Rust-compiled program runs
    /// through the UNCHANGED verification game and settles Confirmed 85/10/5.
    #[test]
    fn real_wasm_job_confirms_and_settles_85_10_5() {
        let mut p = GameParams::default(); // sample_rate_bps = 10_000 => always sampled
        p.p_trap_bps = 0; // keep the honest path deterministic
        let mut l = Ledger::new();

        let (oracle, spec, input) = setup_game();
        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();

        l.credit(submitter, 100);
        l.credit(executor, p.executor_bond);
        for c in &candidates {
            l.credit(*c, p.verifier_bond);
        }
        let total0 = l.total_supply();

        let job = Job {
            id: JobId::derive(&spec.program_hash, &spec.input_hash, &submitter, 0),
            submitter,
            spec,
            budget: 100,
        };

        let honest_claim = |true_hash: &[u8; 32]| *true_hash;
        let honest_reveal =
            |_v: &ParticipantId, true_hash: &[u8; 32], _exec: &[u8; 32]| *true_hash;
        let no_challenge = |_t: &[u8; 32], _e: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: &input,
            executor,
            executor_bond: p.executor_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: p.verifier_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };

        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);
        let (verdict, out) = run_job(&mut l, &p, &inputs, &oracle, &ByteEq, &stake, &mut rng);

        assert!(matches!(verdict, Verdict::Confirmed { .. }), "got {verdict:?}");
        // Same arithmetic as the IteratedHashVm baseline test: 85 worker,
        // 10/3=3 each to k=3 verifiers (9), remainder 1 + 5% slice burned (6).
        assert_eq!(out.worker_paid, 85);
        assert_eq!(out.verifiers_paid, 9);
        assert_eq!(out.burned, 6);
        assert_eq!(l.balance_of(&executor), 85 + p.executor_bond);
        assert_eq!(l.total_supply(), total0, "conservation: no mint");
        assert_eq!(l.escrowed(), 0, "no value stranded in escrow");
    }

    /// A cheating executor against the REAL oracle is caught: the committee
    /// independently re-executes the wasm and reveals the true digest.
    #[test]
    fn cheating_executor_against_real_wasm_is_disputed() {
        let mut p = GameParams::default();
        p.p_trap_bps = 0;
        let mut l = Ledger::new();

        let (oracle, spec, input) = setup_game();
        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();

        l.credit(submitter, 100);
        l.credit(executor, p.executor_bond);
        for c in &candidates {
            l.credit(*c, p.verifier_bond);
        }
        let total0 = l.total_supply();

        let job = Job {
            id: JobId::derive(&spec.program_hash, &spec.input_hash, &submitter, 1),
            submitter,
            spec,
            budget: 100,
        };

        let cheat_claim = |_true_hash: &[u8; 32]| [0xEE; 32]; // skipped the work
        let honest_reveal =
            |_v: &ParticipantId, true_hash: &[u8; 32], _exec: &[u8; 32]| *true_hash;
        let no_challenge = |_t: &[u8; 32], _e: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: &input,
            executor,
            executor_bond: p.executor_bond,
            executor_claim: &cheat_claim,
            candidates: &candidates,
            verifier_bond: p.verifier_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };

        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);
        let (verdict, out) = run_job(&mut l, &p, &inputs, &oracle, &ByteEq, &stake, &mut rng);

        assert!(matches!(verdict, Verdict::Disputed { .. }), "got {verdict:?}");
        assert_eq!(out.submitter_refunded, 100, "submitter made whole");
        assert!(!out.slashed.is_empty(), "the cheater was slashed");
        assert_eq!(l.total_supply(), total0, "conservation holds on the dispute path");
        assert_eq!(l.escrowed(), 0);
    }
}
```

- [ ] **Step 2: Run to verify** — `cargo test -p commputer-pouw --features wasm-runtime --test wasm_runtime game_integration` → PASS (2 tests). These two tests ARE the point of the cycle: the real runtime plugs into the game with zero engine changes.

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/tests/wasm_runtime.rs
git commit -m "test(pouw/wasm): real compiled-wasm job through the UNCHANGED game — Confirmed 85/10/5 + cheater Disputed, conservation holds (Task 11)"
```

---

### Task 12: README, regression sweep, wrap-up

**Files:**
- Modify: `src/staging/pouw/README.md` (additive section only)

- [ ] **Step 1: Append a `## WASM runtime (wasm-runtime feature)` section to the README** covering, briefly: how to run (`cargo test -p commputer-pouw --features wasm-runtime`), the ABI contract (exports `memory`/`alloc`/`run`, packed i64, canonical guest packing formula), the gate rules table (one line each), the limits + their consensus-criticality (exact wasmi pin; fingerprint folded into every digest; upgrading = coordinated protocol change), the guest rebuild instructions (`guest-example/build-guest.sh`), and the founder CI note verbatim from spec §9: the x86_64+aarch64 cross-arch determinism gate is a REQUIRED CI step for the testnet path that could not be run from this machine (same-arch determinism is what the test suite proves).

- [ ] **Step 2: Full regression sweep** (all from `/home/operator/Coin/src`):

Run: `cargo test -p commputer-pouw --features wasm-runtime`
Expected: ALL green — the new matrix plus every pre-existing test.

Run: `cargo test -p commputer-pouw`
Expected: exactly the pre-existing baseline (39 tests incl. the conservation proptest), wasmi not compiled.

Run: `cargo run -p commputer-pouw --bin pouw-sim --release 2>/dev/null | tail -5`
Expected: still prints `HONEST PLAY DOMINATES`.

Run: `cargo build 2>&1 | tail -2` (workspace default build)
Expected: clean — sibling crates unaffected.

- [ ] **Step 3: Commit**

```bash
git add src/staging/pouw/README.md
git commit -m "docs(pouw): WASM runtime README section + full regression sweep green (Task 12)"
```

---

## Definition of done (mirrors spec §12)

1. `cargo test -p commputer-pouw --features wasm-runtime` green — full §9 matrix (Tasks 1–11).
2. Default build untouched and green; pouw-sim regression passes (Task 12).
3. The compiled-Rust guest settles `Confirmed` at exactly 85/10/5 through the unchanged game (Task 11).
4. Only declared integration points among existing files: `lib.rs` one line, `Cargo.toml` feature/deps, README additive section. Everything else NEW under `src/staging/pouw/`.
5. README documents ABI, limits, the consensus-critical pin, and the founder cross-arch CI note.
