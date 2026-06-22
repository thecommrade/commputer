//! Module 2 — the on-chain ↔ staging job-spec mapping (decision-independent).
//!
//! Bridges the post-G3 on-chain `SubmitJob` job format to the staging
//! `JobSpec`/`Job`. The `SubmitJobFields` mirror struct stands in for the real
//! `transaction.rs` change (founder-applied): G3 makes `program_hash = sha256(wasm)`,
//! the `input_hash` binary, and anchors `da_root`.
//!
//! WIRE-IN (founder patch-spec): `event_loop.rs:2196` destructures the on-chain
//! `SubmitJob` into `SubmitJobFields`, calls `onchain_to_staging` to get the staging
//! `(JobSpec, Job)`, and feeds `da_root`/`da_job_id` to the DA sampler at P2/P4.

use commputer_pouw::ids::{JobId, ParticipantId};
use commputer_pouw::job::{Job, JobSpec};
use sha2::{Digest, Sha256};

/// The post-G3 on-chain `SubmitJob` fields (mirror struct — the real `transaction.rs`
/// change is founder-applied). G3: `program_hash = sha256(wasm)`, binary `input_hash`,
/// `da_root` anchored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmitJobFields {
    pub program_hash: [u8; 32],
    pub input_hash: [u8; 32],
    pub da_root: [u8; 32],
    pub comme_budget: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    ZeroBudget,
}

/// Map the on-chain job to the staging `(JobSpec, Job)`. The `da_root` travels alongside
/// (it is NOT in the staging `JobSpec` — it feeds the DA sampler at P2/P4).
pub fn onchain_to_staging(
    f: &SubmitJobFields,
    submitter: ParticipantId,
    nonce: u64,
) -> Result<(JobSpec, Job), MapError> {
    if f.comme_budget == 0 {
        return Err(MapError::ZeroBudget);
    }
    let spec = JobSpec {
        program_hash: f.program_hash,
        input_hash: f.input_hash,
    };
    let id = JobId::derive(&f.program_hash, &f.input_hash, &submitter, nonce);
    let job = Job {
        id,
        submitter,
        spec,
        budget: f.comme_budget,
    };
    Ok((spec, job))
}

/// The DA sampling job_id — stable per job, identical for every verifier
/// (= `sha256(program_hash || input_hash)`; matches pouw-e2e world.rs:84).
pub fn da_job_id(program_hash: &[u8; 32], input_hash: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(program_hash);
    h.update(input_hash);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u8) -> ParticipantId {
        ParticipantId([n; 32])
    }

    /// `onchain_to_staging` sets every field correctly and derives the canonical `JobId`.
    #[test]
    fn maps_all_fields_and_derives_job_id() {
        let f = SubmitJobFields {
            program_hash: [7u8; 32],
            input_hash: [9u8; 32],
            da_root: [11u8; 32],
            comme_budget: 3_960,
        };
        let submitter = pid(0);
        let nonce = 0u64;

        let (spec, job) = onchain_to_staging(&f, submitter, nonce).expect("nonzero budget maps");

        assert_eq!(spec.program_hash, f.program_hash);
        assert_eq!(spec.input_hash, f.input_hash);
        assert_eq!(job.budget, f.comme_budget);
        assert_eq!(job.submitter, submitter);
        assert_eq!(job.spec, spec, "job.spec is the returned spec");
        assert_eq!(
            job.id,
            JobId::derive(&f.program_hash, &f.input_hash, &submitter, nonce),
            "job.id is the canonical derivation"
        );
    }

    /// Zero budget → `Err(MapError::ZeroBudget)` (an unfunded job cannot be escrowed).
    #[test]
    fn zero_budget_is_rejected() {
        let f = SubmitJobFields {
            program_hash: [1u8; 32],
            input_hash: [2u8; 32],
            da_root: [3u8; 32],
            comme_budget: 0,
        };
        assert_eq!(onchain_to_staging(&f, pid(0), 0), Err(MapError::ZeroBudget));
    }

    /// `da_job_id` is `sha256(program_hash || input_hash)`, deterministic, and
    /// order-sensitive.
    #[test]
    fn da_job_id_is_sha256_concat_deterministic_and_ordered() {
        let ph = [7u8; 32];
        let ih = [9u8; 32];

        let mut expected = Sha256::new();
        expected.update(ph);
        expected.update(ih);
        let expected: [u8; 32] = expected.finalize().into();

        assert_eq!(da_job_id(&ph, &ih), expected, "= sha256(program_hash || input_hash)");
        assert_eq!(da_job_id(&ph, &ih), da_job_id(&ph, &ih), "deterministic");
        assert_ne!(
            da_job_id(&ph, &ih),
            da_job_id(&ih, &ph),
            "order-sensitive (program_hash and input_hash are not interchangeable)"
        );
    }
}
