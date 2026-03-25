use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

/// When a BurstCompute or SubmitJob tx is processed, this handler
/// creates the internal ComputeJob and adds it to the job pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeJobRequest {
    pub submitter_hex: String,
    pub job_spec_hash: [u8; 32],
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
    pub max_duration_secs: u64,
    pub comme_budget: u64,
    pub l2_id: Option<String>,
}

impl ComputeJobRequest {
    /// Generate a deterministic job ID from the request parameters.
    pub fn job_id(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.submitter_hex.as_bytes());
        hasher.update(self.job_spec_hash);
        hasher.update(self.comme_budget.to_le_bytes());
        hasher.update(self.max_duration_secs.to_le_bytes());
        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        id
    }

    /// Validate the request.
    pub fn validate(&self) -> Result<(), String> {
        if self.comme_budget < 1_000_000 {
            return Err("Job budget below minimum (0.01 COMME)".into());
        }
        if self.max_duration_secs == 0 || self.max_duration_secs > 86400 {
            return Err("Invalid job duration (1s - 86400s)".into());
        }
        if self.cpu_cores == 0 {
            return Err("At least 1 CPU core required".into());
        }
        Ok(())
    }

    pub fn needs_gpu(&self) -> bool {
        self.gpu_vram_mb > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req() -> ComputeJobRequest {
        ComputeJobRequest {
            submitter_hex: "abc123".into(),
            job_spec_hash: [1u8; 32],
            cpu_cores: 4,
            gpu_vram_mb: 0,
            ram_mb: 1024,
            storage_mb: 0,
            bandwidth_mbps: 0,
            max_duration_secs: 300,
            comme_budget: 10_000_000,
            l2_id: None,
        }
    }

    #[test]
    fn test_job_id_deterministic() {
        let req = make_req();
        let id1 = req.job_id();
        let id2 = req.job_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_validate_budget_too_low() {
        let mut req = make_req();
        req.comme_budget = 100;
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_ok() {
        let req = make_req();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_zero_duration() {
        let mut req = make_req();
        req.max_duration_secs = 0;
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_zero_cores() {
        let mut req = make_req();
        req.cpu_cores = 0;
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_needs_gpu() {
        let mut req = make_req();
        assert!(!req.needs_gpu());
        req.gpu_vram_mb = 1024;
        assert!(req.needs_gpu());
    }
}
