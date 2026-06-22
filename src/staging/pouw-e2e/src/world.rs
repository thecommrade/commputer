//! Composition primitives for the end-to-end harness (spec §4/§5).
//! JobWorld owns one job's world; its methods are the pipeline steps. All public —
//! this is a harness, not a consensus surface.

use commputer_pouw::ids::ParticipantId;
use commputer_pouw::job::{Verdict, SettlementOutcome};
use commputer_pouw::params::GameParams;
use commputer_pouw::wasm::{ProgramStore, WasmLimits};
use commputer_pouw::economics::EconViolation;
use commputer_da::params::{ChunkingParams, DaAttestation, ProviderId};
use commputer_da::commit::{build_attestation, chunk_proof};
use commputer_da::facade::chunk_hash;
use commputer_da::transport::{InMemoryTransport, ManualClock};
use sha2::{Digest, Sha256};

/// How each actor behaves (engine "behavior-as-data" — see spec §5.1). The closures live
/// in the scenario builder; JobWorld borrows them for one run.
pub struct Actors<'a> {
    pub executor_claim: &'a commputer_pouw::engine::ClaimFn<'a>,
    pub verifier_reveal: &'a commputer_pouw::engine::RevealFn<'a>,
    pub challenge: &'a commputer_pouw::engine::ChallengeFn<'a>,
}

/// Funding at (or deliberately below) the fuel-priced minimums.
#[derive(Clone, Copy, Debug)]
pub struct Funding {
    pub budget: u64,
    pub e_bond: u64,
    pub v_bond: u64,
}

/// Result of the DA gate over the candidate pool.
pub struct GateOutcome {
    pub effective: Vec<ParticipantId>,   // candidates that got Available (committed)
    pub abstained: Vec<ParticipantId>,   // candidates that abstained
    pub program_bytes: Option<Vec<u8>>,  // DA-reconstructed bytes (None if nobody got them)
    pub store: ProgramStore,             // populated ONLY from reconstructed bytes
}

/// One job's world. Deterministic from its `seed`.
pub struct JobWorld {
    pub program: Vec<u8>,
    pub input: Vec<u8>,
    pub chunking: ChunkingParams,
    pub params: GameParams,
    pub limits: WasmLimits,
    pub candidates: Vec<ParticipantId>,
    pub executor: ParticipantId,
    pub submitter: ParticipantId,
    pub provider: ProviderId,
    pub epoch: u64,
    pub seed: u64,
    pub transport: InMemoryTransport,
    pub clock: ManualClock,
}

impl JobWorld {
    /// 30 candidate verifiers (ids [10..40)), executor id 9, submitter id 0.
    pub fn new(program: Vec<u8>, input: Vec<u8>, chunking: ChunkingParams) -> Self {
        let candidates = (10u8..40).map(|n| ParticipantId([n; 32])).collect();
        Self {
            program, input, chunking,
            params: GameParams::default(),
            limits: WasmLimits::default(),
            candidates,
            executor: ParticipantId([9; 32]),
            submitter: ParticipantId([0; 32]),
            provider: ProviderId([200; 32]),
            epoch: 1,
            seed: 42,
            transport: InMemoryTransport::new(),
            clock: ManualClock::new(),
        }
    }

    pub fn fuel_cap(&self) -> u64 { self.limits.fuel }
    pub fn input_hash(&self) -> [u8; 32] { Sha256::digest(&self.input).into() }
    pub fn job_spec(&self, att: &DaAttestation) -> commputer_pouw::job::JobSpec {
        commputer_pouw::job::JobSpec { program_hash: att.program_id, input_hash: self.input_hash() }
    }

    /// The DA sampling job_id — stable per job, identical for every verifier. (Anchoring it
    /// to JobSpec is founder open-question #2; here it is derived deterministically.)
    pub fn da_job_id(&self, att: &DaAttestation) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(att.program_id);
        h.update(self.input_hash());
        h.finalize().into()
    }

    /// Encode the program into 2N coded chunks and populate the transport (every chunk
    /// advertised by `self.provider`, with its Merkle inclusion path). Returns the attestation.
    pub fn publish(&self) -> DaAttestation {
        let (att, coded) = build_attestation(&self.program, &self.chunking).expect("build_attestation");
        for i in 0..att.n_total {
            let ch = chunk_hash(&att, i);
            let path = chunk_proof(&coded, i);
            self.transport.put(ch, self.provider, coded[i as usize].clone(), path);
        }
        att
    }

    /// Publish `self.program`'s real chunks but return an attestation whose `program_id` is
    /// overwritten — sampling + Merkle pass against the real da_root, but the sha256 re-bind
    /// against the fake program_id fails (tampered-publish scenario).
    pub fn publish_with_fake_program_id(&self, fake: [u8; 32]) -> DaAttestation {
        let mut att = self.publish();
        att.program_id = fake;
        att
    }

    /// Model withholding: remove one coded chunk from the transport.
    pub fn withhold(&self, att: &DaAttestation, index: u16) {
        self.transport.withhold(chunk_hash(att, index));
    }

    /// DA-gate the candidate pool: each candidate runs verify_available; Available ⇒ kept in
    /// the effective pool and its (re-bound) bytes feed the ProgramStore; Abstain ⇒ dropped.
    /// This is the pool-level gate (spec §7).
    pub fn gate_pool(&self, att: &DaAttestation) -> GateOutcome {
        use commputer_da::adapter::resolve_and_populate;
        use commputer_da::facade::DataAvailability;

        let da = DataAvailability {
            transport: &self.transport,
            clock: &self.clock,
            retry_window_ticks: 1_000,
            max_attempts_per_chunk: 8,
        };
        let job_id = self.da_job_id(att);
        let mut store = ProgramStore::new();
        let mut effective = Vec::new();
        let mut abstained = Vec::new();
        let mut program_bytes: Option<Vec<u8>> = None;

        for c in &self.candidates {
            let mut got: Option<Vec<u8>> = None;
            let available = resolve_and_populate(
                &da, att, job_id, self.epoch, c.0, |b| got = Some(b.to_vec()),
            );
            if available {
                let bytes = got.expect("Available ⇒ insert called exactly once");
                if let Some(prev) = &program_bytes {
                    assert_eq!(prev, &bytes, "DA-reconstructed bytes diverge across verifiers");
                }
                program_bytes.get_or_insert_with(|| bytes.clone());
                store.insert(bytes); // keyed by sha256 == att.program_id (idempotent across verifiers)
                effective.push(*c);
            } else {
                abstained.push(*c);
            }
        }
        GateOutcome { effective, abstained, program_bytes, store }
    }

    /// A job can only be verified if at least a committee's worth of verifiers hold the data.
    pub fn can_form_committee(&self, gate: &GateOutcome) -> bool {
        gate.effective.len() >= self.params.k
    }

    pub fn min_funding(&self) -> Result<Funding, EconViolation> {
        use commputer_pouw::economics::{budget_min, executor_bond_min, verifier_bond_min};
        let f = self.fuel_cap();
        let budget = budget_min(f, &self.params)?;
        let e_bond = executor_bond_min(f, budget, &self.params)?;  // budget arg load-bearing
        let v_bond = verifier_bond_min(f, &self.params)?;
        Ok(Funding { budget, e_bond, v_bond })
    }

    /// Pure economic pre-check (no ledger, no DA, no execution) — scenario 3's gate.
    pub fn precheck(&self, att: &DaAttestation, funding: Funding) -> Result<(), EconViolation> {
        use commputer_pouw::economics::validate_economics;
        use commputer_pouw::engine::JobInputs;
        let job = self.build_job(att, funding.budget);
        let noop_claim = |h: &[u8; 32]| *h;
        let noop_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let noop_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let empty: Vec<ParticipantId> = Vec::new();
        let inputs = JobInputs {
            job,
            input: &self.input,
            executor: self.executor,
            executor_bond: funding.e_bond,
            executor_claim: &noop_claim,
            candidates: &empty,
            verifier_bond: funding.v_bond,
            verifier_reveal: &noop_reveal,
            challenge: &noop_challenge,
            challenger_bond: self.params.challenger_bond,
        };
        validate_economics(&inputs, self.fuel_cap(), &self.params)
    }

    fn build_job(&self, att: &DaAttestation, budget: u64) -> commputer_pouw::job::Job {
        commputer_pouw::job::Job {
            id: commputer_pouw::ids::JobId::derive(&att.program_id, &self.input_hash(), &self.submitter, 0),
            submitter: self.submitter,
            spec: self.job_spec(att),
            budget,
        }
    }

    /// Fund the ledger at `funding`, build the WASM oracle from the gate's reconstructed-bytes
    /// store, run the priced game on the effective pool, and report conservation.
    /// Consumes `gate` (its ProgramStore is moved into the oracle).
    pub fn run_lifecycle(
        &self,
        att: &DaAttestation,
        gate: GateOutcome,
        actors: &Actors,
        funding: Funding,
    ) -> (Result<(Verdict, SettlementOutcome), EconViolation>, bool) {
        use commputer_pouw::economics::run_priced_job;
        use commputer_pouw::engine::JobInputs;
        use commputer_pouw::oracle::{ByteEq, Ledger};
        use commputer_pouw::wasm::WasmOracle;
        use rand::SeedableRng;
        use rand::rngs::StdRng;

        let effective = gate.effective;
        let oracle = WasmOracle::new(gate.store, self.limits.clone());

        let mut ledger = Ledger::new();
        ledger.credit(self.submitter, funding.budget);
        ledger.credit(self.executor, funding.e_bond);
        for c in &effective {
            ledger.credit(*c, funding.v_bond);
        }
        let total0 = ledger.total_supply();

        let inputs = JobInputs {
            job: self.build_job(att, funding.budget),
            input: &self.input,
            executor: self.executor,
            executor_bond: funding.e_bond,
            executor_claim: actors.executor_claim,
            candidates: &effective,
            verifier_bond: funding.v_bond,
            verifier_reveal: actors.verifier_reveal,
            challenge: actors.challenge,
            challenger_bond: self.params.challenger_bond,
        };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(self.seed);

        let res = run_priced_job(
            &mut ledger, &self.params, &inputs, self.fuel_cap(),
            &oracle, &ByteEq, &stake, &mut rng,
        );
        let conserved = ledger.total_supply() == total0 && ledger.escrowed() == 0;
        (res, conserved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programs;
    use commputer_da::commit::verify_chunk;
    use commputer_da::facade::chunk_hash;
    use commputer_da::transport::DaTransport;

    #[test]
    fn publish_makes_every_coded_chunk_fetch_and_merkle_verify() {
        let world = JobWorld::new(
            programs::assemble(programs::DOUBLER),
            programs::DEFAULT_INPUT.to_vec(),
            commputer_da::params::ChunkingParams::default(),
        );
        let att = world.publish();
        // program_id is sha256 of the program bytes (the linchpin).
        assert_eq!(att.program_id, programs::program_id(&world.program));
        // Every coded chunk is fetchable from the transport and Merkle-verifies.
        for i in 0..att.n_total {
            let ch = chunk_hash(&att, i);
            let provs = world.transport.find_providers(ch);
            assert!(!provs.is_empty(), "chunk {i} advertised");
            let (bytes, path) = world.transport.fetch_chunk(ch, provs[0]).expect("chunk present");
            assert!(verify_chunk(&att, i, &bytes, &path), "chunk {i} Merkle-verifies");
        }
    }

    #[test]
    fn withhold_removes_a_chunk() {
        let world = JobWorld::new(
            programs::assemble(programs::DOUBLER),
            programs::DEFAULT_INPUT.to_vec(),
            commputer_da::params::ChunkingParams::default(),
        );
        let att = world.publish();
        world.withhold(&att, 0);
        assert!(!world.transport.has_chunk(chunk_hash(&att, 0)), "chunk 0 withheld");
    }

    #[test]
    fn full_availability_gates_every_candidate_and_reconstructs() {
        let world = JobWorld::new(
            crate::programs::assemble(crate::programs::DOUBLER),
            crate::programs::DEFAULT_INPUT.to_vec(),
            commputer_da::params::ChunkingParams::default(),
        );
        let att = world.publish();
        let gate = world.gate_pool(&att);
        assert_eq!(gate.effective.len(), world.candidates.len(), "all available");
        assert!(gate.abstained.is_empty());
        assert_eq!(gate.program_bytes.as_deref(), Some(&world.program[..]), "DA round-trip");
        // The store is keyed by program_id (the linchpin) and resolves the program.
        assert!(gate.store.get(&att.program_id).is_some());
    }

    #[test]
    fn happy_path_confirms_and_conserves_end_to_end() {
        let world = JobWorld::new(
            crate::programs::assemble(crate::programs::DOUBLER),
            crate::programs::DEFAULT_INPUT.to_vec(),
            commputer_da::params::ChunkingParams::default(),
        );
        let att = world.publish();
        let gate = world.gate_pool(&att);
        let funding = world.min_funding().expect("default params priceable");
        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let actors = Actors {
            executor_claim: &honest_claim,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
        };
        let (res, conserved) = world.run_lifecycle(&att, gate, &actors, funding);
        let (verdict, out) = res.expect("at-minimum funding passes");
        assert!(matches!(verdict, Verdict::Confirmed { .. }));
        // budget_min(default) = 3_960 → 85/10/5 = 3_366 / 396 / 198 (396 splits 3 ways, no remainder).
        assert_eq!(out.worker_paid, 3_366);
        assert_eq!(out.verifiers_paid, 396);
        assert_eq!(out.burned, 198);
        assert!(conserved, "total_supply invariant + escrowed()==0");
    }
}
