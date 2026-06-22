//! Scenario builders — the single source of truth shared by tests/e2e.rs and bin/e2e-trace.rs.
//! Each builder is deterministic and returns a ScenarioReport.

use crate::programs;
use crate::world::{Actors, GateOutcome, JobWorld};
use commputer_da::params::ChunkingParams;
use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{SettlementOutcome, Verdict};
use commputer_pouw::economics::EconViolation;

/// How a scenario terminated.
#[derive(Debug)]
pub enum Terminal {
    /// The game ran and settled.
    Settled(Verdict, SettlementOutcome),
    /// Rejected by the economic pre-check, before any DA work (scenario 3).
    Rejected(EconViolation),
    /// Too few data-holding verifiers to form a committee — short-circuit (scenarios 6/7).
    NoCommittee,
}

pub struct ScenarioReport {
    pub name: &'static str,
    pub effective: usize,
    pub abstained: usize,
    pub program_present: bool,
    pub terminal: Terminal,
    pub conserved: bool,
    pub trace: TraceInfo,
}

/// Human-readable lifecycle facts for the trace binary.
#[derive(Default)]
pub struct TraceInfo {
    pub program_id8: String,
    pub da_root8: String,
    pub n_data: u16,
    pub n_total: u16,
    pub chunk_size: u32,
    pub fuel_cap: u64,
}

fn hex8(b: &[u8; 32]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

fn trace_of(world: &JobWorld, att: &commputer_da::params::DaAttestation) -> TraceInfo {
    TraceInfo {
        program_id8: hex8(&att.program_id),
        da_root8: hex8(&att.da_root),
        n_data: att.n_data,
        n_total: att.n_total,
        chunk_size: att.chunk_size,
        fuel_cap: world.fuel_cap(),
    }
}

/// Honest closures (reused by several scenarios).
fn honest_claim(h: &[u8; 32]) -> [u8; 32] { *h }
fn honest_reveal(_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]) -> [u8; 32] { *h }
fn no_challenge(_: &[u8; 32], _: &[u8; 32]) -> Option<ParticipantId> { None }

pub fn happy_path() -> ScenarioReport {
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();
    let gate = world.gate_pool(&att);
    let (effective, abstained, program_present) =
        (gate.effective.len(), gate.abstained.len(), gate.program_bytes.is_some());
    let trace = trace_of(&world, &att);
    let funding = world.min_funding().expect("priceable");

    let claim = honest_claim;
    let reveal = honest_reveal;
    let challenge = no_challenge;
    let actors = Actors { executor_claim: &claim, verifier_reveal: &reveal, challenge: &challenge };
    let (res, conserved) = world.run_lifecycle(&att, gate, &actors, funding);
    let terminal = match res {
        Ok((v, o)) => Terminal::Settled(v, o),
        Err(e) => Terminal::Rejected(e),
    };
    ScenarioReport { name: "happy_path", effective, abstained, program_present, terminal, conserved, trace }
}

/// Scenario 2: the executor claims a wrong output hash. The committee re-executes on the
/// DA-reconstructed bytes, disagrees, and the dispute slashes the executor while refunding
/// the submitter's full budget.
pub fn cheating_executor() -> ScenarioReport {
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();
    let gate = world.gate_pool(&att);
    let (effective, abstained, program_present) =
        (gate.effective.len(), gate.abstained.len(), gate.program_bytes.is_some());
    let trace = trace_of(&world, &att);
    let funding = world.min_funding().expect("priceable");

    let claim = |_h: &[u8; 32]| [0xAB; 32]; // a wrong hash
    let reveal = honest_reveal;
    let challenge = no_challenge;
    let actors = Actors { executor_claim: &claim, verifier_reveal: &reveal, challenge: &challenge };
    let (res, conserved) = world.run_lifecycle(&att, gate, &actors, funding);
    let terminal = match res {
        Ok((v, o)) => Terminal::Settled(v, o),
        Err(e) => Terminal::Rejected(e),
    };
    ScenarioReport { name: "cheating_executor", effective, abstained, program_present, terminal, conserved, trace }
}

/// Scenario 3: funding one below the priced minimum is rejected by the pure economic
/// pre-check, before any DA fetch or WASM execution happens.
pub fn underfunded() -> ScenarioReport {
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();
    let trace = trace_of(&world, &att);
    let min = world.min_funding().expect("priceable");
    let bad = crate::world::Funding { budget: min.budget - 1, ..min }; // one below the minimum
    let terminal = match world.precheck(&att, bad) {
        Ok(()) => panic!("underfunded must be rejected"),
        Err(e) => Terminal::Rejected(e),
    };
    // We deliberately never call gate_pool → proves "no DA fetch, no execution".
    ScenarioReport {
        name: "underfunded", effective: 0, abstained: 0, program_present: false,
        terminal, conserved: true, trace,
    }
}

/// Scenario 4: a few coded chunks are withheld. With a small chunk_size the ~200-byte
/// DOUBLER yields 2N well above the sample size, so different verifiers sample different
/// strict subsets — those that draw a withheld index abstain, the rest reconstruct from the
/// remaining ≥ N chunks and form a committee. Survivors settle Confirmed.
pub fn partial_withholding() -> ScenarioReport {
    // Small chunk_size so the ~200-byte DOUBLER yields 2N well above 16 (s = 16 < 2N) →
    // different verifiers sample different strict subsets (spec §6).
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams { chunk_size: 8, params_version: 1 },
    );
    let att = world.publish();
    // Withhold a few low chunk indices. Each is sampled by SOME verifiers (not all, since
    // s < 2N), so those abstain; the rest survive and reconstruct from the remaining ≥ N
    // chunks. Withhold < N so reconstruction stays possible (see spec §9 scenario-4 note).
    for idx in [0u16, 1, 2] {
        world.withhold(&att, idx);
    }
    let gate = world.gate_pool(&att);
    let (effective, abstained, program_present) =
        (gate.effective.len(), gate.abstained.len(), gate.program_bytes.is_some());
    let trace = trace_of(&world, &att);
    let funding = world.min_funding().expect("priceable");
    let claim = honest_claim; let reveal = honest_reveal; let challenge = no_challenge;
    let actors = Actors { executor_claim: &claim, verifier_reveal: &reveal, challenge: &challenge };
    let (res, conserved) = world.run_lifecycle(&att, gate, &actors, funding);
    let terminal = match res { Ok((v, o)) => Terminal::Settled(v, o), Err(e) => Terminal::Rejected(e) };
    ScenarioReport { name: "partial_withholding", effective, abstained, program_present, terminal, conserved, trace }
}

/// Scenario 5: the guest's `run` traps. Real WASM execution yields the error sentinel; the
/// honest executor claims that sentinel's hash and the committee honestly agrees, so the job
/// settles Confirmed with the same split as the happy path (founder-locked error-outcome
/// policy: the executor is still paid the worker share for the work performed).
pub fn erroring_guest() -> ScenarioReport {
    let world = JobWorld::new(
        programs::assemble(programs::TRAPPER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();
    let gate = world.gate_pool(&att);
    let (effective, abstained, program_present) =
        (gate.effective.len(), gate.abstained.len(), gate.program_bytes.is_some());
    let trace = trace_of(&world, &att);
    let funding = world.min_funding().expect("priceable");

    let claim = honest_claim;
    let reveal = honest_reveal;
    let challenge = no_challenge;
    let actors = Actors { executor_claim: &claim, verifier_reveal: &reveal, challenge: &challenge };
    let (res, conserved) = world.run_lifecycle(&att, gate, &actors, funding);
    let terminal = match res {
        Ok((v, o)) => Terminal::Settled(v, o),
        Err(e) => Terminal::Rejected(e),
    };
    ScenarioReport { name: "erroring_guest", effective, abstained, program_present, terminal, conserved, trace }
}

/// Scenario 6: every coded chunk that any verifier could sample is unavailable (here, the
/// default chunk_size gives 2N=2 and a single withheld index covers everyone). No candidate
/// gets Available, so no committee can form and the job short-circuits — nothing settles and
/// no ledger is ever created, so conservation is trivially preserved.
pub fn total_withholding() -> ScenarioReport {
    // Default chunk_size → 2N = 2; every verifier samples both indices. Withholding one
    // sampled chunk makes every candidate abstain (spec §9 scenario 6).
    let world = JobWorld::new(
        programs::assemble(programs::DOUBLER),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let att = world.publish();
    world.withhold(&att, 0);
    let gate = world.gate_pool(&att);
    let trace = trace_of(&world, &att);
    assert!(!world.can_form_committee(&gate), "total withholding must leave no committee");
    short_circuit("total_withholding", gate, trace)
}

/// Shared exit for scenarios where no committee can form: report the gate's tallies but mark
/// the job as NoCommittee. No ledger is created or touched, so conservation holds trivially.
fn short_circuit(name: &'static str, gate: GateOutcome, trace: TraceInfo) -> ScenarioReport {
    ScenarioReport {
        name,
        effective: gate.effective.len(),
        abstained: gate.abstained.len(),
        program_present: gate.program_bytes.is_some(),
        terminal: Terminal::NoCommittee, // no committee can form → nothing settles
        conserved: true,                 // no ledger was created or touched
        trace,
    }
}

/// Scenario 7: a tampered publish advertises program B's real coded chunks under an
/// attestation whose program_id is overwritten to program A's. DA sampling and Merkle
/// verification pass (they bind to the real da_root of B) and reconstruction succeeds, but the
/// final sha256(recon)==program_id check binds to A and fails — every candidate Abstains at
/// the re-bind, so no committee forms and the job short-circuits.
pub fn tampered_publish() -> ScenarioReport {
    // Program B = tripler; advertise its real chunks under an attestation whose program_id
    // is overwritten to program A = doubler. Sampling + Merkle verify against da_root_B and
    // reconstruction succeeds, but sha256(recon)=program_id_B ≠ program_id_A → re-bind Abstains.
    let world = JobWorld::new(
        programs::assemble(&programs::tripler_src()),
        programs::DEFAULT_INPUT.to_vec(),
        ChunkingParams::default(),
    );
    let fake_program_id = programs::program_id(&programs::assemble(programs::DOUBLER));
    let att = world.publish_with_fake_program_id(fake_program_id);
    let gate = world.gate_pool(&att);
    let trace = trace_of(&world, &att);
    short_circuit("tampered_publish", gate, trace)
}

/// Dispatch by name for the trace binary. Unknown name → happy path.
pub fn run(name: &str) -> ScenarioReport {
    match name {
        "happy" | "happy_path" => happy_path(),
        "cheat" | "cheating_executor" => cheating_executor(),
        "underfunded" => underfunded(),
        "partial" | "partial_withholding" => partial_withholding(),
        "error" | "erroring_guest" => erroring_guest(),
        "total" | "total_withholding" => total_withholding(),
        "tampered" | "tampered_publish" => tampered_publish(),
        _ => happy_path(),
    }
}
