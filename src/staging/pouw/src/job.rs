//! Core data model (spec §5). Ports the design's `Data model` section verbatim:
//! the job, the executor's claim, the commit/reveal pair, the challenge, the
//! verdict, and the settlement outcome that every settlement branch returns.

use crate::ids::{JobId, ParticipantId};

/// A deterministic-by-construction job specification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct JobSpec {
    /// Identifies the deterministic program.
    pub program_hash: [u8; 32],
    /// Commitment to the input bytes.
    pub input_hash: [u8; 32],
}

/// A submitted job with its escrowed budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Job {
    pub id: JobId,
    pub submitter: ParticipantId,
    pub spec: JobSpec,
    /// Escrowed at submit.
    pub budget: u64,
}

/// The executor's self-reported result plus the bond it posts behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExecutorClaim {
    pub executor: ParticipantId,
    pub result_hash: [u8; 32],
    pub bond: u64,
}

/// A verifier's hiding commitment: `H(result_hash ‖ salt ‖ verifier)`; bond posted on commit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Commitment {
    pub verifier: ParticipantId,
    pub commit: [u8; 32],
    pub bond: u64,
}

/// The opening of a `Commitment`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reveal {
    pub verifier: ParticipantId,
    pub result_hash: [u8; 32],
    pub salt: [u8; 32],
}

/// A staked dispute of an optimistically-accepted (unsampled) result.
/// A `NoQuorum` committee auto-escalates with no challenger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Challenge {
    pub challenger: ParticipantId,
    pub bond: u64,
}

/// The committee/panel verdict over the executor's claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Committee agrees WITH the executor.
    Confirmed { result_hash: [u8; 32] },
    /// Committee agrees on a DIFFERENT value.
    Disputed { correct_hash: [u8; 32] },
    /// No value reached quorum -> escalate.
    NoQuorum,
}

/// The result of a settlement branch. All amounts are raw `u64` units; `slashed`
/// is a log of who lost what (the amounts land in `burned`/`challenger_paid`/`panel_paid`).
#[derive(Default, Clone, Debug, PartialEq)]
pub struct SettlementOutcome {
    pub worker_paid: u64,
    pub verifiers_paid: u64,
    pub burned: u64,
    pub submitter_refunded: u64,
    /// Successful-challenger reward (Disputed-via-escalation).
    pub challenger_paid: u64,
    /// Escalation re-executor compensation.
    pub panel_paid: u64,
    /// All honest participants' bonds returned intact.
    pub bonds_returned: u64,
    /// Executor / challenger / dishonest verifiers (a log).
    pub slashed: Vec<(ParticipantId, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settlement_outcome_starts_empty() {
        let s = SettlementOutcome::default();
        assert_eq!(s.worker_paid, 0);
        assert_eq!(s.burned, 0);
        assert!(s.slashed.is_empty());
    }
    #[test]
    fn verdict_equality() {
        assert_eq!(Verdict::Confirmed { result_hash: [1; 32] }, Verdict::Confirmed { result_hash: [1; 32] });
        assert_ne!(Verdict::Confirmed { result_hash: [1; 32] }, Verdict::NoQuorum);
    }
}
