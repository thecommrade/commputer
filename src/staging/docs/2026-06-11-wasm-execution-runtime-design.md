# WASM Execution Runtime (`WasmOracle`) — Design

**Date:** 2026-06-11
**Status:** Approved by founder (engine choice + all sections) — this document is the committed spec.
**Branch:** `agent-wire-testnet-20260610` (agent work; staging only; no protected files).
**Parent spec:** `2026-06-10-pouw-verification-game-design.md` — this is committed follow-up cycle **#1** from its §10 ("real metered WASM/WASI sandbox behind `ExecutionOracle`").

---

## 1. Goal

Replace the toy `IteratedHashVm` with a **real, deterministic, metered, sandboxed WASM execution
oracle** behind the existing `ExecutionOracle` trait in `src/staging/pouw/src/oracle.rs` —
**without changing the verification game in any way.**

The hard requirement comes from the game itself: the `EquivalenceOracle` is literal byte equality
(`a == b`), and `engine.rs` re-hashes the oracle's output into the 32-byte claim that the
executor, every committee verifier, and every escalation panelist must independently reproduce.
Therefore the runtime must produce **bit-identical output across machines, OSes, and CPU
architectures (x86_64, aarch64), or honest nodes falsely dispute each other.**

Three properties, all mandatory:

1. **Bit-determinism** — same `(program, input)` ⇒ same bytes, everywhere, every time.
2. **Deterministic metering with hard caps** — every node agrees on the exact compute cost
   (fuel), and a malicious program can neither hang nor OOM a verifier (fuel cap + memory cap +
   call-depth cap, all identical constants on every node).
3. **Sandboxing** — untrusted bytecode gets **zero** host capabilities: no filesystem, network,
   clock, randomness, or environment. Input enters only as content-addressed bytes; output leaves
   only as returned bytes.

### Non-goals (each is its own deferred cycle, per the parent spec §10)

- **Cost-model coupling** — measured fuel does NOT yet replace `C_exec`/`C_ver` in the economic
  game. The fuel number is *recorded* (see §7) but settlement is untouched.
- **Data availability** — no fetching. The `ProgramStore` is populated locally (tests/sim);
  retrieval/replication is cycle #2.
- **On-chain wiring** — no consensus params, no `event_loop`/`JobPool` integration (protected
  files; founder-owned, cycle #3).
- **Floats / AI workloads** — float opcodes are **rejected** in v1 (see §5). Reproducible-AI and
  approximate equivalence are cycles #4/#5 on the `EquivalenceOracle` seam, not this one.

## 2. Engine decision: wasmi (pinned), integer-only

**Engine: `wasmi` (wasmi-labs), pinned with an exact `=` version (1.0.x line).**

Rationale (from a five-dimension research sweep across the WASM spec's nondeterminism notes,
wasmtime/wasmer/wasmi docs, and the production record of Polkadot/Substrate, CosmWasm, NEAR,
Arbitrum Stylus, Stellar Soroban, and the Internet Computer):

- A **pure interpreter is deterministic by construction.** There is no JIT codegen surface to
  discipline: no per-version Cranelift machine-code drift, no arch-specific lowering quirks, no
  relaxed-SIMD trap. This is exactly why Stellar Soroban chose wasmi, and wasmi is proven for
  cross-node consensus in Polkadot/Substrate.
- **Built-in fuel metering** — deterministic instruction-stream accounting comes with the engine.
- **Lightest dependency** — interpreter + parser-class deps only (no codegen backend); roughly an
  order of magnitude lighter than wasmtime+Cranelift in transitive crates and cold-build time.
  This matters for a `src/staging` prototype crate.
- **Integer-only policy makes the engine's one weakness moot.** wasmi has no first-class
  NaN-canonicalization flag (wasmtime's headline determinism feature), but v1 **rejects all float
  opcodes at module load**, so there is no NaN bit-pattern to canonicalize. This was CosmWasm's
  posture pre-1.5 and is the strongest possible guarantee.

**The documented fast-path successor is wasmtime+Cranelift behind the SAME trait** — adopted only
if interpreter throughput becomes the bottleneck, and only with the full pinning discipline
(exact version + `cranelift_nan_canonicalization(true)` + relaxed-SIMD off + threads off + frozen
operator-cost table, all consensus-critical). Nothing in this design blocks that swap; the
`ExecutionOracle` trait is the firewall. (Codebase note: the dead stub
`src/node/src/wasm_executor.rs` says "real wasmtime integration planned" — this design supersedes
that note for the consensus path, for the reasons above; the stub itself is untouched.)

**Engine identity is consensus-critical.** `ENGINE_ID = "wasmi"`, the exact pinned version
string, and a `config_fingerprint()` (a SHA-256 over the limits, validation policy version, and
ABI version — see §6) are defined as constants. In the deferred on-chain cycle the founder wires
them into consensus params; upgrading the engine or any limit is then a coordinated network
change, never a silent dependency bump. The fingerprint is folded into every outcome digest (§8),
so two nodes with mismatched configs diverge **always and immediately** (debuggable), not
probabilistically on edge cases.

## 3. The seam decision: the trait does not change

`ExecutionOracle::run(&self, spec: &JobSpec, input: &[u8]) -> Vec<u8>` stays **exactly as it is**
— infallible, unchanged. A new `WasmOracle` type (new files only) implements it. Zero edits to
`oracle.rs`, `engine.rs`, or any existing file. (Honors the agent work method: new files only;
and the scope decision: no cost coupling, so the game never needs to see fuel.)

An infallible signature is reconciled with a fallible runtime by returning a **canonical outcome
digest** (32 bytes) instead of raw output (§8). The rich, fallible interface —
`WasmOracle::execute(&self, spec, input) -> ExecOutcome` with the error variant and consumed fuel
— exists as a **concrete inherent method**, *not* on the trait, so the future cost-coupling cycle
has everything it needs without touching the game now.

## 4. Module layout (all new files; feature-gated)

```
src/staging/pouw/src/wasm/
  mod.rs          // module root; #[cfg(feature = "wasm-runtime")]; re-exports
  limits.rs       // WasmLimits + engine-identity constants + config_fingerprint()
  validation.rs   // the determinism gate: wasmparser scan (§5)
  store.rs        // ProgramStore: content-addressed program_hash -> wasm bytes
  abi.rs          // host side of the zero-import guest ABI (§7)
  error.rs        // ExecError + ExecOutcome + canonical digest fold (§8)
  oracle.rs       // WasmOracle: ExecutionOracle impl + execute()
  fixtures/       // .wat corpus (inline/asset) + prebuilt guest_example.wasm
src/staging/pouw/guest-example/
  // tiny #![no_std] Rust crate, target wasm32-unknown-unknown, NOT a workspace member;
  // its built .wasm is CHECKED IN under fixtures/ with a rebuild script (build-guest.sh)
  // so `cargo test` never requires the wasm32 toolchain.
```

**Guest build constraints (required, or the §5 gate rejects our own showcase):** the default
Rust wasm toolchain emits a memory with *no maximum* and its default allocator (dlmalloc) emits
`memory.grow` — both §5 rejects. `build-guest.sh` must therefore (a) pin the memory with
`-C link-arg=--initial-memory=N -C link-arg=--max-memory=N` (equal `N ≤ max_memory_bytes`, so
min == max), (b) use a static bump allocator in the `#![no_std]` guest (no dlmalloc, no grow),
and (c) the guest crate carries an empty `[workspace]` table in its Cargo.toml so cargo builds it
standalone inside the workspace tree without workspace-inference errors.

`src/staging/pouw/src/lib.rs` gains one line: `pub mod wasm;` (cfg-gated). `Cargo.toml` gains the
optional deps + feature. Every new file carries the standard staging header comment (what it
does, where it wires in, which existing file would need changes — for this cycle: none).

### Dependency containment

```toml
[features]
wasm-runtime = ["dep:wasmi", "dep:wasmparser"]

[dependencies]
wasmi      = { version = "=1.0.x", optional = true }   # exact pin chosen at impl time; consensus-critical
wasmparser = { version = "<pinned>", optional = true }

[dev-dependencies]
wat = "<pinned>"   # .wat -> .wasm assembly for fixtures; test-only
```

Two dependency notes, both deliberate: Cargo has no optional dev-dependencies, so `wat` builds
for every `cargo test -p commputer-pouw` even with the feature off — accepted (it is tiny; the
heavy deps stay gated). And wasmi 1.0 itself depends on `wasmparser`; pin our direct `wasmparser`
to the same major wasmi pulls so the lockfile carries one copy, not duplicate majors.

Default build (feature OFF) is exactly today's crate: `IteratedHashVm` remains the default
oracle, sibling crates and existing CI are unaffected, and wasmi only enters the build graph when
`--features wasm-runtime` is selected. All wasm-runtime tests are `#[cfg(feature =
"wasm-runtime")]`; the verification command for this cycle is
`cargo test -p commputer-pouw --features wasm-runtime`.

## 5. The determinism gate (`validation.rs`)

A `wasmparser` scan over the raw module, run **before** instantiation, on both executor and every
verifier. The engine's own config disables what it can (no SIMD/threads features enabled; fixed
stack limits), but **this scan is the authoritative gate** — determinism is enforced, not assumed.

**The scan is an allow-list:** only the core integer/control/fixed-memory instruction set and the
required export shape are permitted; any construct outside it — including every row below and any
unknown or future operator `wasmparser` surfaces — is rejected by the wildcard arm. The table is
the *complement* of the allow-list, stated as reject rules for reviewability. A module is
**rejected** (deterministically, same verdict on every node) if it contains:

| # | Rejected construct | Why |
|---|---|---|
| 1 | Any float opcode (`f32.*`, `f64.*`, float-typed locals/globals/params/results, v128 float ops) | NaN bit-patterns are the #1 cross-arch nondeterminism source; integer-only v1 removes the class entirely |
| 2 | Any SIMD or relaxed-SIMD opcode | relaxed-SIMD is nondeterministic across archs by design; fixed SIMD is excluded in v1 to shrink the surface |
| 3 | Threads / atomics / shared memory | interleavings are inherently irreproducible |
| 4 | `memory.grow` or `table.grow` | spec-sanctioned nondeterministic success/failure tied to host resources |
| 5 | Memory or table with `min != max` | growth must be impossible by construction; size is fixed at instantiation |
| 6 | Declared memory max exceeding `WasmLimits.max_memory_bytes` | the memory cap is a shared constant, not host RAM |
| 7 | **Any import whatsoever** | the ABI imports nothing; clock/random/fs/net/env nondeterminism is structurally absent |
| 8 | Missing/ill-typed required exports: `memory`, `alloc(i32)->i32`, `run(i32,i32)->i64` | the ABI contract (§7) |
| 9 | A `start` section | a start function executes guest code at instantiation, outside the set-fuel→`alloc`→`run` flow; rejecting it makes the ABI calls the *only* execution path |

Plus two runtime-configured bounds (identical constants everywhere): a **fixed call-depth /
recursion cap** via wasmi's recursion/stack-height config (`Config::set_max_recursion_depth` /
`set_max_stack_height` in the 1.0 API — deep recursion traps at the identical call on every node)
and the **fuel budget** (§6). wasmi's `CompilationMode` is pinned to eager so any translation-time
reject surfaces at module build, keeping error classification stable. Validation failure is an `ExecError` and folds to the
canonical error sentinel (§8) — a node that receives an invalid program still produces the same
claim as every other node.

## 6. Limits & engine identity (`limits.rs`)

```rust
pub struct WasmLimits {
    pub fuel: u64,               // total fuel budget for the whole guest interaction (alloc + run)
    pub max_memory_bytes: u64,   // hard cap on the single linear memory (min==max enforced at §5)
    pub max_call_depth: u32,     // recursion bound (wasmi stack limits)
    pub max_input_bytes: u64,    // input larger than this -> deterministic reject
    pub max_output_bytes: u64,   // declared output larger than this -> deterministic reject
}
```

One `Default` (prototype constants, e.g. fuel 100M, memory 64 MiB, depth 1024, input/output
10 MiB — final numbers fixed in the plan) used by every node. `limits.rs` also defines:

- `ENGINE_ID: &str = "wasmi"`, `ENGINE_VERSION: &str = "<the exact pin>"`,
  `ABI_VERSION: u32 = 1`, `VALIDATION_VERSION: u32 = 1`.
- `config_fingerprint(&WasmLimits) -> [u8; 32]` — SHA-256 over engine id ‖ version ‖ ABI version
  ‖ validation version ‖ every limit, in a fixed serialization. Folded into every outcome digest
  (§8); destined for consensus params in the on-chain cycle.

**Metering rules:** fuel (wasmi built-in; deterministic instruction-stream accounting) is the
*only* compute meter. **Wall-clock is forbidden as a meter** — the dead stub's `max_cpu_time_ms`
pattern is explicitly rejected for the consensus path because it guarantees false disputes. (A
host-side wall-clock watchdog MAY later abort a wedged process as local belt-and-suspenders, but
it is non-consensus and out of scope for v1.) Fuel exhaustion is a deterministic trap at the
identical instruction on every node. Consumed fuel = `budget − remaining`, recorded in
`ExecOutcome.fuel_consumed`, **not** consumed by settlement in this cycle.

## 7. Execution path & guest ABI (`oracle.rs`, `abi.rs`, `store.rs`)

### ProgramStore

In-memory content-addressed map `[u8;32] -> Arc<[u8]>`. `insert(bytes)` hashes and stores under
`sha256(bytes)`; `get(&hash)` returns the bytes or `None` (→ `ExecError::ProgramUnavailable`).
No fetching, no eviction — DA is a later cycle. Tests and the sim populate it directly.

### The zero-import guest ABI (v1)

The guest module exports exactly:

- `memory` — the single linear memory (min == max).
- `alloc(len: i32) -> i32` — returns a pointer to `len` writable bytes in linear memory.
- `run(ptr: i32, len: i32) -> i64` — executes the job on `input = memory[ptr .. ptr+len]`,
  returns out_ptr and out_len packed into one i64. The canonical guest formula is
  `(((out_ptr as u64) << 32) | (out_len as u64)) as i64` — packing through *signed* types would
  sign-extend an out_len with its high bit set and corrupt out_ptr. The host always decodes both
  halves as u32 and bounds-checks, so a mis-packed value is a deterministic `AbiViolation`, never
  a consensus hazard — the formula note is a guest-author footgun warning.

The guest imports **nothing**. Host flow in `WasmOracle::execute`:

1. `store.get(spec.program_hash)` → bytes; verify `sha256(bytes) == spec.program_hash`
   (defense-in-depth re-check) and `sha256(input) == spec.input_hash`; check
   `input.len() <= max_input_bytes`. Any failure → the matching `ExecError`.
2. `validation::validate(bytes, &limits)` (§5) → `ExecError::Rejected(reason)` on failure.
3. Build engine + store with fuel metering on and the stack/memory limits; instantiate
   (instantiation executes no guest code — `start` sections are rejected at §5);
   **set the full fuel budget once** (it covers `alloc` + `run` together).
4. `let ptr = alloc(input.len())`; bounds-check `[ptr, ptr+len)` against the fixed memory size;
   write input.
5. `let packed = run(ptr, len)`; split into `(out_ptr, out_len)`; check
   `out_len <= max_output_bytes` and bounds-check `[out_ptr, out_ptr+out_len)`; read output.
6. Any trap (out-of-fuel, unreachable, OOB, div-by-zero, stack exhaustion) or ABI violation
   (bad pointer, oversized output) at any step → the matching `ExecError`. Otherwise
   `Ok(output_bytes)` + `fuel_consumed`.

Bounds violations are `ExecError::AbiViolation` — a *program* fault (deterministic on every
node), never a host panic.

## 8. Error model & canonical outcome digest (`error.rs`)

```rust
pub enum ExecError {
    ProgramUnavailable,      // hash not in ProgramStore
    HashMismatch,            // program or input bytes don't match the spec's hashes
    Rejected(/*reason*/),    // failed the §5 determinism gate
    OutOfFuel,
    Trapped(/*kind*/),       // unreachable / OOB / div0 / stack overflow / ...
    AbiViolation(/*what*/),  // bad alloc ptr, oversized/out-of-bounds output, bad packing
}

pub struct ExecOutcome {
    pub result: Result<Vec<u8>, ExecError>,
    pub fuel_consumed: u64,  // 0 when execution never started
}
```

**The fold (the consensus-facing value).** `ExecutionOracle::run` returns:

- success: `sha256(DOMAIN ‖ config_fingerprint ‖ 0x00 ‖ output_bytes)`
- any error: `sha256(DOMAIN ‖ config_fingerprint ‖ 0x01)` — **one sentinel for every error kind**

with `DOMAIN = "commputer-pouw-wasm-v1"`. Properties this buys:

- **No false disputes among honest nodes:** every failure mode above is deterministic given
  identical (program, input, limits, engine version), so all honest nodes compute the same
  sentinel — a malformed or hostile program yields an *agreeing* committee, and the game
  settles it like any other claim. **One scoped exception:** `ProgramUnavailable` depends on
  *local* store state, not on (program, input) — it cannot cause an honest-vs-honest split in
  this cycle only because tests/sim populate every node's store explicitly. The DA cycle must
  resolve abstain-vs-sentinel before unavailability can occur in the wild (§10.2).
- **No trap covert channel:** *which* error occurred is not distinguishable in the digest, so a
  malicious program cannot encode nondeterministic signal in its failure mode. The rich variant
  lives only in `ExecOutcome` for local logging/tests and the future cost cycle.
- **No OK/error collision:** domain separation + the discriminant byte make
  `sha256(output)`-style accidental collisions with the sentinel impossible.
- **Config drift fails loud:** the fingerprint in the preimage makes *any* limit/version mismatch
  diverge on every job, immediately, rather than only on rare edge inputs.

`engine.rs`'s `result_hash` helper already re-hashes whatever `run` returns into the fixed claim
array; the digest slots in with zero engine changes.

## 9. Testing matrix

All `#[cfg(feature = "wasm-runtime")]`; run with `cargo test -p commputer-pouw --features
wasm-runtime`. Fixture programs are inline `.wat` assembled by the `wat` dev-dependency, plus the
checked-in `guest_example.wasm`.

**A. Determinism (the property the game stands on)**
- Same (program, input) twice on one oracle → identical digest.
- Two independently constructed `WasmOracle` instances (fresh engine, fresh store — simulating
  executor vs verifier) → identical digest.
- Different input / different program → different digest.
- Proptest: random integer inputs through the example guest; two instances always agree.

**B. Adversarial fixtures (caps + sandbox, each → deterministic sentinel)**
- Infinite loop → `OutOfFuel`; identical sentinel from two instances; `fuel_consumed <= budget`
  and **exactly equal across two independent oracle instances**. (Do NOT assert
  `fuel_consumed == budget`: on an out-of-fuel trap wasmi 1.0 leaves the remaining-fuel counter
  at the pre-trap remainder — load-bearing for its resumable-execution feature — so consumed
  generally lands just under the budget. The consensus property is that the remainder is
  *identical on every node*, not that it is zero. Classify out-of-fuel via `Error::kind()`
  covering all OutOfFuel variants, not only `as_trap_code()`.)
- `unreachable` → `Trapped`.
- OOB memory access → `Trapped`.
- Deep recursion → call-depth trap.
- `run` returns an out-of-bounds / oversized (ptr,len) → `AbiViolation`.
- Oversized input → deterministic reject.

**C. Validation rejects (each → `Rejected`, load-time, no execution)**
- A float opcode; a SIMD opcode; `memory.grow`; memory `min != max`; memory max > cap; any
  import (e.g. a WASI clock); missing `run` export; wrong export signature.
- Plus `ProgramUnavailable` (hash not in store) and `HashMismatch` (tampered input bytes).

**D. Realism showcase**
- `guest_example.wasm` (real Rust→wasm32 integer transform, built by `build-guest.sh` under the
  §4 guest build constraints — pinned min==max memory, static bump allocator, no grow; binary
  checked in) runs end-to-end; output matches the natively-computed expected bytes; rebuild
  script documented but never required by `cargo test`.
- The checked-in binary itself passes the §5 validation gate (regression that the build
  constraints actually held).

**E. Game integration (the point of the cycle)**
- `engine::run_job` driven with `WasmOracle` + `ByteEq` + `Ledger`, honest executor + honest
  committee, a real fixture program → `Confirmed`, exact 85/10/5 settlement, conservation holds.
- Same but the "executor" claims a wrong hash → `Disputed` and slashing, proving the real oracle
  changes nothing about the game.

**F. Regression**
- Default-feature build: the existing 39 tests + conservation proptest still pass untouched;
  `cargo run -p commputer-pouw --bin pouw-sim` still prints HONEST PLAY DOMINATES.

**Founder CI note (cannot be claimed from this machine):** the cross-arch gate — the same corpus
on x86_64 *and* aarch64 runners asserting byte-identical digests and identical `fuel_consumed` —
is specified here as a required CI step for the testnet path, but this session can only verify
same-arch determinism locally. Interpreter-by-construction determinism makes the risk low; the
gate makes it checked.

## 10. Deferred to later cycles (explicit)

1. **Fuel → economics:** replace `C_exec`/`C_ver` with measured fuel in the sim/settlement
   (needs a tokenomics decision on fuel as the work denomination, and possibly a weighted
   per-operator cost table — frozen as a consensus constant). That cycle must also re-affirm or
   change a deliberate v1 policy this design creates: a deterministically failing or
   gate-rejected program settles `Confirmed` on the error sentinel, paying the executor 85% of
   budget — defensible (work was done; the submitter supplied a bad program), but it must remain
   a chosen policy, not an accident.
2. **Data availability:** `ProgramStore` fetching/replication; verifiers retrieving program +
   input by hash. Inherits a hard constraint from §8: it must decide abstain-vs-sentinel for
   `ProgramUnavailable` — an infallible `run()` cannot express abstention, and a node genuinely
   missing data must not assert the error sentinel as its honest claim.
3. **On-chain wiring:** engine identity + limits fingerprint into consensus params; `ChainHooks`
   adapter; `event_loop`/`JobPool` (protected files; founder).
4. **Floats / AI:** lifting reject-rule #1 is a protocol decision that almost certainly means the
   wasmtime fast-path (NaN canonicalization) + the cycle-B/C `EquivalenceOracle` work.
5. **Stack-height static analysis** (NEAR finite-wasm-style) as hardening beyond the runtime
   recursion cap; **local compiled-module caching** keyed on
   `hash(program_hash ‖ engine identity ‖ fingerprint)` if instantiation cost ever matters.

## 11. Risks

- **Engine version drift:** even an interpreter can change fuel accounting between releases; the
  `=` pin + fingerprint-in-digest contains it; the residual risk is a node operator who doesn't
  upgrade in lockstep — that is precisely why the identity belongs in consensus params (cycle #3).
- **Interpreter throughput:** wasmi is materially slower than a JIT, and the game re-executes on
  K verifiers. Accepted for the prototype (correctness ≫ throughput); the wasmtime fast-path is
  the documented successor behind the same trait.
- **Integer-only is a capability ceiling:** real float workloads (ML inference) cannot run on v1.
  Deliberate: float determinism is the cycle-B/C problem, not this cycle's.
- **The gate must stay exhaustive:** any wasm proposal wasmi later enables by default (e.g. new
  opcodes) must be caught by the validation scan, not assumed absent — the scan rejects unknown/
  unlisted constructs by default (allow-list posture), which is the safe failure direction.

## 12. Success criteria (definition of done)

1. `cargo test -p commputer-pouw --features wasm-runtime` green: the full §9 A–E matrix.
2. Default-feature build untouched and green (§9 F).
3. A real Rust-compiled guest program runs through the **unchanged** verification game end-to-end
   and settles `Confirmed` at exactly 85/10/5.
4. Zero modifications to existing files except the two declared integration points
   (`lib.rs` module line, `Cargo.toml` feature/deps) — both staging-crate files, no protected
   files anywhere.
5. The crate README gains a WasmOracle section: ABI contract, limits, the consensus-criticality
   of the pin, and the founder CI note.
