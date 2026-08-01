use std::collections::HashMap;
use commputer_core::identity::Address;
use commputer_storage::job_pool::{JobId, PoolJob, PoolJobStatus};

/// Maximum concurrent jobs per validator.
pub const MAX_CONCURRENT_JOBS: usize = 5;

/// Validator capacity descriptor for job assignment.
#[derive(Debug, Clone)]
pub struct ValidatorCapacity {
    pub address: Address,
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
    pub storage_mb: u64,
    pub bandwidth_mbps: u64,
    /// Proof scores per resource channel (e.g., "cpu" -> 0.95).
    pub proof_scores: HashMap<String, f64>,
    /// Number of jobs currently assigned to this validator.
    pub current_job_count: usize,
}

impl ValidatorCapacity {
    /// Check if validator can handle a job's resource requirements.
    pub fn can_handle(&self, job: &PoolJob) -> bool {
        self.cpu_cores >= job.cpu_cores
            && self.gpu_vram_mb >= job.gpu_vram_mb
            && self.ram_mb >= job.ram_mb
            && self.storage_mb >= job.storage_mb
            && self.bandwidth_mbps >= job.bandwidth_mbps
            && self.current_job_count < MAX_CONCURRENT_JOBS
    }

    /// Aggregate proof score across relevant channels.
    pub fn aggregate_score(&self) -> f64 {
        if self.proof_scores.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.proof_scores.values().sum();
        sum / self.proof_scores.len() as f64
    }
}

/// Assign pending jobs to validators, respecting the flagship/other split.
///
/// Flagship jobs (commputer-analytics-l2) get priority for `flagship_capacity_pct`
/// of total capacity. Other jobs get the remainder.
pub fn assign_jobs(
    pending: &[PoolJob],
    validators: &[ValidatorCapacity],
    flagship_capacity_pct: u64,
) -> Vec<(JobId, Address)> {
    let mut assignments = Vec::new();
    let mut job_counts: HashMap<Address, usize> = validators
        .iter()
        .map(|v| (v.address, v.current_job_count))
        .collect();

    // Split pending into flagship and other queues
    let mut flagship: Vec<&PoolJob> = pending
        .iter()
        .filter(|j| {
            matches!(j.status, PoolJobStatus::Pending)
                && j.l2_id.as_deref() == Some("commputer-analytics-l2")
        })
        .collect();
    let mut other: Vec<&PoolJob> = pending
        .iter()
        .filter(|j| {
            matches!(j.status, PoolJobStatus::Pending)
                && j.l2_id.as_deref() != Some("commputer-analytics-l2")
        })
        .collect();

    // Sort both queues by budget (highest first)
    flagship.sort_by(|a, b| b.comme_budget.cmp(&a.comme_budget));
    other.sort_by(|a, b| b.comme_budget.cmp(&a.comme_budget));

    // Calculate total available slots
    let total_slots: usize = validators
        .iter()
        .map(|v| MAX_CONCURRENT_JOBS.saturating_sub(v.current_job_count))
        .sum();

    let flagship_slots = flagship_slots(total_slots, flagship_capacity_pct);
    let other_slots = other_slots(total_slots, flagship_capacity_pct);

    // Assign flagship jobs first
    let mut flagship_assigned = 0usize;
    for job in &flagship {
        if flagship_assigned >= flagship_slots {
            break;
        }
        if let Some(addr) = best_validator(job, validators, &job_counts) {
            assignments.push((job.job_id, addr));
            *job_counts.entry(addr).or_default() += 1;
            flagship_assigned += 1;
        }
    }

    // If flagship didn't use all its slots, overflow to other
    let overflow = flagship_slots.saturating_sub(flagship_assigned);
    let effective_other_slots = other_slots + overflow;

    let mut other_assigned = 0usize;
    for job in &other {
        if other_assigned >= effective_other_slots {
            break;
        }
        if let Some(addr) = best_validator(job, validators, &job_counts) {
            assignments.push((job.job_id, addr));
            *job_counts.entry(addr).or_default() += 1;
            other_assigned += 1;
        }
    }

    assignments
}

/// Find the best validator for a job based on proof scores and capacity.
fn best_validator(
    job: &PoolJob,
    validators: &[ValidatorCapacity],
    job_counts: &HashMap<Address, usize>,
) -> Option<Address> {
    let mut best: Option<(Address, f64)> = None;

    for v in validators {
        let count = job_counts.get(&v.address).copied().unwrap_or(0);
        if count >= MAX_CONCURRENT_JOBS {
            continue;
        }
        // Check resource requirements using a temporary adjusted capacity
        if v.cpu_cores < job.cpu_cores
            || v.gpu_vram_mb < job.gpu_vram_mb
            || v.ram_mb < job.ram_mb
            || v.storage_mb < job.storage_mb
            || v.bandwidth_mbps < job.bandwidth_mbps
        {
            continue;
        }
        let score = v.aggregate_score();
        if best.is_none() || score > best.unwrap().1 {
            best = Some((v.address, score));
        }
    }

    best.map(|(addr, _)| addr)
}

/// Calculate flagship slot count from total.
fn flagship_slots(total: usize, flagship_pct: u64) -> usize {
    // ceil(total * pct / 100)
    (total as u64 * flagship_pct).div_ceil(100) as usize
}

/// Calculate other slot count from total.
fn other_slots(total: usize, flagship_pct: u64) -> usize {
    total - flagship_slots(total, flagship_pct)
}

// ── Feature 58: Capacity Tracker with 51/49 split ──

/// Tracks capacity allocation between flagship and other job queues.
#[derive(Debug, Clone)]
pub struct CapacityTracker {
    pub total_slots: usize,
    pub flagship_used: usize,
    pub other_used: usize,
}

impl CapacityTracker {
    pub fn new(total_slots: usize) -> Self {
        Self {
            total_slots,
            flagship_used: 0,
            other_used: 0,
        }
    }

    /// Flagship capacity: ceil(total * 51 / 100).
    pub fn flagship_slots(&self) -> usize {
        capacity_flagship_slots(self.total_slots)
    }

    /// Other capacity: total - flagship_slots.
    pub fn other_slots(&self) -> usize {
        capacity_other_slots(self.total_slots)
    }

    /// Can accept a flagship job?
    pub fn can_accept_flagship(&self) -> bool {
        self.flagship_used < self.flagship_slots()
    }

    /// Can accept a non-flagship job?
    /// When flagship queue is empty (flagship_used == 0), its slots overflow to other.
    pub fn can_accept_other(&self) -> bool {
        let effective_other = if self.flagship_used == 0 {
            // All slots available for other
            self.total_slots
        } else {
            self.other_slots()
        };
        self.other_used < effective_other
    }

    /// Record a flagship job assignment.
    pub fn assign_flagship(&mut self) -> bool {
        if self.can_accept_flagship() {
            self.flagship_used += 1;
            true
        } else {
            false
        }
    }

    /// Record an other job assignment.
    pub fn assign_other(&mut self) -> bool {
        if self.can_accept_other() {
            self.other_used += 1;
            true
        } else {
            false
        }
    }

    /// Total used slots.
    pub fn total_used(&self) -> usize {
        self.flagship_used + self.other_used
    }

    /// Total remaining slots.
    pub fn remaining(&self) -> usize {
        self.total_slots.saturating_sub(self.total_used())
    }
}

/// Flagship slots: ceil(total * 51 / 100).
pub fn capacity_flagship_slots(total: usize) -> usize {
    (total as u64 * 51).div_ceil(100) as usize
}

/// Other slots: total - flagship_slots.
pub fn capacity_other_slots(total: usize) -> usize {
    total - capacity_flagship_slots(total)
}

/// Check if tracker can accept a flagship job.
pub fn can_accept_flagship(tracker: &CapacityTracker) -> bool {
    tracker.can_accept_flagship()
}

/// Check if tracker can accept a non-flagship job.
pub fn can_accept_other(tracker: &CapacityTracker) -> bool {
    tracker.can_accept_other()
}

// ── Feature 60: Job Capacity Tracking ──

/// Per-validator job capacity with resource reservation.
#[derive(Debug, Clone)]
pub struct ValidatorJobCapacity {
    pub max_concurrent: usize,
    pub current_count: usize,
    pub reserved_cpu: u16,
    pub reserved_gpu_vram: u64,
    pub reserved_ram: u64,
    pub total_cpu: u16,
    pub total_gpu_vram: u64,
    pub total_ram: u64,
}

/// Calculate max concurrent jobs based on contribution percent and hardware.
pub fn calculate_max_concurrent(contribution_percent: u8, cpu_cores: u16, ram_mb: u64) -> usize {
    // Base concurrent jobs from CPU cores (1 job per 2 cores)
    let cpu_based = (cpu_cores / 2) as usize;
    // Base from RAM (1 job per 4 GB)
    let ram_based = (ram_mb / 4096) as usize;
    // Take the minimum of CPU and RAM based limits
    let hardware_limit = cpu_based.min(ram_based).max(1);
    // Scale by contribution percent
    let scaled = (hardware_limit as u64 * contribution_percent as u64 / 100) as usize;
    // At least 1, at most MAX_CONCURRENT_JOBS
    scaled.max(1).min(MAX_CONCURRENT_JOBS)
}

/// Reserve resources for a job on a validator.
pub fn reserve_resources(capacity: &mut ValidatorJobCapacity, job: &PoolJob) -> bool {
    if capacity.current_count >= capacity.max_concurrent {
        return false;
    }
    let new_cpu = capacity.reserved_cpu + job.cpu_cores;
    let new_gpu = capacity.reserved_gpu_vram + job.gpu_vram_mb;
    let new_ram = capacity.reserved_ram + job.ram_mb;

    if new_cpu > capacity.total_cpu || new_gpu > capacity.total_gpu_vram || new_ram > capacity.total_ram {
        return false;
    }

    capacity.reserved_cpu = new_cpu;
    capacity.reserved_gpu_vram = new_gpu;
    capacity.reserved_ram = new_ram;
    capacity.current_count += 1;
    true
}

/// Release resources when a job completes or is cancelled.
pub fn release_resources(capacity: &mut ValidatorJobCapacity, job: &PoolJob) {
    capacity.reserved_cpu = capacity.reserved_cpu.saturating_sub(job.cpu_cores);
    capacity.reserved_gpu_vram = capacity.reserved_gpu_vram.saturating_sub(job.gpu_vram_mb);
    capacity.reserved_ram = capacity.reserved_ram.saturating_sub(job.ram_mb);
    capacity.current_count = capacity.current_count.saturating_sub(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_address(byte: u8) -> Address {
        Address([byte; 32])
    }

    fn make_job_id(byte: u8) -> JobId {
        JobId([byte; 32])
    }

    fn make_pending_job(id_byte: u8, budget: u64, l2_id: Option<&str>) -> PoolJob {
        PoolJob {
            job_id: make_job_id(id_byte),
            submitter: make_address(0xFF),
            comme_budget: budget,
            cpu_cores: 2,
            gpu_vram_mb: 4096,
            ram_mb: 8192,
            storage_mb: 50000,
            bandwidth_mbps: 100,
            max_duration_secs: 3600,
            job_spec_hash: [id_byte; 32],
            status: PoolJobStatus::Pending,
            submitted_height: 100,
            l2_id: l2_id.map(|s| s.to_string()),
        }
    }

    fn make_validator(byte: u8, score: f64) -> ValidatorCapacity {
        let mut proof_scores = HashMap::new();
        proof_scores.insert("cpu".to_string(), score);
        proof_scores.insert("gpu".to_string(), score);
        ValidatorCapacity {
            address: make_address(byte),
            cpu_cores: 16,
            gpu_vram_mb: 16384,
            ram_mb: 65536,
            storage_mb: 1000000,
            bandwidth_mbps: 1000,
            proof_scores,
            current_job_count: 0,
        }
    }

    #[test]
    fn test_assign_jobs_basic() {
        let jobs = vec![make_pending_job(1, 1000, None)];
        let validators = vec![make_validator(1, 0.9)];
        let assignments = assign_jobs(&jobs, &validators, 51);
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].0, make_job_id(1));
    }

    #[test]
    fn test_assign_prefers_higher_score() {
        let jobs = vec![make_pending_job(1, 1000, None)];
        let validators = vec![make_validator(1, 0.5), make_validator(2, 0.9)];
        let assignments = assign_jobs(&jobs, &validators, 51);
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].1, make_address(2));
    }

    #[test]
    fn test_max_concurrent_limit() {
        let jobs: Vec<PoolJob> = (0..6).map(|i| make_pending_job(i, 1000, None)).collect();
        let validators = vec![make_validator(1, 0.9)]; // Only 1 validator, max 5
        let assignments = assign_jobs(&jobs, &validators, 51);
        assert_eq!(assignments.len(), MAX_CONCURRENT_JOBS);
    }

    #[test]
    fn test_flagship_priority() {
        // 2 validators with 0 current jobs = 10 total slots
        // 51% = 6 flagship slots, 49% = 4 other slots
        let mut flagship_jobs: Vec<PoolJob> = (0..4)
            .map(|i| make_pending_job(i, 2000, Some("commputer-analytics-l2")))
            .collect();
        let mut other_jobs: Vec<PoolJob> = (10..17)
            .map(|i| make_pending_job(i, 1000, None))
            .collect();
        let mut all_jobs = Vec::new();
        all_jobs.append(&mut flagship_jobs);
        all_jobs.append(&mut other_jobs);

        let validators = vec![make_validator(1, 0.9), make_validator(2, 0.8)];
        let assignments = assign_jobs(&all_jobs, &validators, 51);

        // Should assign all 4 flagship + some other jobs
        let flagship_assigned: Vec<_> = assignments
            .iter()
            .filter(|(id, _)| id.0[0] < 10)
            .collect();
        assert_eq!(flagship_assigned.len(), 4);
    }

    #[test]
    fn test_resource_mismatch_skips_validator() {
        let mut job = make_pending_job(1, 1000, None);
        job.cpu_cores = 32; // More than any validator has
        let validators = vec![make_validator(1, 0.9)];
        let assignments = assign_jobs(&[job], &validators, 51);
        assert!(assignments.is_empty());
    }

    #[test]
    fn test_no_validators_no_assignments() {
        let jobs = vec![make_pending_job(1, 1000, None)];
        let assignments = assign_jobs(&jobs, &[], 51);
        assert!(assignments.is_empty());
    }

    #[test]
    fn test_no_jobs_no_assignments() {
        let validators = vec![make_validator(1, 0.9)];
        let assignments = assign_jobs(&[], &validators, 51);
        assert!(assignments.is_empty());
    }

    // ── Feature 58 tests ──

    #[test]
    fn test_capacity_flagship_slots_51_pct() {
        assert_eq!(capacity_flagship_slots(100), 51);
        assert_eq!(capacity_other_slots(100), 49);
    }

    #[test]
    fn test_capacity_tracker_rounding() {
        // 10 total: ceil(10 * 51 / 100) = ceil(5.1) = 6
        assert_eq!(capacity_flagship_slots(10), 6);
        assert_eq!(capacity_other_slots(10), 4);
    }

    #[test]
    fn test_capacity_tracker_basic() {
        let mut tracker = CapacityTracker::new(10);
        assert!(tracker.can_accept_flagship());
        assert!(tracker.can_accept_other());

        // Fill flagship slots (6)
        for _ in 0..6 {
            assert!(tracker.assign_flagship());
        }
        assert!(!tracker.can_accept_flagship());

        // Fill other slots (4)
        for _ in 0..4 {
            assert!(tracker.assign_other());
        }
        assert!(!tracker.can_accept_other());
        assert_eq!(tracker.remaining(), 0);
    }

    #[test]
    fn test_flagship_overflow_to_other() {
        let mut tracker = CapacityTracker::new(10);
        // Don't assign any flagship jobs — all 10 slots should be available for other
        assert!(tracker.can_accept_other());
        for _ in 0..10 {
            assert!(tracker.assign_other());
        }
        assert!(!tracker.can_accept_other());
    }

    #[test]
    fn test_flagship_partial_usage_no_overflow() {
        let mut tracker = CapacityTracker::new(10);
        // Assign 1 flagship job — flagship queue is non-empty, so no overflow
        tracker.assign_flagship();
        // Other slots = 4
        for _ in 0..4 {
            assert!(tracker.assign_other());
        }
        assert!(!tracker.can_accept_other());
    }

    #[test]
    fn test_capacity_single_slot() {
        // 1 total: ceil(1 * 51 / 100) = 1 flagship, 0 other
        assert_eq!(capacity_flagship_slots(1), 1);
        assert_eq!(capacity_other_slots(1), 0);

        let tracker = CapacityTracker::new(1);
        assert!(tracker.can_accept_flagship());
        // Other can use all slots when flagship is empty
        assert!(tracker.can_accept_other());
    }

    // ── Feature 60 tests ──

    #[test]
    fn test_calculate_max_concurrent() {
        // 16 cores, 65536 MB RAM, 100% contribution
        // cpu_based = 8, ram_based = 16, min = 8, scaled = 8, capped at 5
        assert_eq!(calculate_max_concurrent(100, 16, 65536), MAX_CONCURRENT_JOBS);
    }

    #[test]
    fn test_calculate_max_concurrent_low_contribution() {
        // 16 cores, 65536 MB, 10% contribution
        // cpu_based = 8, ram_based = 16, min = 8, scaled = 0.8 -> max(1, 0) = 1
        assert_eq!(calculate_max_concurrent(10, 16, 65536), 1);
    }

    #[test]
    fn test_calculate_max_concurrent_minimum() {
        // Minimum is always 1
        assert_eq!(calculate_max_concurrent(1, 1, 1024), 1);
    }

    #[test]
    fn test_reserve_resources_success() {
        let mut cap = ValidatorJobCapacity {
            max_concurrent: 3,
            current_count: 0,
            reserved_cpu: 0,
            reserved_gpu_vram: 0,
            reserved_ram: 0,
            total_cpu: 16,
            total_gpu_vram: 16384,
            total_ram: 65536,
        };
        let job = make_pending_job(1, 1000, None);
        assert!(reserve_resources(&mut cap, &job));
        assert_eq!(cap.current_count, 1);
        assert_eq!(cap.reserved_cpu, 2);
        assert_eq!(cap.reserved_ram, 8192);
    }

    #[test]
    fn test_reserve_resources_max_concurrent() {
        let mut cap = ValidatorJobCapacity {
            max_concurrent: 1,
            current_count: 1,
            reserved_cpu: 0,
            reserved_gpu_vram: 0,
            reserved_ram: 0,
            total_cpu: 16,
            total_gpu_vram: 16384,
            total_ram: 65536,
        };
        let job = make_pending_job(1, 1000, None);
        assert!(!reserve_resources(&mut cap, &job));
    }

    #[test]
    fn test_reserve_resources_insufficient_cpu() {
        let mut cap = ValidatorJobCapacity {
            max_concurrent: 5,
            current_count: 0,
            reserved_cpu: 15,
            reserved_gpu_vram: 0,
            reserved_ram: 0,
            total_cpu: 16,
            total_gpu_vram: 16384,
            total_ram: 65536,
        };
        let job = make_pending_job(1, 1000, None); // needs 2 cores
        assert!(!reserve_resources(&mut cap, &job));
    }

    #[test]
    fn test_release_resources() {
        let mut cap = ValidatorJobCapacity {
            max_concurrent: 5,
            current_count: 2,
            reserved_cpu: 4,
            reserved_gpu_vram: 8192,
            reserved_ram: 16384,
            total_cpu: 16,
            total_gpu_vram: 16384,
            total_ram: 65536,
        };
        let job = make_pending_job(1, 1000, None);
        release_resources(&mut cap, &job);
        assert_eq!(cap.current_count, 1);
        assert_eq!(cap.reserved_cpu, 2);
        assert_eq!(cap.reserved_ram, 8192);
    }
}
