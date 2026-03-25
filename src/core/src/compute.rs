use serde::{Deserialize, Serialize};
use borsh::{BorshDeserialize, BorshSerialize};
use crate::identity::Address;
use crate::token::Amount;

/// Unique 32-byte job identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct JobId(pub [u8; 32]);

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0[..8]))
    }
}

/// Resource requirements for a compute job.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ResourceRequirements {
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
}

impl ResourceRequirements {
    pub fn cpu_only(cores: u16, ram_mb: u64) -> Self {
        Self { cpu_cores: cores, gpu_vram_mb: 0, ram_mb, storage_mb: 0, bandwidth_mbps: 0 }
    }
    pub fn with_gpu(cores: u16, gpu_vram_mb: u64, ram_mb: u64) -> Self {
        Self { cpu_cores: cores, gpu_vram_mb, ram_mb, storage_mb: 0, bandwidth_mbps: 0 }
    }
    /// Whether this job requires GPU resources.
    pub fn needs_gpu(&self) -> bool { self.gpu_vram_mb > 0 }
}

/// Status of a compute job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum JobStatus {
    Pending,
    Assigned { executor: Address },
    Running { executor: Address, started_at: u64 },
    Completed { executor: Address, result_hash: [u8; 32] },
    Failed { executor: Address, reason: String },
    Disputed { challenger: Address },
}

/// A compute job submitted to the network.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ComputeJob {
    pub job_id: JobId,
    pub submitter: Address,
    pub resources: ResourceRequirements,
    pub job_spec_hash: [u8; 32],
    pub max_duration_secs: u64,
    pub comme_budget: Amount,
    pub status: JobStatus,
    pub submitted_at_height: u64,
    pub l2_id: Option<String>,
    pub assigned_at_height: Option<u64>,
    pub completed_at_height: Option<u64>,
}

/// Job specification format (JSON schema for job submissions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub runtime: JobRuntime,
    pub input_hash: [u8; 32],
    pub expected_output_format: String,
    pub resource_limits: ResourceRequirements,
    pub timeout_secs: u64,
    pub wasm_hash: Option<[u8; 32]>,
}

/// Runtime environment for job execution.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum JobRuntime {
    Wasm,
    Native,
}

/// Constants for compute jobs.
pub const MAX_JOB_DURATION_SECS: u64 = 86400; // 24 hours
pub const MIN_JOB_BUDGET: u64 = 1_000_000; // 0.01 COMME minimum
pub const MAX_CONCURRENT_JOBS_PER_VALIDATOR: usize = 5;
pub const VERIFICATION_SAMPLE_SIZE: usize = 3;
pub const VERIFICATION_REWARD_BPS: u64 = 500; // 5% of job budget
pub const CANCELLATION_FEE_BPS: u64 = 200; // 2% of job budget

/// Flagship L2 identifier — gets 51% capacity reservation.
pub const FLAGSHIP_L2_ID: &str = "commputer-analytics-l2";
/// Percentage of network capacity reserved for flagship L2.
pub const FLAGSHIP_CAPACITY_PERCENT: u64 = 51;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address() -> Address {
        Address([1u8; 32])
    }

    fn test_job_id() -> JobId {
        JobId([0xAB; 32])
    }

    #[test]
    fn test_job_id_display() {
        let id = test_job_id();
        let display = format!("{}", id);
        assert_eq!(display, "abababababababab");
    }

    #[test]
    fn test_resource_requirements_cpu_only() {
        let r = ResourceRequirements::cpu_only(4, 8192);
        assert_eq!(r.cpu_cores, 4);
        assert_eq!(r.ram_mb, 8192);
        assert_eq!(r.gpu_vram_mb, 0);
        assert!(!r.needs_gpu());
    }

    #[test]
    fn test_resource_requirements_with_gpu() {
        let r = ResourceRequirements::with_gpu(8, 16384, 32768);
        assert_eq!(r.cpu_cores, 8);
        assert_eq!(r.gpu_vram_mb, 16384);
        assert_eq!(r.ram_mb, 32768);
        assert!(r.needs_gpu());
    }

    #[test]
    fn test_job_status_transitions() {
        let addr = test_address();
        let status = JobStatus::Pending;
        assert_eq!(status, JobStatus::Pending);

        let assigned = JobStatus::Assigned { executor: addr };
        assert_eq!(assigned, JobStatus::Assigned { executor: addr });

        let running = JobStatus::Running { executor: addr, started_at: 100 };
        assert_eq!(running, JobStatus::Running { executor: addr, started_at: 100 });

        let completed = JobStatus::Completed { executor: addr, result_hash: [0xFF; 32] };
        assert_eq!(completed, JobStatus::Completed { executor: addr, result_hash: [0xFF; 32] });

        let failed = JobStatus::Failed { executor: addr, reason: "timeout".into() };
        assert_eq!(failed, JobStatus::Failed { executor: addr, reason: "timeout".into() });

        let disputed = JobStatus::Disputed { challenger: addr };
        assert_eq!(disputed, JobStatus::Disputed { challenger: addr });
    }

    #[test]
    fn test_compute_job_creation() {
        let job = ComputeJob {
            job_id: test_job_id(),
            submitter: test_address(),
            resources: ResourceRequirements::cpu_only(2, 4096),
            job_spec_hash: [0xCC; 32],
            max_duration_secs: 3600,
            comme_budget: Amount::from_raw(MIN_JOB_BUDGET),
            status: JobStatus::Pending,
            submitted_at_height: 42,
            l2_id: None,
            assigned_at_height: None,
            completed_at_height: None,
        };
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.max_duration_secs, 3600);
        assert!(job.l2_id.is_none());
    }

    #[test]
    fn test_compute_job_with_l2() {
        let job = ComputeJob {
            job_id: test_job_id(),
            submitter: test_address(),
            resources: ResourceRequirements::with_gpu(4, 8192, 16384),
            job_spec_hash: [0xDD; 32],
            max_duration_secs: 7200,
            comme_budget: Amount::from_raw(10_000_000),
            status: JobStatus::Pending,
            submitted_at_height: 100,
            l2_id: Some(FLAGSHIP_L2_ID.to_string()),
            assigned_at_height: None,
            completed_at_height: None,
        };
        assert_eq!(job.l2_id.as_deref(), Some(FLAGSHIP_L2_ID));
        assert!(job.resources.needs_gpu());
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_JOB_DURATION_SECS, 86400);
        assert_eq!(MIN_JOB_BUDGET, 1_000_000);
        assert_eq!(MAX_CONCURRENT_JOBS_PER_VALIDATOR, 5);
        assert_eq!(VERIFICATION_SAMPLE_SIZE, 3);
        assert_eq!(VERIFICATION_REWARD_BPS, 500);
        assert_eq!(CANCELLATION_FEE_BPS, 200);
        assert_eq!(FLAGSHIP_CAPACITY_PERCENT, 51);
    }

    #[test]
    fn test_job_id_equality() {
        let a = JobId([1u8; 32]);
        let b = JobId([1u8; 32]);
        let c = JobId([2u8; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_borsh_roundtrip() {
        let job = ComputeJob {
            job_id: test_job_id(),
            submitter: test_address(),
            resources: ResourceRequirements::cpu_only(2, 4096),
            job_spec_hash: [0xCC; 32],
            max_duration_secs: 3600,
            comme_budget: Amount::from_raw(MIN_JOB_BUDGET),
            status: JobStatus::Pending,
            submitted_at_height: 42,
            l2_id: None,
            assigned_at_height: None,
            completed_at_height: None,
        };
        let bytes = borsh::to_vec(&job).unwrap();
        let decoded: ComputeJob = borsh::from_slice(&bytes).unwrap();
        assert_eq!(decoded.job_id, job.job_id);
        assert_eq!(decoded.submitter, job.submitter);
        assert_eq!(decoded.status, job.status);
    }
}
