use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A stored result for a completed compute job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: [u8; 32],
    pub result_hash: [u8; 32],
    pub output_data: Option<Vec<u8>>,
    pub stored_at_height: u64,
    pub executor_hex: String,
}

/// In-memory store for job results.
#[derive(Debug, Default)]
pub struct JobResultStore {
    results: HashMap<[u8; 32], JobResult>,
    /// Insertion-ordered list of job IDs for listing.
    order: Vec<[u8; 32]>,
}

impl JobResultStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a job result. Returns false if a result for this job already exists.
    pub fn store_result(&mut self, result: JobResult) -> bool {
        let job_id = result.job_id;
        if self.results.contains_key(&job_id) {
            return false;
        }
        self.order.push(job_id);
        self.results.insert(job_id, result);
        true
    }

    /// Retrieve a job result by job ID.
    pub fn get_result(&self, job_id: &[u8; 32]) -> Option<&JobResult> {
        self.results.get(job_id)
    }

    /// List the last N job results in insertion order.
    pub fn list_results(&self, last_n: usize) -> Vec<&JobResult> {
        let start = if self.order.len() > last_n {
            self.order.len() - last_n
        } else {
            0
        };
        self.order[start..]
            .iter()
            .filter_map(|id| self.results.get(id))
            .collect()
    }

    /// Total number of stored results.
    pub fn count(&self) -> usize {
        self.results.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(id_byte: u8, height: u64) -> JobResult {
        JobResult {
            job_id: [id_byte; 32],
            result_hash: [id_byte.wrapping_add(1); 32],
            output_data: Some(vec![id_byte; 8]),
            stored_at_height: height,
            executor_hex: format!("validator_{}", id_byte),
        }
    }

    #[test]
    fn test_store_and_get() {
        let mut store = JobResultStore::new();
        let r = make_result(1, 100);
        assert!(store.store_result(r.clone()));
        let got = store.get_result(&[1u8; 32]).unwrap();
        assert_eq!(got.stored_at_height, 100);
        assert_eq!(got.executor_hex, "validator_1");
    }

    #[test]
    fn test_duplicate_rejected() {
        let mut store = JobResultStore::new();
        let r = make_result(1, 100);
        assert!(store.store_result(r.clone()));
        assert!(!store.store_result(r));
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn test_get_missing() {
        let store = JobResultStore::new();
        assert!(store.get_result(&[99u8; 32]).is_none());
    }

    #[test]
    fn test_list_results_last_n() {
        let mut store = JobResultStore::new();
        for i in 0..10u8 {
            store.store_result(make_result(i, i as u64));
        }
        let last3 = store.list_results(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].job_id, [7u8; 32]);
        assert_eq!(last3[1].job_id, [8u8; 32]);
        assert_eq!(last3[2].job_id, [9u8; 32]);
    }

    #[test]
    fn test_list_results_more_than_available() {
        let mut store = JobResultStore::new();
        store.store_result(make_result(1, 10));
        let all = store.list_results(100);
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_count() {
        let mut store = JobResultStore::new();
        assert_eq!(store.count(), 0);
        store.store_result(make_result(1, 1));
        store.store_result(make_result(2, 2));
        assert_eq!(store.count(), 2);
    }
}
