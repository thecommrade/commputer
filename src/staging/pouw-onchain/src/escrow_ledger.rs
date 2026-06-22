//! Module 3 — the reference per-job escrow ledger (G1-decided; reference for P1).
//!
//! A `ChainHooks` impl with **per-job escrow pots** keyed by `job_id` — what the real
//! `storage/state.rs` must do under adopt-escrow (blueprint G1). The staging `Ledger`
//! (pouw/src/oracle.rs) holds a SINGLE escrow counter; this adds job-keyed accounting so
//! concurrent jobs do not co-mingle escrow. Bind to one job via [`EscrowLedger::for_job`]
//! before driving it through `run_priced_job`.
//!
//! ## Conservation invariant
//! `total_supply = Σ balances + Σ all job escrow pots + burned` is INVARIANT across every
//! op (`credit` is the ONLY mint, used at funding time; the `ChainHooks` ops only move
//! value between those three buckets). After a complete settlement, the job's pot is 0.
//!
//! ## Underflow policy (matches staging `Ledger` semantics)
//! `escrow`/`pay`/`burn` use `checked_sub` and **panic on underflow**. Every escrow must
//! be pre-funded by admission; paying or burning more than a pot holds is a consensus bug,
//! not a recoverable condition — so it panics rather than silently saturating.
//!
//! WIRE-IN (founder patch-spec): `storage/state.rs`'s `ChainState` gains a
//! `escrow_by_job: HashMap<JobId, u64>` and routes the settlement money-path through these
//! same five ops, calling `for_job` at the start of each job's settlement.

use commputer_pouw::ids::ParticipantId;
use commputer_pouw::oracle::ChainHooks;
use std::collections::HashMap;

/// Reference escrow ledger. `total_supply = Σ balances + Σ all job escrow pots + burned`,
/// INVARIANT across every op. Bound to one `job_id` at a time via [`Self::for_job`].
pub struct EscrowLedger {
    balances: HashMap<ParticipantId, u64>,
    escrow_by_job: HashMap<[u8; 32], u64>,
    burned: u64,
    /// The job whose pot `escrow`/`pay`/`burn` currently target. Set by `for_job`.
    active_job: Option<[u8; 32]>,
}

impl Default for EscrowLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl EscrowLedger {
    pub fn new() -> Self {
        Self {
            balances: HashMap::new(),
            escrow_by_job: HashMap::new(),
            burned: 0,
            active_job: None,
        }
    }

    /// Mint `amount` into `who`'s balance (funding time only — the sole mint surface).
    pub fn credit(&mut self, who: ParticipantId, amount: u64) {
        *self.balances.entry(who).or_insert(0) += amount;
    }

    pub fn balance_of(&self, who: &ParticipantId) -> u64 {
        *self.balances.get(who).unwrap_or(&0)
    }

    /// Value held in `job_id`'s escrow pot (still in supply, not yet paid out or burned).
    pub fn escrowed_for(&self, job_id: &[u8; 32]) -> u64 {
        *self.escrow_by_job.get(job_id).unwrap_or(&0)
    }

    /// Total value held across ALL job escrow pots.
    pub fn total_escrowed(&self) -> u64 {
        self.escrow_by_job.values().sum()
    }

    pub fn total_supply(&self) -> u64 {
        self.balances.values().sum::<u64>() + self.total_escrowed() + self.burned
    }

    /// Bind subsequent `ChainHooks` escrow/pay/burn ops to this job's pot.
    pub fn for_job(&mut self, job_id: [u8; 32]) {
        self.active_job = Some(job_id);
    }

    /// The active job's pot. Panics if `for_job` was never called — every escrow op on a
    /// per-job ledger MUST be scoped to a job.
    fn active(&self) -> [u8; 32] {
        self.active_job
            .expect("for_job must be called before any escrow/pay/burn op")
    }
}

impl ChainHooks for EscrowLedger {
    /// balance -> active job's pot. Panics if `who`'s balance is below `amount` (every
    /// escrow must be pre-funded by admission).
    fn escrow(&mut self, who: ParticipantId, amount: u64) {
        let job = self.active();
        let b = self.balances.entry(who).or_insert(0);
        *b = b.checked_sub(amount).expect("escrow exceeds balance");
        *self.escrow_by_job.entry(job).or_insert(0) += amount;
    }

    /// active job's pot -> balance. Panics if the pot holds less than `amount`.
    fn pay(&mut self, to: ParticipantId, amount: u64) {
        let job = self.active();
        let pot = self.escrow_by_job.entry(job).or_insert(0);
        *pot = pot.checked_sub(amount).expect("pay exceeds job escrow");
        *self.balances.entry(to).or_insert(0) += amount;
    }

    /// active job's pot -> burned. Panics if the pot holds less than `amount`.
    fn burn(&mut self, amount: u64) {
        let job = self.active();
        let pot = self.escrow_by_job.entry(job).or_insert(0);
        *pot = pot.checked_sub(amount).expect("burn exceeds job escrow");
        self.burned += amount;
    }

    /// un-escrowed balance -> burned. NOT on the settlement money-path (settlement bonds
    /// are already escrowed, so they move via pay/burn); this matches the staging `Ledger`
    /// surface and applies solely to un-escrowed stake. Panics if balance < amount.
    fn slash(&mut self, who: ParticipantId, amount: u64) {
        let b = self.balances.entry(who).or_insert(0);
        *b = b.checked_sub(amount).expect("slash exceeds balance");
        self.burned += amount;
    }

    /// = balance (G4: bonded/token stake is the balance in this reference impl).
    fn stake_of(&self, who: &ParticipantId) -> u64 {
        self.balance_of(who)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_pouw::economics::{budget_min, executor_bond_min, run_priced_job, verifier_bond_min};
    use commputer_pouw::engine::JobInputs;
    use commputer_pouw::ids::JobId;
    use commputer_pouw::job::{Job, JobSpec, Verdict};
    use commputer_pouw::oracle::{ByteEq, IteratedHashVm};
    use commputer_pouw::params::GameParams;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// A fully-funded-at-minimum honest world for the default params, mirroring
    /// `pouw/src/economics.rs::priced_world()` but funding an `EscrowLedger`.
    /// Returns the ledger, the built job, the actors' ids, and the funding levels.
    #[allow(clippy::type_complexity)]
    fn priced_world() -> (
        GameParams,
        EscrowLedger,
        Job,
        ParticipantId,
        Vec<ParticipantId>,
        u64,
        u64,
        u64,
    ) {
        let p = GameParams::default();
        let f = 100_000_000u64;
        let budget = budget_min(f, &p).unwrap(); // 3_960
        let e_bond = executor_bond_min(f, budget, &p).unwrap(); // 3_960
        let v_bond = verifier_bond_min(f, &p).unwrap(); // 1_650
        let submitter = pid(0);
        let executor = pid(9);
        let candidates: Vec<ParticipantId> = (10u8..30).map(pid).collect();

        let mut l = EscrowLedger::new();
        l.credit(submitter, budget);
        l.credit(executor, e_bond);
        for c in &candidates {
            l.credit(*c, v_bond);
        }

        let spec = JobSpec { program_hash: [7; 32], input_hash: [9; 32] };
        let job = Job {
            id: JobId::derive(&[7; 32], &[9; 32], &submitter, 0),
            submitter,
            spec,
            budget,
        };
        (p, l, job, executor, candidates, budget, e_bond, v_bond)
    }

    /// Drive a full honest job through `run_priced_job` against an `EscrowLedger`:
    /// settles `Confirmed` 85/10/5, conserves `total_supply`, and leaves the job's pot 0.
    #[test]
    fn happy_path_confirms_settles_85_10_5_and_conserves() {
        let (p, mut l, job, executor, candidates, _budget, e_bond, v_bond) = priced_world();
        let job_id = job.id.0; // JobId is a newtype over [u8; 32]
        let total0 = l.total_supply();

        l.for_job(job_id);

        let honest_claim = |h: &[u8; 32]| *h;
        let honest_reveal = |_: &ParticipantId, h: &[u8; 32], _: &[u8; 32]| *h;
        let no_challenge = |_: &[u8; 32], _: &[u8; 32]| None;
        let inputs = JobInputs {
            job,
            input: b"in",
            executor,
            executor_bond: e_bond,
            executor_claim: &honest_claim,
            candidates: &candidates,
            verifier_bond: v_bond,
            verifier_reveal: &honest_reveal,
            challenge: &no_challenge,
            challenger_bond: p.challenger_bond,
        };
        let vm = IteratedHashVm { rounds: 10 };
        let stake = |_: &ParticipantId| 1u64;
        let mut rng = StdRng::seed_from_u64(42);

        let (verdict, out) =
            run_priced_job(&mut l, &p, &inputs, 100_000_000, &vm, &ByteEq, &stake, &mut rng)
                .expect("at-minimum funding passes");

        assert!(matches!(verdict, Verdict::Confirmed { .. }), "honest run confirms");
        // budget_min(default) = 3_960 → 85/10/5 = 3_366 / 396 / 198 (396 splits 3 ways exactly).
        assert_eq!(out.worker_paid, 3_366, "worker gets 85%");
        assert_eq!(out.verifiers_paid, 396, "verifiers split 10%");
        assert_eq!(out.burned, 198, "5% burned");

        assert_eq!(l.total_supply(), total0, "total_supply conserved end-to-end");
        assert_eq!(l.escrowed_for(&job_id), 0, "job pot fully drained after settle");
        assert_eq!(l.total_escrowed(), 0, "no escrow stranded anywhere");
    }

    /// Two interleaved jobs keep SEPARATE pots: escrowing/settling one never touches the
    /// other's escrow. (Bind A, escrow into A; bind B, escrow into B; pay/burn out of A;
    /// B's pot is unchanged throughout.)
    #[test]
    fn two_interleaved_jobs_keep_separate_pots() {
        let job_a = [0xAAu8; 32];
        let job_b = [0xBBu8; 32];
        let alice = pid(1);
        let bob = pid(2);
        let sink = pid(3);

        let mut l = EscrowLedger::new();
        l.credit(alice, 100);
        l.credit(bob, 100);
        let total0 = l.total_supply();

        // Fund A's pot from Alice.
        l.for_job(job_a);
        l.escrow(alice, 60);
        assert_eq!(l.escrowed_for(&job_a), 60);
        assert_eq!(l.escrowed_for(&job_b), 0);

        // Fund B's pot from Bob — A is untouched.
        l.for_job(job_b);
        l.escrow(bob, 40);
        assert_eq!(l.escrowed_for(&job_a), 60, "B's escrow did not touch A");
        assert_eq!(l.escrowed_for(&job_b), 40);

        // Settle A (pay 50 to sink, burn 10) — B's pot is unchanged.
        l.for_job(job_a);
        l.pay(sink, 50);
        l.burn(10);
        assert_eq!(l.escrowed_for(&job_a), 0, "A fully settled");
        assert_eq!(l.escrowed_for(&job_b), 40, "settling A left B's pot intact");

        assert_eq!(l.balance_of(&sink), 50);
        assert_eq!(l.total_supply(), total0, "conserved across both jobs");
        assert_eq!(l.total_escrowed(), 40, "only B's pot remains");
    }

    /// Each of credit/escrow/pay/burn/slash preserves `total_supply` (no mint after
    /// funding) — the conservation backbone, op by op.
    #[test]
    fn every_op_preserves_total_supply() {
        let a = pid(1);
        let b = pid(2);
        let job = [7u8; 32];
        let mut l = EscrowLedger::new();

        // credit IS the mint — record supply after funding, then assert invariance.
        l.credit(a, 100);
        l.credit(b, 50);
        let total0 = l.total_supply();
        assert_eq!(total0, 150);

        l.for_job(job);
        l.escrow(a, 40);
        assert_eq!(l.total_supply(), total0, "escrow: balance -> pot");
        l.pay(b, 25);
        assert_eq!(l.total_supply(), total0, "pay: pot -> balance");
        l.burn(10);
        assert_eq!(l.total_supply(), total0, "burn: pot -> burned");
        l.slash(b, 5);
        assert_eq!(l.total_supply(), total0, "slash: un-escrowed balance -> burned");
    }

    /// `pay` beyond the active pot panics (underflow policy — a consensus bug, not
    /// silently saturated).
    #[test]
    #[should_panic(expected = "pay exceeds job escrow")]
    fn pay_beyond_pot_panics() {
        let mut l = EscrowLedger::new();
        l.credit(pid(1), 100);
        l.for_job([7u8; 32]);
        l.escrow(pid(1), 30);
        l.pay(pid(2), 31); // pot holds 30
    }
}
