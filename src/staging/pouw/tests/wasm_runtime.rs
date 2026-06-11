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
    // wasmi leaves a small remainder on the OOF trap (empirically verified at
    // multiple budgets; see ExecOutcome::fuel_consumed's doc note). NEVER assert
    // == budget; cross-instance equality above is the consensus property (§9.B).
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

// ---- EXTRA tests requested by reviews (Task 3 + Task 5) ----

/// EXTRA (Task 3 review): drive out-of-fuel through BULK-MEMORY ops so the
/// engine's fuel charging on memory operations is exercised — a regression
/// guard for the Memory/Fuel OutOfFuel classification arms across future
/// coordinated wasmi bumps. Must classify as OutOfFuel and be consensus-equal.
#[test]
fn bulk_memory_out_of_fuel_is_classified_and_deterministic() {
    let fixture = format!(
        "{ABI_SHELL_TOP} (loop $l (memory.fill (i32.const 0) (i32.const 0) (i32.const 65536)) (br $l)) (i64.const 0)))"
    );
    let outcome = assert_consensus(&fixture, b"in");
    assert_eq!(outcome.result, Err(ExecError::OutOfFuel));
}

/// EXTRA (Task 5 review): the table gate rules had no coverage. Exercised
/// directly through the public validate_module — same gate the oracle runs.
#[test]
fn growable_table_rejected_by_gate() {
    use commputer_pouw::wasm::validation::validate_module;
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1 1)
            (table 1 2 funcref)
            (func (export "alloc") (param i32) (result i32) (i32.const 0))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("fixture assembles");
    match validate_module(&wasm, &WasmLimits::default()) {
        Err(ExecError::Rejected(why)) => assert!(why.contains("table"), "got {why:?}"),
        other => panic!("expected Rejected(table...), got {other:?}"),
    }
}

/// EXTRA (Task 5 review): table.grow opcode is scanned out even on a fixed table.
/// `table.grow` requires the reference-types proposal, which GATE_FEATURES disables.
/// wat =1.251.0 assembles reference-types syntax by default; the assembled module
/// must be rejected — either at the feature gate layer ("feature gate") or at the
/// operator scan layer ("table.grow"). Both are correct and accepted.
#[test]
fn table_grow_rejected_by_gate() {
    use commputer_pouw::wasm::validation::validate_module;
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1 1)
            (table 1 1 funcref)
            (func (export "alloc") (param i32) (result i32)
                (table.grow (ref.null func) (i32.const 1)))
            (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("wat =1.251.0 assembles reference-types syntax by default; if a wat bump changed this, update the fixture deliberately");
    match validate_module(&wasm, &WasmLimits::default()) {
        Err(ExecError::Rejected(why)) => assert!(
            why.contains("table.grow") || why.contains("feature gate"),
            "got {why:?}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

// ---- Task 9: property-based determinism testing ----

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
        /// output AND fuel, and the output is the expected transform.
        #[test]
        fn independent_oracles_always_agree(input in proptest::collection::vec(any::<u8>(), 0..512)) {
            let (a, spec) = setup(DOUBLER, &input);
            let (b, _) = setup(DOUBLER, &input);
            let (ra, rb) = (a.execute(&spec, &input), b.execute(&spec, &input));
            prop_assert_eq!(&ra.result, &rb.result);
            prop_assert_eq!(ra.fuel_consumed, rb.fuel_consumed);
            let expected: Vec<u8> = input.iter().map(|b| b.wrapping_mul(2)).collect();
            prop_assert_eq!(ra.result, Ok(expected));
        }
    }
}

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
        // defensive: traps are sim-layer only today; pinned off in case the engine wires the trap draw later
        let p = GameParams { p_trap_bps: 0, ..GameParams::default() }; // sample_rate_bps = 10_000 => always sampled
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
        // defensive: traps are sim-layer only today; pinned off in case the engine wires the trap draw later
        let p = GameParams { p_trap_bps: 0, ..GameParams::default() };
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
        assert_eq!(out.slashed, vec![(executor, p.executor_bond)], "exactly the cheater's bond is slashed");
        assert_eq!(l.balance_of(&executor), 0, "the cheater's bond was consumed");
        assert_eq!(l.total_supply(), total0, "conservation holds on the dispute path");
        assert_eq!(l.escrowed(), 0);
    }
}

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
