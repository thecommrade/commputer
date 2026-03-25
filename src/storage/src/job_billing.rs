use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A billing record for a completed compute job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobBillingRecord {
    pub job_id: [u8; 32],
    pub submitter_hex: String,
    pub comme_spent: u64,
    pub cpu_cores_used: u16,
    pub gpu_vram_used: u64,
    pub ram_used: u64,
    pub duration_secs: u64,
    pub result_hash: [u8; 32],
    pub billed_at_height: u64,
}

/// In-memory store for billing records.
#[derive(Debug, Default)]
pub struct BillingStore {
    records: HashMap<[u8; 32], JobBillingRecord>,
    total_billed: u64,
}

impl BillingStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a billing entry. Returns false if a record for this job already exists.
    pub fn record_billing(&mut self, record: JobBillingRecord) -> bool {
        let job_id = record.job_id;
        if self.records.contains_key(&job_id) {
            return false;
        }
        self.total_billed = self.total_billed.saturating_add(record.comme_spent);
        self.records.insert(job_id, record);
        true
    }

    /// Get a billing record by job ID.
    pub fn get_record(&self, job_id: &[u8; 32]) -> Option<&JobBillingRecord> {
        self.records.get(job_id)
    }

    /// Total COMME billed across all jobs.
    pub fn total_billed(&self) -> u64 {
        self.total_billed
    }

    /// All billing records for a given submitter address.
    pub fn records_for_address(&self, submitter_hex: &str) -> Vec<&JobBillingRecord> {
        self.records
            .values()
            .filter(|r| r.submitter_hex == submitter_hex)
            .collect()
    }

    /// Total number of billing records.
    pub fn count(&self) -> usize {
        self.records.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id_byte: u8, submitter: &str, spent: u64) -> JobBillingRecord {
        JobBillingRecord {
            job_id: [id_byte; 32],
            submitter_hex: submitter.into(),
            comme_spent: spent,
            cpu_cores_used: 4,
            gpu_vram_used: 0,
            ram_used: 2048,
            duration_secs: 120,
            result_hash: [id_byte.wrapping_add(1); 32],
            billed_at_height: 1000,
        }
    }

    #[test]
    fn test_record_and_get() {
        let mut store = BillingStore::new();
        let rec = make_record(1, "alice", 5_000_000);
        assert!(store.record_billing(rec));
        let got = store.get_record(&[1u8; 32]).unwrap();
        assert_eq!(got.submitter_hex, "alice");
        assert_eq!(got.comme_spent, 5_000_000);
    }

    #[test]
    fn test_duplicate_rejected() {
        let mut store = BillingStore::new();
        let rec = make_record(1, "alice", 5_000_000);
        assert!(store.record_billing(rec.clone()));
        assert!(!store.record_billing(rec));
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn test_total_billed() {
        let mut store = BillingStore::new();
        store.record_billing(make_record(1, "alice", 1_000_000));
        store.record_billing(make_record(2, "bob", 2_000_000));
        store.record_billing(make_record(3, "alice", 3_000_000));
        assert_eq!(store.total_billed(), 6_000_000);
    }

    #[test]
    fn test_records_for_address() {
        let mut store = BillingStore::new();
        store.record_billing(make_record(1, "alice", 1_000_000));
        store.record_billing(make_record(2, "bob", 2_000_000));
        store.record_billing(make_record(3, "alice", 3_000_000));
        let alice_records = store.records_for_address("alice");
        assert_eq!(alice_records.len(), 2);
        let bob_records = store.records_for_address("bob");
        assert_eq!(bob_records.len(), 1);
    }

    #[test]
    fn test_records_for_unknown_address() {
        let store = BillingStore::new();
        assert!(store.records_for_address("nobody").is_empty());
    }

    #[test]
    fn test_get_missing() {
        let store = BillingStore::new();
        assert!(store.get_record(&[99u8; 32]).is_none());
    }

    #[test]
    fn test_empty_store() {
        let store = BillingStore::new();
        assert_eq!(store.total_billed(), 0);
        assert_eq!(store.count(), 0);
    }
}
