use serde::{Deserialize, Serialize};

/// Client library for the flagship analytics L2 to interact with Commputer L1.
#[derive(Debug, Clone)]
pub struct L2Client {
    pub rpc_url: String,
    pub l2_id: String,
    pub api_key: Option<String>,
}

/// Job submission request from L2 to L1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2JobRequest {
    pub job_spec_hash: String,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub comme_budget: u64,
    pub max_duration_secs: u64,
    pub priority: JobPriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobPriority {
    Normal,
    High,
    Critical,
}

/// Response from L1 after job submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2JobResponse {
    pub job_id: String,
    pub accepted: bool,
    pub estimated_start_secs: u64,
    pub error: Option<String>,
}

/// Job result returned to L2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2JobResult {
    pub job_id: String,
    pub status: String,
    pub result_hash: Option<String>,
    pub output_url: Option<String>,
    pub execution_time_ms: u64,
}

impl L2Client {
    pub fn new(rpc_url: &str, l2_id: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            l2_id: l2_id.to_string(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, key: &str) -> Self {
        self.api_key = Some(key.to_string());
        self
    }

    /// Build the job submission URL.
    pub fn submit_url(&self) -> String {
        format!("{}/compute/jobs", self.rpc_url)
    }

    /// Build the job status URL.
    pub fn status_url(&self, job_id: &str) -> String {
        format!("{}/compute/jobs/{}", self.rpc_url, job_id)
    }

    /// Build the result URL.
    pub fn result_url(&self, job_id: &str) -> String {
        format!("{}/compute/jobs/{}/result", self.rpc_url, job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_client_new() {
        let client = L2Client::new("http://localhost:9944", "analytics-l2");
        assert_eq!(client.rpc_url, "http://localhost:9944");
        assert_eq!(client.l2_id, "analytics-l2");
        assert!(client.api_key.is_none());
    }

    #[test]
    fn test_l2_client_with_api_key() {
        let client = L2Client::new("http://localhost:9944", "analytics-l2")
            .with_api_key("secret-key");
        assert_eq!(client.api_key, Some("secret-key".to_string()));
    }

    #[test]
    fn test_url_building() {
        let client = L2Client::new("http://node.commputer.io", "l2-test");
        assert_eq!(client.submit_url(), "http://node.commputer.io/compute/jobs");
        assert_eq!(
            client.status_url("abc123"),
            "http://node.commputer.io/compute/jobs/abc123"
        );
        assert_eq!(
            client.result_url("abc123"),
            "http://node.commputer.io/compute/jobs/abc123/result"
        );
    }

    #[test]
    fn test_job_request_serialization() {
        let req = L2JobRequest {
            job_spec_hash: "abc123".to_string(),
            cpu_cores: 4,
            gpu_vram_mb: 8192,
            ram_mb: 16384,
            comme_budget: 500_000_000,
            max_duration_secs: 3600,
            priority: JobPriority::High,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("abc123"));
        assert!(json.contains("High"));
    }
}
