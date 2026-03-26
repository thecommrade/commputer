#![allow(dead_code)]
use serde::{Deserialize, Serialize};

/// Status of a compute job in the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Pending,
    Assigned,
    Running,
    Completed,
    Failed,
    Disputed,
}

/// Summary info for a job, returned in list responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub job_id_hex: String,
    pub submitter_hex: String,
    pub status: JobStatus,
    pub comme_budget: u64,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub submitted_height: u64,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Request to list jobs with optional filters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobListRequest {
    pub status_filter: Option<JobStatus>,
    pub submitter_filter: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Response for job listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobListResponse {
    pub jobs: Vec<JobInfo>,
    pub total_count: usize,
    pub pending_count: usize,
    pub active_count: usize,
}

/// Request to submit a new job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSubmitRequest {
    pub job_spec_hash_hex: String,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
    pub max_duration_secs: u64,
    pub comme_budget: u64,
    pub l2_id: Option<String>,
}

/// Response after submitting a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSubmitResponse {
    pub job_id_hex: String,
    pub accepted: bool,
    pub error: Option<String>,
}

/// Response for querying a single job's status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatusResponse {
    pub job_id_hex: String,
    pub status: JobStatus,
    pub result_hash: Option<String>,
    pub executor_hex: Option<String>,
}

/// Dashboard view for a validator's assigned jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorJobDashboard {
    pub validator_hex: String,
    pub assigned_jobs: Vec<JobInfo>,
    pub completed_count: u64,
    pub total_earned: u64,
}

// ---------------------------------------------------------------------------
// Handler stubs (will be integrated into the RPC router later)
// ---------------------------------------------------------------------------

/// Handle a job list request. Stub implementation.
pub fn handle_job_list(request: &JobListRequest, _jobs: &[JobInfo]) -> JobListResponse {
    let limit = request.limit.unwrap_or(50);
    let offset = request.offset.unwrap_or(0);

    // In a real implementation, we'd query the job pool.
    // For now, return empty results.
    let _ = (limit, offset);
    JobListResponse {
        jobs: Vec::new(),
        total_count: 0,
        pending_count: 0,
        active_count: 0,
    }
}

/// Handle a job submit request. Stub implementation.
pub fn handle_job_submit(request: &JobSubmitRequest) -> JobSubmitResponse {
    // Basic validation
    if request.cpu_cores == 0 {
        return JobSubmitResponse {
            job_id_hex: String::new(),
            accepted: false,
            error: Some("At least 1 CPU core required".into()),
        };
    }
    if request.comme_budget < 1_000_000 {
        return JobSubmitResponse {
            job_id_hex: String::new(),
            accepted: false,
            error: Some("Budget below minimum (0.01 COMME)".into()),
        };
    }

    // Generate a stub job ID from the spec hash
    let job_id_hex = format!("job_{}", &request.job_spec_hash_hex[..8.min(request.job_spec_hash_hex.len())]);

    JobSubmitResponse {
        job_id_hex,
        accepted: true,
        error: None,
    }
}

/// Handle a job status query. Stub implementation.
pub fn handle_job_status(job_id_hex: &str) -> JobStatusResponse {
    // In a real implementation, we'd look up the job.
    JobStatusResponse {
        job_id_hex: job_id_hex.to_string(),
        status: JobStatus::Pending,
        result_hash: None,
        executor_hex: None,
    }
}

/// Get validator dashboard. Stub implementation.
pub fn handle_validator_dashboard(validator_hex: &str) -> ValidatorJobDashboard {
    ValidatorJobDashboard {
        validator_hex: validator_hex.to_string(),
        assigned_jobs: Vec::new(),
        completed_count: 0,
        total_earned: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_submit_ok() {
        let req = JobSubmitRequest {
            job_spec_hash_hex: "abcdef1234567890".into(),
            cpu_cores: 4,
            gpu_vram_mb: 0,
            ram_mb: 1024,
            storage_mb: 0,
            bandwidth_mbps: 0,
            max_duration_secs: 300,
            comme_budget: 10_000_000,
            l2_id: None,
        };
        let resp = handle_job_submit(&req);
        assert!(resp.accepted);
        assert!(resp.error.is_none());
        assert!(!resp.job_id_hex.is_empty());
    }

    #[test]
    fn test_job_submit_zero_cores() {
        let req = JobSubmitRequest {
            job_spec_hash_hex: "abc".into(),
            cpu_cores: 0,
            gpu_vram_mb: 0,
            ram_mb: 1024,
            storage_mb: 0,
            bandwidth_mbps: 0,
            max_duration_secs: 300,
            comme_budget: 10_000_000,
            l2_id: None,
        };
        let resp = handle_job_submit(&req);
        assert!(!resp.accepted);
    }

    #[test]
    fn test_job_submit_low_budget() {
        let req = JobSubmitRequest {
            job_spec_hash_hex: "abc".into(),
            cpu_cores: 1,
            gpu_vram_mb: 0,
            ram_mb: 1024,
            storage_mb: 0,
            bandwidth_mbps: 0,
            max_duration_secs: 300,
            comme_budget: 100,
            l2_id: None,
        };
        let resp = handle_job_submit(&req);
        assert!(!resp.accepted);
    }

    #[test]
    fn test_job_list_empty() {
        let req = JobListRequest {
            status_filter: None,
            submitter_filter: None,
            limit: Some(10),
            offset: None,
        };
        let resp = handle_job_list(&req, &[]);
        assert_eq!(resp.total_count, 0);
        assert!(resp.jobs.is_empty());
    }

    #[test]
    fn test_job_status_stub() {
        let resp = handle_job_status("abc123");
        assert_eq!(resp.job_id_hex, "abc123");
        assert_eq!(resp.status, JobStatus::Pending);
    }

    #[test]
    fn test_validator_dashboard_stub() {
        let dash = handle_validator_dashboard("validator_abc");
        assert_eq!(dash.validator_hex, "validator_abc");
        assert_eq!(dash.completed_count, 0);
        assert_eq!(dash.total_earned, 0);
    }

    #[test]
    fn test_job_status_serde_roundtrip() {
        let status = JobStatus::Completed;
        let json = serde_json::to_string(&status).unwrap();
        let back: JobStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}
