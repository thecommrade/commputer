#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

/// Resource requirements for a single job in a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResources {
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
}

/// A single job item within a batch submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSubmitItem {
    pub job_spec_hash: String,
    pub resources: JobResources,
    pub comme_budget: u64,
}

/// A batch of jobs submitted together for efficiency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSubmission {
    pub jobs: Vec<JobSubmitItem>,
    pub optimize_locality: bool,
}

/// Result of processing a batch submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub job_ids: Vec<String>,
    pub total_budget: u64,
    pub accepted_count: usize,
}

/// Process a batch submission and return results.
pub fn process_batch(batch: &BatchSubmission) -> BatchResult {
    let mut job_ids = Vec::new();
    let mut total_budget: u64 = 0;

    for (i, item) in batch.jobs.iter().enumerate() {
        // Generate a deterministic job ID from spec hash + index
        let mut hasher = Sha256::new();
        hasher.update(item.job_spec_hash.as_bytes());
        hasher.update(i.to_le_bytes());
        let hash = hasher.finalize();
        let job_id = hex::encode(&hash[..16]);
        job_ids.push(job_id);
        total_budget = total_budget.saturating_add(item.comme_budget);
    }

    let accepted_count = job_ids.len();
    BatchResult {
        job_ids,
        total_budget,
        accepted_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(hash: &str, budget: u64) -> JobSubmitItem {
        JobSubmitItem {
            job_spec_hash: hash.to_string(),
            resources: JobResources {
                cpu_cores: 2,
                gpu_vram_mb: 0,
                ram_mb: 4096,
            },
            comme_budget: budget,
        }
    }

    #[test]
    fn test_empty_batch() {
        let batch = BatchSubmission {
            jobs: vec![],
            optimize_locality: false,
        };
        let result = process_batch(&batch);
        assert_eq!(result.accepted_count, 0);
        assert_eq!(result.total_budget, 0);
    }

    #[test]
    fn test_single_job_batch() {
        let batch = BatchSubmission {
            jobs: vec![make_item("spec-abc", 100_000_000)],
            optimize_locality: false,
        };
        let result = process_batch(&batch);
        assert_eq!(result.accepted_count, 1);
        assert_eq!(result.total_budget, 100_000_000);
        assert_eq!(result.job_ids.len(), 1);
    }

    #[test]
    fn test_multi_job_batch() {
        let batch = BatchSubmission {
            jobs: vec![
                make_item("spec-1", 50_000_000),
                make_item("spec-2", 75_000_000),
                make_item("spec-3", 25_000_000),
            ],
            optimize_locality: true,
        };
        let result = process_batch(&batch);
        assert_eq!(result.accepted_count, 3);
        assert_eq!(result.total_budget, 150_000_000);
        // Each job should have a unique ID
        assert_ne!(result.job_ids[0], result.job_ids[1]);
        assert_ne!(result.job_ids[1], result.job_ids[2]);
    }

    #[test]
    fn test_deterministic_ids() {
        let batch = BatchSubmission {
            jobs: vec![make_item("spec-x", 100)],
            optimize_locality: false,
        };
        let r1 = process_batch(&batch);
        let r2 = process_batch(&batch);
        assert_eq!(r1.job_ids, r2.job_ids);
    }
}
